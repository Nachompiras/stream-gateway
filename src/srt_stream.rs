use std::{sync::Arc};
use srt_rs::{SrtAsyncStream, SrtStream};
use tokio::{io::AsyncReadExt, sync::{broadcast::{self}, RwLock}, task::JoinHandle, time::Instant};
use futures::{stream::{AbortHandle, Abortable}, FutureExt};
use bytes::Bytes;
use crate::models::*;
use std::time::Duration;
use tokio::time::sleep;
use async_trait::async_trait;          // 1-liner: macro para traits async
use std::io;

pub fn spawn_srt_output(
    mut sink: Box<dyn SrtSink>,
    mut rx:   broadcast::Receiver<Bytes>,
) -> (AbortHandle, JoinHandle<()>) {

    // (abort_handle, reg) para poder cancelar desde la API
    let (abort_handle, reg) = AbortHandle::new_pair();

    let handle = tokio::spawn(
        Abortable::new(async move {
            loop {
                // 1) Obtener / reconectar socket
                let sock = match sink.get_socket().await {
                    Ok(s)  => s,
                    Err(e) => {
                        eprintln!("sink.get_socket(): {e}");
                        sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                };

                // 2) Bucle de envío
                loop {
                    match rx.recv().await {
                        Ok(pkt) => {
                            if sock.socket.send(&pkt).is_err() {
                                eprintln!("envío falló; reconectando…");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
                sleep(Duration::from_secs(2)).await;
            }
        }, reg)
        .map(|_| ())
    );

    (abort_handle, handle)
}

pub fn create_srt_output(
    input_id:       i64,
    cfg:            SrtOutputConfig,
    input:         &InputInfo,
    output_id:      i64,
    name:           Option<String>,
) -> actix_web::Result<OutputInfo> {

    println!("Creando output SRT con config: {:?}", cfg);

    let rx = input.packet_tx.subscribe();
    let (abort_handle, _) = spawn_srt_output(Box::new(cfg.clone()), rx);

    println!("Output SRT creado con config: {:?}", cfg);

    let (auto_name, destination) = match &cfg {
        SrtOutputConfig::Caller { destination_addr, .. } =>
            (Some(format!("SRT Caller to {}", destination_addr)), destination_addr.clone()),
        SrtOutputConfig::Listener { listen_port, .. } =>
            (Some(format!("SRT Listener on {}", listen_port)), format!(":{}", listen_port)),
    };
    let final_name = name.or(auto_name);
    println!("Output SRT destino: {}", destination);

    let info = OutputInfo {
        id: output_id,
        name: final_name,
        input_id,
        kind: match cfg {
            SrtOutputConfig::Caller { .. }   => OutputKind::SrtCaller,
            SrtOutputConfig::Listener { .. } => OutputKind::SrtListener,
        },
        destination,
        stats: Arc::new(RwLock::new(None)),
        abort_handle,
    };

    println!("Output creado: {:?}", info);
    Ok(info)
}

#[async_trait]
impl SrtSource for SrtInputConfig {
    async fn get_socket(&mut self) -> io::Result<SrtAsyncStream> {
        match self {
            //-------------------------------------------------- LISTENER
            SrtInputConfig::Listener { listen_port, common } => {
                let addr = format!("0.0.0.0:{listen_port}");
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

                println!("SRT listener: aceptada conexión de {peer}");

                // 3) si tu código necesita la versión síncrona
                //    conviértela (o trabaja directamente con la async)
                Ok(stream_async)
            }

            //-------------------------------------------------- CALLER
            // (caller podía quedarse como estaba – ya no bloquea)
            SrtInputConfig::Caller { target_addr, common } => {
                let addr = target_addr.clone();
                let stream_async = common
                    .async_builder()
                    .connect(&addr)?
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                println!("Caller: conectado a {addr}");
                Ok(stream_async)
            }
        }
    }
}

#[async_trait]
impl SrtSink for SrtOutputConfig {
    async fn get_socket(&mut self) -> io::Result<SrtStream> {
        match self {
            /* ---------------- SRT CALLER ---------------- */
            SrtOutputConfig::Caller { destination_addr, common, .. } => {
                // connect() es sincrónico → spawn_blocking
                let addr = destination_addr.clone();
                let common = common.clone();
                tokio::task::spawn_blocking(move || {
                    let builder = common.builder();
                    let caller = builder
                            .connect(&addr)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                        println!("Caller: conectado a {addr}");
                        Ok(caller)
                })
                .await?
            }

            /* --------------- SRT LISTENER ---------------- */
            SrtOutputConfig::Listener { listen_port, common, .. } => {
                let bind = format!("0.0.0.0:{listen_port}");
                // Creas (o reutilizas) un listener y aceptas 1 peer.
                // Guardamos el listener en Option para re-usar el puerto.
                // if common.__listener.is_none() {
                //     let lst = common.apply(srt::builder())
                //                     .listen(&bind, 2)?;
                //     common.__listener = Some(lst);     // guardamos
                // }
                // // accept() es async (ya no bloquea hilo)
                // let (s, peer) = common.__listener.as_ref().unwrap()
                //                    .accept().await?;
                let builder = common.builder();
                let lst = builder.listen(&bind, 2)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                println!("output listener ► esperando conexiones en {}", bind);
                let (s, peer) = lst.accept().map_err(|e| io::Error::new(io::ErrorKind::Other, e))?; 
                println!("output listener ► peer {peer}");
                Ok(s)
            }
        }
    }
}

impl Forwarder {
    pub fn spawn_with_stats(
        mut source: Box<dyn SrtSource>,
        reconnect_delay: Duration,
    ) -> ForwardHandle {
        // 1) canal de salida
        let (tx, _rx_dummy) = broadcast::channel::<Bytes>(BROADCAST_CAPACITY);

        // 2) celda de estadísticas
        let stats_cell: StatsCell = Arc::new(RwLock::new(None));

        // 3) tarea principal
        let tx_clone     = tx.clone();
        let stats_clone  = stats_cell.clone();

        let handle: JoinHandle<()> = tokio::spawn(async move {
            let mut buf = vec![0u8; 2048]; // buffer de lectura

            loop {                            
                // --------------------------------------------------------
                // intentar conseguir un socket SRT
                // --------------------------------------------------------
                let mut sock = match source.get_socket().await {
                    Ok(s)  => s,
                    Err(e) => {
                        eprintln!("Forwarder: get_socket() error: {e}");
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
                loop {       
                    buf.reserve(1316);                                                   
                    let read_res = sock.read(&mut buf).await;

                    //2) cada 1 s pedir bistats
                    if next_stats.elapsed() >= Duration::from_secs(1) {
                        if let Ok(s) = sock.socket.srt_bistats(0, 1) {
                            //println!("Forwarder: SRT stats: {s:?}");
                            *stats_clone.write().await = Some(InputStats::Srt(s));
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
                            // ignorar si no hay consumidores
                            let _ = tx_clone.send(Bytes::copy_from_slice(&buf[..n]));
                        }
                        Err(e) => {
                            println!("Forwarder: error recv(): {e}");
                            break;
                        }
                    }                }

                println!(
                    "Forwarder: reconectando en {:?}…",
                    reconnect_delay
                );                
                sleep(reconnect_delay).await;
                println!("Forwarder: sesión SRT cerrada ❌");
            }
        });

        // 4) devolver manejadores al llamante
        ForwardHandle {
            tx,
            stats: stats_cell,
            handle,
        }
    }
}