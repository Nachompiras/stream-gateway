use bytes::Bytes;
use log::{info, warn, error};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;

use crate::models::*;
use crate::mpegts_filter::ProgramFilter;
use crate::metrics;
use crate::GLOBAL_CANCEL_TOKEN;

/// Spawn an SPTS input that filters a specific program from an MPTS source
///
/// This creates a new input stream that subscribes to an existing MPTS input,
/// filters packets for a specific program number, and provides its own broadcast
/// channel that outputs can subscribe to.
///
/// # Performance Considerations
/// - Uses 16384 slot broadcast buffer (same as UDP inputs) for high bitrate MPTS
/// - Zero-copy filtering with Bytes references
/// - Lock-free atomic statistics updates
/// - Handles source lag gracefully without blocking
pub fn spawn_spts_input(
    source_input: &InputInfo,
    spts_id: i64,
    program_number: u16,
    fill_with_nulls: bool,
    name: Option<String>,
    state_tx: Option<StateChangeSender>,
) -> Result<InputInfo, actix_web::Error> {
    info!(
        "[SPTS {}] Creating SPTS input for program {} from source input {}",
        spts_id, program_number, source_input.id
    );

    // Create broadcast channel with large capacity for high bitrate streams
    // 16384 slots allows ~1.6s buffer at 120Mbps
    let (tx, _rx) = broadcast::channel::<Bytes>(16384);

    // Statistics cell for this SPTS input
    let stats: StatsCell = Arc::new(RwLock::new(None));
    let atomic_stats = Arc::new(UdpStatsAtomic::new());

    // Subscribe to the source MPTS input
    let mut source_rx = source_input.packet_tx.subscribe();

    // Clone for async task
    let tx_for_task = tx.clone();
    let state_tx_task = state_tx.clone();
    let source_input_id = source_input.id;
    let atomic_stats_task = atomic_stats.clone();
    let stats_task = stats.clone();
    let name_for_task = name.clone();
    
    // Spawn filtering task
    let handle: JoinHandle<()> = tokio::spawn(async move {
        info!("[SPTS {}] Starting SPTS filtering task for program {}", spts_id, program_number);

        // Create program filter
        let mut filter = ProgramFilter::new(program_number, fill_with_nulls);

        // Pre-calculate source address string to avoid allocations in hot path
        let source_addr_string = format!("MPTS Input {} Program {}", source_input_id, program_number);

        // Notify listening state initially
        if let Some(ref tx) = state_tx_task {
            let _ = tx.send(StateChange::InputStateChanged {
                input_id: spts_id,
                new_status: StreamStatus::Listening,
                connected_at: None,
                source_address: Some(source_addr_string.clone()),
            });
        }

        let mut is_connected = false;
        let connected_time = std::time::SystemTime::now(); // Pre-calculate for when needed
        let mut window_start  = Instant::now();
        let mut window_bytes_out  = 0u64;
        let mut window_packets_out = 0u64;                

        loop {
            tokio::select! {
                result = source_rx.recv() => {
                    match result {
                        Ok(mpts_data) => {
                            // Filter packets for our program
                            let filtered = filter.filter_packets(&mpts_data);

                            // Only broadcast if we have filtered data
                            if !filtered.is_empty() {

                                // Transition to Connected state on first filtered packet
                                if !is_connected {
                                    is_connected = true;
                                    if let Some(ref tx) = state_tx_task {
                                        let _ = tx.send(StateChange::InputStateChanged {
                                            input_id: spts_id,
                                            new_status: StreamStatus::Connected,
                                            connected_at: Some(connected_time),
                                            source_address: Some(source_addr_string.clone()),
                                        });
                                    }
                                }

                                // Update stats with filtered data
                                window_bytes_out += filtered.len() as u64;
                                window_packets_out += filtered.len() as u64 / 188;

                                // Broadcast filtered SPTS data (ignore send errors - no receivers is OK)
                                let _ = tx_for_task.send(filtered);
                            }

                            //Update stats every second
                            // if window_start.elapsed() >= Duration::from_secs(2) {
                            //     let bitrate = window_bytes_out * 8; // bits/s
                            //     let pps = window_packets_out;

                            //     // Update atomic counters only (lock-free, no blocking)
                            //     atomic_stats_task.packets_per_sec.store(pps, Ordering::Relaxed);
                            //     atomic_stats_task.bitrate_bps.store(bitrate, Ordering::Relaxed);

                            //     // Try non-blocking write to stats cell
                            //     let snapshot = atomic_stats_task.snapshot();
                            //     if let Ok(mut guard) = stats_task.try_write() {
                            //         *guard = Some(InputStats::Udp(snapshot));
                            //     }

                            //     metrics::record_input_bytes(&name_for_task, spts_id, "spts", window_bytes_out);
                            //     metrics::record_input_packets(&name_for_task, spts_id, "spts", window_packets_out);

                            //     window_start = Instant::now();
                            //     window_bytes_out = 0;
                            //     window_packets_out = 0;
                            // }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("[SPTS {}] Lagged by {} messages from source input {}", spts_id, n, source_input_id);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            error!("[SPTS {}] Source input {} broadcast channel closed", spts_id, source_input_id);

                            // Notify stopped state
                            if let Some(ref tx) = state_tx_task {
                                let _ = tx.send(StateChange::InputStateChanged {
                                    input_id: spts_id,
                                    new_status: StreamStatus::Stopped,
                                    connected_at: None,
                                    source_address: None,
                                });
                            }
                            break;
                        }
                    }
                }
                _ = GLOBAL_CANCEL_TOKEN.cancelled() => {
                    info!("[SPTS {}] Cancelled by global token", spts_id);
                    break;
                }
            }
        }

        info!("[SPTS {}] SPTS filtering task stopped", spts_id);
    });

    // Increment active inputs counter
    metrics::increment_active_inputs();

    let final_name = name.or(Some(format!("SPTS Program {} from Input {}", program_number, source_input.id)));

    Ok(InputInfo {
        id: spts_id,
        name: final_name.clone(),
        status: StreamStatus::Listening, // Will change to connected when first filtered packet arrives
        packet_tx: tx,
        stats,
        task_handle: Some(handle),
        config: CreateInputRequest::Spts {
            source_input_id: source_input.id,
            program_number,
            fill_with_nulls: Some(fill_with_nulls),
            name: final_name,
        },
        output_tasks: std::collections::HashMap::new(),
        stopped_outputs: std::collections::HashMap::new(),
        analysis_tasks: std::collections::HashMap::new(),
        paused_analysis: Vec::new(),
        started_at: Some(std::time::SystemTime::now()),
        connected_at: None, // Will be set when first filtered packet arrives
        state_tx,
        source_address: Some(format!("MPTS Input {} Program {}", source_input.id, program_number)),
    })
}
