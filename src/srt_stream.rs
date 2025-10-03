use std::{sync::Arc};
use srt_rs::{SrtAsyncStream};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, sync::{broadcast::{self}, RwLock}, task::JoinHandle, time::Instant};
use futures::{stream::{AbortHandle, Abortable}, FutureExt};
use bytes::Bytes;
use crate::models::*;
use crate::metrics;
use std::time::{Duration, SystemTime};
use tokio::time::sleep;
use async_trait::async_trait;          // 1-liner: macro para traits async
use std::io;

pub fn spawn_srt_output(
    mut sink: Box<dyn SrtSink>,
    mut rx:   broadcast::Receiver<Bytes>,
    input_id: i64,
    output_id: i64,
    output_name: Option<String>,
    state_tx: Option<StateChangeSender>,
) -> (AbortHandle, JoinHandle<()>, StatsCell) {

    // (abort_handle, reg) para poder cancelar desde la API
    let (abort_handle, reg) = AbortHandle::new_pair();

    let stats_cell: StatsCell = Arc::new(RwLock::new(None));

    let stats_clone  = stats_cell.clone();
    let state_tx_clone = state_tx.clone();

    let handle = tokio::spawn(
        Abortable::new(async move {
            loop {
                // 1) Obtener / reconectar socket
                // Note: State notifications (Listening/Connecting/Connected) are handled in SrtSinkWithState::get_socket()
                let mut sock = match sink.get_socket().await {
                    Ok(s)  => s,
                    Err(e) => {
                        eprintln!("sink.get_socket(): {e}");
                        // Notify reconnecting state on error
                        if let Some(ref tx) = state_tx_clone {
                            let _ = tx.send(StateChange::OutputStateChanged {
                                input_id,
                                output_id,
                                new_status: StreamStatus::Reconnecting,
                                connected_at: None,
                                peer_address: None,
                            });
                        }
                        sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                };

                // para refrescar stats cada segundo sin segunda tarea
                let mut next_stats = Instant::now();

                // 2) Bucle de envío
                loop {
                    match rx.recv().await {
                        Ok(pkt) => {
                            let bytes_sent = pkt.len() as u64;                                                        

                            match sock.write(&pkt).await {
                                Ok(n) if n == pkt.len() => {
                                    // Sent successfully
                                    metrics::record_output_bytes(&output_name, input_id, output_id, "srt", bytes_sent);
                                    metrics::record_output_packets(&output_name, input_id, output_id, "srt", 1);
                                },
                                Ok(n) => {
                                    eprintln!("envío parcial: {}/{} bytes", n, pkt.len());
                                    // Record error metric for partial send
                                    metrics::record_stream_error(&output_name, output_id, "srt", "partial_send");
                                },
                                Err(e) => {
                                    eprintln!("error en envío: {e}");
                                    // Record error metric for send failure
                                    metrics::record_stream_error(&output_name, output_id, "srt", "send_failed");
                                    // Break to reconnect
                                    break;
                                }
                            }                           
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }

                    if next_stats.elapsed() >= Duration::from_secs(1) {
                        if let Ok(s) = sock.socket.srt_bistats(0, 1) {
                            //println!("Output: SRT stats: {s:?}");
                            *stats_clone.write().await = Some(InputStats::Srt(Box::new(s)));
                        }
                        next_stats = Instant::now();
                    }
                }

                // Notify reconnecting state when connection breaks
                if let Some(ref tx) = state_tx_clone {
                    let _ = tx.send(StateChange::OutputStateChanged {
                        input_id,
                        output_id,
                        new_status: StreamStatus::Reconnecting,
                        connected_at: None,
                        peer_address: None,
                    });
                }

                sleep(Duration::from_secs(2)).await;
            }
        }, reg)
        .map(|_| ())
    );

    (abort_handle, handle, stats_cell)
}

pub fn create_srt_output(
    input_id:       i64,
    cfg:            SrtOutputConfig,
    input:         &InputInfo,
    output_id:      i64,
    name:           Option<String>,
    state_tx:       Option<StateChangeSender>,
) -> actix_web::Result<OutputInfo> {

    println!("Creando output SRT con config: {:?}", cfg);

    let (auto_name, destination) = match &cfg {
        SrtOutputConfig::Caller { .. } => {
            let host = cfg.get_remote_host().unwrap_or_else(|| "unknown".to_string());
            let port = cfg.get_remote_port().unwrap_or(0);
            (Some(format!("SRT Caller to {}:{}", host, port)), format!("{}:{}", host, port))
        },
        SrtOutputConfig::Listener { .. } => {
            let port = cfg.get_bind_port().unwrap_or(0);
            (Some(format!("SRT Listener on {}", port)), format!(":{}", port))
        },
    };
    let final_name = name.or(auto_name);

    let sink_with_state = SrtSinkWithState {
        config: cfg.clone(),
        input_id,
        output_id,
        state_tx: state_tx.clone(),
    };
    let rx = input.packet_tx.subscribe();
    let (abort_handle, _, stats) = spawn_srt_output(Box::new(sink_with_state), rx, input_id, output_id, final_name.clone(), state_tx.clone());

    // Increment active outputs counter
    metrics::increment_active_outputs();

    println!("Output SRT creado con config: {:?}", cfg);
    println!("Output SRT destino: {}", destination);

    let initial_status = match cfg {
        SrtOutputConfig::Caller { .. }   => StreamStatus::Connecting,
        SrtOutputConfig::Listener { .. } => StreamStatus::Listening,
    };

    let info = OutputInfo {
        id: output_id,
        name: final_name.clone(),
        input_id,
        kind: match cfg {
            SrtOutputConfig::Caller { .. }   => OutputKind::SrtCaller,
            SrtOutputConfig::Listener { .. } => OutputKind::SrtListener,
        },
        status: initial_status,
        destination,
        stats,
        abort_handle: Some(abort_handle),
        config: CreateOutputRequest::Srt {
            input_id,
            name: final_name,
            config: cfg,
        },
        started_at: Some(std::time::SystemTime::now()),
        connected_at: None, // Will be set when connection is established
        state_tx,
        peer_address: None, // Will be set when peer connects (for SRT listeners)
    };

    println!("Output creado: {:?}", info);
    Ok(info)
}

// Wrapper to know the SRT type for state notifications
pub struct SrtSourceWithState {
    pub config: SrtInputConfig,
    pub input_id: i64,
    pub state_tx: Option<StateChangeSender>,
}

// Wrapper for SRT output sink with state notifications
pub struct SrtSinkWithState {
    pub config: SrtOutputConfig,
    pub input_id: i64,
    pub output_id: i64,
    pub state_tx: Option<StateChangeSender>,
}

#[async_trait]
impl SrtSource for SrtSourceWithState {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream> {
        match &self.config {
            //-------------------------------------------------- LISTENER
            SrtInputConfig::Listener { common, .. } => {
                // Notify listening state
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: self.input_id,
                        new_status: StreamStatus::Listening,
                        connected_at: None,
                        source_address: None,
                    });
                }

                let host = self.config.get_bind_host();
                let port = self.config.get_bind_port();
                let addr = format!("{}:{}", host, port);
                println!("SRT listener ► esperando conexiones en {addr}");

                // 1) builder asíncrono  (¡no bloquea!)
                let listener = common
                    .async_builder()// <───
                    .listen(&addr, 2, None)   // callback = None
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;


                // 2) await sobre el future `accept()`
                //    – si pulsas Ctrl-C y cancelas la tarea, este await
                //      se despierta con Err(Interrupted) y sale enseguida.
                let (stream_async, peer) = listener
                    .accept()
                    .await?;                  // <───  100 % async

                let peer_str = peer.to_string();
                println!("SRT listener: aceptada conexión de {peer_str}");

                // Notify connected state with source address
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: self.input_id,
                        new_status: StreamStatus::Connected,
                        connected_at: Some(std::time::SystemTime::now()),
                        source_address: Some(peer_str),
                    });
                }

                // 3) si tu código necesita la versión síncrona
                //    conviértela (o trabaja directamente con la async)
                Ok(stream_async)
            }

            //-------------------------------------------------- CALLER
            // (caller podía quedarse como estaba – ya no bloquea)
            SrtInputConfig::Caller { common, .. } => {
                // Notify connecting state for callers
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: self.input_id,
                        new_status: StreamStatus::Connecting,
                        connected_at: None,
                        source_address: None,
                    });
                }

                let host = self.config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                let port = self.config.get_remote_port().unwrap_or(8000);
                let addr = format!("{}:{}", host, port);

                let builder = common.async_builder();

                // Set local bind address if specified
                if let Some(bind_host) = self.config.get_caller_bind_host() {
                    if !bind_host.is_empty() && bind_host != "0.0.0.0" {
                        let bind_addr = format!("{}:0", bind_host); // Use port 0 for automatic assignment
                        // Note: SRT library may not support local bind for callers in all versions
                        // This is a placeholder for when the feature is available
                        println!("SRT Caller: attempting to bind to local address {}", bind_addr);
                        // builder = builder.set_local_addr(&bind_addr); // Uncomment if supported
                    }
                }

                let stream_async = builder
                    .connect(&addr)?
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                println!("Caller: conectado a {addr}");

                // Notify connected state
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id: self.input_id,
                        new_status: StreamStatus::Connected,
                        connected_at: Some(SystemTime::now()),
                        source_address: Some(addr.clone()),
                    });
                }

                Ok(stream_async)
            }
        }
    }
}

#[async_trait]
impl SrtSink for SrtSinkWithState {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream> {
        match &self.config {
            /* ---------------- SRT CALLER ---------------- */
            SrtOutputConfig::Caller { .. } => {
                // Notify connecting state
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::OutputStateChanged {
                        input_id: self.input_id,
                        output_id: self.output_id,
                        new_status: StreamStatus::Connecting,
                        connected_at: None,
                        peer_address: None,
                    });
                }

                let host = self.config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                let port = self.config.get_remote_port().unwrap_or(8000);
                let addr = format!("{}:{}", host, port);
                let common = match &self.config {
                    SrtOutputConfig::Caller { common, .. } => common,
                    _ => unreachable!(),
                };
                let bind_host = self.config.get_caller_bind_host();

                let builder = common.async_builder();

                // Set local bind address if specified
                if let Some(bind_host) = bind_host {
                    if !bind_host.is_empty() && bind_host != "0.0.0.0" {
                        let bind_addr = format!("{}:0", bind_host); // Use port 0 for automatic assignment
                        println!("SRT Output Caller: attempting to bind to local address {}", bind_addr);
                        // builder = builder.set_local_addr(&bind_addr); // Uncomment if supported
                    }
                }

                let stream_async = builder
                    .connect(&addr)?
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                println!("Caller: conectado a {addr}");

                // Notify connected state
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::OutputStateChanged {
                        input_id: self.input_id,
                        output_id: self.output_id,
                        new_status: StreamStatus::Connected,
                        connected_at: Some(SystemTime::now()),
                        peer_address: Some(addr.clone()),
                    });
                }

                // Convert async stream to sync for the output (SrtStream)
                Ok(stream_async)
            }

            /* --------------- SRT LISTENER ---------------- */
            SrtOutputConfig::Listener { .. } => {
                // Notify listening state
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::OutputStateChanged {
                        input_id: self.input_id,
                        output_id: self.output_id,
                        new_status: StreamStatus::Listening,
                        connected_at: None,
                        peer_address: None,
                    });
                }

                let host = self.config.get_bind_host().unwrap_or_else(|| "0.0.0.0".to_string());
                let port = self.config.get_bind_port().unwrap_or(8000);
                let bind = format!("{}:{}", host, port);

                let common = match &self.config {
                    SrtOutputConfig::Listener { common, .. } => common,
                    _ => unreachable!(),
                };

                // Use async_builder instead of builder
                let listener = common
                    .async_builder()
                    .listen(&bind, 2, None)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                println!("output listener ► esperando conexiones en {}", bind);

                // await on accept() - fully async
                let (stream_async, peer) = listener
                    .accept()
                    .await?;

                let peer_str = peer.to_string();
                println!("output listener ► peer {peer_str}");

                // Notify peer address when connected
                if let Some(ref tx) = self.state_tx {
                    let _ = tx.send(StateChange::OutputStateChanged {
                        input_id: self.input_id,
                        output_id: self.output_id,
                        new_status: StreamStatus::Connected,
                        connected_at: Some(SystemTime::now()),
                        peer_address: Some(peer_str),
                    });
                }

                // Convert async stream to sync for the output (SrtStream)
                Ok(stream_async)
            }
        }
    }
}

impl Forwarder {
    pub fn spawn_with_stats(
        mut source: Box<dyn SrtSource>,
        reconnect_delay: Duration,
        input_id: i64,
        input_name: Option<String>,
        state_tx: Option<StateChangeSender>,
    ) -> ForwardHandle {
        // 1) canal de salida
        let (tx, _rx_dummy) = broadcast::channel::<Bytes>(BROADCAST_CAPACITY);

        // 2) celda de estadísticas
        let stats_cell: StatsCell = Arc::new(RwLock::new(None));

        // 3) tarea principal
        let tx_clone     = tx.clone();
        let stats_clone  = stats_cell.clone();
        let state_tx_clone = state_tx.clone();
        let input_name_clone = input_name.clone();

        let handle: JoinHandle<()> = tokio::spawn(async move {
            let mut buf = vec![0u8; 65536]; // buffer de lectura

            loop {
                // --------------------------------------------------------
                // intentar conseguir un socket SRT
                // --------------------------------------------------------
                // No need to notify state here - the get_socket() implementation will handle it

                let mut sock = match source.get_socket().await {
                    Ok(s)  => {
                        // Note: State notification (Connected with source_address) is already sent by get_socket()
                        s
                    },
                    Err(e) => {
                        eprintln!("Forwarder: get_socket() error: {e}");
                        // Notify error or reconnecting state
                        if let Some(ref tx) = state_tx_clone {
                            let _ = tx.send(StateChange::InputStateChanged {
                                input_id,
                                new_status: StreamStatus::Error,
                                connected_at: None,
                                source_address: None,
                            });
                        }
                        sleep(reconnect_delay).await;
                        continue;
                    }
                };
                println!("Forwarder: sesión SRT abierta ✅");

                // para refrescar stats cada segundo sin segunda tarea
                let mut next_stats = Instant::now();

                // --------------------------------------------------------
                // bucle de lectura del socket
                // --------------------------------------------------------
                buf.reserve(1316);
                loop {                                                                              
                    let read_res = sock.read(&mut buf).await;

                    //2) cada 1 s pedir bistats
                    if next_stats.elapsed() >= Duration::from_secs(1) {
                        if let Ok(s) = sock.socket.srt_bistats(0, 1) {
                            //println!("Forwarder: SRT stats: {s:?}");
                            *stats_clone.write().await = Some(InputStats::Srt(Box::new(s)));
                        }
                        next_stats = Instant::now();
                    }

                    // 3) procesar resultado de la lectura
                    match read_res {
                        Ok(0) => {
                            println!("Forwarder: EOF, peer cerró");
                            break;
                        }
                        Ok(n) => {
                            // Record metrics for received data
                            metrics::record_input_bytes(&input_name_clone, input_id, "srt", n as u64);
                            metrics::record_input_packets(&input_name_clone, input_id, "srt", 1);

                            // ignorar si no hay consumidores
                            let _ = tx_clone.send(Bytes::copy_from_slice(&buf[..n]));
                        }
                        Err(e) => {
                            println!("Forwarder: error recv(): {e}");
                            break;
                        }
                    }                
                }

                // Notify reconnecting state when connection breaks
                if let Some(ref tx) = state_tx_clone {
                    let _ = tx.send(StateChange::InputStateChanged {
                        input_id,
                        new_status: StreamStatus::Reconnecting,
                        connected_at: None,
                        source_address: None,
                    });
                }

                println!(
                    "Forwarder: reconectando en {:?}…",
                    reconnect_delay
                );
                sleep(reconnect_delay).await;
                println!("Forwarder: sesión SRT cerrada ❌");
            }
        });

        // 4) devolver manejadores al llamante
        // Increment active inputs counter
        metrics::increment_active_inputs();

        ForwardHandle {
            tx,
            stats: stats_cell,
            handle,
        }
    }
}