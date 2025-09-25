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
        &["input_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static OUTPUT_BYTES_SENT: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_output_bytes_sent_total", "Total bytes sent by output streams"),
        &["input_id", "output_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static INPUT_PACKETS_RECEIVED: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_input_packets_received_total", "Total packets received by input streams"),
        &["input_id", "stream_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static OUTPUT_PACKETS_SENT: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_output_packets_sent_total", "Total packets sent by output streams"),
        &["input_id", "output_id", "stream_type"]
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
        &["stream_type", "error_type"]
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

pub fn record_input_bytes(input_id: &str, stream_type: &str, bytes: u64) {
    INPUT_BYTES_RECEIVED
        .with_label_values(&[input_id, stream_type])
        .inc_by(bytes as f64);
}

pub fn record_output_bytes(input_id: &str, output_id: &str, stream_type: &str, bytes: u64) {
    OUTPUT_BYTES_SENT
        .with_label_values(&[input_id, output_id, stream_type])
        .inc_by(bytes as f64);
}

pub fn record_input_packets(input_id: &str, stream_type: &str, packets: u64) {
    INPUT_PACKETS_RECEIVED
        .with_label_values(&[input_id, stream_type])
        .inc_by(packets as f64);
}

pub fn record_output_packets(input_id: &str, output_id: &str, stream_type: &str, packets: u64) {
    OUTPUT_PACKETS_SENT
        .with_label_values(&[input_id, output_id, stream_type])
        .inc_by(packets as f64);
}

pub fn record_stream_error(stream_type: &str, error_type: &str) {
    STREAM_ERRORS
        .with_label_values(&[stream_type, error_type])
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