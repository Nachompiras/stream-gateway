use bytes::Bytes;
use futures::stream::{AbortHandle};
use srt_rs::{self as srt, SrtAsyncStream, SrtStream};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use tokio::sync::{broadcast, RwLock, mpsc}; // Agregamos mpsc
use async_trait::async_trait;          // 1-liner: macro para traits async
use std::io;
use sqlx::FromRow;
use tokio::task::JoinHandle;
use std::sync::Arc;

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

#[derive(Deserialize, Debug, Clone)]
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

#[derive(Serialize)]
pub struct InputResponse {
    pub id: i64,
    pub name: Option<String>,
    pub status: String,
    pub assigned_port: Option<u16>, // Port assigned automatically or specified
    pub outputs: Vec<OutputResponse>, // Lista de outputs asociados
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
    pub source_address: Option<String>, // Address of connected source (for SRT listeners)
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

/// Rasgo: “dame un socket listo para recibir datos SRT”.
#[async_trait]
pub trait SrtSource: Send + Sync + 'static {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream>;
}

#[async_trait]
pub trait SrtSink: Send + Sync + 'static {
    async fn get_socket(&mut self) -> io::Result<SrtStream>;
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
    }
}

pub fn input_type_display_string(kind: &str) -> &'static str {
    match kind {
        "udp" => "UDP Listener",
        "srt_listener" => "SRT Listener",
        "srt_caller" => "SRT Caller",
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
        }
    }
    
    /// Get the effective bind host, handling backward compatibility  
    pub fn get_bind_host(&self) -> String {
        match self {
            CreateInputRequest::Udp { bind_host, .. } => {
                bind_host.clone().unwrap_or_else(|| "0.0.0.0".to_string())
            },
            CreateInputRequest::Srt { config, .. } => config.get_bind_host(),
        }
    }
    
    /// Get remote host for caller modes
    pub fn get_remote_host(&self) -> Option<String> {
        match self {
            CreateInputRequest::Udp { .. } => None, // UDP inputs don't have remote hosts
            CreateInputRequest::Srt { config, .. } => config.get_remote_host(),
        }
    }
    
    /// Get remote port for caller modes
    pub fn get_remote_port(&self) -> Option<u16> {
        match self {
            CreateInputRequest::Udp { .. } => None, // UDP inputs don't have remote ports
            CreateInputRequest::Srt { config, .. } => config.get_remote_port(),
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
            CreateOutputRequest::Udp { automatic_port, remote_port, destination_addr, .. } => {
                // If automatic_port is explicitly set to true, use auto port
                if automatic_port == &Some(true) {
                    return true;
                }
                // If automatic_port is explicitly false, use specified ports
                if automatic_port == &Some(false) {
                    return false;
                }
                // If automatic_port is None, check port values
                crate::port_utils::should_use_auto_port_output(*remote_port, None, destination_addr.as_ref())
            },
            CreateOutputRequest::Srt { config, .. } => config.is_automatic_port(),
        }
    }

    /// Get the effective remote host
    pub fn get_remote_host(&self) -> Option<String> {
        match self {
            CreateOutputRequest::Udp { remote_host, destination_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(host) = remote_host {
                    if !host.is_empty() {
                        Some(host.clone())
                    } else {
                        None
                    }
                } else if let Some(addr) = destination_addr {
                    addr.split(':').next().map(|s| s.to_string())
                } else {
                    None
                }
            },
            CreateOutputRequest::Srt { config, .. } => config.get_remote_host(),
        }
    }
    
    /// Get the effective remote port
    pub fn get_remote_port(&self) -> Option<u16> {
        match self {
            CreateOutputRequest::Udp { remote_port, destination_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(port) = remote_port {
                    if *port != 0 {
                        Some(*port)
                    } else {
                        None
                    }
                } else if let Some(addr) = destination_addr {
                    addr.split(':').nth(1).and_then(|s| s.parse().ok())
                } else {
                    None
                }
            },
            CreateOutputRequest::Srt { config, .. } => config.get_remote_port(),
        }
    }
    
    /// Get bind port for listener outputs
    pub fn get_bind_port(&self) -> Option<u16> {
        match self {
            CreateOutputRequest::Udp { .. } => None, // UDP outputs don't bind
            CreateOutputRequest::Srt { config, .. } => config.get_bind_port(),
        }
    }
    
    /// Get bind host for listener outputs
    pub fn get_bind_host(&self) -> Option<String> {
        match self {
            CreateOutputRequest::Udp { bind_host, .. } => bind_host.clone(), // UDP outputs can bind to specific interface
            CreateOutputRequest::Srt { config, .. } => config.get_bind_host(),
        }
    }
}

impl SrtOutputConfig {
    /// Check if automatic port assignment is requested
    pub fn is_automatic_port(&self) -> bool {
        match self {
            SrtOutputConfig::Listener { automatic_port, bind_port, listen_port, .. } => {
                // If automatic_port is explicitly set to true, use auto port
                if automatic_port == &Some(true) {
                    return true;
                }
                // If automatic_port is explicitly false, use specified ports
                if automatic_port == &Some(false) {
                    return false;
                }
                // If automatic_port is None, check port values
                crate::port_utils::should_use_auto_port_output(None, *bind_port, None) ||
                crate::port_utils::should_use_auto_port_input(*bind_port, *listen_port)
            },
            SrtOutputConfig::Caller { .. } => false, // Callers connect to existing listeners, don't need auto port
        }
    }

    /// Get remote host for SRT output
    pub fn get_remote_host(&self) -> Option<String> {
        match self {
            SrtOutputConfig::Listener { .. } => None,
            SrtOutputConfig::Caller { remote_host, destination_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(host) = remote_host {
                    if !host.is_empty() {
                        Some(host.clone())
                    } else {
                        None
                    }
                } else if let Some(addr) = destination_addr {
                    addr.split(':').next().map(|s| s.to_string())
                } else {
                    None
                }
            },
        }
    }

    /// Get local bind host for SRT caller output
    pub fn get_caller_bind_host(&self) -> Option<String> {
        match self {
            SrtOutputConfig::Listener { .. } => None,
            SrtOutputConfig::Caller { bind_host, .. } => bind_host.clone(),
        }
    }
    
    /// Get remote port for SRT output
    pub fn get_remote_port(&self) -> Option<u16> {
        match self {
            SrtOutputConfig::Listener { .. } => None,
            SrtOutputConfig::Caller { remote_port, destination_addr, .. } => {
                // Prefer new field, fallback to parsing legacy field
                if let Some(port) = remote_port {
                    if *port != 0 {
                        Some(*port)
                    } else {
                        None
                    }
                } else if let Some(addr) = destination_addr {
                    addr.split(':').nth(1).and_then(|s| s.parse().ok())
                } else {
                    None
                }
            },
        }
    }
    
    /// Get bind port for SRT listener output
    pub fn get_bind_port(&self) -> Option<u16> {
        match self {
            SrtOutputConfig::Listener { bind_port, listen_port, .. } => {
                // Prefer legacy field first for backward compatibility, then new field
                listen_port.or(*bind_port)
            },
            SrtOutputConfig::Caller { .. } => None,
        }
    }
    
    /// Get bind host for SRT listener output
    pub fn get_bind_host(&self) -> Option<String> {
        match self {
            SrtOutputConfig::Listener { bind_host, .. } => {
                Some(bind_host.clone().unwrap_or_else(|| "0.0.0.0".to_string()))
            },
            SrtOutputConfig::Caller { .. } => None,
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

// Helper functions to extract ports from configuration JSON
impl CreateInputRequest {
    /// Extract the assigned port from a stored input configuration
    pub fn extract_assigned_port(&self) -> Option<u16> {
        match self {
            CreateInputRequest::Udp { bind_port, listen_port, .. } => {
                // Prefer listen_port for backward compatibility, then bind_port
                listen_port.or(*bind_port).filter(|&p| p != 0)
            },
            CreateInputRequest::Srt { config, .. } => config.extract_assigned_port(),
        }
    }
}

impl SrtInputConfig {
    /// Extract the assigned port from SRT input configuration
    pub fn extract_assigned_port(&self) -> Option<u16> {
        match self {
            SrtInputConfig::Listener { bind_port, listen_port, .. } => {
                // Prefer listen_port for backward compatibility, then bind_port
                listen_port.or(*bind_port).filter(|&p| p != 0)
            },
            SrtInputConfig::Caller { .. } => None, // Callers don't bind ports
        }
    }
}

impl CreateOutputRequest {
    /// Extract the assigned port from a stored output configuration
    pub fn extract_assigned_port(&self) -> Option<u16> {
        match self {
            CreateOutputRequest::Udp { remote_port, .. } => {
                // UDP outputs use remote port
                remote_port.filter(|&p| p != 0)
            },
            CreateOutputRequest::Srt { config, .. } => config.extract_assigned_port(),
        }
    }
}

impl SrtOutputConfig {
    /// Extract the assigned port from SRT output configuration
    pub fn extract_assigned_port(&self) -> Option<u16> {
        match self {
            SrtOutputConfig::Listener { bind_port, listen_port, .. } => {
                // SRT listener outputs bind to ports
                listen_port.or(*bind_port).filter(|&p| p != 0)
            },
            SrtOutputConfig::Caller { remote_port, .. } => {
                // SRT caller outputs connect to remote ports
                remote_port.filter(|&p| p != 0)
            },
        }
    }
}

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