use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use anyhow::{Result, anyhow};
use log::{info, error};

use crate::models::*;
use crate::{ACTIVE_STREAMS, STATE_CHANGE_TX};
use crate::udp_stream::{spawn_udp_input_with_stats, create_udp_output};
use crate::srt_stream::{create_srt_output, SrtSourceWithState};
use crate::analysis;

/// Start a stopped input stream
pub async fn start_input(input_id: i64) -> Result<()> {
    info!("Starting input {}", input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    if input_info.status.is_active() {
        return Err(anyhow!("Input {} is already running", input_id));
    }

    // Recreate the input task based on the stored config
    match &input_info.config {
        CreateInputRequest::Udp { .. } => {
            let state_tx = input_info.state_tx.clone().or_else(|| {
                // If no state_tx, try to get the global one
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        STATE_CHANGE_TX.lock().await.clone()
                    })
                })
            });

            let port = input_info.config.get_bind_port();
            let bind_host = Some(input_info.config.get_bind_host()).filter(|h| h != "0.0.0.0");
            let (multicast_group, source_specific_multicast) = match &input_info.config {
                CreateInputRequest::Udp { multicast_group, source_specific_multicast, .. } => {
                    (multicast_group.clone(), source_specific_multicast.clone())
                },
                _ => (None, None),
            };

            let new_input_info = spawn_udp_input_with_stats(
                input_info.id,
                input_info.name.clone(),
                port,
                bind_host,
                multicast_group,
                source_specific_multicast,
                state_tx,
            ).map_err(|e| anyhow::anyhow!("Failed to spawn UDP input: {}", e))?;

            // Update the existing InputInfo with new task and sender
            input_info.task_handle = new_input_info.task_handle;
            input_info.packet_tx = new_input_info.packet_tx;
            input_info.stats = new_input_info.stats;
            input_info.status = StreamStatus::Listening; // UDP starts listening
            input_info.started_at = Some(SystemTime::now());
        },
        CreateInputRequest::Srt { config, .. } => {
            let state_tx = input_info.state_tx.clone().or_else(|| {
                // If no state_tx, try to get the global one
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        STATE_CHANGE_TX.lock().await.clone()
                    })
                })
            });

            let source_with_state = SrtSourceWithState {
                config: config.clone(),
                input_id: input_info.id,
                state_tx: state_tx.clone(),
            };
            let sender = Forwarder::spawn_with_stats(
                Box::new(source_with_state),
                Duration::from_secs(1),
                input_info.id,
                input_info.name.clone(),
                state_tx,
            );

            input_info.task_handle = Some(sender.handle);
            input_info.packet_tx = sender.tx;
            input_info.stats = sender.stats;
            input_info.status = match config {
                SrtInputConfig::Listener { .. } => StreamStatus::Listening,
                SrtInputConfig::Caller { .. } => StreamStatus::Connecting,
            };
            input_info.started_at = Some(SystemTime::now());
        }
    }

    // Restart running outputs that were stopped when input was stopped
    let mut outputs_to_start = Vec::new();
    for (output_id, output_config) in input_info.stopped_outputs.drain() {
        outputs_to_start.push((output_id, output_config));
    }

    for (output_id, output_config) in outputs_to_start {
        if let Err(e) = start_output_internal(input_info, output_id, output_config).await {
            error!("Failed to restart output {}: {}", output_id, e);
        }
    }

    // Restart paused analysis
    let paused_analysis: Vec<AnalysisType> = input_info.paused_analysis.drain(..).collect();
    for analysis_type in paused_analysis {
        if let Err(e) = analysis::start_analysis(input_id, analysis_type.clone()).await {
            error!("Failed to restart {} analysis for input {}: {}", analysis_type, input_id, e);
            // Add back to paused list if failed
            input_info.paused_analysis.push(analysis_type);
        }
    }

    info!("Input {} started successfully", input_id);
    Ok(())
}

/// Stop a running input stream
pub async fn stop_input(input_id: i64) -> Result<()> {
    info!("Stopping input {}", input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    if input_info.status == StreamStatus::Stopped {
        return Err(anyhow!("Input {} is already stopped", input_id));
    }

    // Stop the main input task
    if let Some(handle) = input_info.task_handle.take() {
        handle.abort();
    }

    // Move running outputs to stopped_outputs and abort their tasks
    let mut outputs_to_stop = HashMap::new();
    for (output_id, mut output_info) in input_info.output_tasks.drain() {
        if output_info.status.is_active() {
            // Stop the output task
            if let Some(handle) = output_info.abort_handle.take() {
                handle.abort();
            }
            output_info.status = StreamStatus::Stopped;
            output_info.started_at = None;

            // Store the config to restart later
            outputs_to_stop.insert(output_id, output_info.config.clone());
        }
        // Note: We're not putting the output back in output_tasks since it's stopped
    }
    input_info.stopped_outputs.extend(outputs_to_stop);

    // Pause active analysis
    let active_analysis: Vec<AnalysisType> = input_info.analysis_tasks.values().map(|info| info.analysis_type.clone())
        .collect();

    for analysis_type in active_analysis {
        if let Err(e) = analysis::stop_analysis(input_id, analysis_type.clone()).await {
            error!("Failed to pause {} analysis for input {}: {}", analysis_type, input_id, e);
        } else {
            input_info.paused_analysis.push(analysis_type);
        }
    }

    input_info.status = StreamStatus::Stopped;
    input_info.started_at = None;
    info!("Input {} stopped successfully", input_id);
    Ok(())
}

/// Start a stopped output stream
pub async fn start_output(input_id: i64, output_id: i64) -> Result<()> {
    info!("Starting output {} for input {}", output_id, input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    if input_info.status == StreamStatus::Stopped {
        return Err(anyhow!("Cannot start output {} because input {} is stopped", output_id, input_id));
    }

    // Check if output is in stopped_outputs
    if let Some(output_config) = input_info.stopped_outputs.remove(&output_id) {
        start_output_internal(input_info, output_id, output_config).await?;
        info!("Output {} started successfully", output_id);
        Ok(())
    } else {
        // Check if it's already running
        if input_info.output_tasks.contains_key(&output_id) {
            Err(anyhow!("Output {} is already running", output_id))
        } else {
            Err(anyhow!("Output {} not found in input {}", output_id, input_id))
        }
    }
}

/// Stop a running output stream
pub async fn stop_output(input_id: i64, output_id: i64) -> Result<()> {
    info!("Stopping output {} for input {}", output_id, input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    if let Some(mut output_info) = input_info.output_tasks.remove(&output_id) {
        // Stop the output task
        if let Some(handle) = output_info.abort_handle.take() {
            handle.abort();
        }

        output_info.status = StreamStatus::Stopped;
        output_info.started_at = None;

        // Move to stopped_outputs for later restart
        input_info.stopped_outputs.insert(output_id, output_info.config.clone());

        info!("Output {} stopped successfully", output_id);
        Ok(())
    } else {
        // Check if it's already stopped
        if input_info.stopped_outputs.contains_key(&output_id) {
            Err(anyhow!("Output {} is already stopped", output_id))
        } else {
            Err(anyhow!("Output {} not found in input {}", output_id, input_id))
        }
    }
}

/// Internal helper to start an output with given config
async fn start_output_internal(input_info: &mut InputInfo, output_id: i64, output_config: CreateOutputRequest) -> Result<()> {
    let output_info = match &output_config {
        CreateOutputRequest::Udp { name, input_id, bind_host, multicast_ttl, multicast_interface, .. } => {
            // Use helper methods to construct destination_addr
            let host = output_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
            let port = output_config.get_remote_port().unwrap_or(8000);
            let destination_addr = format!("{}:{}", host, port);

            // Check if destination is multicast and create config
            let multicast_config = if let Ok(addr) = host.parse::<std::net::IpAddr>() {
                if addr.is_multicast() {
                    Some(crate::udp_stream::MulticastOutputConfig {
                        ttl: multicast_ttl.unwrap_or(1),
                        interface: multicast_interface.clone(),
                    })
                } else {
                    None
                }
            } else {
                None
            };

            create_udp_output(                
                destination_addr,
                input_info,
                output_id,
                name.clone(),
                bind_host.clone(),
                multicast_config,
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        STATE_CHANGE_TX.lock().await.clone()
                    })
                }),
            ).await.map_err(|e| anyhow::anyhow!("Failed to create UDP output: {}", e))?
        },
        CreateOutputRequest::Srt { config, name, input_id, .. } => {
            create_srt_output(
                *input_id,
                config.clone(),
                input_info,
                output_id,
                name.clone(),
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        STATE_CHANGE_TX.lock().await.clone()
                    })
                }),
            ).map_err(|e| anyhow::anyhow!("Failed to create SRT output: {}", e))?
        }
    };

    input_info.output_tasks.insert(output_id, output_info);
    Ok(())
}