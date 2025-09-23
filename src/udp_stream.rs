use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use actix_web::error::ErrorBadRequest;
use bytes::Bytes;
use tokio::{net::UdpSocket, sync::{broadcast::{self, Receiver}, RwLock}};
use futures::stream::{AbortHandle, Abortable};
use log::{info};
use crate::models::*;

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
        let mut buf = Bytes::new();
        loop {
            match packet_rx.recv().await {
                Ok(bytes) => {
                    buf = bytes;
                    let _ = sock.send_to(&buf, dest_addr).await;
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

    let final_name = name.or(Some(format!("UDP Output to {}", destination_addr)));
    Ok(OutputInfo {
        id: output_id,
        name: final_name,
        input_id,
        destination: destination_addr,
        kind: OutputKind::Udp,
        stats: Arc::new(RwLock::new(None)),
        abort_handle,
    })
}

pub fn spawn_udp_input_with_stats(
    id: i64,
    name: Option<String>,
    details: String,
    listen_port: u16,
) -> Result<InputInfo, actix_web::Error> {
    // canal interno
    let (tx, _rx) = broadcast::channel::<Bytes>(1024);
    let stats: StatsCell = Arc::new(RwLock::new(None));

    let tx_for_task = tx.clone();
    let stats_task = stats.clone();

    // tarea: leer de UDP y publicar en broadcast
    let handle = tokio::spawn(async move {
        use tokio::net::UdpSocket;
        use tokio::time::timeout;

        let sock = match UdpSocket::bind(("0.0.0.0", listen_port)).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error binding UDP socket on port {}: {}", listen_port, e);
                return;
            }
        };
                   
        let mut buf = [0u8; 2048];
        let mut total_bytes   = 0u64;
        let mut total_packets = 0u64;
        let mut window_bytes  = 0u64;
        let mut window_pkts   = 0u64;
        let mut window_start  = Instant::now();

        loop {
            // Use timeout to make recv responsive to cancellation
            match timeout(Duration::from_millis(500), sock.recv(&mut buf)).await {
                Ok(Ok(n)) => {
                    let _ = tx_for_task.send(Bytes::copy_from_slice(&buf[..n]));
                    total_bytes += n as u64;
                    total_packets += 1;
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
                    // Timeout - check if we should continue
                    if tx_for_task.receiver_count() == 0 {
                        println!("UDP input {listen_port}: no receivers, stopping");
                        break;
                    }
                    // Continue loop if there are still receivers
                }
            }
        }
        println!("UDP input {listen_port}: shutdown completed");
    });

    Ok(InputInfo {
        id,
        name,
        details,
        packet_tx: tx,
        task_handle: handle,
        output_tasks: std::collections::HashMap::new(),
        analysis_tasks: std::collections::HashMap::new(),
        stats,
    })
}