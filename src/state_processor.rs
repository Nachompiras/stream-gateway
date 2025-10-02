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
    println!("Input {} state changed to: {}", input_id, new_status);
    let mut streams = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = streams.get_mut(&input_id) {
        let old_status = input_info.status.clone();
        input_info.status = new_status.clone();

        let stream_type = get_input_stream_type(input_info);

        // Handle different state transitions
        if new_status.is_connected() && !old_status.is_connected() {
            handle_input_connection_established(input_info, input_id, stream_type, connected_at, source_address);
        } else if old_status.is_connected() && !new_status.is_connected() {
            handle_input_connection_lost(input_info, input_id, stream_type);
        } else if new_status.is_connected() {
            // Update source address for already connected streams
            input_info.connected_at = connected_at;
            input_info.source_address = source_address;
        } else if !new_status.is_active() {
            // Stream stopped or in error state
            input_info.connected_at = None;
            input_info.source_address = None;
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
    let mut streams = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = streams.get_mut(&input_id) {
        if let Some(output_info) = input_info.output_tasks.get_mut(&output_id) {
            let old_status = output_info.status.clone();
            output_info.status = new_status.clone();

            let stream_type = get_output_stream_type(output_info);

            // Handle different state transitions
            if new_status.is_connected() && !old_status.is_connected() {
                handle_output_connection_established(output_info, output_id, stream_type, connected_at, peer_address);
            } else if old_status.is_connected() && !new_status.is_connected() {
                handle_output_connection_lost(output_info, output_id, stream_type);
            } else if new_status.is_connected() {
                // Update peer address for already connected streams
                output_info.connected_at = connected_at;
                output_info.peer_address = peer_address;
            } else if !new_status.is_active() {
                // Stream stopped or in error state
                output_info.connected_at = None;
                output_info.peer_address = None;
            }
        }
    }
}

fn handle_input_connection_established(
    input_info: &mut InputInfo,
    input_id: i64,
    stream_type: &str,
    connected_at: Option<std::time::SystemTime>,
    source_address: Option<String>
) {
    input_info.connected_at = connected_at;
    input_info.source_address = source_address.clone();

    metrics::record_connection_event(
        &input_info.name,
        input_id,
        stream_type,
        "input",
        "connected",
        &source_address
    );
    metrics::update_active_connections(
        &input_info.name,
        input_id,
        stream_type,
        "input",
        true
    );
}

fn handle_input_connection_lost(
    input_info: &mut InputInfo,
    input_id: i64,
    stream_type: &str
) {
    metrics::record_connection_event(
        &input_info.name,
        input_id,
        stream_type,
        "input",
        "disconnected",
        &input_info.source_address
    );
    metrics::update_active_connections(
        &input_info.name,
        input_id,
        stream_type,
        "input",
        false
    );

    // Record connection duration if we have connected_at time
    if let Some(connected_time) = input_info.connected_at {
        if let Ok(duration) = connected_time.elapsed() {
            metrics::record_connection_duration_event(
                &input_info.name,
                input_id,
                stream_type,
                "input",
                duration.as_secs_f64()
            );
        }
    }

    input_info.connected_at = None;
    input_info.source_address = None;
}

fn handle_output_connection_established(
    output_info: &mut OutputInfo,
    output_id: i64,
    stream_type: &str,
    connected_at: Option<std::time::SystemTime>,
    peer_address: Option<String>
) {
    output_info.connected_at = connected_at;
    output_info.peer_address = peer_address.clone();

    metrics::record_connection_event(
        &output_info.name,
        output_id,
        stream_type,
        "output",
        "connected",
        &peer_address
    );
    metrics::update_active_connections(
        &output_info.name,
        output_id,
        stream_type,
        "output",
        true
    );
}

fn handle_output_connection_lost(
    output_info: &mut OutputInfo,
    output_id: i64,
    stream_type: &str
) {
    metrics::record_connection_event(
        &output_info.name,
        output_id,
        stream_type,
        "output",
        "disconnected",
        &output_info.peer_address
    );
    metrics::update_active_connections(
        &output_info.name,
        output_id,
        stream_type,
        "output",
        false
    );

    // Record connection duration if we have connected_at time
    if let Some(connected_time) = output_info.connected_at {
        if let Ok(duration) = connected_time.elapsed() {
            metrics::record_connection_duration_event(
                &output_info.name,
                output_id,
                stream_type,
                "output",
                duration.as_secs_f64()
            );
        }
    }

    output_info.connected_at = None;
    output_info.peer_address = None;
}

fn get_input_stream_type(input_info: &InputInfo) -> &'static str {
    match &input_info.config {
        models::CreateInputRequest::Udp { .. } => "udp",
        models::CreateInputRequest::Srt { .. } => "srt",
    }
}

fn get_output_stream_type(output_info: &OutputInfo) -> &'static str {
    match &output_info.config {
        models::CreateOutputRequest::Udp { .. } => "udp",
        models::CreateOutputRequest::Srt { .. } => "srt",
        models::CreateOutputRequest::Spts { .. } => "spts",
    }
}
