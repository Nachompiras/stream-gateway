use actix_web::error::ErrorBadRequest;
use bytes::Bytes;
use futures::stream::AbortHandle;
use log::{info, error, warn};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use std::net::SocketAddr;

use crate::models::{
    CreateOutputRequest, InputInfo, OutputInfo, OutputKind, SpsDestinationConfig,
    SrtOutputConfig, StateChangeSender, StreamStatus
};
use crate::mpegts_filter::ProgramFilter;
use crate::metrics;
use crate::udp_stream::MulticastOutputConfig;

/// Create an SPTS output that filters a specific program from an MPTS input
pub async fn create_spts_output(
    input: &InputInfo,
    output_id: i64,
    name: Option<String>,
    program_number: u16,
    fill_with_nulls: bool,
    destination: SpsDestinationConfig,
    state_tx: Option<StateChangeSender>,
) -> Result<OutputInfo, actix_web::Error> {
    info!("Creating SPTS output {} for program {} from input {}",
          output_id, program_number, input.id);

    // Generate appropriate name
    let final_name = name.or(Some(format!("SPTS Program {} Output", program_number)));

    // Subscribe to input packets
    let packet_rx = input.packet_tx.subscribe();

    // Create the config for storage
    let config = CreateOutputRequest::Spts {
        input_id: input.id,
        name: final_name.clone(),
        program_number,
        fill_with_nulls: Some(fill_with_nulls),
        destination: destination.clone(),
    };

    // Determine destination address for display
    let destination_addr = match &destination {
        SpsDestinationConfig::Udp { remote_host, remote_port, destination_addr, .. } => {
            if let Some(ref addr) = destination_addr {
                addr.clone()
            } else {
                let host = remote_host.as_deref().unwrap_or("127.0.0.1");
                let port = remote_port.unwrap_or(8000);
                format!("{}:{}", host, port)
            }
        },
        SpsDestinationConfig::Srt { config } => {
            match config {
                SrtOutputConfig::Caller { remote_host, remote_port, destination_addr, .. } => {
                    if let Some(ref addr) = destination_addr {
                        addr.clone()
                    } else {
                        let host = remote_host.as_deref().unwrap_or("127.0.0.1");
                        let port = remote_port.unwrap_or(8000);
                        format!("{}:{}", host, port)
                    }
                },
                SrtOutputConfig::Listener { bind_port, listen_port, .. } => {
                    let port = listen_port.or(*bind_port).unwrap_or(8000);
                    format!(":{}", port)
                },
            }
        }
    };

    // Spawn the filtering and sending task
    let (abort_handle, status) = match destination {
        SpsDestinationConfig::Udp { remote_host, remote_port, destination_addr, bind_host, multicast_ttl, multicast_interface, .. } => {
            let dest_str = if let Some(ref addr) = destination_addr {
                addr.clone()
            } else {
                let host = remote_host.as_deref().unwrap_or("127.0.0.1");
                let port = remote_port.unwrap_or(8000);
                format!("{}:{}", host, port)
            };

            let dest_addr = tokio::net::lookup_host(&dest_str)
                .await
                .map_err(|e| ErrorBadRequest(format!("Error resolving '{}': {}", dest_str, e)))?
                .next()
                .ok_or_else(|| ErrorBadRequest(format!("Could not resolve '{}'", dest_str)))?;

            let multicast_config = if let Some(ref host) = remote_host {
                if let Ok(addr) = host.parse::<std::net::IpAddr>() {
                    if addr.is_multicast() {
                        Some(MulticastOutputConfig {
                            ttl: multicast_ttl.unwrap_or(1),
                            interface: multicast_interface.clone(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let handle = spawn_spts_udp_sender(
                packet_rx,
                dest_addr,
                program_number,
                fill_with_nulls,
                input.id,
                output_id,
                bind_host.clone(),
                multicast_config,
            );

            (handle, StreamStatus::Connected) // UDP is immediately connected
        },
        SpsDestinationConfig::Srt { config } => {
            let handle = spawn_spts_srt_sender(
                packet_rx,
                config.clone(),
                program_number,
                fill_with_nulls,
                input.id,
                output_id,
            );

            let status = match config {
                SrtOutputConfig::Listener { .. } => StreamStatus::Listening,
                SrtOutputConfig::Caller { .. } => StreamStatus::Connecting,
            };

            (handle, status)
        },
    };

    // Increment active outputs counter
    metrics::increment_active_outputs();

    let is_connected = status == StreamStatus::Connected;

    Ok(OutputInfo {
        id: output_id,
        name: final_name,
        input_id: input.id,
        destination: destination_addr,
        kind: OutputKind::Spts,
        status,
        stats: Arc::new(RwLock::new(None)),
        abort_handle: Some(abort_handle),
        config,
        started_at: Some(std::time::SystemTime::now()),
        connected_at: if is_connected {
            Some(std::time::SystemTime::now())
        } else {
            None
        },
        state_tx,
        peer_address: None,
    })
}

/// Spawn a task that filters SPTS and sends via UDP
fn spawn_spts_udp_sender(
    mut packet_rx: broadcast::Receiver<Bytes>,
    dest_addr: SocketAddr,
    program_number: u16,
    fill_with_nulls: bool,
    input_id: i64,
    output_id: i64,
    bind_host: Option<String>,
    multicast_config: Option<MulticastOutputConfig>,
) -> AbortHandle {
    info!("[Input {}] Spawning SPTS UDP sender for output {} to {} (program {})",
          input_id, output_id, dest_addr, program_number);

    let (abort_handle, abort_registration) = AbortHandle::new_pair();

    tokio::spawn(futures::future::Abortable::new(
        async move {
            // Create UDP socket
            let socket = match socket2::Socket::new(
                socket2::Domain::for_address(dest_addr),
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            ) {
                Ok(s) => s,
                Err(e) => {
                    error!("[Output {}] Failed to create UDP socket: {}", output_id, e);
                    return;
                }
            };

            // Bind to specific interface if requested
            if let Some(ref bind_addr) = bind_host {
                if let Ok(addr) = bind_addr.parse::<std::net::IpAddr>() {
                    let bind_socket_addr = SocketAddr::new(addr, 0);
                    if let Err(e) = socket.bind(&socket2::SockAddr::from(bind_socket_addr)) {
                        error!("[Output {}] Failed to bind to {}: {}", output_id, bind_addr, e);
                        return;
                    }
                    info!("[Output {}] Bound to interface {}", output_id, bind_addr);
                }
            }

            // Configure multicast if needed
            if let Some(ref mc_config) = multicast_config {
                if let Err(e) = socket.set_multicast_ttl_v4(mc_config.ttl as u32) {
                    warn!("[Output {}] Failed to set multicast TTL: {}", output_id, e);
                }

                if let Some(ref interface) = mc_config.interface {
                    if let Ok(addr) = interface.parse::<std::net::Ipv4Addr>() {
                        if let Err(e) = socket.set_multicast_if_v4(&addr) {
                            warn!("[Output {}] Failed to set multicast interface: {}", output_id, e);
                        } else {
                            info!("[Output {}] Set multicast interface to {}", output_id, interface);
                        }
                    }
                }
            }

            // Connect to destination
            if let Err(e) = socket.connect(&socket2::SockAddr::from(dest_addr)) {
                error!("[Output {}] Failed to connect to {}: {}", output_id, dest_addr, e);
                return;
            }

            let socket: std::net::UdpSocket = socket.into();
            socket.set_nonblocking(true).ok();
            let socket = tokio::net::UdpSocket::from_std(socket).ok();
            let Some(socket) = socket else {
                error!("[Output {}] Failed to convert to tokio socket", output_id);
                return;
            };

            // Create program filter
            let mut filter = ProgramFilter::new(program_number, fill_with_nulls);

            info!("[Output {}] SPTS UDP sender started for program {}", output_id, program_number);

            // Main loop: receive packets, filter, and send
            loop {
                match packet_rx.recv().await {
                    Ok(data) => {
                        // Filter packets for our program
                        let filtered = filter.filter_packets(&data);

                        // Only send if we have filtered data
                        if !filtered.is_empty() {
                            if let Err(e) = socket.send(&filtered).await {
                                error!("[Output {}] Error sending UDP data: {}", output_id, e);
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[Output {}] Receiver lagged by {} messages", output_id, n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("[Output {}] Input stream closed", output_id);
                        break;
                    }
                }
            }

            info!("[Output {}] SPTS UDP sender stopped", output_id);
        },
        abort_registration,
    ));

    abort_handle
}

/// Spawn a task that filters SPTS and sends via SRT
fn spawn_spts_srt_sender(
    mut packet_rx: broadcast::Receiver<Bytes>,
    _config: SrtOutputConfig,
    program_number: u16,
    fill_with_nulls: bool,
    input_id: i64,
    output_id: i64,
) -> AbortHandle {
    info!("[Input {}] Spawning SPTS SRT sender for output {} (program {})",
          input_id, output_id, program_number);

    let (abort_handle, abort_registration) = AbortHandle::new_pair();

    tokio::spawn(futures::future::Abortable::new(
        async move {
            // Create program filter
            let mut filter = ProgramFilter::new(program_number, fill_with_nulls);

            info!("[Output {}] SPTS SRT sender started for program {}", output_id, program_number);

            // TODO: Implement SRT connection and sending
            // For now, just log that this is not fully implemented
            warn!("[Output {}] SPTS SRT output not fully implemented yet, filtering only", output_id);

            // Main loop: receive packets and filter (but not send yet)
            loop {
                match packet_rx.recv().await {
                    Ok(data) => {
                        // Filter packets for our program
                        let _filtered = filter.filter_packets(&data);

                        // TODO: Send filtered data via SRT
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[Output {}] Receiver lagged by {} messages", output_id, n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("[Output {}] Input stream closed", output_id);
                        break;
                    }
                }
            }

            info!("[Output {}] SPTS SRT sender stopped", output_id);
        },
        abort_registration,
    ));

    abort_handle
}
