use bytes::Bytes;
use futures::stream::{AbortHandle};
use srt_rs::{self as srt, SrtAsyncStream};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use tokio::sync::{broadcast, RwLock, mpsc}; // Agregamos mpsc
use async_trait::async_trait;          // 1-liner: macro para traits async
use std::io;
use sqlx::FromRow;
use tokio::task::JoinHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const BROADCAST_CAPACITY: usize = 1024; // Capacidad del buffer del canal broadcast

// Información sobre un stream de entrada activo


#[derive(Debug)]
pub struct InputInfo {
    pub id:               i64,
    pub name:             Option<String>,
    pub status:           StreamStatus,
    pub packet_tx:        broadcast::Sender<Bytes>,
    pub stats:            StatsCell,
    pub task_handle:      Option<JoinHandle<()>>,  // None when stopped
    pub config:           CreateInputRequest,      // Store config to restart
    pub output_tasks:     HashMap<i64, OutputInfo>,
    pub stopped_outputs:  HashMap<i64, CreateOutputRequest>, // Stopped outputs config
    pub analysis_tasks:   HashMap<String, AnalysisInfo>,
    pub paused_analysis:  Vec<AnalysisType>,       // Analysis that were active when input stopped
    pub started_at:       Option<std::time::SystemTime>, // When stream started, None when stopped
    pub connected_at:     Option<std::time::SystemTime>, // When stream connected, None when not connected
    pub state_tx:         Option<StateChangeSender>, // Channel to notify state changes
    pub source_address:   Option<String>,          // Address of the connected source (for SRT listeners)
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub id:           i64,
    pub name:         Option<String>,
    pub input_id:     i64,
    pub kind:         OutputKind,
    pub status:       StreamStatus,
    pub stats:        StatsCell,
    pub destination:  String,
    pub abort_handle: Option<AbortHandle>,      // None when stopped
    pub config:       CreateOutputRequest,     // Store config to restart
    pub started_at:   Option<std::time::SystemTime>, // When stream started, None when stopped
    pub connected_at: Option<std::time::SystemTime>, // When stream connected, None when not connected
    pub state_tx:     Option<StateChangeSender>, // Channel to notify state changes
    pub peer_address: Option<String>,          // Address of the connected peer (for SRT listeners)
}


#[derive(Debug,Clone, PartialEq, Eq)]
pub enum OutputKind {                 // para log o API
    Udp,
    SrtCaller,
    SrtListener,
}

impl fmt::Display for OutputKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputKind::Udp => write!(f, "UDP"),
            OutputKind::SrtCaller => write!(f, "SRT Caller"),
            OutputKind::SrtListener => write!(f, "SRT Listener"),
        }
    }
}

#[derive(Serialize,Deserialize, Debug, Clone, Default)]
pub struct SrtCommonConfig {
    pub latency_ms: Option<i32>,
    pub stream_id: Option<String>,
    pub passphrase: Option<String>,    
}

impl SrtCommonConfig {
    /// Devuelve un `srt::Builder` ya pre-configurado con los campos de
    /// `self`.
    pub fn builder(&self) -> srt::SrtBuilder {
        let mut b = srt::builder();
        if let Some(lat)  = self.latency_ms { b = b.set_peer_latency(lat); }
        b = b.set_stream_id(self.stream_id.clone());
        b = b.set_passphrase(self.passphrase.clone());
        b.set_live_transmission_type()
    }

    pub fn async_builder(&self) -> srt::SrtAsyncBuilder {
        let mut b = srt::async_builder();
        if let Some(lat)  = self.latency_ms { b = b.set_peer_latency(lat); }
        b = b.set_stream_id(self.stream_id.clone());
        b = b.set_passphrase(self.passphrase.clone());
        b.set_live_transmission_type()
    }
}

// --- Estructuras para las peticiones API ---
#[derive(Serialize,Deserialize, Debug,Clone)]
#[serde(tag = "type")] // Usa el campo "type" para determinar qué variante deserializar
pub enum CreateInputRequest {
    #[serde(rename = "udp")]
    Udp {
        // New naming scheme
        #[serde(default)]
        bind_host: Option<String>,  // Optional, defaults to "0.0.0.0"
        #[serde(default)]
        bind_port: Option<u16>,     // New field
        #[serde(default)]
        automatic_port: Option<bool>, // If true, automatically assign an available port
        name: Option<String>,

        // Multicast support
        #[serde(default)]
        multicast_group: Option<String>,  // Multicast group to join (e.g. "239.1.1.1")
        #[serde(default)]
        source_specific_multicast: Option<String>,  // Source IP for SSM (optional)

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        listen_port: Option<u16>,  // Deprecated, use bind_port
    },
    #[serde(rename = "srt")]
    Srt {
        name: Option<String>,
        #[serde(flatten)] // Absorbe los campos del enum interno
        config: SrtInputConfig,
    },
    #[serde(rename = "spts")]
    Spts {
        source_input_id: i64,
        program_number: u16,
        #[serde(default)]
        fill_with_nulls: Option<bool>,
        name: Option<String>,
    },
}

#[derive(Serialize,Deserialize, Debug,Clone)]
#[serde(tag = "mode")] // Dentro de SRT, usa "mode"
pub enum SrtInputConfig {
    #[serde(rename = "listener")]
    Listener {
        // New naming scheme
        #[serde(default)]
        bind_host: Option<String>,  // Optional, defaults to "0.0.0.0"
        #[serde(default)]
        bind_port: Option<u16>,     // New field
        #[serde(default)]
        automatic_port: Option<bool>, // If true, automatically assign an available port
        #[serde(flatten)]
        common: SrtCommonConfig,

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        listen_port: Option<u16>,  // Deprecated, use bind_port
    },
    #[serde(rename = "caller")]
    Caller {
        // New naming scheme
        #[serde(default)]
        remote_host: Option<String>, // New field
        #[serde(default)]
        remote_port: Option<u16>,    // New field
        #[serde(flatten)]
        common: SrtCommonConfig,

        // Network interface binding for multi-NIC support
        #[serde(default)]
        bind_host: Option<String>,  // Local IP to bind from (for multi-NIC scenarios)

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        target_addr: Option<String>,  // Deprecated, use remote_host:remote_port
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum CreateOutputRequest {
    #[serde(rename = "udp")]
    Udp {
        input_id: i64,            // ID del input al que conectar
        // New naming scheme
        #[serde(default)]
        remote_host: Option<String>, // New field
        #[serde(default)]
        remote_port: Option<u16>,    // New field
        #[serde(default)]
        automatic_port: Option<bool>, // If true, automatically assign an available port
        name: Option<String>,

        // Network interface binding for multi-NIC support
        #[serde(default)]
        bind_host: Option<String>,  // Source IP to bind from (for multi-NIC scenarios)

        // Multicast output support
        #[serde(default)]
        multicast_ttl: Option<u8>,  // TTL for multicast packets (1-255, default varies by OS)
        #[serde(default)]
        multicast_interface: Option<String>,  // Interface for multicast sending (IP address)

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        destination_addr: Option<String>, // Deprecated, use remote_host:remote_port
    },
    #[serde(rename = "srt")]
    Srt {
        input_id: i64,
        name: Option<String>,
        #[serde(flatten)] // Absorbe los campos del enum interno
        config: SrtOutputConfig,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "mode")] // Dentro de SRT, usa "mode"
pub enum SrtOutputConfig {
    #[serde(rename = "listener")]
    Listener {
        // New naming scheme
        #[serde(default)]
        bind_host: Option<String>,  // Optional, defaults to "0.0.0.0"
        #[serde(default)]
        bind_port: Option<u16>,     // New field
        #[serde(default)]
        automatic_port: Option<bool>, // If true, automatically assign an available port
        #[serde(flatten)]
        common: SrtCommonConfig,

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        listen_port: Option<u16>,  // Deprecated, use bind_port
    },
    #[serde(rename = "caller")]
    Caller {
        // New naming scheme
        #[serde(default)]
        remote_host: Option<String>, // New field
        #[serde(default)]
        remote_port: Option<u16>,    // New field
        #[serde(flatten)]
        common: SrtCommonConfig,

        // Network interface binding for multi-NIC support
        #[serde(default)]
        bind_host: Option<String>,  // Local IP to bind from (for multi-NIC scenarios)

        // Legacy fields for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        destination_addr: Option<String>, // Deprecated, use remote_host:remote_port
    },
}

#[derive(Deserialize)]
pub struct DeleteInputRequest {
    pub input_id: i64,
}

#[derive(Deserialize)]
pub struct DeleteOutputRequest {
    pub input_id: i64,
    pub output_id: i64,
}

// Update request types for editing existing inputs/outputs
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum UpdateInputRequest {
    #[serde(rename = "udp")]
    Udp {
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        multicast_group: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_specific_multicast: Option<String>,
    },
    #[serde(rename = "srt")]
    Srt {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(flatten)]
        config: UpdateSrtInputConfig,
    },
    #[serde(rename = "spts")]
    Spts {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fill_with_nulls: Option<bool>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "mode")]
pub enum UpdateSrtInputConfig {
    #[serde(rename = "listener")]
    Listener {
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
    #[serde(rename = "caller")]
    Caller {
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        local_host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum UpdateOutputRequest {
    #[serde(rename = "udp")]
    Udp {
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        local_host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        multicast_ttl: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        multicast_interface: Option<String>,
    },
    #[serde(rename = "srt")]
    Srt {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(flatten)]
        config: UpdateSrtOutputConfig,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "mode")]
pub enum UpdateSrtOutputConfig {
    #[serde(rename = "listener")]
    Listener {
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
    #[serde(rename = "caller")]
    Caller {
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_host")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "remote_port")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none", alias = "bind_host")]
        local_host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
}

#[derive(Serialize)]
pub struct InputResponse {
    pub id: i64,
    pub name: Option<String>,
    pub status: String,
    pub host: Option<String>, // Host address (for listeners) or remote host (for callers)
    pub port: Option<u16>, // Port bound/connected to
    pub outputs: Vec<OutputResponse>, // Lista de outputs asociados
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub source_address: Option<String>, // Address of connected source (for SRT listeners)
    pub bitrate_bps: Option<u64>, // Bitrate in bits per second
}

#[derive(Serialize)]
pub struct OutputResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_id: i64,
    pub destination: String,
    pub output_type: String, // "UDP" o "SRT Caller"
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub peer_address: Option<String>, // Address of connected peer (for SRT listeners)
    pub bitrate_bps: Option<u64>, // Bitrate in bits per second
}

// New response models for CRUD endpoints
#[derive(Serialize)]
pub struct InputListResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_type: String, // "UDP", "SRT Listener", "SRT Caller"
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub output_count: usize, // Número de outputs asociados
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub source_address: Option<String>, // Address of connected source (for SRT listeners)
}

#[derive(Serialize)]
pub struct InputDetailResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_type: String,
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub outputs: Vec<OutputDetailResponse>,
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub source_address: Option<String>, // Address of connected source (for SRT listeners)
    pub config: Option<String>, // Full configuration JSON from database
}

#[derive(Serialize)]
pub struct OutputDetailResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_id: i64,
    pub destination: String,
    pub output_type: String,
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub config: Option<String>, // JSON config if needed
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub peer_address: Option<String>, // Address of connected peer (for SRT listeners)
}

#[derive(Serialize)]
pub struct OutputListResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_id: i64,
    pub input_name: Option<String>, // Para contexto en la lista
    pub destination: String,
    pub output_type: String,
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub peer_address: Option<String>, // Address of connected peer (for SRT listeners)
    pub config: Option<String>, // Full configuration JSON from database
}

/* SRT models */
pub type SrtStats = srt::SrtStats;

pub type StatsCell = Arc<RwLock<Option<InputStats>>>;

#[derive(Clone,Debug)]
pub enum InputStats {
    Srt(Box<SrtStats>),
    Udp(UdpStats),
}

// Custom Serialize implementation for InputStats to handle SrtStats serialization
impl serde::Serialize for InputStats {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            InputStats::Srt(stats) => {
                // Serialize only fields of SrtStats that are serializable, or as a string/debug
                serializer.serialize_str(&format!("{:?}", stats))
            }
            InputStats::Udp(stats) => stats.serialize(serializer),
        }
    }
}

#[derive(Clone, Default, Serialize, Debug)]
pub struct UdpStats {
    pub total_packets:     u64,
    pub total_bytes:       u64,
    pub packets_per_sec:   u64,
    pub bitrate_bps:       u64,
}

// Lock-free UDP stats using atomics
#[derive(Debug)]
pub struct UdpStatsAtomic {
    pub total_packets:     AtomicU64,
    pub total_bytes:       AtomicU64,
    pub packets_per_sec:   AtomicU64,
    pub bitrate_bps:       AtomicU64,
}

impl UdpStatsAtomic {
    pub fn new() -> Self {
        Self {
            total_packets: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            packets_per_sec: AtomicU64::new(0),
            bitrate_bps: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> UdpStats {
        UdpStats {
            total_packets: self.total_packets.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            packets_per_sec: self.packets_per_sec.load(Ordering::Relaxed),
            bitrate_bps: self.bitrate_bps.load(Ordering::Relaxed),
        }
    }
}

/// Rasgo: “dame un socket listo para recibir datos SRT”.
#[async_trait]
pub trait SrtSource: Send + Sync + 'static {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream>;
}

#[async_trait]
pub trait SrtSink: Send + Sync + 'static {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream>;
}

pub struct Forwarder;
pub struct ForwardHandle {
    pub tx:     broadcast::Sender<Bytes>,
    pub handle: JoinHandle<()>,
    pub stats:  StatsCell,        // si también las quieres
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct InputRow {
    pub id:          i64,
    pub name:        Option<String>,
    pub kind:        String,     // "udp", "srt_listener", "srt_caller"
    pub config_json: String,
    pub status:      String,     // "running", "stopped"
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct OutputRow {
    pub id:          i64,
    pub name:        Option<String>,
    pub input_id:    i64,
    pub kind:        String,
    pub destination: Option<String>,
    pub listen_port: Option<u16>,
    pub config_json: Option<String>,
    pub status:      String,     // "running", "stopped"
}

pub fn output_kind_string(k: &OutputKind) -> &'static str {
    match k {
        OutputKind::Udp        => "udp",
        OutputKind::SrtCaller  => "srt_caller",
        OutputKind::SrtListener=> "srt_listener",
    }
}

pub fn input_kind_string(k: &CreateInputRequest) -> &'static str {
    match k {
        CreateInputRequest::Udp { .. } => "udp",
        CreateInputRequest::Srt { config, .. } => match config {
            SrtInputConfig::Listener { .. } => "srt_listener",
            SrtInputConfig::Caller { .. }   => "srt_caller",
        },
        CreateInputRequest::Spts { .. } => "spts",
    }
}

pub fn input_type_display_string(kind: &str) -> &'static str {
    match kind {
        "udp" => "UDP Listener",
        "srt_listener" => "SRT Listener",
        "srt_caller" => "SRT Caller",
        "spts" => "SPTS Filter",
        _ => "Unknown",
    }
}

// Helper functions for backward compatibility and field extraction

impl CreateInputRequest {
    /// Check if automatic port assignment is requested
    pub fn is_automatic_port(&self) -> bool {
        match self {
            CreateInputRequest::Udp { automatic_port, bind_port, listen_port, .. } => {
                // If automatic_port is explicitly set to true, use auto port
                if automatic_port == &Some(true) {
                    return true;
                }
                // If automatic_port is explicitly false, use specified ports
                if automatic_port == &Some(false) {
                    return false;
                }
                // If automatic_port is None, check port values (0 or None means auto)
                crate::port_utils::should_use_auto_port_input(*bind_port, *listen_port)
            },
            CreateInputRequest::Srt { config, .. } => config.is_automatic_port(),
            CreateInputRequest::Spts { .. } => false, // SPTS inputs don't need ports
        }
    }

    /// Get the effective bind port, handling backward compatibility
    pub fn get_bind_port(&self) -> u16 {
        match self {
            CreateInputRequest::Udp { bind_port, listen_port, .. } => {
                // Prefer legacy field first for backward compatibility, then new field
                listen_port.or(*bind_port).unwrap_or(0)
            },
            CreateInputRequest::Srt { config, .. } => config.get_bind_port(),
            CreateInputRequest::Spts { .. } => 0, // SPTS inputs don't bind to ports
        }
    }
    
    /// Get the effective bind host, handling backward compatibility
    pub fn get_bind_host(&self) -> String {
        match self {
            CreateInputRequest::Udp { bind_host, .. } => {
                bind_host.clone().unwrap_or_else(|| "0.0.0.0".to_string())
            },
            CreateInputRequest::Srt { config, .. } => config.get_bind_host(),
            CreateInputRequest::Spts { .. } => "0.0.0.0".to_string(), // SPTS inputs don't bind
        }
    }
    
    /// Get remote host for caller modes
    pub fn get_remote_host(&self) -> Option<String> {
        match self {
            CreateInputRequest::Udp { .. } => None, // UDP inputs don't have remote hosts
            CreateInputRequest::Srt { config, .. } => config.get_remote_host(),
            CreateInputRequest::Spts { .. } => None, // SPTS inputs don't have remote hosts
        }
    }
    
    /// Get remote port for caller modes
    pub fn get_remote_port(&self) -> Option<u16> {
        match self {
            CreateInputRequest::Udp { .. } => None, // UDP inputs don't have remote ports
            CreateInputRequest::Srt { config, .. } => config.get_remote_port(),
            CreateInputRequest::Spts { .. } => None, // SPTS inputs don't have remote ports
        }
    }
}

impl SrtInputConfig {
    /// Check if automatic port assignment is requested
    pub fn is_automatic_port(&self) -> bool {
        match self {
            SrtInputConfig::Listener { automatic_port, bind_port, listen_port, .. } => {
                // If automatic_port is explicitly set to true, use auto port
                if automatic_port == &Some(true) {
                    return true;
                }
                // If automatic_port is explicitly false, use specified ports
                if automatic_port == &Some(false) {
                    return false;
                }
                // If automatic_port is None, check port values (0 or None means auto)
                crate::port_utils::should_use_auto_port_input(*bind_port, *listen_port)
            },
            SrtInputConfig::Caller { .. } => false, // Callers never need auto port assignment
        }
    }

    /// Get the effective bind port for SRT config
    pub fn get_bind_port(&self) -> u16 {
        match self {
            SrtInputConfig::Listener { bind_port, listen_port, .. } => {
                // Prefer legacy field first for backward compatibility, then new field
                listen_port.or(*bind_port).unwrap_or(0)
            },
            SrtInputConfig::Caller { .. } => 0, // Callers don't bind
        }
    }
    
    /// Get the effective bind host for SRT config
    pub fn get_bind_host(&self) -> String {
        match self {
            SrtInputConfig::Listener { bind_host, .. } => {
                bind_host.clone().unwrap_or_else(|| "0.0.0.0".to_string())
            },
            SrtInputConfig::Caller { .. } => "0.0.0.0".to_string(), // Callers don't bind
        }
    }
    
    /// Get remote host for SRT caller
    pub fn get_remote_host(&self) -> Option<String> {
        match self {
            SrtInputConfig::Listener { .. } => None,
            SrtInputConfig::Caller { remote_host, target_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(host) = remote_host {
                    if !host.is_empty() {
                        Some(host.clone())
                    } else {
                        None
                    }
                } else if let Some(addr) = target_addr {
                    // Parse "host:port" format
                    addr.split(':').next().map(|s| s.to_string())
                } else {
                    None
                }
            },
        }
    }

    /// Get local bind host for SRT caller
    pub fn get_caller_bind_host(&self) -> Option<String> {
        match self {
            SrtInputConfig::Listener { .. } => None,
            SrtInputConfig::Caller { bind_host, .. } => bind_host.clone(),
        }
    }
    
    /// Get remote port for SRT caller
    pub fn get_remote_port(&self) -> Option<u16> {
        match self {
            SrtInputConfig::Listener { .. } => None,
            SrtInputConfig::Caller { remote_port, target_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(port) = remote_port {
                    if *port != 0 {
                        Some(*port)
                    } else {
                        None
                    }
                } else if let Some(addr) = target_addr {
                    // Parse "host:port" format
                    addr.split(':').nth(1).and_then(|s| s.parse().ok())
                } else {
                    None
                }
            },
        }
    }
}

impl CreateOutputRequest {
    /// Check if automatic port assignment is requested
    pub fn is_automatic_port(&self) -> bool {
        match self {
            CreateOutputRequest::Udp { port, _legacy_remote_port, _legacy_automatic_port, _legacy_destination_addr, .. } => {
                // Check new field first
                if let Some(p) = port {
                    return *p == 0;
                }
                // Check legacy fields
                if let Some(true) = _legacy_automatic_port {
                    return true;
                }
                // Check legacy port field
                if let Some(p) = _legacy_remote_port {
                    return p == 0;
                }
                // Try parsing from legacy destination_addr
                if let Some(addr) = _legacy_destination_addr {
                    if let Some(port_str) = addr.split(':').nth(1) {
                        if let Ok(p) = port_str.parse::<u16>() {
                            return p == 0;
                        }
                    }
                }
                true // Default to auto if no port specified
            },
            CreateOutputRequest::Srt { config, .. } => config.is_automatic_port(),
        }
    }

    /// Get the effective host
    pub fn get_host(&self) -> String {
        match self {
            CreateOutputRequest::Udp { host, _legacy_remote_host, _legacy_destination_addr, .. } => {
                // Prefer new field, fallback to legacy fields
                if let Some(h) = host.clone().or_else(|| _legacy_remote_host.clone()) {
                    if !h.is_empty() {
                        return h;
                    }
                }
                // Try parsing from legacy destination_addr
                if let Some(addr) = _legacy_destination_addr {
                    if let Some(host_part) = addr.split(':').next() {
                        return host_part.to_string();
                    }
                }
                "127.0.0.1".to_string()
            },
            CreateOutputRequest::Srt { config, .. } => config.get_host(),
        }
    }

    /// Get the effective port
    pub fn get_port(&self) -> u16 {
        match self {
            CreateOutputRequest::Udp { port, _legacy_remote_port, _legacy_destination_addr, .. } => {
                // Prefer new field, fallback to legacy fields
                if let Some(p) = port.or(*_legacy_remote_port) {
                    return p;
                }
                // Try parsing from legacy destination_addr
                if let Some(addr) = _legacy_destination_addr {
                    if let Some(port_str) = addr.split(':').nth(1) {
                        if let Ok(p) = port_str.parse() {
                            return p;
                        }
                    }
                }
                0
            },
            CreateOutputRequest::Srt { config, .. } => config.get_port(),
        }
    }

    /// Get local bind host for UDP outputs (for multi-NIC support)
    pub fn get_local_host(&self) -> Option<String> {
        match self {
            CreateOutputRequest::Udp { local_host, _legacy_bind_host, .. } => {
                local_host.clone().or_else(|| _legacy_bind_host.clone())
            },
            CreateOutputRequest::Srt { config, .. } => config.get_local_host(),
        }
    }

    /// Extract port from configuration (for responses)
    pub fn extract_port(&self) -> Option<u16> {
        match self {
            CreateOutputRequest::Udp { port, _legacy_remote_port, .. } => {
                port.or(*_legacy_remote_port).filter(|&p| p != 0)
            },
            CreateOutputRequest::Srt { config, .. } => config.extract_port(),
        }
    }

    pub fn extract_host(&self) -> Option<String> {
        match self {
            CreateOutputRequest::Udp { host, _legacy_remote_host, .. } => {
                host.clone().or_else(|| _legacy_remote_host.clone())
            },
            CreateOutputRequest::Srt { config, .. } => config.extract_host(),
        }
    }
}

impl SrtOutputConfig {
    /// Check if automatic port assignment is requested
    pub fn is_automatic_port(&self) -> bool {
        match self {
            SrtOutputConfig::Listener { port, _legacy_bind_port, _legacy_listen_port, _legacy_automatic_port, .. } => {
                // Check new field first
                if let Some(p) = port {
                    return *p == 0;
                }
                // Check legacy fields
                if let Some(true) = _legacy_automatic_port {
                    return true;
                }
                // Check legacy port fields
                _legacy_listen_port.or(*_legacy_bind_port).map(|p| p == 0).unwrap_or(true)
            },
            SrtOutputConfig::Caller { .. } => false, // Callers connect to existing listeners, don't need auto port
        }
    }

    /// Get the effective host
    pub fn get_host(&self) -> String {
        match self {
            SrtOutputConfig::Listener { host, _legacy_bind_host, .. } => {
                host.clone().or_else(|| _legacy_bind_host.clone()).unwrap_or_else(|| "0.0.0.0".to_string())
            },
            SrtOutputConfig::Caller { host, _legacy_remote_host, _legacy_destination_addr, .. } => {
                // For callers, host is the remote host
                if let Some(h) = host.clone().or_else(|| _legacy_remote_host.clone()) {
                    if !h.is_empty() {
                        return h;
                    }
                }
                // Try parsing from legacy destination_addr
                if let Some(addr) = _legacy_destination_addr {
                    if let Some(host_part) = addr.split(':').next() {
                        return host_part.to_string();
                    }
                }
                "127.0.0.1".to_string()
            },
        }
    }

    /// Get the effective port
    pub fn get_port(&self) -> u16 {
        match self {
            SrtOutputConfig::Listener { port, _legacy_bind_port, _legacy_listen_port, .. } => {
                port.or(*_legacy_listen_port).or(*_legacy_bind_port).unwrap_or(0)
            },
            SrtOutputConfig::Caller { port, _legacy_remote_port, _legacy_destination_addr, .. } => {
                // For callers, port is the remote port
                if let Some(p) = port.or(*_legacy_remote_port) {
                    return p;
                }
                // Try parsing from legacy destination_addr
                if let Some(addr) = _legacy_destination_addr {
                    if let Some(port_str) = addr.split(':').nth(1) {
                        if let Ok(p) = port_str.parse() {
                            return p;
                        }
                    }
                }
                0
            },
        }
    }

    /// Get local bind host for SRT caller output (for multi-NIC support)
    pub fn get_local_host(&self) -> Option<String> {
        match self {
            SrtOutputConfig::Listener { .. } => None,
            SrtOutputConfig::Caller { local_host, _legacy_bind_host, .. } => {
                local_host.clone().or_else(|| _legacy_bind_host.clone())
            },
        }
    }

    /// Extract port from configuration (for responses)
    pub fn extract_port(&self) -> Option<u16> {
        match self {
            SrtOutputConfig::Listener { port, _legacy_bind_port, _legacy_listen_port, .. } => {
                port.or(*_legacy_listen_port).or(*_legacy_bind_port).filter(|&p| p != 0)
            },
            SrtOutputConfig::Caller { port, _legacy_remote_port, .. } => {
                port.or(*_legacy_remote_port).filter(|&p| p != 0)
            },
        }
    }

    pub fn extract_host(&self) -> Option<String> {
        match self {
            SrtOutputConfig::Listener { host, _legacy_bind_host, .. } => {
                host.clone().or_else(|| _legacy_bind_host.clone())
            },
            SrtOutputConfig::Caller { host, _legacy_remote_host, .. } => {
                host.clone().or_else(|| _legacy_remote_host.clone())
            },
        }
    }
}

// Stream connection status with granular states
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamStatus {
    Stopped,      // Stream detenido manualmente
    Listening,    // UDP bound / SRT Listener esperando conexiones
    Connecting,   // SRT Caller intentando conectar
    Connected,    // Conexión establecida y funcionando
    Reconnecting, // SRT perdió conexión, reintentando
    Error,        // Error irrecuperable
}

impl fmt::Display for StreamStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamStatus::Stopped => write!(f, "stopped"),
            StreamStatus::Listening => write!(f, "listening"),
            StreamStatus::Connecting => write!(f, "connecting"),
            StreamStatus::Connected => write!(f, "connected"),
            StreamStatus::Reconnecting => write!(f, "reconnecting"),
            StreamStatus::Error => write!(f, "error"),
        }
    }
}

impl std::str::FromStr for StreamStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stopped" => Ok(StreamStatus::Stopped),
            "listening" => Ok(StreamStatus::Listening),
            "connecting" => Ok(StreamStatus::Connecting),
            "connected" => Ok(StreamStatus::Connected),
            "reconnecting" => Ok(StreamStatus::Reconnecting),
            "error" => Ok(StreamStatus::Error),
            // Backward compatibility
            "running" => Ok(StreamStatus::Connected),
            _ => Err(format!("Invalid stream status: {}", s)),
        }
    }
}

impl StreamStatus {
    /// Returns true if the stream is in a connected state where uptime should be counted
    pub fn is_connected(&self) -> bool {
        matches!(self, StreamStatus::Connected)
    }

    /// Returns true if the stream is active (not stopped or error)
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            StreamStatus::Listening | StreamStatus::Connecting | StreamStatus::Connected | StreamStatus::Reconnecting
        )
    }
}

// State notification system
#[derive(Debug, Clone)]
pub enum StateChange {
    InputStateChanged {
        input_id: i64,
        new_status: StreamStatus,
        connected_at: Option<std::time::SystemTime>,
        source_address: Option<String>,
    },
    OutputStateChanged {
        input_id: i64,
        output_id: i64,
        new_status: StreamStatus,
        connected_at: Option<std::time::SystemTime>,
        peer_address: Option<String>,
    },
}

// Type alias for the state change sender
pub type StateChangeSender = mpsc::UnboundedSender<StateChange>;

// MPEG-TS Analysis types and structures
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnalysisType {
    Mux,
    Tr101,
}

impl fmt::Display for AnalysisType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnalysisType::Mux => write!(f, "mux"),
            AnalysisType::Tr101 => write!(f, "tr101"),
        }
    }
}

impl std::str::FromStr for AnalysisType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mux" => Ok(AnalysisType::Mux),
            "tr101" => Ok(AnalysisType::Tr101),
            _ => Err(format!("Invalid analysis type: {}", s)),
        }
    }
}

#[derive(Debug)]
pub struct AnalysisInfo {
    pub id: String, // Unique identifier for this analysis
    pub analysis_type: AnalysisType,
    pub input_id: i64,
    pub task_handle: JoinHandle<()>,
    pub created_at: std::time::SystemTime,
    pub report_data: Arc<RwLock<Option<AnalysisDataReport>>>, // Latest analysis data
}

// API request/response models for analysis endpoints
#[derive(Serialize)]
pub struct AnalysisStatusResponse {
    pub id: String,
    pub analysis_type: String,
    pub input_id: i64,
    pub status: String, // "running", "stopped", "error"
    pub created_at: String, // ISO 8601 format
}

#[derive(Serialize)]
pub struct AnalysisListResponse {
    pub input_id: i64,
    pub active_analyses: Vec<AnalysisStatusResponse>,
}

// Helper functions to merge update requests with existing configurations
impl UpdateInputRequest {
    pub fn merge_with(&self, existing: &CreateInputRequest) -> CreateInputRequest {
        match (self, existing) {
            (UpdateInputRequest::Udp { host, port, name, multicast_group, source_specific_multicast },
             CreateInputRequest::Udp { host: ex_host, port: ex_port, name: ex_name,
                                       multicast_group: ex_mcast, source_specific_multicast: ex_ssm, .. }) => {
                CreateInputRequest::Udp {
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    name: name.clone().or_else(|| ex_name.clone()),
                    multicast_group: multicast_group.clone().or_else(|| ex_mcast.clone()),
                    source_specific_multicast: source_specific_multicast.clone().or_else(|| ex_ssm.clone()),
                    // Legacy fields - don't expose in merge, they're only for backward compat deserialization
                    _legacy_bind_host: None,
                    _legacy_bind_port: None,
                    _legacy_listen_port: None,
                    _legacy_automatic_port: None,
                }
            },
            (UpdateInputRequest::Srt { name, config },
             CreateInputRequest::Srt { name: ex_name, config: ex_config }) => {
                CreateInputRequest::Srt {
                    name: name.clone().or_else(|| ex_name.clone()),
                    config: config.merge_with(ex_config),
                }
            },
            (UpdateInputRequest::Spts { name, fill_with_nulls },
             CreateInputRequest::Spts { name: ex_name, source_input_id, program_number, fill_with_nulls: ex_fill }) => {
                CreateInputRequest::Spts {
                    source_input_id: *source_input_id,
                    program_number: *program_number,
                    fill_with_nulls: fill_with_nulls.or(*ex_fill),
                    name: name.clone().or_else(|| ex_name.clone()),
                }
            },
            _ => existing.clone(), // Type mismatch, return existing
        }
    }
}

impl UpdateSrtInputConfig {
    pub fn merge_with(&self, existing: &SrtInputConfig) -> SrtInputConfig {
        match (self, existing) {
            (UpdateSrtInputConfig::Listener { host, port, latency_ms, stream_id, passphrase },
             SrtInputConfig::Listener { host: ex_host, port: ex_port, common, .. }) => {
                SrtInputConfig::Listener {
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    common: SrtCommonConfig {
                        latency_ms: latency_ms.or(common.latency_ms),
                        stream_id: stream_id.clone().or_else(|| common.stream_id.clone()),
                        passphrase: passphrase.clone().or_else(|| common.passphrase.clone()),
                    },
                    // Legacy fields
                    _legacy_bind_host: None,
                    _legacy_bind_port: None,
                    _legacy_listen_port: None,
                    _legacy_automatic_port: None,
                }
            },
            (UpdateSrtInputConfig::Caller { host, port, local_host, latency_ms, stream_id, passphrase },
             SrtInputConfig::Caller { host: ex_host, port: ex_port, local_host: ex_local_host, common, .. }) => {
                SrtInputConfig::Caller {
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    local_host: local_host.clone().or_else(|| ex_local_host.clone()),
                    common: SrtCommonConfig {
                        latency_ms: latency_ms.or(common.latency_ms),
                        stream_id: stream_id.clone().or_else(|| common.stream_id.clone()),
                        passphrase: passphrase.clone().or_else(|| common.passphrase.clone()),
                    },
                    // Legacy fields
                    _legacy_remote_host: None,
                    _legacy_remote_port: None,
                    _legacy_bind_host: None,
                    _legacy_target_addr: None,
                }
            },
            _ => existing.clone(), // Mode mismatch, return existing
        }
    }
}

impl UpdateOutputRequest {
    pub fn merge_with(&self, existing: &CreateOutputRequest, input_id: i64) -> CreateOutputRequest {
        match (self, existing) {
            (UpdateOutputRequest::Udp { host, port, name, local_host, multicast_ttl, multicast_interface },
             CreateOutputRequest::Udp { host: ex_host, port: ex_port, name: ex_name, local_host: ex_local_host,
                                        multicast_ttl: ex_ttl, multicast_interface: ex_iface, .. }) => {
                CreateOutputRequest::Udp {
                    input_id,
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    name: name.clone().or_else(|| ex_name.clone()),
                    local_host: local_host.clone().or_else(|| ex_local_host.clone()),
                    multicast_ttl: multicast_ttl.or(*ex_ttl),
                    multicast_interface: multicast_interface.clone().or_else(|| ex_iface.clone()),
                    // Legacy fields
                    _legacy_remote_host: None,
                    _legacy_remote_port: None,
                    _legacy_bind_host: None,
                    _legacy_automatic_port: None,
                    _legacy_destination_addr: None,
                }
            },
            (UpdateOutputRequest::Srt { name, config },
             CreateOutputRequest::Srt { name: ex_name, config: ex_config, .. }) => {
                CreateOutputRequest::Srt {
                    input_id,
                    name: name.clone().or_else(|| ex_name.clone()),
                    config: config.merge_with(ex_config),
                }
            },
            _ => existing.clone(), // Type mismatch, return existing
        }
    }
}

impl UpdateSrtOutputConfig {
    pub fn merge_with(&self, existing: &SrtOutputConfig) -> SrtOutputConfig {
        match (self, existing) {
            (UpdateSrtOutputConfig::Listener { host, port, latency_ms, stream_id, passphrase },
             SrtOutputConfig::Listener { host: ex_host, port: ex_port, common, .. }) => {
                SrtOutputConfig::Listener {
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    common: SrtCommonConfig {
                        latency_ms: latency_ms.or(common.latency_ms),
                        stream_id: stream_id.clone().or_else(|| common.stream_id.clone()),
                        passphrase: passphrase.clone().or_else(|| common.passphrase.clone()),
                    },
                    // Legacy fields
                    _legacy_bind_host: None,
                    _legacy_bind_port: None,
                    _legacy_listen_port: None,
                    _legacy_automatic_port: None,
                }
            },
            (UpdateSrtOutputConfig::Caller { host, port, local_host, latency_ms, stream_id, passphrase },
             SrtOutputConfig::Caller { host: ex_host, port: ex_port, local_host: ex_local_host, common, .. }) => {
                SrtOutputConfig::Caller {
                    host: host.clone().or_else(|| ex_host.clone()),
                    port: port.or(*ex_port),
                    local_host: local_host.clone().or_else(|| ex_local_host.clone()),
                    common: SrtCommonConfig {
                        latency_ms: latency_ms.or(common.latency_ms),
                        stream_id: stream_id.clone().or_else(|| common.stream_id.clone()),
                        passphrase: passphrase.clone().or_else(|| common.passphrase.clone()),
                    },
                    // Legacy fields
                    _legacy_remote_host: None,
                    _legacy_remote_port: None,
                    _legacy_bind_host: None,
                    _legacy_destination_addr: None,
                }
            },
            _ => existing.clone(), // Mode mismatch, return existing
        }
    }
}

// Analysis data structures - serializable versions of mpegts_inspector data
#[derive(Serialize, Clone, Debug)]
pub struct AnalysisDataReport {
    pub timestamp: String, // ISO 8601 format
    pub programs: Vec<ProgramData>,
    pub tr101_metrics: Option<Tr101MetricsData>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProgramData {
    pub program_number: u16,
    pub streams: Vec<StreamData>,
}

#[derive(Serialize, Clone, Debug)]
pub struct StreamData {
    pub stream_type: u8,
    pub pid: u16,
    pub codec: Option<CodecData>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum CodecData {
    #[serde(rename = "video")]
    Video {
        codec: String,
        width: u32,
        height: u32,
        fps: f64,
    },
    #[serde(rename = "audio")]
    Audio {
        codec: String,
        sample_rate: Option<u32>,
        channels: Option<u8>,
    },
    #[serde(rename = "subtitle")]
    Subtitle {
        codec: String,
    },
}

#[derive(Serialize, Clone, Debug)]
pub struct Tr101MetricsData {
    pub sync_byte_errors: u64,
    pub continuity_counter_errors: u64,
    pub pat_errors: u64,
    pub pmt_errors: u64,
    pub pid_errors: u64,
    pub transport_errors: u64,
    pub crc_errors: u64,
    pub pcr_repetition_errors: u64,
    pub pcr_discontinuity_errors: u64,
    pub pcr_accuracy_errors: u64,
    pub pts_errors: u64,
    pub cat_errors: u64,
}