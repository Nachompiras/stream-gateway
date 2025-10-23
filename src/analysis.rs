use bytes::Bytes;
use log::{info, error, warn};
use std::time::SystemTime;
use std::sync::Arc;
use tokio::{sync::{broadcast, RwLock}, task::JoinHandle};
use uuid::Uuid;
use anyhow::{Result, anyhow};

use crate::models::{
    AnalysisType, AnalysisInfo, AnalysisDataReport, ProgramData,
    StreamData, CodecData, Tr101MetricsData
};
use crate::ACTIVE_STREAMS;
use mpegts_inspector::inspector::{self, CodecInfo, InspectorReport};

/// Start MPEG-TS analysis for a specific input
pub async fn start_analysis(input_id: i64, analysis_type: AnalysisType, timeout_minutes: Option<u64>) -> Result<String> {
    // Generate unique ID for this analysis task
    let analysis_id = Uuid::new_v4().to_string();

    if let Some(timeout) = timeout_minutes {
        info!("Starting {} analysis for input {} with ID {} (timeout: {} minutes)",
              analysis_type, input_id, analysis_id, timeout);
    } else {
        info!("Starting {} analysis for input {} with ID {} (no timeout)",
              analysis_type, input_id, analysis_id);
    }

    // Get the broadcast receiver from the input
    let rx = {
        let guard = ACTIVE_STREAMS.read().await;
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

    // Create shared report data container
    let report_data = Arc::new(RwLock::new(None));

    // Calculate expiration time if timeout is set
    let (expires_at, timeout_duration) = if let Some(minutes) = timeout_minutes {
        let duration = std::time::Duration::from_secs(minutes * 60);
        let expires = SystemTime::now() + duration;
        (Some(expires), Some(duration))
    } else {
        (None, None)
    };

    // Spawn the analysis task based on type
    let task_handle = match analysis_type {
        AnalysisType::Mux => spawn_mux_analysis(rx, input_id, analysis_id.clone(), report_data.clone(), timeout_duration).await?,
        AnalysisType::Tr101 => spawn_tr101_analysis(rx, input_id, analysis_id.clone(), report_data.clone(), timeout_duration).await?,
    };

    // Create AnalysisInfo and add to the input's analysis_tasks
    let analysis_info = AnalysisInfo {
        id: analysis_id.clone(),
        analysis_type: analysis_type.clone(),
        input_id,
        task_handle,
        created_at: SystemTime::now(),
        report_data,
        timeout_minutes,
        expires_at,
    };

    // Add to the active streams map
    {
        let mut guard = ACTIVE_STREAMS.write().await;
        if let Some(input_info) = guard.get_mut(&input_id) {
            input_info.analysis_tasks.insert(analysis_id.clone(), analysis_info);
        } else {
            return Err(anyhow!("Input {} not found when trying to add analysis task", input_id));
        }
    }

    if let Some(timeout) = timeout_minutes {
        info!("Successfully started {} analysis for input {} with ID {} (will stop after {} minutes)",
              analysis_type, input_id, analysis_id, timeout);
    } else {
        info!("Successfully started {} analysis for input {} with ID {}",
              analysis_type, input_id, analysis_id);
    }

    Ok(analysis_id)
}

/// Stop a specific analysis task
pub async fn stop_analysis(input_id: i64, analysis_type: AnalysisType) -> Result<()> {
    info!("Stopping {} analysis for input {}", analysis_type, input_id);

    let mut guard = ACTIVE_STREAMS.write().await;
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

    let mut guard = ACTIVE_STREAMS.write().await;
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
pub async fn get_active_analyses(input_id: i64) -> Result<Vec<(String, AnalysisType, SystemTime, Option<u64>, Option<SystemTime>)>> {
    let guard = ACTIVE_STREAMS.read().await;
    let input_info = guard.get(&input_id)
        .ok_or_else(|| anyhow!("Input {} not found", input_id))?;

    let analyses = input_info.analysis_tasks.iter()
        .map(|(id, info)| (
            id.clone(),
            info.analysis_type.clone(),
            info.created_at,
            info.timeout_minutes,
            info.expires_at
        ))
        .collect();

    Ok(analyses)
}

/// Spawn MUX analysis task
async fn spawn_mux_analysis(
    mut rx: broadcast::Receiver<Bytes>,
    input_id: i64,
    analysis_id: String,
    report_data: Arc<RwLock<Option<AnalysisDataReport>>>,
    timeout_duration: Option<std::time::Duration>
) -> Result<JoinHandle<()>> {
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

        // Clone for moving into the callback
        let report_data_clone = report_data.clone();

        let analysis_future = inspector::run_from_broadcast(
            rx_vec,
            2,
            false,
            move |report: InspectorReport| {
        // Convert and store the report data
        let analysis_data = convert_inspector_report(&report);

        // Update the shared report data (spawn task to avoid blocking)
        let report_data_update = report_data_clone.clone();
        tokio::spawn(async move {
            *report_data_update.write().await = Some(analysis_data);
        });

        // Process structured data directly (no JSON parsing needed) - keep for logging
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
        });

        // Apply timeout if configured
        if let Some(timeout) = timeout_duration {
            match tokio::time::timeout(timeout, analysis_future).await {
                Ok(inner_result) => {
                    match inner_result {
                        Ok(_) => {
                            info!("MUX analysis task {} for input {} completed successfully", analysis_id, input_id);
                        },
                        Err(e) => {
                            error!("MUX analysis task {} for input {} failed: {}", analysis_id, input_id, e);
                        }
                    }
                },
                Err(_) => {
                    info!("MUX analysis task {} for input {} stopped due to timeout", analysis_id, input_id);
                }
            }
        } else {
            // No timeout, run indefinitely
            match analysis_future.await {
                Ok(_) => {
                    info!("MUX analysis task {} for input {} completed successfully", analysis_id, input_id);
                },
                Err(e) => {
                    error!("MUX analysis task {} for input {} failed: {}", analysis_id, input_id, e);
                }
            }
        }

        conversion_handle.abort();
    });

    Ok(handle)
}

/// Spawn TR-101 analysis task
async fn spawn_tr101_analysis(
    mut rx: broadcast::Receiver<Bytes>,
    input_id: i64,
    analysis_id: String,
    report_data: Arc<RwLock<Option<AnalysisDataReport>>>,
    timeout_duration: Option<std::time::Duration>
) -> Result<JoinHandle<()>> {
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

        // Clone for moving into the callback
        let report_data_clone = report_data.clone();

        let analysis_future = inspector::run_from_broadcast(rx_vec, 2, true, move |report: InspectorReport| {
        // Convert and store the report data
        let analysis_data = convert_inspector_report(&report);

        // Update the shared report data (spawn task to avoid blocking)
        let report_data_update = report_data_clone.clone();
        tokio::spawn(async move {
            *report_data_update.write().await = Some(analysis_data);
        });

        // Process structured data directly (no JSON parsing needed) - keep for logging
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
        });

        // Apply timeout if configured
        if let Some(timeout) = timeout_duration {
            match tokio::time::timeout(timeout, analysis_future).await {
                Ok(inner_result) => {
                    match inner_result {
                        Ok(_) => {
                            info!("TR-101 analysis task {} for input {} completed successfully", analysis_id, input_id);
                        },
                        Err(e) => {
                            error!("TR-101 analysis task {} for input {} failed: {}", analysis_id, input_id, e);
                        }
                    }
                },
                Err(_) => {
                    info!("TR-101 analysis task {} for input {} stopped due to timeout", analysis_id, input_id);
                }
            }
        } else {
            // No timeout, run indefinitely
            match analysis_future.await {
                Ok(_) => {
                    info!("TR-101 analysis task {} for input {} completed successfully", analysis_id, input_id);
                },
                Err(e) => {
                    error!("TR-101 analysis task {} for input {} failed: {}", analysis_id, input_id, e);
                }
            }
        }

        conversion_handle.abort();
    });

    Ok(handle)
}

/// Convert InspectorReport to serializable AnalysisDataReport
fn convert_inspector_report(report: &InspectorReport) -> AnalysisDataReport {
    let programs = report.programs.iter().map(|program| {
        let streams = program.streams.iter().map(|stream| {
            let codec = match &stream.codec {
                Some(CodecInfo::Video(v)) => Some(CodecData::Video {
                    codec: v.codec.clone(),
                    width: v.width as u32,
                    height: v.height as u32,
                    fps: v.fps as f64,
                }),
                Some(CodecInfo::Audio(a)) => Some(CodecData::Audio {
                    codec: a.codec.clone(),
                    sample_rate: a.sample_rate,
                    channels: a.channels,
                }),
                Some(CodecInfo::Subtitle(s)) => Some(CodecData::Subtitle {
                    codec: s.codec.clone(),
                }),
                None => None,
            };

            StreamData {
                stream_type: stream.stream_type,
                pid: stream.pid,
                codec,
            }
        }).collect();

        ProgramData {
            program_number: program.program_number,
            streams,
        }
    }).collect();

    let tr101_metrics = Some(Tr101MetricsData {
        sync_byte_errors: report.tr101_metrics.sync_byte_errors,
        continuity_counter_errors: report.tr101_metrics.continuity_counter_errors,
        pat_errors: report.tr101_metrics.pat_crc_errors,
        pmt_errors: report.tr101_metrics.pmt_crc_errors,
        pid_errors: report.tr101_metrics.pid_errors,
        transport_errors: report.tr101_metrics.transport_error_indicator,
        crc_errors: report.tr101_metrics.pat_crc_errors + report.tr101_metrics.pmt_crc_errors,
        pcr_repetition_errors: report.tr101_metrics.pcr_repetition_errors,
        pcr_discontinuity_errors: 0, // Field not available in current mpegts_inspector version
        pcr_accuracy_errors: report.tr101_metrics.pcr_accuracy_errors,
        pts_errors: report.tr101_metrics.pts_errors,
        cat_errors: report.tr101_metrics.cat_crc_errors,
    });

    // Generate ISO 8601 timestamp
    let timestamp = match SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            let nanos = duration.subsec_nanos();
            sqlx::types::chrono::DateTime::from_timestamp(secs as i64, nanos)
                .unwrap_or_default()
                .to_rfc3339()
        }
        Err(_) => "unknown".to_string(),
    };

    AnalysisDataReport {
        timestamp,
        programs,
        tr101_metrics,
    }
}