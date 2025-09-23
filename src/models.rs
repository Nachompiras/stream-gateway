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
    pub details:          String,
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
        listen_port: u16,
        name: Option<String>,
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
        listen_port: u16,
        #[serde(flatten)]
        common: SrtCommonConfig,
    },
    #[serde(rename = "caller")]
    Caller {
        target_addr: String,
        #[serde(flatten)]
        common: SrtCommonConfig,
    },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum CreateOutputRequest {
    #[serde(rename = "udp")]
    Udp {
        input_id: i64,            // ID del input al que conectar
        destination_addr: String, // "host:port"
        name: Option<String>,
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
        listen_port: u16,        
        #[serde(flatten)]
        common: SrtCommonConfig,
    },
    #[serde(rename = "caller")]
    Caller {
        destination_addr: String, // "host:port"        
        #[serde(flatten)]
        common: SrtCommonConfig,
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
    pub details: String,
    pub status: String,
    pub outputs: Vec<OutputResponse>, // Lista de outputs asociados
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
}

#[derive(Serialize)]
pub struct OutputResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_id: i64,
    pub destination: String,
    pub output_type: String, // "UDP" o "SRT Caller"
    pub status: String,
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
}

// New response models for CRUD endpoints
#[derive(Serialize)]
pub struct InputListResponse {
    pub id: i64,
    pub name: Option<String>,
    pub details: String,
    pub input_type: String, // "UDP", "SRT Listener", "SRT Caller"
    pub status: String,
    pub output_count: usize, // Número de outputs asociados
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
}

#[derive(Serialize)]
pub struct InputDetailResponse {
    pub id: i64,
    pub name: Option<String>,
    pub details: String,
    pub input_type: String,
    pub status: String,
    pub outputs: Vec<OutputDetailResponse>,
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
}

#[derive(Serialize)]
pub struct OutputDetailResponse {
    pub id: i64,
    pub name: Option<String>,
    pub input_id: i64,
    pub destination: String,
    pub output_type: String,
    pub status: String,
    pub config: Option<String>, // JSON config if needed
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
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
    pub uptime_seconds: Option<u64>, // Uptime in seconds, None if stopped
}

/* SRT models */
pub type SrtStats = srt::SrtStats;

pub type StatsCell = Arc<RwLock<Option<InputStats>>>;

#[derive(Clone,Debug)]
pub enum InputStats {
    Srt(SrtStats),
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
    pub details:     String,
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
        connected_at: Option<std::time::SystemTime>
    },
    OutputStateChanged {
        input_id: i64,
        output_id: i64,
        new_status: StreamStatus,
        connected_at: Option<std::time::SystemTime>
    },
}

// Type alias for the state change sender
pub type StateChangeSender = mpsc::UnboundedSender<StateChange>;
pub type StateChangeReceiver = mpsc::UnboundedReceiver<StateChange>;

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