use actix_web::{web, Responder, HttpResponse, Result as ActixResult, error::ErrorInternalServerError};
use std::collections::HashMap;
use std::time::Duration;
use ::log::{info, error};
use anyhow::Result;

use crate::udp_stream::{create_udp_output, spawn_udp_input_with_stats};
use crate::srt_stream::{
    create_srt_output,
};
use crate::models::*;
use crate::database::{self, check_output_exists, check_port_conflict, get_input_by_id, get_output_by_id, update_input_status_in_db, update_output_status_in_db, get_input_id_for_output};
use crate::ACTIVE_STREAMS;
use crate::analysis;
use crate::stream_control;
use sqlx::types::chrono;
use tokio::sync::{broadcast, RwLock};
use std::sync::Arc;

pub type InputsMap = std::collections::HashMap<i64, InputInfo>;

pub struct AppState {
    pub pool: sqlx::SqlitePool,
}

#[actix_web::post("/inputs")]
async fn create_input(
    state: web::Data<AppState>,
    req:   web::Json<CreateInputRequest>,
) -> ActixResult<impl Responder> {    

    println!("Petición para crear input: {:?}", req);

    // Generate name and details for the input
    let (auto_name, details) = generate_input_name_and_details(&req);
    let name = get_name_from_request(&req).or(auto_name);

    // Save to database FIRST to get auto-generated ID
    let id = match database::save_input_to_db(&state.pool, name.as_deref(), &req, &details).await {
        Ok(id) => id,
        Err(e) => {
            error!("Error saving input to database: {}", e);
            return Err(ErrorInternalServerError(format!("Database error: {e}")));
        }
    };
    
    println!("Input '{}' guardado en la base de datos con ID: {}", name.as_deref().unwrap_or("sin nombre"), id);

    // Now spawn the input tasks
    match spawn_input(req.clone(), id, name.clone(), details.clone()) {
        Ok(info) => {
            println!("Input creado con ID: {}", info.id);
            
            let mut guard = ACTIVE_STREAMS.lock().await;
            guard.insert(info.id, info);            

            println!("Input '{}' añadido al estado compartido", id);
            Ok(HttpResponse::Created().json(
                serde_json::json!({ 
                    "id": id,
                    "name": name
                })
            ))
        }
        Err(e) => {
            // If spawning fails, we should clean up the database entry
            if let Err(db_err) = database::delete_input_from_db(&state.pool, id).await {
                error!("Error cleaning up database after spawn failure: {}", db_err);
            }
            Err(ErrorInternalServerError(e))
        },
    }
}


#[actix_web::delete("/inputs")] 
pub async fn delete_input(
    state: web::Data<AppState>,
    req: web::Json<DeleteInputRequest>,
) -> ActixResult<impl Responder> {
    let input_id = req.input_id;
    let mut state_guard = ACTIVE_STREAMS.lock().await;

    if let Some(mut input_info) = state_guard.remove(&input_id) {
        info!("Iniciando cierre del Input '{}'", input_id);

        // 1. Abortar la tarea principal del Input
        info!("[{}] Abortando tarea principal del Input", input_id);
        if let Some(handle) = input_info.task_handle.take() {
            handle.abort();
        }
        // El drop del broadcast::Sender (input_info.packet_tx) ocurrirá cuando input_info salga del scope.
        // Esto hará que los receivers de los outputs obtengan RecvError::Closed.

        // 2. Abortar explícitamente las tareas de output asociadas
        for (output_id, output_info) in input_info.output_tasks {
            info!("[{}] Abortando output task [{}]", input_id, output_id);
            if let Some(handle) = output_info.abort_handle {
                handle.abort();
            }
        }

        // 3. Abortar explícitamente las tareas de análisis asociadas
        for (analysis_id, analysis_info) in input_info.analysis_tasks {
            info!("[{}] Abortando analysis task [{}]", input_id, analysis_id);
            analysis_info.task_handle.abort();
        }

        // 4. Remove from database
        if let Err(e) = database::delete_input_from_db(&state.pool, input_id).await {
            error!("Error deleting input from database: {}", e);
            // Continue anyway - we already removed from memory
        }

        info!("Input '{}' eliminado", input_id);
        Ok(HttpResponse::Ok().json(serde_json::json!({
            "message": "Input eliminado",
            "id": input_id
        })))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

#[actix_web::post("/outputs")]
pub async fn create_output(
    state: web::Data<AppState>,
    req: web::Json<CreateOutputRequest>,
) -> ActixResult<impl Responder> {

    info!("Petición para crear output: {:?}", req);

    // Bloqueamos el estado una sola vez
    let mut guard = ACTIVE_STREAMS.lock().await;

    // Clone the request so we can use it after into_inner()
    let req_val = req.into_inner();

    // get input id from the request (to check it exists)
    let input_id = match &req_val {
        CreateOutputRequest::Udp { input_id, .. } => *input_id,
        CreateOutputRequest::Srt { input_id, .. } => *input_id,
    };

    // get InputInfo
    let input = match guard.get(&input_id) {
        Some(i) => i,
        None => {
            return Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)));
        }
    };

    // Validation logic
    let (destination_addr, listen_port) = match &req_val {
        CreateOutputRequest::Udp { destination_addr, .. } => (destination_addr.clone(), None),
        CreateOutputRequest::Srt { config, .. } => {
            match config {
                SrtOutputConfig::Caller { destination_addr, .. } => (destination_addr.clone(), None),
                SrtOutputConfig::Listener { listen_port, .. } => (format!(":{}", listen_port), Some(*listen_port)),
            }
        }
    };

    // Check if output already exists for this input + destination
    match check_output_exists(&state.pool, input_id, &destination_addr).await {
        Ok(exists) if exists => {
            return Ok(HttpResponse::Conflict().body(format!(
                "Output ya existe para input {} con destino '{}'",
                input_id, destination_addr
            )));
        }
        Ok(_) => {},
        Err(e) => {
            error!("Error checking output existence: {}", e);
            return Err(ErrorInternalServerError(format!("Database error: {e}")));
        }
    }

    // Check port conflicts for SRT Listener outputs
    if let Some(port) = listen_port {
        match check_port_conflict(&state.pool, port, None).await {
            Ok(conflict) if conflict => {
                return Ok(HttpResponse::Conflict().body(format!(
                    "Puerto {} ya está en uso por otro output", port
                )));
            }
            Ok(_) => {},
            Err(e) => {
                error!("Error checking port conflict: {}", e);
                return Err(ErrorInternalServerError(format!("Database error: {e}")));
            }
        }
    }

    // Save to database first to get auto-generated ID
    let (name, kind, config_json) = match &req_val {
        CreateOutputRequest::Udp { name: user_name, .. } => {
            let auto_name = Some(format!("UDP Output to {}", destination_addr));
            let final_name = user_name.clone().or(auto_name);
            (final_name, "udp", None)
        }
        CreateOutputRequest::Srt { name: user_name, config, .. } => {
            let kind_str = match config {
                SrtOutputConfig::Caller { .. } => "srt_caller",
                SrtOutputConfig::Listener { .. } => "srt_listener",
            };
            let auto_name = match config {
                SrtOutputConfig::Caller { destination_addr, .. } =>
                    format!("SRT Caller to {}", destination_addr),
                SrtOutputConfig::Listener { listen_port, .. } =>
                    format!("SRT Listener on {}", listen_port),
            };
            let final_name = user_name.clone().or(Some(auto_name));
            let config_json = Some(serde_json::to_string(config)
                .map_err(|e| ErrorInternalServerError(format!("Serialization error: {e}")))?);
            (final_name, kind_str, config_json)
        }
    };

    let output_id = database::save_output_to_db(
        &state.pool,
        name.as_deref(),
        input_id,
        kind,
        &destination_addr,
        config_json.as_deref(),
        listen_port,
    )
    .await
    .map_err(|e| ErrorInternalServerError(format!("Database error: {e}")))?;

    // Create the output with the generated ID
    let output_info = match &req_val {
        CreateOutputRequest::Udp { destination_addr, name: user_name, .. } =>
            create_udp_output(input_id, destination_addr.clone(), input, output_id, user_name.clone()).await?,

        CreateOutputRequest::Srt { config, name: user_name, .. } =>
            create_srt_output(input_id, config.clone(), input, output_id, user_name.clone())?,
    };

    // Insertamos el output en el Input correspondiente
    guard
        .get_mut(&output_info.input_id)
        .expect("Input debe existir aquí")   // ya comprobado
        .output_tasks
        .insert(output_info.id, output_info.clone());

    let response = serde_json::json!({
        "message": "Output creado",
        "output_id": output_info.id,
        "input_id": output_info.input_id,
        "destination": output_info.destination,
        "type": output_info.kind.to_string(),
    });

    Ok(HttpResponse::Created().json(response))
}

#[actix_web::get("/inputs")]
pub async fn list_inputs(state: web::Data<AppState>) -> ActixResult<impl Responder> {
    let state_guard = ACTIVE_STREAMS.lock().await;
    let mut response: Vec<InputListResponse> = Vec::new();

    for (input_id, input_info) in state_guard.iter() {
        // Get input type from database to have accurate classification
        let input_type = if let Ok(Some(db_input)) = database::get_input_by_id(&state.pool, *input_id).await {
            input_type_display_string(&db_input.kind).to_string()
        } else {
            "Unknown".to_string()
        };

        response.push(InputListResponse {
            id: *input_id,
            name: input_info.name.clone(),
            details: input_info.details.clone(),
            input_type,
            status: input_info.status.to_string(),
            output_count: input_info.output_tasks.len(),
        });
    }

    Ok(HttpResponse::Ok().json(response))
}

#[actix_web::get("/inputs/{id}")]
pub async fn get_input(
    state: web::Data<AppState>,
    path: web::Path<i64>
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();
    let state_guard = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = state_guard.get(&input_id) {
        // Get input type from database
        let input_type = if let Ok(Some(db_input)) = get_input_by_id(&state.pool, input_id).await {
            input_type_display_string(&db_input.kind).to_string()
        } else {
            "Unknown".to_string()
        };

        // Build outputs list with detailed information
        let mut outputs: Vec<OutputDetailResponse> = Vec::new();
        for (output_id, output_info) in input_info.output_tasks.iter() {
            let config = if let Ok(Some(db_output)) = get_output_by_id(&state.pool, *output_id).await {
                db_output.config_json
            } else {
                None
            };

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                config,
            });
        }

        let response = InputDetailResponse {
            id: input_id,
            name: input_info.name.clone(),
            details: input_info.details.clone(),
            input_type,
            status: input_info.status.to_string(),
            outputs,
        };

        Ok(HttpResponse::Ok().json(response))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

#[actix_web::get("/outputs")]
pub async fn list_outputs(_state: web::Data<AppState>) -> ActixResult<impl Responder> {
    let state_guard = ACTIVE_STREAMS.lock().await;
    let mut response: Vec<OutputListResponse> = Vec::new();

    for (input_id, input_info) in state_guard.iter() {
        for (output_id, output_info) in input_info.output_tasks.iter() {
            response.push(OutputListResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id: *input_id,
                input_name: input_info.name.clone(),
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
            });
        }
    }

    Ok(HttpResponse::Ok().json(response))
}

#[actix_web::get("/outputs/{id}")]
pub async fn get_output(
    state: web::Data<AppState>,
    path: web::Path<i64>
) -> ActixResult<impl Responder> {
    let output_id = path.into_inner();
    let state_guard = ACTIVE_STREAMS.lock().await;

    // Search for the output across all inputs
    for (input_id, input_info) in state_guard.iter() {
        if let Some(output_info) = input_info.output_tasks.get(&output_id) {
            // Get additional config from database
            let config = if let Ok(Some(db_output)) = database::get_output_by_id(&state.pool, output_id).await {
                db_output.config_json
            } else {
                None
            };

            let response = OutputDetailResponse {
                id: output_id,
                name: output_info.name.clone(),
                input_id: *input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                config,
            };

            return Ok(HttpResponse::Ok().json(response));
        }
    }

    Ok(HttpResponse::NotFound().body(format!("Output con ID '{}' no encontrado", output_id)))
}

#[actix_web::get("/inputs/{input_id}/outputs")]
pub async fn get_input_outputs(
    state: web::Data<AppState>,
    path: web::Path<i64>
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();
    let state_guard = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = state_guard.get(&input_id) {
        let mut outputs: Vec<OutputDetailResponse> = Vec::new();

        for (output_id, output_info) in input_info.output_tasks.iter() {
            let config = if let Ok(Some(db_output)) = database::get_output_by_id(&state.pool, *output_id).await {
                db_output.config_json
            } else {
                None
            };

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                config,
            });
        }

        Ok(HttpResponse::Ok().json(outputs))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

#[actix_web::delete("/outputs")] // Cambiado a plural
pub async fn delete_output(
    state: web::Data<AppState>,
    req: web::Json<DeleteOutputRequest>,
) -> ActixResult<impl Responder> {
    let input_id = req.input_id;
    let output_id = req.output_id;

    let mut state_guard = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = state_guard.get_mut(&input_id) {
        if let Some(output_info) = input_info.output_tasks.remove(&output_id) {
            // Abortar la tarea de envío
            if let Some(handle) = output_info.abort_handle {
                handle.abort();
            }

            // Remove from database
            if let Err(e) = database::delete_output_from_db(&state.pool, output_id).await {
                error!("Error deleting output from database: {}", e);
                // Continue anyway - we already removed from memory
            }

            info!("Output [{}] eliminado para Input '{}'", output_id, input_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Output eliminado",
                "input_id": input_id,
                "output_id": output_id
            })))
        } else {
            Ok(HttpResponse::NotFound().body(format!("Output con ID '{}' no encontrado para Input '{}'", output_id, input_id)))
        }
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

// --- Endpoint para listar Inputs y sus Outputs ---
#[actix_web::get("/status")]
pub async fn get_status(_state: web::Data<AppState>) -> ActixResult<impl Responder> {
    let state_guard = ACTIVE_STREAMS.lock().await;
    let mut response: Vec<InputResponse> = Vec::new();

    for (input_id, input_info) in state_guard.iter() {
        let mut outputs_resp: Vec<OutputResponse> = Vec::new();
        for (output_id, output_info) in input_info.output_tasks.iter() {
             let o_type = output_kind_string(&output_info.kind);

            outputs_resp.push(OutputResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id: *input_id,
                destination: output_info.destination.clone(),
                output_type: o_type.to_string(),
                status: output_info.status.to_string(),
            });
        }

        response.push(InputResponse {
            id: *input_id,
            name: input_info.name.clone(),
            details: input_info.details.clone(),
            status: input_info.status.to_string(),
            outputs: outputs_resp,
        });
    }

    Ok(HttpResponse::Ok().json(response))
}


#[actix_web::get("/inputs/{id}/stats")]
async fn input_stats(
    _state: web::Data<AppState>,
    path:  web::Path<i64>,
) -> impl Responder {
    let id = path.into_inner();
    let guard = ACTIVE_STREAMS.lock().await;

    println!("Solicitando stats para input '{}'", id);

    if let Some(info) = guard.get(&id) {
        println!("Obteniendo stats para input '{}'", id);
        if let Some(stats) = info.stats.read().await.clone() {
            println!("Stats obtenidos: {:?}", stats);
            return HttpResponse::Ok().json(stats);
        } else {
            println!("No hay stats disponibles aún para input '{}'", id);
            return HttpResponse::NoContent().finish();
        }
    }
    println!("Input '{}' no encontrado al solicitar stats", id);
    HttpResponse::NotFound().body("input no encontrado")
}

pub async fn load_from_db(state: &AppState) -> anyhow::Result<()> {
    println!("Cargando inputs desde la base de datos...");
    let rows = database::get_all_inputs(&state.pool).await?;
    
    // Process inputs outside of the mutex lock first
    let mut loaded_inputs = HashMap::new();
    
    for r in rows {
        println!("Procesando input ID {} de tipo {} con status {}", r.id, r.kind, r.status);

        // Solo recrear inputs que están marcados como "running"
        // Para inputs "stopped", crear InputInfo en estado stopped
        if r.status != "running" {
            println!("Input {} está en status '{}', creando entrada stopped en memoria", r.id, r.status);

            // Recrear CreateInputRequest para store en memoria
            let create_req = match r.kind.as_str() {
                "udp" => {
                    let json_val: serde_json::Value = serde_json::from_str(&r.config_json)?;
                    let listen_port = json_val["listen_port"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("Missing listen_port in UDP config"))?
                        as u16;
                    CreateInputRequest::Udp { listen_port, name: r.name.clone() }
                },
                "srt_listener" | "srt_caller" => CreateInputRequest::Srt {
                    name: r.name.clone(),
                    config: serde_json::from_str(&r.config_json)?
                },
                _ => {
                    println!("Tipo de input desconocido: {}, saltando", r.kind);
                    continue;
                },
            };

            // Crear InputInfo en estado stopped
            let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
            let stopped_input_info = InputInfo {
                id: r.id,
                name: r.name,
                details: r.details,
                status: StreamStatus::Stopped,
                packet_tx: tx,
                stats: Arc::new(RwLock::new(None)),
                task_handle: None, // No task when stopped
                config: create_req,
                output_tasks: HashMap::new(),
                stopped_outputs: HashMap::new(),
                analysis_tasks: HashMap::new(),
                paused_analysis: Vec::new(),
            };

            loaded_inputs.insert(r.id, stopped_input_info);
            continue;
        }

        // recrear el Input según su tipo
        let create_req = match r.kind.as_str() {
            "udp" => {
                let json_val: serde_json::Value = serde_json::from_str(&r.config_json)?;
                println!("Configuración UDP: {:?}", json_val);
                let listen_port = json_val["listen_port"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Missing listen_port in UDP config"))?
                    as u16;
                CreateInputRequest::Udp { listen_port, name: None }
            },
            "srt_listener" => CreateInputRequest::Srt {
                name: None,
                config: serde_json::from_str(&r.config_json)?
            },
            "srt_caller" => CreateInputRequest::Srt {
                name: None,
                config: serde_json::from_str(&r.config_json)?
            },
            _ => {
                println!("Tipo de input desconocido: {}, saltando", r.kind);
                continue;
            },
        };

        match spawn_input(create_req, r.id, r.name.clone(), r.details.clone()) {
            Ok(info) => {
                println!("Input {} recreado exitosamente", r.id);
                loaded_inputs.insert(r.id, info);
            }
            Err(e) => {
                error!("Error recreating input {}: {}", r.id, e);
            }
        }
    }

    println!("Cargando outputs desde la base de datos...");
    
    let outs = database::get_all_outputs(&state.pool).await?;
    let mut outputs_by_input: HashMap<i64, Vec<OutputRow>> = HashMap::new();
    
    println!("Recreando outputs para inputs cargados...");
    // Group outputs by input_id
    for o in outs {
        outputs_by_input.entry(o.input_id).or_insert(Vec::new()).push(o);
    }

    println!("Procesando outputs para cada input cargado...");

    // Process outputs for each loaded input
    for (input_id, outputs) in outputs_by_input {
        println!("Procesando {} outputs para input {}", outputs.len(), input_id);
        if let Some(input) = loaded_inputs.get_mut(&input_id) {
            println!("Recreando {} outputs para input {}", outputs.len(), input_id);
            
            for o in outputs {
                let _ = input.packet_tx.subscribe();

                let destination = match o.destination {
                    Some(ref d) => d.clone(),
                    None => String::new(),
                };            

                // Solo recrear outputs que están marcados como "running"
                if o.status != "running" {
                    println!("Output {} está en status '{}', agregando a stopped_outputs", o.id, o.status);

                    // Recrear CreateOutputRequest para store en stopped_outputs
                    let output_config = match o.kind.as_str() {
                        "udp" => CreateOutputRequest::Udp {
                            input_id,
                            destination_addr: destination.clone(),
                            name: o.name.clone(),
                        },
                        "srt_caller" => {
                            let config_json = o.config_json.unwrap_or_default();
                            let config: SrtOutputConfig = serde_json::from_str(&config_json)
                                .unwrap_or(SrtOutputConfig::Caller {
                                    destination_addr: destination.clone(),
                                    common: SrtCommonConfig::default()
                                });
                            CreateOutputRequest::Srt {
                                input_id,
                                name: o.name.clone(),
                                config,
                            }
                        },
                        "srt_listener" => {
                            let config_json = o.config_json.unwrap_or_default();
                            let config: SrtOutputConfig = serde_json::from_str(&config_json)
                                .unwrap_or(SrtOutputConfig::Listener {
                                    listen_port: o.listen_port.unwrap_or(8000),
                                    common: SrtCommonConfig::default()
                                });
                            CreateOutputRequest::Srt {
                                input_id,
                                name: o.name.clone(),
                                config,
                            }
                        },
                        _ => {
                            println!("Tipo de output desconocido: {}, saltando", o.kind);
                            continue;
                        }
                    };

                    input.stopped_outputs.insert(o.id, output_config);
                    continue;
                }

                let result = match o.kind.as_str() {
                    "udp" => {
                        if let Ok(_dest) = destination.parse::<std::net::SocketAddr>() {
                            // Use create_udp_output to properly handle names
                            match create_udp_output(input_id, destination.clone(), input, o.id, o.name.clone()).await {
                                Ok(output_info) => {
                                    input.output_tasks.insert(o.id, output_info);
                                    Ok(())
                                }
                                Err(e) => Err(anyhow::anyhow!("Error recreating UDP output: {}", e))
                            }
                        } else {
                            Err(anyhow::anyhow!("Invalid UDP destination: {}", destination))
                        }
                    }
                    "srt_caller" => {
                        let cfg: SrtCommonConfig = serde_json::from_str(
                            o.config_json.as_deref().unwrap_or("{}")
                        )?;
                        let output_config = SrtOutputConfig::Caller { destination_addr: destination.clone(), common: cfg };
                        match create_srt_output(input_id, output_config, input, o.id, o.name.clone()) {
                            Ok(output_info) => {
                                input.output_tasks.insert(o.id, output_info);
                                Ok(())
                            }
                            Err(e) => Err(anyhow::anyhow!("Error recreating SRT Caller output: {}", e))
                        }
                    }
                    "srt_listener" => { 
                        let cfg: SrtCommonConfig = serde_json::from_str(
                            o.config_json.as_deref().unwrap_or("{}")
                        )?;
                        
                        let listen_port = match o.listen_port {
                            Some(port) => port,
                            None => return Err(anyhow::anyhow!("Missing listen_port for SRT Listener output {}", o.id)),
                        };

                        let output_config = SrtOutputConfig::Listener { listen_port, common: cfg };

                        let output_info = match create_srt_output(input_id, output_config, input, o.id, o.name.clone()) {
                            Ok(_) => {
                                println!("Output SRT Listener {} creado para input {}", o.id, input_id);
                                Ok(())
                            }
                            Err(e) => Err(anyhow::anyhow!("Error recreating SRT Listener output: {}", e))
                        };
                        println!("Output SRT Listener {} recreado para input {}", o.id, input_id);
                        output_info
                    }
                    _ => {
                        println!("Tipo de output desconocido: {}, saltando", o.kind);
                        Ok(())
                    }
                };
                
                if let Err(e) = result {
                    error!("Error recreating output {} for input {}: {}", o.id, input_id, e);
                }
            }
        } else {
            println!("Input {} no encontrado para outputs, saltando", input_id);
        }
    }    

    // Now acquire the mutex lock only briefly to insert all loaded inputs
    {
        let mut inputs = ACTIVE_STREAMS.lock().await;
        for (id, input_info) in loaded_inputs {
            inputs.insert(id, input_info);
        }
    }

    println!("Carga desde DB completada");
    Ok(())
}


fn get_name_from_request(req: &CreateInputRequest) -> Option<String> {
    match req {
        CreateInputRequest::Udp { name, .. } => name.clone(),
        CreateInputRequest::Srt { name, .. } => name.clone(),
    }
}

fn generate_input_name_and_details(req: &CreateInputRequest) -> (Option<String>, String) {
    match req {
        CreateInputRequest::Udp { listen_port, .. } => {
            (Some(format!("UDP Listener {listen_port}")), format!("UDP Listener on port {listen_port}"))
        }
        CreateInputRequest::Srt { config, .. } => {
            let name = match config {
                SrtInputConfig::Listener { listen_port, .. } =>
                    format!("SRT Listener {listen_port}"),
                SrtInputConfig::Caller { target_addr, .. } =>
                    format!("SRT Caller {}", target_addr),
            };
            (Some(name), format!("{:?}", config))
        }
    }
}

fn spawn_input(req: CreateInputRequest, id: i64, name: Option<String>, details: String) -> Result<InputInfo, actix_web::Error> {
    match req {
        /* ----------------------------- UDP ----------------------------- */
        CreateInputRequest::Udp { listen_port, .. } => {
            spawn_udp_input_with_stats(id, name, details, listen_port)
        }

        /* ----------------------- SRT  (caller o listener) -------------- */
        CreateInputRequest::Srt { ref config, .. } => {            
            // Lanza el forwarder (con reconexión automática)
            let sender =
                Forwarder::spawn_with_stats(Box::new(config.clone()), Duration::from_secs(1));

            println!("Input SRT '{id}' creado con detalles: {details}");
            Ok(InputInfo {
                id,
                name,
                details,
                status: StreamStatus::Running,
                packet_tx: sender.tx,
                stats: sender.stats,
                task_handle: Some(sender.handle),
                config: req,
                output_tasks: HashMap::new(),
                stopped_outputs: HashMap::new(),
                analysis_tasks: HashMap::new(),
                paused_analysis: Vec::new(),
            })
        }
    }
}

// ==================== Analysis Endpoints ====================

#[actix_web::post("/inputs/{id}/analysis/{analysis_type}/start")]
pub async fn start_analysis(
    path: web::Path<(i64, String)>,
) -> ActixResult<impl Responder> {
    let (input_id, analysis_type_str) = path.into_inner();

    info!("Starting {} analysis for input {}", analysis_type_str, input_id);

    // Parse analysis type
    let analysis_type = match analysis_type_str.parse::<AnalysisType>() {
        Ok(t) => t,
        Err(e) => {
            error!("Invalid analysis type '{}': {}", analysis_type_str, e);
            return Err(ErrorInternalServerError(format!("Invalid analysis type: {}", e)));
        }
    };

    // Start the analysis
    match analysis::start_analysis(input_id, analysis_type).await {
        Ok(analysis_id) => {
            info!("Analysis started with ID: {}", analysis_id);
            Ok(HttpResponse::Created().json(serde_json::json!({
                "message": "Analysis started successfully",
                "analysis_id": analysis_id,
                "input_id": input_id,
                "analysis_type": analysis_type_str
            })))
        }
        Err(e) => {
            error!("Failed to start analysis: {}", e);
            Err(ErrorInternalServerError(format!("Failed to start analysis: {}", e)))
        }
    }
}

#[actix_web::post("/inputs/{id}/analysis/{analysis_type}/stop")]
pub async fn stop_analysis(
    path: web::Path<(i64, String)>,
) -> ActixResult<impl Responder> {
    let (input_id, analysis_type_str) = path.into_inner();

    info!("Stopping {} analysis for input {}", analysis_type_str, input_id);

    // Parse analysis type
    let analysis_type = match analysis_type_str.parse::<AnalysisType>() {
        Ok(t) => t,
        Err(e) => {
            error!("Invalid analysis type '{}': {}", analysis_type_str, e);
            return Err(ErrorInternalServerError(format!("Invalid analysis type: {}", e)));
        }
    };

    // Stop the analysis
    match analysis::stop_analysis(input_id, analysis_type).await {
        Ok(()) => {
            info!("Analysis stopped successfully");
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Analysis stopped successfully",
                "input_id": input_id,
                "analysis_type": analysis_type_str
            })))
        }
        Err(e) => {
            error!("Failed to stop analysis: {}", e);
            Err(ErrorInternalServerError(format!("Failed to stop analysis: {}", e)))
        }
    }
}

#[actix_web::get("/inputs/{id}/analysis")]
pub async fn get_analysis_status(
    path: web::Path<i64>,
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();

    match analysis::get_active_analyses(input_id).await {
        Ok(analyses) => {
            let mut active_analyses = Vec::new();

            for (id, analysis_type, created_at) in analyses {
                // Convert SystemTime to ISO 8601 string
                let created_at_str = match created_at.duration_since(std::time::UNIX_EPOCH) {
                    Ok(duration) => {
                        let secs = duration.as_secs();
                        let nanos = duration.subsec_nanos();
                        chrono::DateTime::from_timestamp(secs as i64, nanos)
                            .unwrap_or_default()
                            .to_rfc3339()
                    }
                    Err(_) => "unknown".to_string(),
                };

                active_analyses.push(AnalysisStatusResponse {
                    id,
                    analysis_type: analysis_type.to_string(),
                    input_id,
                    status: "running".to_string(),
                    created_at: created_at_str,
                });
            }

            let response = AnalysisListResponse {
                input_id,
                active_analyses,
            };

            Ok(HttpResponse::Ok().json(response))
        }
        Err(e) => {
            error!("Failed to get analysis status: {}", e);
            Err(ErrorInternalServerError(format!("Failed to get analysis status: {}", e)))
        }
    }
}

// ==================== Stream Control Endpoints ====================

#[actix_web::put("/inputs/{id}/start")]
pub async fn start_input_endpoint(
    path: web::Path<i64>,
    state: web::Data<AppState>,
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();

    info!("Request to start input {}", input_id);

    match stream_control::start_input(input_id).await {
        Ok(()) => {
            // Update database status
            if let Err(e) = update_input_status_in_db(&state.pool, input_id, "running").await {
                error!("Failed to update input status in database: {}", e);
                // Continue anyway - the stream is started in memory
            }

            info!("Input {} started successfully", input_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Input started successfully",
                "input_id": input_id,
                "status": "running"
            })))
        }
        Err(e) => {
            error!("Failed to start input {}: {}", input_id, e);
            Err(ErrorInternalServerError(format!("Failed to start input: {}", e)))
        }
    }
}

#[actix_web::put("/inputs/{id}/stop")]
pub async fn stop_input_endpoint(
    path: web::Path<i64>,
    state: web::Data<AppState>,
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();

    info!("Request to stop input {}", input_id);

    match stream_control::stop_input(input_id).await {
        Ok(()) => {
            // Update database status
            if let Err(e) = update_input_status_in_db(&state.pool, input_id, "stopped").await {
                error!("Failed to update input status in database: {}", e);
                // Continue anyway - the stream is stopped in memory
            }

            info!("Input {} stopped successfully", input_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Input stopped successfully",
                "input_id": input_id,
                "status": "stopped"
            })))
        }
        Err(e) => {
            error!("Failed to stop input {}: {}", input_id, e);
            Err(ErrorInternalServerError(format!("Failed to stop input: {}", e)))
        }
    }
}

#[actix_web::put("/outputs/{id}/start")]
pub async fn start_output_endpoint(
    path: web::Path<i64>,
    state: web::Data<AppState>,
) -> ActixResult<impl Responder> {
    let output_id = path.into_inner();

    info!("Request to start output {}", output_id);

    // First get the input_id for this output from database
    let input_id = match get_input_id_for_output(&state.pool, output_id).await {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to find input for output {}: {}", output_id, e);
            return Err(ErrorInternalServerError(format!("Failed to find input for output: {}", e)));
        }
    };

    match stream_control::start_output(input_id, output_id).await {
        Ok(()) => {
            // Update database status
            if let Err(e) = update_output_status_in_db(&state.pool, output_id, "running").await {
                error!("Failed to update output status in database: {}", e);
                // Continue anyway - the stream is started in memory
            }

            info!("Output {} started successfully", output_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Output started successfully",
                "output_id": output_id,
                "input_id": input_id,
                "status": "running"
            })))
        }
        Err(e) => {
            error!("Failed to start output {}: {}", output_id, e);
            Err(ErrorInternalServerError(format!("Failed to start output: {}", e)))
        }
    }
}

#[actix_web::put("/outputs/{id}/stop")]
pub async fn stop_output_endpoint(
    path: web::Path<i64>,
    state: web::Data<AppState>,
) -> ActixResult<impl Responder> {
    let output_id = path.into_inner();

    info!("Request to stop output {}", output_id);

    // First get the input_id for this output from database
    let input_id = match get_input_id_for_output(&state.pool, output_id).await {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to find input for output {}: {}", output_id, e);
            return Err(ErrorInternalServerError(format!("Failed to find input for output: {}", e)));
        }
    };

    match stream_control::stop_output(input_id, output_id).await {
        Ok(()) => {
            // Update database status
            if let Err(e) = update_output_status_in_db(&state.pool, output_id, "stopped").await {
                error!("Failed to update output status in database: {}", e);
                // Continue anyway - the stream is stopped in memory
            }

            info!("Output {} stopped successfully", output_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Output stopped successfully",
                "output_id": output_id,
                "input_id": input_id,
                "status": "stopped"
            })))
        }
        Err(e) => {
            error!("Failed to stop output {}: {}", output_id, e);
            Err(ErrorInternalServerError(format!("Failed to stop output: {}", e)))
        }
    }
}