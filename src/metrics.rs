use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramVec, Registry, TextEncoder, Encoder
};
use std::collections::HashMap;
use once_cell::sync::Lazy;

// Global metrics registry
pub static METRICS_REGISTRY: Lazy<Registry> = Lazy::new(|| Registry::new());

// Stream metrics
pub static ACTIVE_INPUTS: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::new("stream_gateway_active_inputs_total", "Number of active input streams").unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static ACTIVE_OUTPUTS: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::new("stream_gateway_active_outputs_total", "Number of active output streams").unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static INPUT_BYTES_RECEIVED: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_input_bytes_received_total", "Total bytes received by input streams"),
        &["stream_name", "input_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static OUTPUT_BYTES_SENT: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_output_bytes_sent_total", "Total bytes sent by output streams"),
        &["stream_name", "input_id", "output_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static INPUT_PACKETS_RECEIVED: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_input_packets_received_total", "Total packets received by input streams"),
        &["stream_name", "input_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static OUTPUT_PACKETS_SENT: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_output_packets_sent_total", "Total packets sent by output streams"),
        &["stream_name", "input_id", "output_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static STREAM_CONNECTION_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new("stream_gateway_connection_duration_seconds", "Duration of stream connections in seconds"),
        &["stream_type", "connection_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(histogram.clone())).unwrap();
    histogram
});

pub static STREAM_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_errors_total", "Total number of stream errors"),
        &["stream_name", "stream_id", "stream_type", "error_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static HTTP_REQUESTS_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_http_requests_total", "Total HTTP requests"),
        &["method", "endpoint", "status"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static HTTP_REQUEST_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new("stream_gateway_http_request_duration_seconds", "HTTP request duration in seconds"),
        &["method", "endpoint"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(histogram.clone())).unwrap();
    histogram
});

// Connection event metrics
pub static CONNECTION_EVENTS_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_connection_events_total", "Total number of stream connection events"),
        &["stream_name", "stream_id", "stream_type", "direction", "event_type", "peer_address"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static ACTIVE_CONNECTIONS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("stream_gateway_active_connections", "Number of currently active stream connections"),
        &["stream_name", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static CONNECTION_DURATIONS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new("stream_gateway_connection_durations_seconds", "Duration of stream connections in seconds")
            .buckets(vec![1.0, 10.0, 60.0, 300.0, 1800.0, 3600.0, 21600.0, 86400.0]),
        &["stream_name", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(histogram.clone())).unwrap();
    histogram
});

// Utility functions for metrics collection
pub fn get_metrics_text() -> Result<String, prometheus::Error> {
    let encoder = TextEncoder::new();
    let metric_families = METRICS_REGISTRY.gather();
    encoder.encode_to_string(&metric_families)
}

pub fn increment_active_inputs() {
    ACTIVE_INPUTS.inc();
}

pub fn decrement_active_inputs() {
    ACTIVE_INPUTS.dec();
}

pub fn increment_active_outputs() {
    ACTIVE_OUTPUTS.inc();
}

pub fn decrement_active_outputs() {
    ACTIVE_OUTPUTS.dec();
}

pub fn record_input_bytes(stream_name: &Option<String>, input_id: i64, stream_type: &str, bytes: u64) {
    let normalized_name = normalize_stream_name(stream_name, input_id);
    let input_id_str = input_id.to_string();

    INPUT_BYTES_RECEIVED
        .with_label_values(&[normalized_name.as_str(), input_id_str.as_str(), stream_type])
        .inc_by(bytes as f64);
}

pub fn record_output_bytes(stream_name: &Option<String>, input_id: i64, output_id: i64, stream_type: &str, bytes: u64) {
    let normalized_name = normalize_stream_name(stream_name, output_id);
    let input_id_str = input_id.to_string();
    let output_id_str = output_id.to_string();

    OUTPUT_BYTES_SENT
        .with_label_values(&[normalized_name.as_str(), input_id_str.as_str(), output_id_str.as_str(), stream_type])
        .inc_by(bytes as f64);
}

pub fn record_input_packets(stream_name: &Option<String>, input_id: i64, stream_type: &str, packets: u64) {
    let normalized_name = normalize_stream_name(stream_name, input_id);
    let input_id_str = input_id.to_string();

    INPUT_PACKETS_RECEIVED
        .with_label_values(&[normalized_name.as_str(), input_id_str.as_str(), stream_type])
        .inc_by(packets as f64);
}

pub fn record_output_packets(stream_name: &Option<String>, input_id: i64, output_id: i64, stream_type: &str, packets: u64) {
    let normalized_name = normalize_stream_name(stream_name, output_id);
    let input_id_str = input_id.to_string();
    let output_id_str = output_id.to_string();

    OUTPUT_PACKETS_SENT
        .with_label_values(&[normalized_name.as_str(), input_id_str.as_str(), output_id_str.as_str(), stream_type])
        .inc_by(packets as f64);
}

pub fn record_stream_error(stream_name: &Option<String>, stream_id: i64, stream_type: &str, error_type: &str) {
    let normalized_name = normalize_stream_name(stream_name, stream_id);
    let stream_id_str = stream_id.to_string();

    STREAM_ERRORS
        .with_label_values(&[normalized_name.as_str(), stream_id_str.as_str(), stream_type, error_type])
        .inc();
}

pub fn record_connection_duration(stream_type: &str, connection_type: &str, duration: f64) {
    STREAM_CONNECTION_DURATION
        .with_label_values(&[stream_type, connection_type])
        .observe(duration);
}

pub fn record_http_request(method: &str, endpoint: &str, status: &str, duration: f64) {
    HTTP_REQUESTS_TOTAL
        .with_label_values(&[method, endpoint, status])
        .inc();

    HTTP_REQUEST_DURATION
        .with_label_values(&[method, endpoint])
        .observe(duration);
}

// Helper function to normalize stream names for Prometheus labels
pub fn normalize_stream_name(name: &Option<String>, id: i64) -> String {
    match name {
        Some(n) if !n.trim().is_empty() => {
            // Replace spaces with underscores and remove invalid characters
            n.trim()
                .chars()
                .map(|c| match c {
                    ' ' | '-' => '_',
                    c if c.is_alphanumeric() || c == '_' => c,
                    _ => '_'
                })
                .collect::<String>()
                .trim_matches('_')
                .to_string()
        },
        _ => format!("stream_{}", id)
    }
}

// Record connection events with full context
pub fn record_connection_event(
    stream_name: &Option<String>,
    stream_id: i64,
    stream_type: &str,
    direction: &str,
    event_type: &str,
    peer_address: &Option<String>
) {
    let normalized_name = normalize_stream_name(stream_name, stream_id);
    let stream_id_str = stream_id.to_string();
    let peer_addr = peer_address.as_deref().unwrap_or("unknown");

    CONNECTION_EVENTS_TOTAL
        .with_label_values(&[
            normalized_name.as_str(),
            stream_id_str.as_str(),
            stream_type,
            direction,
            event_type,
            peer_addr
        ])
        .inc();
}

// Update active connections gauge
pub fn update_active_connections(
    stream_name: &Option<String>,
    stream_id: i64,
    stream_type: &str,
    direction: &str,
    connected: bool
) {
    let normalized_name = normalize_stream_name(stream_name, stream_id);

    let gauge = ACTIVE_CONNECTIONS
        .with_label_values(&[normalized_name.as_str(), stream_type, direction]);

    if connected {
        gauge.inc();
    } else {
        gauge.dec();
    }
}

// Record connection duration when it ends
pub fn record_connection_duration_event(
    stream_name: &Option<String>,
    stream_id: i64,
    stream_type: &str,
    direction: &str,
    duration_seconds: f64
) {
    let normalized_name = normalize_stream_name(stream_name, stream_id);

    CONNECTION_DURATIONS
        .with_label_values(&[normalized_name.as_str(), stream_type, direction])
        .observe(duration_seconds);
}