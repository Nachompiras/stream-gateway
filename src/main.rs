
use srt_rs::{startup,cleanup};
use actix_web::{web, App, HttpServer};
use tokio::signal;
mod analysis;
mod api;
mod config;
mod port_utils;
mod udp_stream;
mod models;
mod srt_stream;
mod stream_control;
mod database;
mod metrics;
use tokio::sync::Mutex;
use std::{collections::HashMap, sync::Arc};
use once_cell::sync::Lazy;
use api::*;
use api::{AppState, InputsMap};
use tokio_util::sync::CancellationToken;
use models::{StateChange, StateChangeSender, StateChangeReceiver};
use tokio::sync::mpsc;

use crate::database::init_database;

// Estado global compartido para todos los inputs/outputs
pub static ACTIVE_STREAMS: Lazy<Arc<Mutex<InputsMap>>> = Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));
pub static GLOBAL_CANCEL_TOKEN: std::sync::LazyLock<CancellationToken> = std::sync::LazyLock::new(CancellationToken::new);

// Global state change notification system
pub static STATE_CHANGE_TX: Lazy<Arc<Mutex<Option<StateChangeSender>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

#[tokio::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    // Create the mpsc channel for packet forwarding
    //console_subscriber::init(); // Commented out - requires tokio_unstable
    
    let _ = startup();

    // Initialize state change notification system
    let (state_tx, mut state_rx) = mpsc::unbounded_channel::<StateChange>();
    {
        let mut tx_guard = STATE_CHANGE_TX.lock().await;
        *tx_guard = Some(state_tx);
    }

    // Crear o conectar a la base de datos SQLite
    let pool = init_database()
        .await
        .expect("error creando/conectando a la base de datos");

    // GLOBAL_INPUTS is already initialized via Lazy and does not need to be set again
    
    /* 3. Construir AppState e hidratar inputs desde la BD */
    let state = AppState {
        pool: pool.clone(),
    };
    load_from_db(&state)
        .await
        .expect("error cargando inputs/outputs desde BD");

    // Start the state change processor task
    tokio::spawn(async move {
        println!("State change processor started");
        while let Some(change) = state_rx.recv().await {
            match change {
                StateChange::InputStateChanged { input_id, new_status, connected_at } => {
                    println!("Input {} state changed to: {}", input_id, new_status);
                    let mut streams = ACTIVE_STREAMS.lock().await;
                    if let Some(input_info) = streams.get_mut(&input_id) {
                        input_info.status = new_status.clone();
                        if new_status.is_connected() {
                            input_info.connected_at = connected_at;
                        } else if !new_status.is_active() {
                            input_info.connected_at = None;
                        }
                    }
                }
                StateChange::OutputStateChanged { input_id, output_id, new_status, connected_at } => {
                    println!("Output {} (input {}) state changed to: {}", output_id, input_id, new_status);
                    let mut streams = ACTIVE_STREAMS.lock().await;
                    if let Some(input_info) = streams.get_mut(&input_id) {
                        if let Some(output_info) = input_info.output_tasks.get_mut(&output_id) {
                            output_info.status = new_status.clone();
                            if new_status.is_connected() {
                                output_info.connected_at = connected_at;
                            } else if !new_status.is_active() {
                                output_info.connected_at = None;
                            }
                        }
                    }
                }
            }
        }
        println!("State change processor ended");
    });

    let app_state = web::Data::new(state);
    let app_state_for_server = app_state.clone();

    let server_addr = "127.0.0.1:8080";
    println!("Iniciando servidor API en http://{}", server_addr);

    let server = HttpServer::new(move || {
        App::new()
            .app_data(app_state_for_server.clone()) // Compartir estado con los handlers
            // Input endpoints
            .service(create_input)
            .service(delete_input)
            .service(list_inputs)
            .service(get_input)
            .service(input_stats)
            // Output endpoints
            .service(create_output)
            .service(delete_output)
            .service(list_outputs)
            .service(get_output)
            .service(get_input_outputs)
            // Analysis endpoints
            .service(start_analysis)
            .service(stop_analysis)
            .service(get_analysis_status)
            // Stream control endpoints
            .service(start_input_endpoint)
            .service(stop_input_endpoint)
            .service(start_output_endpoint)
            .service(stop_output_endpoint)
            // General status
            .service(get_status)
            // Metrics endpoint
            .service(get_metrics)
    })
    .bind(server_addr)?
    .run();

    println!("Servidor iniciado. Presiona Ctrl+C para cerrar gracefully.");
    
    // Setup graceful shutdown
    let handle = server.handle();
    tokio::spawn(async move {
        match signal::ctrl_c().await {
            Ok(()) => {
                println!("\nSeñal de shutdown recibida, cerrando servidor...");
                handle.stop(true).await;
            }
            Err(err) => {
                eprintln!("Error configurando manejador de señal: {}", err);
            }
        }
    });

    // Run server and wait for shutdown
    let result = server.await;
    
    // Cleanup phase
    println!("Iniciando cleanup...");
    
    GLOBAL_CANCEL_TOKEN.cancel();
    
    // Stop all input tasks using global state
    {
        let mut inputs = ACTIVE_STREAMS.lock().await;
        for (input_id, input_info) in inputs.drain() {
            println!("Cerrando input {}", input_id);
            if let Some(handle) = input_info.task_handle {
                handle.abort();
            }
            for (output_id, output_info) in input_info.output_tasks {
                println!("Cerrando output {} del input {}", output_id, input_id);
                if let Some(handle) = output_info.abort_handle {
                    handle.abort();
                }
            }
            for (analysis_id, analysis_info) in input_info.analysis_tasks {
                println!("Cerrando análisis {} del input {}", analysis_id, input_id);
                analysis_info.task_handle.abort();
            }
        }
    }
    
    // Close database connection
    app_state.pool.close().await;
    println!("Conexión de base de datos cerrada");
    
    // SRT cleanup
    let _ = cleanup();
    println!("SRT cleanup completado");
    
    println!("Shutdown completado");
    result
}