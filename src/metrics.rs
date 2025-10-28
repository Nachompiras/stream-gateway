use prometheus::{
    CounterVec, Gauge, GaugeVec, HistogramVec, Registry, TextEncoder
};
use once_cell::sync::Lazy;

// Global metrics registry
pub static METRICS_REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

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

pub static STREAM_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    let counter = CounterVec::new(
        prometheus::Opts::new("stream_gateway_errors_total", "Total number of stream errors"),
        &["stream_name", "stream_id", "stream_type", "error_type"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
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

// ==================== SRT-specific metrics ====================

// SRT Traffic metrics (Gauges for cumulative values reported by SRT)
pub static SRT_PACKETS_SENT_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_sent_total", "Total SRT packets sent (cumulative from SRT stats)"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_PACKETS_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_received_total", "Total SRT packets received (cumulative from SRT stats)"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BYTES_SENT_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bytes_sent_total", "Total SRT bytes sent (cumulative from SRT stats)"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BYTES_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bytes_received_total", "Total SRT bytes received (cumulative from SRT stats)"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

// SRT Quality/Loss metrics
pub static SRT_PACKETS_LOST_SENT_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_lost_sent_total", "Total SRT packets lost during sending"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_PACKETS_LOST_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_lost_received_total", "Total SRT packets lost during receiving"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_PACKETS_RETRANSMITTED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_retransmitted_total", "Total SRT packets retransmitted"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_PACKETS_DROPPED_SENT_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_dropped_sent_total", "Total SRT packets dropped during sending"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_PACKETS_DROPPED_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_packets_dropped_received_total", "Total SRT packets dropped during receiving"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BYTES_LOST_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bytes_lost_received_total", "Total SRT bytes lost during receiving"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BYTES_DROPPED_SENT_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bytes_dropped_sent_total", "Total SRT bytes dropped during sending"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BYTES_DROPPED_RECEIVED_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bytes_dropped_received_total", "Total SRT bytes dropped during receiving"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

// SRT Performance metrics
pub static SRT_SEND_RATE_MBPS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_send_rate_mbps", "SRT sending rate in Mbps"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RECEIVE_RATE_MBPS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_receive_rate_mbps", "SRT receiving rate in Mbps"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RTT_MS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_rtt_ms", "SRT Round Trip Time in milliseconds"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_BANDWIDTH_MBPS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_bandwidth_mbps", "SRT estimated bandwidth in Mbps"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_FLIGHT_SIZE_PACKETS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_flight_size_packets", "SRT number of packets in flight"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

// SRT Buffer metrics
pub static SRT_SEND_BUFFER_PACKETS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_send_buffer_packets", "SRT send buffer size in packets"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_SEND_BUFFER_BYTES: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_send_buffer_bytes", "SRT send buffer size in bytes"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_SEND_BUFFER_MS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_send_buffer_ms", "SRT send buffer size in milliseconds"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RECEIVE_BUFFER_PACKETS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_receive_buffer_packets", "SRT receive buffer size in packets"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RECEIVE_BUFFER_BYTES: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_receive_buffer_bytes", "SRT receive buffer size in bytes"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RECEIVE_BUFFER_MS: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_receive_buffer_ms", "SRT receive buffer size in milliseconds"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_SEND_BUFFER_AVAILABLE_BYTES: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_send_buffer_available_bytes", "SRT send buffer available space in bytes"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static SRT_RECEIVE_BUFFER_AVAILABLE_BYTES: Lazy<GaugeVec> = Lazy::new(|| {
    let gauge = GaugeVec::new(
        prometheus::Opts::new("srt_receive_buffer_available_bytes", "SRT receive buffer available space in bytes"),
        &["stream_name", "stream_id", "stream_type", "direction"]
    ).unwrap();
    METRICS_REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
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

// Record SRT statistics from SrtStats structure
pub fn record_srt_stats(
    stream_name: &Option<String>,
    stream_id: i64,
    stream_type: &str,
    direction: &str,
    stats: &srt_rs::SrtStats
) {
    let normalized_name = normalize_stream_name(stream_name, stream_id);
    let stream_id_str = stream_id.to_string();
    let labels = &[
        normalized_name.as_str(),
        stream_id_str.as_str(),
        stream_type,
        direction
    ];

    // Traffic metrics - cumulative counters from SRT
    SRT_PACKETS_SENT_TOTAL
        .with_label_values(labels)
        .set(stats.pktSentTotal as f64);

    SRT_PACKETS_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.pktRecvTotal as f64);

    SRT_BYTES_SENT_TOTAL
        .with_label_values(labels)
        .set(stats.byteSentTotal as f64);

    SRT_BYTES_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.byteRecvTotal as f64);

    // Quality/Loss metrics
    SRT_PACKETS_LOST_SENT_TOTAL
        .with_label_values(labels)
        .set(stats.pktSndLossTotal as f64);

    SRT_PACKETS_LOST_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.pktRcvLossTotal as f64);

    SRT_PACKETS_RETRANSMITTED_TOTAL
        .with_label_values(labels)
        .set(stats.pktRetransTotal as f64);

    SRT_PACKETS_DROPPED_SENT_TOTAL
        .with_label_values(labels)
        .set(stats.pktSndDropTotal as f64);

    SRT_PACKETS_DROPPED_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.pktRcvDropTotal as f64);

    SRT_BYTES_LOST_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.byteRcvLossTotal as f64);

    SRT_BYTES_DROPPED_SENT_TOTAL
        .with_label_values(labels)
        .set(stats.byteSndDropTotal as f64);

    SRT_BYTES_DROPPED_RECEIVED_TOTAL
        .with_label_values(labels)
        .set(stats.byteRcvDropTotal as f64);

    // Performance metrics - instantaneous gauges
    SRT_SEND_RATE_MBPS
        .with_label_values(labels)
        .set(stats.mbpsSendRate);

    SRT_RECEIVE_RATE_MBPS
        .with_label_values(labels)
        .set(stats.mbpsRecvRate);

    SRT_RTT_MS
        .with_label_values(labels)
        .set(stats.msRTT);

    SRT_BANDWIDTH_MBPS
        .with_label_values(labels)
        .set(stats.mbpsBandwidth);

    SRT_FLIGHT_SIZE_PACKETS
        .with_label_values(labels)
        .set(stats.pktFlightSize as f64);

    // Buffer metrics
    SRT_SEND_BUFFER_PACKETS
        .with_label_values(labels)
        .set(stats.pktSndBuf as f64);

    SRT_SEND_BUFFER_BYTES
        .with_label_values(labels)
        .set(stats.byteSndBuf as f64);

    SRT_SEND_BUFFER_MS
        .with_label_values(labels)
        .set(stats.msSndBuf as f64);

    SRT_RECEIVE_BUFFER_PACKETS
        .with_label_values(labels)
        .set(stats.pktRcvBuf as f64);

    SRT_RECEIVE_BUFFER_BYTES
        .with_label_values(labels)
        .set(stats.byteRcvBuf as f64);

    SRT_RECEIVE_BUFFER_MS
        .with_label_values(labels)
        .set(stats.msRcvBuf as f64);

    SRT_SEND_BUFFER_AVAILABLE_BYTES
        .with_label_values(labels)
        .set(stats.byteAvailSndBuf as f64);

    SRT_RECEIVE_BUFFER_AVAILABLE_BYTES
        .with_label_values(labels)
        .set(stats.byteAvailRcvBuf as f64);
}