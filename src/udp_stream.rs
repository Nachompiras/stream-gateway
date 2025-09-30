use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use actix_web::error::ErrorBadRequest;
use bytes::Bytes;
use tokio::{net::UdpSocket, sync::{broadcast::{self, Receiver}, RwLock}};
use futures::stream::{AbortHandle, Abortable};
use log::{info};
use crate::models::*;
use crate::metrics;
use std::time::SystemTime;
use socket2::{Socket, Domain, Type, Protocol, SockAddr};

#[derive(Debug, Clone)]
pub struct MulticastOutputConfig {
    pub ttl: u8,
    pub interface: Option<String>,
}

// --- Tareas Asíncronas ---
// Tarea que escucha en un socket UDP y transmite los paquetes recibidos

pub fn spawn_output_sender(
    mut packet_rx: Receiver<Bytes>,
    dest_addr: SocketAddr,
    input_id: i64,
    output_id: i64,
    bind_host: Option<String>,
    multicast_config: Option<MulticastOutputConfig>,
) -> AbortHandle {
    let (abort_handle, reg) = futures::future::AbortHandle::new_pair();
    let out_input = input_id;
    let out_output = output_id;

    tokio::spawn(Abortable::new(async move {
        let sock = match create_multicast_output_socket(bind_host.as_deref(), multicast_config.as_ref()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error creating UDP output socket: {}", e);
                return;
            }
        };
        loop {
            match packet_rx.recv().await {
                Ok(bytes) => {
                    let buf = bytes;
                    let bytes_sent = buf.len() as u64;
                    if sock.send_to(&buf, dest_addr).await.is_ok() {
                        // Record metrics for successful send
                        metrics::record_output_bytes(&output_name, input_id, output_id, "udp", bytes_sent);
                        metrics::record_output_packets(&output_name, input_id, output_id, "udp", 1);
                    } else {
                        // Record error metric
                        metrics::record_stream_error(&output_name, output_id, "udp", "send_failed");
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
        info!(
            "[{}] Tarea UDP output '{}' terminada",
            out_input, out_output
        );
    }, reg));

    abort_handle
}

pub async fn create_udp_output(
    input_id: i64,
    destination_addr: String,
    input: &InputInfo,
    output_id: i64,
    name: Option<String>,
    bind_host: Option<String>,
    multicast_config: Option<MulticastOutputConfig>,
    state_tx: Option<StateChangeSender>,
) -> Result<OutputInfo, actix_web::Error> {
    // Resolver la dirección una sola vez
    let dest_addr = tokio::net::lookup_host(&destination_addr)
        .await
        .map_err(|e| ErrorBadRequest(format!("Error resolviendo '{}': {e}", destination_addr)))?
        .next()
        .ok_or_else(|| ErrorBadRequest(format!("No se pudo resolver '{}'", destination_addr)))?;

    let final_name = name.or(Some(format!("UDP Output to {}", destination_addr)));
    let packet_rx = input.packet_tx.subscribe();
    let abort_handle =
        spawn_output_sender(packet_rx, dest_addr, input_id, output_id, bind_host.clone(), multicast_config.clone());

    // Increment active outputs counter
    metrics::increment_active_outputs();
    Ok(OutputInfo {
        id: output_id,
        name: final_name.clone(),
        input_id,
        destination: destination_addr.clone(),
        kind: OutputKind::Udp,
        status: StreamStatus::Connected, // UDP outputs are immediately connected
        stats: Arc::new(RwLock::new(None)),
        abort_handle: Some(abort_handle),
        config: CreateOutputRequest::Udp {
            input_id,
            remote_host: None,
            remote_port: None,
            automatic_port: None,
            name: final_name,
            bind_host: bind_host,
            multicast_ttl: multicast_config.as_ref().map(|c| c.ttl),
            multicast_interface: multicast_config.as_ref().and_then(|c| c.interface.clone()),
            destination_addr: Some(destination_addr),
        },
        started_at: Some(std::time::SystemTime::now()),
        connected_at: Some(std::time::SystemTime::now()), // UDP outputs are immediately connected
        state_tx,
        peer_address: None, // UDP doesn't track connected peers
    })
}

pub fn spawn_udp_input_with_stats(
    id: i64,
    name: Option<String>,
    listen_port: u16,
    bind_host: Option<String>,
    multicast_group: Option<String>,
    source_specific_multicast: Option<String>,
    state_tx: Option<StateChangeSender>,
) -> Result<InputInfo, actix_web::Error> {
    // canal interno
    let (tx, _rx) = broadcast::channel::<Bytes>(1024);
    let stats: StatsCell = Arc::new(RwLock::new(None));

    let tx_for_task = tx.clone();
    let stats_task = stats.clone();
    let state_tx_task = state_tx.clone();
    let bind_host_task = bind_host.clone();
    let multicast_group_task = multicast_group.clone();
    let source_specific_multicast_task = source_specific_multicast.clone();

    // tarea: leer de UDP y publicar en broadcast
    let handle = tokio::spawn(async move {
        use tokio::time::timeout;

        let bind_addr = bind_host_task.as_deref().unwrap_or("0.0.0.0");

        // Create socket with socket2 for multicast support
        let sock = match create_multicast_socket(bind_addr, listen_port, multicast_group_task.as_deref(), source_specific_multicast_task.as_deref()).await {
            Ok(s) => {
                // Notify listening state
                if let Some(ref tx) = state_tx_task {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: id,
                        new_status: StreamStatus::Listening,
                        connected_at: None,
                        source_address: None,
                    });
                }
                s
            },
            Err(e) => {
                eprintln!("Error creating UDP socket on {}:{}: {}", bind_addr, listen_port, e);
                // Notify error state
                if let Some(ref tx) = state_tx_task {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: id,
                        new_status: StreamStatus::Error,
                        connected_at: None,
                        source_address: None,
                    });
                }
                return;
            }
        };
                   
        let mut buf = [0u8; 2048];
        let mut total_bytes   = 0u64;
        let mut total_packets = 0u64;
        let mut window_bytes  = 0u64;
        let mut window_pkts   = 0u64;
        let mut window_start  = Instant::now();
        let mut is_connected  = false; // Track connection state
        let mut last_packet_time = Instant::now(); // Track last packet received
        const IDLE_TIMEOUT: Duration = Duration::from_secs(10); // Timeout to go back to listening

        loop {
            // Use timeout to make recv responsive to cancellation
            match timeout(Duration::from_millis(500), sock.recv_from(&mut buf)).await {
                Ok(Ok((n, peer_addr))) => {
                    //println!("UDP input {listen_port}: received {n} bytes from {peer_addr}");
                    let _ = tx_for_task.send(Bytes::copy_from_slice(&buf[..n]));
                    total_bytes += n as u64;
                    total_packets += 1;

                    // Record metrics for received data
                    metrics::record_input_bytes(&name_for_task, id, "udp", n as u64);
                    metrics::record_input_packets(&name_for_task, id, "udp", 1);

                    // Update last packet time
                    last_packet_time = Instant::now();

                    // Transition to Connected state on first packet
                    if !is_connected {
                        is_connected = true;
                        if let Some(ref tx) = state_tx_task {
                            let _ = tx.send(StateChange::InputStateChanged {
                                input_id: id,
                                new_status: StreamStatus::Connected,
                                connected_at: Some(SystemTime::now()),
                                source_address: Some(format!("{}:{}", peer_addr.ip(), listen_port_for_task)),
                            });
                        }
                    }
                    window_bytes += n as u64;
                    window_pkts += 1;
                    /* actualizar stats cada segundo */
                    if window_start.elapsed() >= Duration::from_secs(1) {
                        let bitrate = window_bytes * 8; // bits/s
                        let pps     = window_pkts;

                        let snapshot = InputStats::Udp(UdpStats {
                            total_packets,
                            total_bytes,
                            packets_per_sec: pps,
                            bitrate_bps: bitrate,
                        });

                        *stats_task.write().await = Some(snapshot);

                        window_start = Instant::now();
                        window_bytes = 0;
                        window_pkts  = 0;
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("udp {listen_port}: {e}");
                    break;
                }
                Err(_) => {
                    // Timeout - check if we should transition back to listening
                    if is_connected && last_packet_time.elapsed() >= IDLE_TIMEOUT {
                        is_connected = false;
                        if let Some(ref tx) = state_tx_task {
                            let _ = tx.send(StateChange::InputStateChanged {
                                input_id: id,
                                new_status: StreamStatus::Listening,
                                connected_at: None,
                                source_address: None,
                            });
                        }
                        println!("UDP input {}: transitioned back to listening due to inactivity", listen_port);
                    }
                    // Continue listening for packets regardless
                }
            }
        }
        println!("UDP input {listen_port}: shutdown completed");
    });

    // Increment active inputs counter
    metrics::increment_active_inputs();

    Ok(InputInfo {
        id,
        name: name.clone(),
        status: StreamStatus::Listening, // Start in listening state, will change to connected on first packet
        packet_tx: tx,
        stats,
        task_handle: Some(handle),
        config: CreateInputRequest::Udp {
            bind_host: bind_host.clone(),
            bind_port: Some(listen_port),
            automatic_port: None,
            name,
            multicast_group: multicast_group.clone(),
            source_specific_multicast: source_specific_multicast.clone(),
            listen_port: Some(listen_port)
        },
        output_tasks: std::collections::HashMap::new(),
        stopped_outputs: std::collections::HashMap::new(),
        analysis_tasks: std::collections::HashMap::new(),
        paused_analysis: Vec::new(),
        started_at: Some(std::time::SystemTime::now()),
        connected_at: None, // Will be set when first packet arrives
        state_tx,
        source_address: None, // UDP doesn't track individual source addresses
    })
}

/// Create a UDP socket with multicast support using socket2
async fn create_multicast_socket(
    bind_addr: &str,
    port: u16,
    multicast_group: Option<&str>,
    source_specific_multicast: Option<&str>,
) -> Result<UdpSocket, std::io::Error> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // Create socket2 socket
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // Enable SO_REUSEADDR to allow multiple processes to bind to the same multicast address
    socket.set_reuse_address(true)?;

    #[cfg(unix)]
    {
        socket.set_reuse_port(true)?;
    }

    // Bind to the address and port
    let bind_sockaddr: SockAddr = format!("{}:{}", bind_addr, port).parse::<SocketAddr>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?.into();
    socket.bind(&bind_sockaddr)?;

    // Join multicast group if specified
    if let Some(group_addr_str) = multicast_group {
        let group_addr: IpAddr = group_addr_str.parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput,
                format!("Invalid multicast group address '{}': {}", group_addr_str, e)))?;

        match group_addr {
            IpAddr::V4(group_ipv4) => {
                // Validate it's a multicast address (224.0.0.0 - 239.255.255.255)
                if !group_ipv4.is_multicast() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
                        format!("Address '{}' is not a valid IPv4 multicast address", group_addr_str)));
                }

                let interface_addr: Ipv4Addr = bind_addr.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);

                if let Some(source_addr_str) = source_specific_multicast {
                    // Source-Specific Multicast (SSM)
                    let source_addr: Ipv4Addr = source_addr_str.parse()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput,
                            format!("Invalid source address '{}': {}", source_addr_str, e)))?;

                    socket.join_ssm_v4(&group_ipv4, &interface_addr, &source_addr)?;
                    info!("Joined SSM group {} with source {} on interface {}", group_ipv4, source_addr, interface_addr);
                } else {
                    // Any-Source Multicast (ASM)
                    socket.join_multicast_v4(&group_ipv4, &interface_addr)?;
                    info!("Joined multicast group {} on interface {}", group_ipv4, interface_addr);
                }
            },
            IpAddr::V6(group_ipv6) => {
                // For IPv6, we need the interface index (0 means any interface)
                let interface_index = 0; // TODO: Could be made configurable
                socket.join_multicast_v6(&group_ipv6, interface_index)?;
                info!("Joined IPv6 multicast group {} on interface index {}", group_ipv6, interface_index);
            },
        }
    }

    // Convert socket2 socket to tokio UdpSocket
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Create a UDP socket for multicast output with proper configuration
async fn create_multicast_output_socket(
    bind_host: Option<&str>,
    multicast_config: Option<&MulticastOutputConfig>,
) -> Result<UdpSocket, std::io::Error> {
    use std::net::{IpAddr, Ipv4Addr};

    // Create socket2 socket
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // Bind to specified interface or any interface
    let bind_addr = if let Some(host) = bind_host {
        format!("{}:0", host)
    } else {
        "0.0.0.0:0".to_string()
    };

    let bind_sockaddr: SockAddr = bind_addr.parse::<SocketAddr>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?.into();
    socket.bind(&bind_sockaddr)?;

    // Configure multicast settings if specified
    if let Some(config) = multicast_config {
        // Set multicast TTL
        socket.set_multicast_ttl_v4(config.ttl as u32)?;

        // Set multicast interface if specified
        if let Some(interface_ip) = &config.interface {
            if let Ok(IpAddr::V4(interface_ipv4)) = interface_ip.parse() {
                socket.set_multicast_if_v4(&interface_ipv4)?;
                info!("Set multicast interface to {}", interface_ip);
            }
        }

        info!("Configured multicast output with TTL {}", config.ttl);
    }

    // Convert socket2 socket to tokio UdpSocket
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}