use crate::{ACTIVE_STREAMS, models, metrics};
use tokio::sync::mpsc;
use models::{StateChange, StreamStatus, InputInfo, OutputInfo};

pub fn start_processor(mut state_rx: mpsc::UnboundedReceiver<StateChange>) {
    tokio::spawn(async move {
        println!("State change processor started");
        while let Some(change) = state_rx.recv().await {
            match change {
                StateChange::InputStateChanged { input_id, new_status, connected_at, source_address } => {
                    handle_input_state_change(input_id, new_status, connected_at, source_address).await;
                }
                StateChange::OutputStateChanged { input_id, output_id, new_status, connected_at, peer_address } => {
                    handle_output_state_change(input_id, output_id, new_status, connected_at, peer_address).await;
                }
            }
        }
        println!("State change processor ended");
    });
}

async fn handle_input_state_change(
    input_id: i64,
    new_status: StreamStatus,
    connected_at: Option<std::time::SystemTime>,
    source_address: Option<String>
) {
    println!("Input {} state changed to: {} {:?}", input_id, new_status, source_address);

    // Collect data needed for metrics outside the lock
    let (old_status, stream_type, stream_name) = {
        let mut streams = ACTIVE_STREAMS.write().await;

        if let Some(input_info) = streams.get_mut(&input_id) {
            let old_status = input_info.status.clone();
            let stream_type = get_input_stream_type(input_info);
            let stream_name = input_info.name.clone();

            input_info.status = new_status.clone();

            // Update metadata
            if new_status.is_connected() {
                input_info.connected_at = connected_at;
                input_info.source_address = source_address.clone();
            } else if !new_status.is_active() {
                input_info.connected_at = None;
                input_info.source_address = None;
            }

            (old_status, stream_type, stream_name)
        } else {
            return; // Input not found
        }
    }; // Lock released here

    // Handle metrics outside the lock
    if new_status.is_connected() && !old_status.is_connected() {
        metrics::record_connection_event(&stream_name, input_id, stream_type, "input", "connected", &source_address);
        metrics::update_active_connections(&stream_name, input_id, stream_type, "input", true);
    } else if old_status.is_connected() && !new_status.is_connected() {
        metrics::record_connection_event(&stream_name, input_id, stream_type, "input", "disconnected", &source_address);
        metrics::update_active_connections(&stream_name, input_id, stream_type, "input", false);

        // Record connection duration if we have connected_at time
        if let Some(connected_time) = connected_at {
            if let Ok(duration) = connected_time.elapsed() {
                metrics::record_connection_duration_event(&stream_name, input_id, stream_type, "input", duration.as_secs_f64());
            }
        }
    }
}

async fn handle_output_state_change(
    input_id: i64,
    output_id: i64,
    new_status: StreamStatus,
    connected_at: Option<std::time::SystemTime>,
    peer_address: Option<String>
) {
    println!("Output {} (input {}) state changed to: {}", output_id, input_id, new_status);

    // Collect data needed for metrics outside the lock
    let (old_status, stream_type, stream_name) = {
        let mut streams = ACTIVE_STREAMS.write().await;

        if let Some(input_info) = streams.get_mut(&input_id) {
            if let Some(output_info) = input_info.output_tasks.get_mut(&output_id) {
                let old_status = output_info.status.clone();
                let stream_type = get_output_stream_type(output_info);
                let stream_name = output_info.name.clone();

                output_info.status = new_status.clone();

                // Update metadata
                if new_status.is_connected() {
                    output_info.connected_at = connected_at;
                    output_info.peer_address = peer_address.clone();
                } else if !new_status.is_active() {
                    output_info.connected_at = None;
                    output_info.peer_address = None;
                }

                (old_status, stream_type, stream_name)
            } else {
                return; // Output not found
            }
        } else {
            return; // Input not found
        }
    }; // Lock released here

    // Handle metrics outside the lock
    if new_status.is_connected() && !old_status.is_connected() {
        metrics::record_connection_event(&stream_name, output_id, stream_type, "output", "connected", &peer_address);
        metrics::update_active_connections(&stream_name, output_id, stream_type, "output", true);
    } else if old_status.is_connected() && !new_status.is_connected() {
        metrics::record_connection_event(&stream_name, output_id, stream_type, "output", "disconnected", &peer_address);
        metrics::update_active_connections(&stream_name, output_id, stream_type, "output", false);

        // Record connection duration if we have connected_at time
        if let Some(connected_time) = connected_at {
            if let Ok(duration) = connected_time.elapsed() {
                metrics::record_connection_duration_event(&stream_name, output_id, stream_type, "output", duration.as_secs_f64());
            }
        }
    }
}


fn get_input_stream_type(input_info: &InputInfo) -> &'static str {
    match &input_info.config {
        models::CreateInputRequest::Udp { .. } => "udp",
        models::CreateInputRequest::Srt { .. } => "srt",
        models::CreateInputRequest::Spts { .. } => "spts",
    }
}

fn get_output_stream_type(output_info: &OutputInfo) -> &'static str {
    match &output_info.config {
        models::CreateOutputRequest::Udp { .. } => "udp",
        models::CreateOutputRequest::Srt { .. } => "srt",
    }
}
