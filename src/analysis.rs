use bytes::Bytes;
use log::{info, error, warn};
use std::time::SystemTime;
use tokio::{sync::broadcast, task::JoinHandle};
use uuid::Uuid;
use anyhow::{Result, anyhow};

use crate::models::{AnalysisType, AnalysisInfo};
use crate::ACTIVE_STREAMS;
use mpegts_inspector::inspector::{self, CodecInfo, InspectorReport};

/// Start MPEG-TS analysis for a specific input
pub async fn start_analysis(input_id: i64, analysis_type: AnalysisType) -> Result<String> {
    // Generate unique ID for this analysis task
    let analysis_id = Uuid::new_v4().to_string();

    info!("Starting {} analysis for input {} with ID {}",
          analysis_type, input_id, analysis_id);

    // Get the broadcast receiver from the input
    let rx = {
        let guard = ACTIVE_STREAMS.lock().await;
        let input_info = guard.get(&input_id)
            .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

        // Check if this analysis type is already running
        let existing_analysis = input_info.analysis_tasks.values()
            .find(|analysis| analysis.analysis_type == analysis_type);

        if existing_analysis.is_some() {
            return Err(anyhow!("Analysis type {} is already running for input {}",
                             analysis_type, input_id));
        }

        // Subscribe to the broadcast channel
        input_info.packet_tx.subscribe()
    };

    // Spawn the analysis task based on type
    let task_handle = match analysis_type {
        AnalysisType::Mux => spawn_mux_analysis(rx, input_id, analysis_id.clone()).await?,
        AnalysisType::Tr101 => spawn_tr101_analysis(rx, input_id, analysis_id.clone()).await?,
    };

    // Create AnalysisInfo and add to the input's analysis_tasks
    let analysis_info = AnalysisInfo {
        id: analysis_id.clone(),
        analysis_type: analysis_type.clone(),
        input_id,
        task_handle,
        created_at: SystemTime::now(),
    };

    // Add to the active streams map
    {
        let mut guard = ACTIVE_STREAMS.lock().await;
        if let Some(input_info) = guard.get_mut(&input_id) {
            input_info.analysis_tasks.insert(analysis_id.clone(), analysis_info);
        } else {
            return Err(anyhow!("Input {} not found when trying to add analysis task", input_id));
        }
    }

    info!("Successfully started {} analysis for input {} with ID {}",
          analysis_type, input_id, analysis_id);

    Ok(analysis_id)
}

/// Stop a specific analysis task
pub async fn stop_analysis(input_id: i64, analysis_type: AnalysisType) -> Result<()> {
    info!("Stopping {} analysis for input {}", analysis_type, input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    // Find and remove the analysis task
    let analysis_to_remove = input_info.analysis_tasks.iter()
        .find(|(_, analysis)| analysis.analysis_type == analysis_type)
        .map(|(id, _)| id.clone());

    if let Some(analysis_id) = analysis_to_remove {
        if let Some(analysis_info) = input_info.analysis_tasks.remove(&analysis_id) {
            // Abort the task
            analysis_info.task_handle.abort();
            info!("Successfully stopped {} analysis (ID: {}) for input {}",
                  analysis_type, analysis_id, input_id);
        }
    } else {
        warn!("No active {} analysis found for input {}", analysis_type, input_id);
        return Err(anyhow!("No active {} analysis found for input {}", analysis_type, input_id));
    }

    Ok(())
}

/// Stop all analysis tasks for a specific input
pub async fn stop_all_analysis(input_id: i64) -> Result<()> {
    info!("Stopping all analysis tasks for input {}", input_id);

    let mut guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get_mut(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    let analysis_count = input_info.analysis_tasks.len();

    // Stop all analysis tasks
    for (analysis_id, analysis_info) in input_info.analysis_tasks.drain() {
        analysis_info.task_handle.abort();
        info!("Stopped analysis task {} for input {}", analysis_id, input_id);
    }

    info!("Stopped {} analysis tasks for input {}", analysis_count, input_id);
    Ok(())
}

/// Get list of active analyses for an input
pub async fn get_active_analyses(input_id: i64) -> Result<Vec<(String, AnalysisType, SystemTime)>> {
    let guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    let analyses = input_info.analysis_tasks.iter()
        .map(|(id, info)| (id.clone(), info.analysis_type.clone(), info.created_at))
        .collect();

    Ok(analyses)
}

/// Spawn MUX analysis task
async fn spawn_mux_analysis(mut rx: broadcast::Receiver<Bytes>, input_id: i64, analysis_id: String) -> Result<JoinHandle<()>> {
    let handle = tokio::spawn(async move {
        info!("Starting MUX analysis task {} for input {}", analysis_id, input_id);

        // Create a new channel to convert Bytes to Vec<u8>
        let (tx, rx_vec) = broadcast::channel(1000);

        // Spawn a conversion task
        let conversion_handle = tokio::spawn(async move {
            while let Ok(bytes) = rx.recv().await {
                let vec_data = bytes.to_vec();
                if tx.send(vec_data).is_err() {
                    break; // Receiver dropped
                }
            }
        });

        match inspector::run_from_broadcast(
            rx_vec, 
            2, 
            false, 
            |report: InspectorReport| {
        // Process structured data directly (no JSON parsing needed)
            for program in &report.programs {
                println!("Program {}", program.program_number);
                for stream in &program.streams {
                    match &stream.codec {
                        Some(CodecInfo::Video(v)) => println!("  Video: {} {}x{} @ {:.1}fps",
                            v.codec, v.width, v.height, v.fps),
                        Some(CodecInfo::Audio(a)) => println!("  Audio: {} {}Hz {}ch",
                            a.codec, a.sample_rate.unwrap_or(0), a.channels.unwrap_or(0)),
                        Some(CodecInfo::Subtitle(s)) => println!("  Subtitle: {}", s.codec),
                        None => println!("  Unknown stream type {}", stream.stream_type),
                    }
                }   
            }

            // Access TR-101 metrics (filtered by priority level)
            if report.tr101_metrics.sync_byte_errors > 0 {
                println!("⚠️ Critical: Sync byte errors: {}", report.tr101_metrics.sync_byte_errors);
            }
            if report.tr101_metrics.pcr_accuracy_errors > 0 {
                println!("⚠️ Timing: PCR accuracy errors: {}", report.tr101_metrics.pcr_accuracy_errors);
            }
            // Priority 3 errors (like service_id_mismatch) are automatically filtered out
        }).await {
            Ok(_) => {
                info!("MUX analysis task {} for input {} completed successfully", analysis_id, input_id);
            },
            Err(e) => {
                error!("MUX analysis task {} for input {} failed: {}", analysis_id, input_id, e);
            }
        }

        conversion_handle.abort();
    });

    Ok(handle)
}

/// Spawn TR-101 analysis task
async fn spawn_tr101_analysis(mut rx: broadcast::Receiver<Bytes>, input_id: i64, analysis_id: String) -> Result<JoinHandle<()>> {
    let handle = tokio::spawn(async move {
        info!("Starting TR-101 analysis task {} for input {}", analysis_id, input_id);

        // Create a new channel to convert Bytes to Vec<u8>
        let (tx, rx_vec) = broadcast::channel(1000);

        // Spawn a conversion task
        let conversion_handle = tokio::spawn(async move {
            while let Ok(bytes) = rx.recv().await {
                let vec_data = bytes.to_vec();
                if tx.send(vec_data).is_err() {
                    break; // Receiver dropped
                }
            }
        });

        match inspector::run_from_broadcast(rx_vec, 2, true,|report: InspectorReport| {
        // Process structured data directly (no JSON parsing needed)
            for program in &report.programs {
                println!("Program {}", program.program_number);
                for stream in &program.streams {
                    match &stream.codec {
                        Some(CodecInfo::Video(v)) => println!("  Video: {} {}x{} @ {:.1}fps",
                            v.codec, v.width, v.height, v.fps),
                        Some(CodecInfo::Audio(a)) => println!("  Audio: {} {}Hz {}ch",
                            a.codec, a.sample_rate.unwrap_or(0), a.channels.unwrap_or(0)),
                        Some(CodecInfo::Subtitle(s)) => println!("  Subtitle: {}", s.codec),
                        None => println!("  Unknown stream type {}", stream.stream_type),
                    }
                }   
            }

            // Access TR-101 metrics (filtered by priority level)
            if report.tr101_metrics.sync_byte_errors > 0 {
                println!("⚠️ Critical: Sync byte errors: {}", report.tr101_metrics.sync_byte_errors);
            }
            if report.tr101_metrics.pcr_accuracy_errors > 0 {
                println!("⚠️ Timing: PCR accuracy errors: {}", report.tr101_metrics.pcr_accuracy_errors);
            }
            // Priority 3 errors (like service_id_mismatch) are automatically filtered out
        }).await {
            Ok(_) => {
                info!("TR-101 analysis task {} for input {} completed successfully", analysis_id, input_id);
            },
            Err(e) => {
                error!("TR-101 analysis task {} for input {} failed: {}", analysis_id, input_id, e);
            }
        }

        conversion_handle.abort();
    });

    Ok(handle)
}