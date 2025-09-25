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

// --- Tareas Asíncronas ---
// Tarea que escucha en un socket UDP y transmite los paquetes recibidos

pub fn spawn_output_sender(
    mut packet_rx: Receiver<Bytes>,
    dest_addr: SocketAddr,
    input_id: i64,
    output_id: i64,
) -> AbortHandle {
    let (abort_handle, reg) = futures::future::AbortHandle::new_pair();
    let out_input = input_id;
    let out_output = output_id;

    tokio::spawn(Abortable::new(async move {
        let sock = UdpSocket::bind("0.0.0.0:0")
            .await
            .expect("bind local udp"); // no debería fallar
        loop {
            match packet_rx.recv().await {
                Ok(bytes) => {
                    let buf = bytes;
                    let bytes_sent = buf.len() as u64;
                    if sock.send_to(&buf, dest_addr).await.is_ok() {
                        // Record metrics for successful send
                        metrics::record_output_bytes(&input_id.to_string(), &output_id.to_string(), "udp", bytes_sent);
                        metrics::record_output_packets(&input_id.to_string(), &output_id.to_string(), "udp", 1);
                    } else {
                        // Record error metric
                        metrics::record_stream_error("udp", "send_failed");
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
    state_tx: Option<StateChangeSender>,
) -> Result<OutputInfo, actix_web::Error> {
    // Resolver la dirección una sola vez
    let dest_addr = tokio::net::lookup_host(&destination_addr)
        .await
        .map_err(|e| ErrorBadRequest(format!("Error resolviendo '{}': {e}", destination_addr)))?
        .next()
        .ok_or_else(|| ErrorBadRequest(format!("No se pudo resolver '{}'", destination_addr)))?;

    let packet_rx = input.packet_tx.subscribe();
    let abort_handle =
        spawn_output_sender(packet_rx, dest_addr, input_id, output_id);

    // Increment active outputs counter
    metrics::increment_active_outputs();

    let final_name = name.or(Some(format!("UDP Output to {}", destination_addr)));
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
            destination_addr: Some(destination_addr),
        },
        started_at: Some(std::time::SystemTime::now()),
        connected_at: Some(std::time::SystemTime::now()), // UDP outputs are immediately connected
        state_tx,
    })
}

pub fn spawn_udp_input_with_stats(
    id: i64,
    name: Option<String>,
    listen_port: u16,
    state_tx: Option<StateChangeSender>,
) -> Result<InputInfo, actix_web::Error> {
    // canal interno
    let (tx, _rx) = broadcast::channel::<Bytes>(1024);
    let stats: StatsCell = Arc::new(RwLock::new(None));

    let tx_for_task = tx.clone();
    let stats_task = stats.clone();
    let state_tx_task = state_tx.clone();

    // tarea: leer de UDP y publicar en broadcast
    let handle = tokio::spawn(async move {
        use tokio::net::UdpSocket;
        use tokio::time::timeout;

        let sock = match UdpSocket::bind(("0.0.0.0", listen_port)).await {
            Ok(s) => {
                // Notify listening state
                if let Some(ref tx) = state_tx_task {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: id,
                        new_status: StreamStatus::Listening,
                        connected_at: None,
                    });
                }
                s
            },
            Err(e) => {
                eprintln!("Error binding UDP socket on port {}: {}", listen_port, e);
                // Notify error state
                if let Some(ref tx) = state_tx_task {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: id,
                        new_status: StreamStatus::Error,
                        connected_at: None,
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
            match timeout(Duration::from_millis(500), sock.recv(&mut buf)).await {
                Ok(Ok(n)) => {
                    let _ = tx_for_task.send(Bytes::copy_from_slice(&buf[..n]));
                    total_bytes += n as u64;
                    total_packets += 1;

                    // Record metrics for received data
                    metrics::record_input_bytes(&id.to_string(), "udp", n as u64);
                    metrics::record_input_packets(&id.to_string(), "udp", 1);

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
            bind_host: None,
            bind_port: None,
            automatic_port: None,
            name,
            listen_port: Some(listen_port)
        },
        output_tasks: std::collections::HashMap::new(),
        stopped_outputs: std::collections::HashMap::new(),
        analysis_tasks: std::collections::HashMap::new(),
        paused_analysis: Vec::new(),
        started_at: Some(std::time::SystemTime::now()),
        connected_at: None, // Will be set when first packet arrives
        state_tx,
    })
}