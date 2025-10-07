use actix_web::{web, Responder, HttpResponse, Result as ActixResult, error::ErrorInternalServerError};
use std::collections::HashMap;
use std::time::Duration;
use ::log::{info, error};
use anyhow::Result;

use crate::udp_stream::{create_udp_output, spawn_udp_input_with_stats};
use crate::srt_stream::{
    create_srt_output, SrtSourceWithState,
};
use crate::spts_input::spawn_spts_input;
use crate::models::*;
use crate::database::{self, check_output_exists, check_port_conflict, get_input_by_id, get_output_by_id, update_input_status_in_db, update_output_status_in_db, get_input_id_for_output};
use crate::{ACTIVE_STREAMS, STATE_CHANGE_TX};
use crate::analysis;
use crate::stream_control;
use crate::metrics;
use crate::interfaces;
use sqlx::types::chrono;
use tokio::sync::{broadcast, RwLock};
use std::sync::Arc;
use std::time::SystemTime;
use std::net::{IpAddr};

pub type InputsMap = std::collections::HashMap<i64, InputInfo>;

pub struct AppState {
    pub pool: sqlx::SqlitePool,
}

/// Validate multicast group address
fn validate_multicast_group(group_addr: &str) -> Result<(), String> {
    let addr: IpAddr = group_addr.parse()
        .map_err(|_| format!("Invalid IP address format: '{}'", group_addr))?;

    match addr {
        IpAddr::V4(ipv4) => {
            if !ipv4.is_multicast() {
                return Err(format!("Address '{}' is not a valid IPv4 multicast address (must be in range 224.0.0.0 - 239.255.255.255)", group_addr));
            }
        },
        IpAddr::V6(ipv6) => {
            if !ipv6.is_multicast() {
                return Err(format!("Address '{}' is not a valid IPv6 multicast address", group_addr));
            }
        },
    }

    Ok(())
}

/// Validate bind IP address
fn validate_bind_address(bind_addr: &str) -> Result<(), String> {
    if bind_addr == "0.0.0.0" || bind_addr == "::" {
        return Ok(()); // These are always valid
    }

    let _addr: IpAddr = bind_addr.parse()
        .map_err(|_| format!("Invalid bind IP address format: '{}'", bind_addr))?;

    // Check if the bind address exists on one of the system's network interfaces
    if !interfaces::is_valid_bind_address(bind_addr) {
        return Err(format!("Bind address '{}' not found in system interfaces. Use GET /interfaces to see available addresses.", bind_addr));
    }

    Ok(())
}

/// Validate UDP input configuration
fn validate_udp_input_config(config: &CreateInputRequest) -> Result<(), String> {
    if let CreateInputRequest::Udp { bind_host, multicast_group, source_specific_multicast, .. } = config {
        // Validate bind address if specified
        if let Some(bind_addr) = bind_host {
            if !bind_addr.is_empty() {
                validate_bind_address(bind_addr)?;
            }
        }

        // Validate multicast group if specified
        if let Some(group_addr) = multicast_group {
            if !group_addr.is_empty() {
                validate_multicast_group(group_addr)?;
            }
        }

        // Validate source-specific multicast requirements
        if let Some(source_addr) = source_specific_multicast {
            if !source_addr.is_empty() {
                // Source-specific multicast requires a multicast group
                if multicast_group.is_none() || multicast_group.as_ref().unwrap().is_empty() {
                    return Err("Source-specific multicast requires a multicast group to be specified".to_string());
                }

                // Validate source address format
                let _addr: IpAddr = source_addr.parse()
                    .map_err(|_| format!("Invalid source IP address format: '{}'", source_addr))?;
            }
        }
    }

    Ok(())
}

/// Validate SRT configuration
fn validate_srt_config(config: &CreateInputRequest) -> Result<(), String> {
    if let CreateInputRequest::Srt { config: srt_config, .. } = config {
        match srt_config {
            SrtInputConfig::Listener { bind_host, .. } => {
                if let Some(bind_addr) = bind_host {
                    if !bind_addr.is_empty() {
                        validate_bind_address(bind_addr)?;
                    }
                }
            },
            SrtInputConfig::Caller { bind_host, .. } => {
                if let Some(bind_addr) = bind_host {
                    if !bind_addr.is_empty() {
                        validate_bind_address(bind_addr)?;
                    }
                }
            },
        }
    }

    Ok(())
}

/// Validate UDP output configuration
fn validate_udp_output_config(config: &CreateOutputRequest) -> Result<(), String> {
    if let CreateOutputRequest::Udp { bind_host, multicast_ttl, multicast_interface, remote_host, .. } = config {
        // Validate bind address if specified
        if let Some(bind_addr) = bind_host {
            if !bind_addr.is_empty() {
                validate_bind_address(bind_addr)?;
            }
        }

        // Validate multicast interface if specified
        if let Some(mcast_interface) = multicast_interface {
            if !mcast_interface.is_empty() {
                validate_bind_address(mcast_interface)?;
            }
        }

        // Validate multicast TTL range
        if let Some(ttl) = multicast_ttl {
            if *ttl == 0 {
                return Err("Multicast TTL must be between 1 and 255".to_string());
            }
        }

        // Check if destination is multicast address
        if let Some(remote_addr) = remote_host {
            if let Ok(addr) = remote_addr.parse::<std::net::IpAddr>() {
                if addr.is_multicast() {
                    // For multicast destinations, recommend setting TTL if not specified
                    if multicast_ttl.is_none() {
                        // This is just a warning, not an error - we'll use system default
                    }
                }
            }
        }
    }

    Ok(())
}

/// Validate SRT output configuration
fn validate_srt_output_config(config: &CreateOutputRequest) -> Result<(), String> {
    if let CreateOutputRequest::Srt { config: srt_config, .. } = config {
        match srt_config {
            SrtOutputConfig::Listener { bind_host, .. } => {
                if let Some(bind_addr) = bind_host {
                    if !bind_addr.is_empty() {
                        validate_bind_address(bind_addr)?;
                    }
                }
            },
            SrtOutputConfig::Caller { bind_host, .. } => {
                if let Some(bind_addr) = bind_host {
                    if !bind_addr.is_empty() {
                        validate_bind_address(bind_addr)?;
                    }
                }
            },
        }
    }

    Ok(())
}

#[actix_web::post("/inputs")]
async fn create_input(
    state: web::Data<AppState>,
    req:   web::Json<CreateInputRequest>,
) -> ActixResult<impl Responder> {

    println!("Petición para crear input: {:?}", req);

    // Validate configuration before processing
    if let Err(validation_error) = validate_udp_input_config(&req) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }
    if let Err(validation_error) = validate_srt_config(&req) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }

    // Generate name for the input
    let auto_name = generate_input_name(&req);
    let name = get_name_from_request(&req).or(auto_name);

    // Handle automatic port assignment by modifying the request configuration
    let mut final_req = req.clone();
    let assigned_port = if req.is_automatic_port() {
        let port = crate::port_utils::find_available_port(&state.pool, None).await
            .map_err(|e| ErrorInternalServerError(format!("Failed to find available port: {e}")))?;

        // Modify the configuration to include the assigned port
        match &mut final_req {
            CreateInputRequest::Udp { bind_port, listen_port, automatic_port, .. } => {
                *bind_port = Some(port);
                *listen_port = Some(port); // For backward compatibility
                *automatic_port = None; // Remove the automatic_port flag from saved config
            },
            CreateInputRequest::Srt { config, .. } => {
                match config {
                    SrtInputConfig::Listener { bind_port, listen_port, automatic_port, .. } => {
                        *bind_port = Some(port);
                        *listen_port = Some(port); // For backward compatibility
                        *automatic_port = None; // Remove the automatic_port flag from saved config
                    },
                    _ => {} // Callers don't need auto port assignment
                }
            },
            CreateInputRequest::Spts { .. } => {
                // SPTS inputs don't need port assignment
            }
        }

        Some(port)
    } else {
        None
    };

    // Save to database with the modified configuration
    let id = match database::save_input_to_db(&state.pool, name.as_deref(), &final_req).await {
        Ok(id) => id,
        Err(e) => {
            error!("Error saving input to database: {}", e);
            return Err(ErrorInternalServerError(format!("Database error: {e}")));
        }
    };

    println!("Input '{}' guardado en la base de datos con ID: {}", name.as_deref().unwrap_or("sin nombre"), id);

    // Now spawn the input tasks using the modified configuration
    match spawn_input(final_req, id, name.clone(), None).await {
        Ok(info) => {
            println!("Input creado con ID: {}", info.id);
            
            let mut guard = ACTIVE_STREAMS.lock().await;
            guard.insert(info.id, info);            

            println!("Input '{}' añadido al estado compartido", id);
            let mut response = serde_json::json!({
                "id": id,
                "name": name
            });

            if let Some(port) = assigned_port {
                response["assigned_port"] = serde_json::json!(port);
            }

            Ok(HttpResponse::Created().json(response))
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
        // Decrement active inputs counter
        metrics::decrement_active_inputs();
        // El drop del broadcast::Sender (input_info.packet_tx) ocurrirá cuando input_info salga del scope.
        // Esto hará que los receivers de los outputs obtengan RecvError::Closed.

        // 2. Abortar explícitamente las tareas de output asociadas
        let output_count = input_info.output_tasks.len();
        for (output_id, output_info) in input_info.output_tasks {
            info!("[{}] Abortando output task [{}]", input_id, output_id);
            if let Some(handle) = output_info.abort_handle {
                handle.abort();
            }
        }
        // Decrement active outputs counter for all outputs of this input
        for _ in 0..output_count {
            metrics::decrement_active_outputs();
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

#[actix_web::patch("/inputs/{id}")]
pub async fn update_input(
    state: web::Data<AppState>,
    path: web::Path<i64>,
    req: web::Json<UpdateInputRequest>,
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();

    info!("Request to update input {}: {:?}", input_id, req);

    // Get current input from database
    let current_input_row = match database::get_input_by_id(&state.pool, input_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)));
        }
        Err(e) => {
            error!("Error fetching input from database: {}", e);
            return Err(ErrorInternalServerError(format!("Database error: {e}")));
        }
    };

    // Deserialize current configuration
    let current_config: CreateInputRequest = match serde_json::from_str(&current_input_row.config_json) {
        Ok(config) => config,
        Err(e) => {
            error!("Error deserializing input config: {}", e);
            return Err(ErrorInternalServerError(format!("Config deserialization error: {e}")));
        }
    };

    // Merge update request with current configuration
    let updated_config = req.merge_with(&current_config);

    // Validate the updated configuration
    if let Err(validation_error) = validate_udp_input_config(&updated_config) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }
    if let Err(validation_error) = validate_srt_config(&updated_config) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }

    // Check for port conflicts if port changed
    let old_port = current_config.get_bind_port();
    let new_port = updated_config.get_bind_port();
    if old_port != new_port && new_port != 0 {
        match check_input_port_conflict(&state.pool, new_port, Some(input_id)).await {
            Ok(conflict) if conflict => {
                return Ok(HttpResponse::Conflict().body(format!(
                    "Puerto {} ya está en uso", new_port
                )));
            }
            Ok(_) => {},
            Err(e) => {
                error!("Error checking port conflict: {}", e);
                return Err(ErrorInternalServerError(format!("Database error: {e}")));
            }
        }
    }

    // Update configuration in database
    if let Err(e) = database::update_input_config_in_db(&state.pool, input_id, &updated_config).await {
        error!("Error updating input config in database: {}", e);
        return Err(ErrorInternalServerError(format!("Database error: {e}")));
    }

    // Get lock on active streams
    let mut state_guard = ACTIVE_STREAMS.lock().await;

    // If input is currently active, restart it with new configuration
    if let Some(mut input_info) = state_guard.remove(&input_id) {
        info!("Restarting input {} with new configuration", input_id);

        // Preserve outputs and analysis state
        let outputs = input_info.output_tasks.clone();
        let stopped_outputs = input_info.stopped_outputs.clone();
        let active_analyses: Vec<AnalysisType> = input_info.analysis_tasks.values()
            .map(|a| a.analysis_type.clone())
            .collect();

        // Stop current input task
        if let Some(handle) = input_info.task_handle.take() {
            handle.abort();
        }

        // Stop all output tasks (they will be recreated)
        for (output_id, output_info) in &outputs {
            info!("[{}] Stopping output task [{}] for restart", input_id, output_id);
            if let Some(handle) = &output_info.abort_handle {
                handle.abort();
            }
        }

        // Stop all analysis tasks
        for (_, analysis_info) in &input_info.analysis_tasks {
            analysis_info.task_handle.abort();
        }

        // Spawn new input with updated configuration
        let updated_name = match &updated_config {
            CreateInputRequest::Udp { name, .. } => name.clone(),
            CreateInputRequest::Srt { name, .. } => name.clone(),
            CreateInputRequest::Spts { name, .. } => name.clone(),
        }.or(current_input_row.name);

        match spawn_input(updated_config.clone(), input_id, updated_name.clone(), None).await {
            Ok(mut new_input_info) => {
                info!("Input {} restarted successfully", input_id);

                // Recreate outputs for the new input
                for (output_id, output_info) in outputs {
                    info!("Recreating output {} for input {}", output_id, input_id);

                    let recreated_output = match &output_info.kind {
                        OutputKind::Udp => {
                            create_udp_output(
                                output_info.destination.clone(),
                                &new_input_info,
                                output_id,
                                output_info.name.clone(),
                                output_info.config.get_host(),
                                None, // multicast config would need to be extracted from output_info.config
                                get_state_change_sender().await
                            ).await
                        },
                        OutputKind::SrtCaller | OutputKind::SrtListener => {
                            if let CreateOutputRequest::Srt { config, .. } = &output_info.config {
                                create_srt_output(
                                    input_id,
                                    config.clone(),
                                    &new_input_info,
                                    output_id,
                                    output_info.name.clone(),
                                    get_state_change_sender().await
                                )
                            } else {
                                Err(actix_web::error::ErrorInternalServerError("Invalid output config"))
                            }
                        }
                    };

                    match recreated_output {
                        Ok(output) => {
                            new_input_info.output_tasks.insert(output_id, output);
                        }
                        Err(e) => {
                            error!("Error recreating output {}: {}", output_id, e);
                            // Continue with other outputs
                        }
                    }
                }

                // Restore stopped outputs
                new_input_info.stopped_outputs = stopped_outputs;

                // Restart analyses that were active
                for analysis_type in active_analyses {
                    info!("Restarting {} analysis for input {}", analysis_type, input_id);
                    // Note: This would require access to the analysis module
                    // For now, we'll just log that analyses need to be manually restarted
                }

                state_guard.insert(input_id, new_input_info);

                Ok(HttpResponse::Ok().json(serde_json::json!({
                    "message": "Input actualizado y reiniciado",
                    "id": input_id,
                    "name": updated_name
                })))
            }
            Err(e) => {
                error!("Error restarting input {}: {}", input_id, e);

                // Try to restore original input
                input_info.stopped_outputs = stopped_outputs;
                state_guard.insert(input_id, input_info);

                Err(ErrorInternalServerError(format!("Failed to restart input: {e}")))
            }
        }
    } else {
        // Input is not active, just update database
        info!("Input {} updated in database (not currently active)", input_id);

        Ok(HttpResponse::Ok().json(serde_json::json!({
            "message": "Input actualizado (no estaba activo)",
            "id": input_id
        })))
    }
}

#[actix_web::post("/outputs")]
pub async fn create_output(
    state: web::Data<AppState>,
    req: web::Json<CreateOutputRequest>,
) -> ActixResult<impl Responder> {

    info!("Petición para crear output: {:?}", req);

    // Validate configuration before processing
    if let Err(validation_error) = validate_udp_output_config(&req) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }
    if let Err(validation_error) = validate_srt_output_config(&req) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }

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

    // Handle automatic port assignment by modifying the request configuration
    let mut final_req = req_val.clone();
    let assigned_port = if req_val.is_automatic_port() {
        let port = crate::port_utils::find_available_port(&state.pool, None).await
            .map_err(|e| ErrorInternalServerError(format!("Failed to find available port: {e}")))?;

        // Modify the configuration to include the assigned port
        match &mut final_req {
            CreateOutputRequest::Udp { remote_port, automatic_port, .. } => {
                *remote_port = Some(port);
                *automatic_port = None; // Remove the automatic_port flag from saved config
            },
            CreateOutputRequest::Srt { config, .. } => {
                match config {
                    SrtOutputConfig::Listener { bind_port, listen_port, automatic_port, .. } => {
                        *bind_port = Some(port);
                        *listen_port = Some(port); // For backward compatibility
                        *automatic_port = None; // Remove the automatic_port flag from saved config
                    },
                    _ => {} // Callers don't need auto port assignment
                }
            },
        }

        Some(port)
    } else {
        None
    };

    // Validation logic - build destination string using the modified request
    let (destination_addr, listen_port) = match &final_req {
        CreateOutputRequest::Udp { .. } => {
            // Use helper methods to get host and port, with fallbacks for legacy fields
            let host = final_req.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
            let port = final_req.get_remote_port().unwrap_or(8000);
            (format!("{}:{}", host, port), None)
        },
        CreateOutputRequest::Srt { config, .. } => {
            match config {
                SrtOutputConfig::Caller { .. } => {
                    let host = config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), None)
                },
                SrtOutputConfig::Listener { .. } => {
                    let port = config.get_bind_port().unwrap_or(8000);
                    (format!(":{}", port), Some(port))
                },
            }
        },
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

    // Save to database first to get auto-generated ID using final request
    let (name, kind, config_json) = match &final_req {
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
                SrtOutputConfig::Caller { .. } => {
                    let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                    let port = config.get_remote_port().unwrap_or(0);
                    format!("SRT Caller to {}:{}", host, port)
                },
                SrtOutputConfig::Listener { .. } => {
                    let port = config.get_bind_port().unwrap_or(0);
                    format!("SRT Listener on {}", port)
                },
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

    // Create the output with the generated ID using the final request
    let output_info = match &final_req {
        CreateOutputRequest::Udp { name: user_name, bind_host, multicast_ttl, multicast_interface, remote_host, .. } => {
            // Check if destination is multicast and create config
            let multicast_config = if let Some(ref host) = remote_host {
                if let Ok(addr) = host.parse::<std::net::IpAddr>() {
                    if addr.is_multicast() {
                        Some(crate::udp_stream::MulticastOutputConfig {
                            ttl: multicast_ttl.unwrap_or(1), // Default TTL of 1 for local network
                            interface: multicast_interface.clone(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // Use the already computed destination_addr string
            create_udp_output(destination_addr.clone(), input, output_id, user_name.clone(), bind_host.clone(), multicast_config, get_state_change_sender().await).await?
        },

        CreateOutputRequest::Srt { config, name: user_name, .. } =>
            create_srt_output(input_id, config.clone(), input, output_id, user_name.clone(), get_state_change_sender().await)?,
    };

    // Insertamos el output en el Input correspondiente
    guard
        .get_mut(&output_info.input_id)
        .expect("Input debe existir aquí")   // ya comprobado
        .output_tasks
        .insert(output_info.id, output_info.clone());

    let mut response = serde_json::json!({
        "message": "Output creado",
        "output_id": output_info.id,
        "input_id": output_info.input_id,
        "destination": output_info.destination,
        "type": output_info.kind.to_string(),
    });

    if let Some(port) = assigned_port {
        response["assigned_port"] = serde_json::json!(port);
    }

    Ok(HttpResponse::Created().json(response))
}

#[actix_web::get("/inputs")]
pub async fn list_inputs(_state: web::Data<AppState>) -> ActixResult<impl Responder> {
    // Clone necessary data while holding the lock briefly
    let inputs_snapshot: Vec<(i64, Option<String>, String, usize, StreamStatus, Option<u16>, Option<SystemTime>, Option<String>)> = {
        let state_guard = ACTIVE_STREAMS.lock().await;
        state_guard.iter().map(|(input_id, input_info)| {
            (
                *input_id,
                input_info.name.clone(),
                input_kind_string(&input_info.config).to_string(),
                input_info.output_tasks.len(),
                input_info.status.clone(),
                input_info.config.extract_assigned_port(),
                input_info.connected_at,
                input_info.source_address.clone(),
            )
        }).collect()
    }; // Lock released here

    let mut response: Vec<InputListResponse> = Vec::new();

    for (input_id, name, kind, output_count, status, assigned_port, connected_at, source_address) in inputs_snapshot {
        let input_type = input_type_display_string(&kind).to_string();

        response.push(InputListResponse {
            id: input_id,
            name,
            input_type,
            status: status.to_string(),
            assigned_port,
            output_count,
            uptime_seconds: calculate_connection_uptime(&status, connected_at),
            source_address,
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
        // Get input type and config from database
        let (input_type, config_json) = if let Ok(Some(db_input)) = get_input_by_id(&state.pool, input_id).await {
            (input_type_display_string(&db_input.kind).to_string(), Some(db_input.config_json))
        } else {
            ("Unknown".to_string(), None)
        };

        // Build outputs list with detailed information
        let mut outputs: Vec<OutputDetailResponse> = Vec::new();

        // Add active outputs
        for (output_id, output_info) in input_info.output_tasks.iter() {
            let (config, assigned_port) = if let Ok(Some(db_output)) = get_output_by_id(&state.pool, *output_id).await {
                let port = if let Some(config_json) = &db_output.config_json {
                    // Extract port from configuration JSON
                    if let Ok(config) = serde_json::from_str::<CreateOutputRequest>(config_json) {
                        config.extract_assigned_port()
                    } else {
                        None
                    }
                } else {
                    None
                };
                (db_output.config_json, port)
            } else {
                (None, None)
            };

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                assigned_port,
                config: config_json,
                uptime_seconds: calculate_connection_uptime(&output_info.status, output_info.connected_at),
                peer_address: output_info.peer_address.clone(),
            });
        }

        // Add stopped outputs
        for (output_id, output_config) in input_info.stopped_outputs.iter() {
            let (destination, output_type) = match output_config {
                CreateOutputRequest::Udp { .. } => {
                    let host = output_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = output_config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), "udp")
                },
                CreateOutputRequest::Srt { config, .. } => {
                    let kind_str = match config {
                        SrtOutputConfig::Caller { .. } => "srt_caller",
                        SrtOutputConfig::Listener { .. } => "srt_listener",
                    };
                    let dest = match config {
                        SrtOutputConfig::Caller { .. } => {
                            let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                            let port = config.get_remote_port().unwrap_or(0);
                            format!("{}:{}", host, port)
                        },
                        SrtOutputConfig::Listener { .. } => {
                            let port = config.get_bind_port().unwrap_or(0);
                            format!(":{}", port)
                        },
                    };
                    (dest, kind_str)
                },
            };

            let name = match output_config {
                CreateOutputRequest::Udp { name, .. } => name.clone(),
                CreateOutputRequest::Srt { name, .. } => name.clone(),
            };

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name,
                input_id,
                destination,
                output_type: output_type.to_string(),
                status: "stopped".to_string(),
                assigned_port: output_config.extract_assigned_port(),
                config: Some(serde_json::to_string(output_config).unwrap_or_default()),
                uptime_seconds: None,
                peer_address: None,
            });
        }

        let input_assigned_port = if let Ok(Some(db_input)) = get_input_by_id(&state.pool, input_id).await {
            // Extract port from configuration JSON
            if let Ok(config) = serde_json::from_str::<CreateInputRequest>(&db_input.config_json) {
                config.extract_assigned_port()
            } else {
                None
            }
        } else {
            None
        };

        let response = InputDetailResponse {
            id: input_id,
            name: input_info.name.clone(),
            input_type,
            status: input_info.status.to_string(),
            assigned_port: input_assigned_port,
            outputs,
            uptime_seconds: calculate_connection_uptime(&input_info.status, input_info.connected_at),
            source_address: input_info.source_address.clone(),
            config: config_json,
        };

        Ok(HttpResponse::Ok().json(response))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

#[actix_web::get("/outputs")]
pub async fn list_outputs(state: web::Data<AppState>) -> ActixResult<impl Responder> {
    // Clone necessary data while holding the lock briefly
    let outputs_snapshot: Vec<OutputSnapshotData> = {
        let state_guard = ACTIVE_STREAMS.lock().await;
        let mut snapshot = Vec::new();

        for (input_id, input_info) in state_guard.iter() {
            // Collect active outputs
            for (output_id, output_info) in input_info.output_tasks.iter() {
                snapshot.push(OutputSnapshotData::Active {
                    id: *output_id,
                    name: output_info.name.clone(),
                    input_id: *input_id,
                    input_name: input_info.name.clone(),
                    destination: output_info.destination.clone(),
                    kind: output_info.kind.clone(),
                    status: output_info.status.clone(),
                    assigned_port: output_info.config.extract_assigned_port(),
                    connected_at: output_info.connected_at,
                    peer_address: output_info.peer_address.clone(),
                });
            }

            // Collect stopped outputs
            for (output_id, output_config) in input_info.stopped_outputs.iter() {
                snapshot.push(OutputSnapshotData::Stopped {
                    id: *output_id,
                    input_id: *input_id,
                    input_name: input_info.name.clone(),
                    config: output_config.clone(),
                });
            }
        }
        snapshot
    }; // Lock released here

    let mut response: Vec<OutputListResponse> = Vec::new();

    for snapshot_data in outputs_snapshot {
        match snapshot_data {
            OutputSnapshotData::Active {
                id,
                name,
                input_id,
                input_name,
                destination,
                kind,
                status,
                assigned_port,
                connected_at,
                peer_address,
            } => {
                // Get config from database for active outputs
                let config_json = if let Ok(Some(db_output)) = database::get_output_by_id(&state.pool, id).await {
                    db_output.config_json
                } else {
                    None
                };

                response.push(OutputListResponse {
                    id,
                    name,
                    input_id,
                    input_name,
                    destination,
                    output_type: output_kind_string(&kind).to_string(),
                    status: status.to_string(),
                    assigned_port,
                    uptime_seconds: calculate_connection_uptime(&status, connected_at),
                    peer_address,
                    config: config_json,
                });
            }
            OutputSnapshotData::Stopped {
                id,
                input_id,
                input_name,
                config,
            } => {
                let (destination, output_type, name) = match &config {
                    CreateOutputRequest::Udp { name, .. } => {
                        let host = config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                        let port = config.get_remote_port().unwrap_or(8000);
                        (format!("{}:{}", host, port), "udp".to_string(), name.clone())
                    },
                    CreateOutputRequest::Srt { name, config: srt_config, .. } => {
                        let kind_str = match srt_config {
                            SrtOutputConfig::Caller { .. } => "srt_caller",
                            SrtOutputConfig::Listener { .. } => "srt_listener",
                        };
                        let dest = match srt_config {
                            SrtOutputConfig::Caller { .. } => {
                                let host = srt_config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                                let port = srt_config.get_remote_port().unwrap_or(0);
                                format!("{}:{}", host, port)
                            },
                            SrtOutputConfig::Listener { .. } => {
                                let port = srt_config.get_bind_port().unwrap_or(0);
                                format!(":{}", port)
                            },
                        };
                        (dest, kind_str.to_string(), name.clone())
                    },
                };

                let config_json = serde_json::to_string(&config).ok();

                response.push(OutputListResponse {
                    id,
                    name,
                    input_id,
                    input_name,
                    destination,
                    output_type,
                    status: "stopped".to_string(),
                    assigned_port: config.extract_assigned_port(),
                    uptime_seconds: None,
                    peer_address: None,
                    config: config_json,
                });
            }
        }
    }

    Ok(HttpResponse::Ok().json(response))
}

// Helper enum for snapshot data
enum OutputSnapshotData {
    Active {
        id: i64,
        name: Option<String>,
        input_id: i64,
        input_name: Option<String>,
        destination: String,
        kind: OutputKind,
        status: StreamStatus,
        assigned_port: Option<u16>,
        connected_at: Option<SystemTime>,
        peer_address: Option<String>,
    },
    Stopped {
        id: i64,
        input_id: i64,
        input_name: Option<String>,
        config: CreateOutputRequest,
    },
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
            // Use config from memory first, fallback to database
            let config_json = serde_json::to_string(&output_info.config).ok();
            let assigned_port = output_info.config.extract_assigned_port();

            let response = OutputDetailResponse {
                id: output_id,
                name: output_info.name.clone(),
                input_id: *input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                assigned_port,
                config: config_json,
                uptime_seconds: calculate_connection_uptime(&output_info.status, output_info.connected_at),
                peer_address: output_info.peer_address.clone(),
            };

            return Ok(HttpResponse::Ok().json(response));
        }

        // Check stopped outputs as well
        if let Some(output_config) = input_info.stopped_outputs.get(&output_id) {
            let (destination, output_type) = match output_config {
                CreateOutputRequest::Udp { .. } => {
                    let host = output_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = output_config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), "udp")
                },
                CreateOutputRequest::Srt { config, .. } => {
                    let kind_str = match config {
                        SrtOutputConfig::Caller { .. } => "srt_caller",
                        SrtOutputConfig::Listener { .. } => "srt_listener",
                    };
                    let dest = match config {
                        SrtOutputConfig::Caller { .. } => {
                            let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                            let port = config.get_remote_port().unwrap_or(0);
                            format!("{}:{}", host, port)
                        },
                        SrtOutputConfig::Listener { .. } => {
                            let port = config.get_bind_port().unwrap_or(0);
                            format!(":{}", port)
                        },
                    };
                    (dest, kind_str)
                },
            };

            let name = match output_config {
                CreateOutputRequest::Udp { name, .. } => name.clone(),
                CreateOutputRequest::Srt { name, .. } => name.clone(),
            };

            let response = OutputDetailResponse {
                id: output_id,
                name,
                input_id: *input_id,
                destination,
                output_type: output_type.to_string(),
                status: "stopped".to_string(),
                assigned_port: output_config.extract_assigned_port(),
                config: serde_json::to_string(output_config).ok(),
                uptime_seconds: None,
                peer_address: None,
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

        // Add active outputs
        for (output_id, output_info) in input_info.output_tasks.iter() {
            let config_json = serde_json::to_string(&output_info.config).ok();
            let assigned_port = output_info.config.extract_assigned_port();

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id,
                destination: output_info.destination.clone(),
                output_type: output_kind_string(&output_info.kind).to_string(),
                status: output_info.status.to_string(),
                assigned_port,
                config: config_json,
                uptime_seconds: calculate_connection_uptime(&output_info.status, output_info.connected_at),
                peer_address: output_info.peer_address.clone(),
            });
        }

        // Add stopped outputs
        for (output_id, output_config) in input_info.stopped_outputs.iter() {
            let (destination, output_type) = match output_config {
                CreateOutputRequest::Udp { .. } => {
                    let host = output_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = output_config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), "udp")
                },
                CreateOutputRequest::Srt { config, .. } => {
                    let kind_str = match config {
                        SrtOutputConfig::Caller { .. } => "srt_caller",
                        SrtOutputConfig::Listener { .. } => "srt_listener",
                    };
                    let dest = match config {
                        SrtOutputConfig::Caller { .. } => {
                            let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                            let port = config.get_remote_port().unwrap_or(0);
                            format!("{}:{}", host, port)
                        },
                        SrtOutputConfig::Listener { .. } => {
                            let port = config.get_bind_port().unwrap_or(0);
                            format!(":{}", port)
                        },
                    };
                    (dest, kind_str)
                },
            };

            let name = match output_config {
                CreateOutputRequest::Udp { name, .. } => name.clone(),
                CreateOutputRequest::Srt { name, .. } => name.clone(),
            };

            outputs.push(OutputDetailResponse {
                id: *output_id,
                name,
                input_id,
                destination,
                output_type: output_type.to_string(),
                status: "stopped".to_string(),
                assigned_port: output_config.extract_assigned_port(),
                config: Some(serde_json::to_string(output_config).unwrap_or_default()),
                uptime_seconds: None,
                peer_address: None,
            });
        }

        Ok(HttpResponse::Ok().json(outputs))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Input con ID '{}' no encontrado", input_id)))
    }
}

#[actix_web::delete("/outputs")]
pub async fn delete_output(
    state: web::Data<AppState>,
    req: web::Json<DeleteOutputRequest>,
) -> ActixResult<impl Responder> {
    let input_id = req.input_id;
    let output_id = req.output_id;

    let mut state_guard = ACTIVE_STREAMS.lock().await;

    if let Some(input_info) = state_guard.get_mut(&input_id) {
        // Try to remove from active outputs first
        if let Some(output_info) = input_info.output_tasks.remove(&output_id) {
            // Abortar la tarea de envío
            if let Some(handle) = output_info.abort_handle {
                handle.abort();
            }
            // Decrement active outputs counter
            metrics::decrement_active_outputs();

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
        } else if input_info.stopped_outputs.remove(&output_id).is_some() {
            // Output was stopped, remove from stopped_outputs
            // Remove from database
            if let Err(e) = database::delete_output_from_db(&state.pool, output_id).await {
                error!("Error deleting output from database: {}", e);
                // Continue anyway - we already removed from memory
            }

            info!("Output [{}] (stopped) eliminado para Input '{}'", output_id, input_id);
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

#[actix_web::patch("/outputs/{id}")]
pub async fn update_output(
    state: web::Data<AppState>,
    path: web::Path<i64>,
    req: web::Json<UpdateOutputRequest>,
) -> ActixResult<impl Responder> {
    let output_id = path.into_inner();

    info!("Request to update output {}: {:?}", output_id, req);

    // Get current output from database
    let current_output_row = match database::get_output_by_id(&state.pool, output_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().body(format!("Output con ID '{}' no encontrado", output_id)));
        }
        Err(e) => {
            error!("Error fetching output from database: {}", e);
            return Err(ErrorInternalServerError(format!("Database error: {e}")));
        }
    };

    let input_id = current_output_row.input_id;

    // Deserialize current configuration
    let current_config: CreateOutputRequest = match &current_output_row.config_json {
        Some(json) => match serde_json::from_str(json) {
            Ok(config) => config,
            Err(e) => {
                error!("Error deserializing output config: {}", e);
                return Err(ErrorInternalServerError(format!("Config deserialization error: {e}")));
            }
        },
        None => {
            error!("Output {} has no config_json", output_id);
            return Err(ErrorInternalServerError("Output configuration not found"));
        }
    };

    // Merge update request with current configuration
    let updated_config = req.merge_with(&current_config, input_id);

    // Validate the updated configuration
    if let Err(validation_error) = validate_udp_output_config(&updated_config) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }
    if let Err(validation_error) = validate_srt_output_config(&updated_config) {
        return Err(actix_web::error::ErrorBadRequest(validation_error));
    }

    // Build new destination string
    let (destination_addr, listen_port) = match &updated_config {
        CreateOutputRequest::Udp { .. } => {
            let host = updated_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
            let port = updated_config.get_remote_port().unwrap_or(8000);
            (format!("{}:{}", host, port), None)
        },
        CreateOutputRequest::Srt { config, .. } => {
            match config {
                SrtOutputConfig::Caller { .. } => {
                    let host = config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), None)
                },
                SrtOutputConfig::Listener { .. } => {
                    let port = config.get_bind_port().unwrap_or(8000);
                    (format!(":{}", port), Some(port))
                },
            }
        },
    };

    // Check for port conflicts if port changed (for SRT listeners)
    if let Some(new_port) = listen_port {
        let old_port = current_output_row.listen_port.map(|p| p as u16);
        if old_port != Some(new_port) {
            match check_port_conflict(&state.pool, new_port, Some(output_id)).await {
                Ok(conflict) if conflict => {
                    return Ok(HttpResponse::Conflict().body(format!(
                        "Puerto {} ya está en uso", new_port
                    )));
                }
                Ok(_) => {},
                Err(e) => {
                    error!("Error checking port conflict: {}", e);
                    return Err(ErrorInternalServerError(format!("Database error: {e}")));
                }
            }
        }
    }

    // Update configuration in database
    if let Err(e) = database::update_output_config_in_db(&state.pool, output_id, &updated_config, &destination_addr, listen_port).await {
        error!("Error updating output config in database: {}", e);
        return Err(ErrorInternalServerError(format!("Database error: {e}")));
    }

    // Get lock on active streams
    let mut state_guard = ACTIVE_STREAMS.lock().await;

    // Check if input exists
    let input_info = match state_guard.get_mut(&input_id) {
        Some(info) => info,
        None => {
            info!("Output {} updated in database, but input {} is not active", output_id, input_id);
            return Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Output actualizado (input no está activo)",
                "output_id": output_id,
                "input_id": input_id
            })));
        }
    };

    // If output is currently active, restart it with new configuration
    if let Some(mut output_info) = input_info.output_tasks.remove(&output_id) {
        info!("Restarting output {} with new configuration", output_id);

        // Stop current output task
        if let Some(handle) = output_info.abort_handle.take() {
            handle.abort();
        }

        // Spawn new output with updated configuration
        let updated_name = match &updated_config {
            CreateOutputRequest::Udp { name, .. } => name.clone(),
            CreateOutputRequest::Srt { name, .. } => name.clone(),
        }.or(current_output_row.name);

        // Recreate the output
        let recreated_output = match &updated_config {
            CreateOutputRequest::Udp { host, multicast_ttl, multicast_interface, remote_host, .. } => {
                // Check if destination is multicast and create config
                let multicast_config = if let Some(ref host) = remote_host {
                    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
                        if addr.is_multicast() {
                            Some(crate::udp_stream::MulticastOutputConfig {
                                ttl: multicast_ttl.unwrap_or(1),
                                interface: multicast_interface.clone(),
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                create_udp_output(
                    destination_addr.clone(),
                    input_info,
                    output_id,
                    updated_name.clone(),
                    host.clone(),
                    multicast_config,
                    get_state_change_sender().await
                ).await
            },
            CreateOutputRequest::Srt { config, .. } => {
                create_srt_output(
                    input_id,
                    config.clone(),
                    input_info,
                    output_id,
                    updated_name.clone(),
                    get_state_change_sender().await
                )
            }
        };

        match recreated_output {
            Ok(new_output_info) => {
                info!("Output {} restarted successfully", output_id);
                input_info.output_tasks.insert(output_id, new_output_info);

                Ok(HttpResponse::Ok().json(serde_json::json!({
                    "message": "Output actualizado y reiniciado",
                    "output_id": output_id,
                    "input_id": input_id,
                    "destination": destination_addr
                })))
            }
            Err(e) => {
                error!("Error restarting output {}: {}", output_id, e);

                // Try to restore original output
                output_info.config = current_config;
                input_info.output_tasks.insert(output_id, output_info);

                Err(ErrorInternalServerError(format!("Failed to restart output: {e}")))
            }
        }
    } else if input_info.stopped_outputs.contains_key(&output_id) {
        // Output is stopped, just update the stopped config
        input_info.stopped_outputs.insert(output_id, updated_config);

        info!("Stopped output {} configuration updated", output_id);

        Ok(HttpResponse::Ok().json(serde_json::json!({
            "message": "Output actualizado (no estaba activo)",
            "output_id": output_id,
            "input_id": input_id
        })))
    } else {
        Ok(HttpResponse::NotFound().body(format!("Output con ID '{}' no encontrado para Input '{}'", output_id, input_id)))
    }
}

// --- Endpoint para listar Inputs y sus Outputs ---
#[actix_web::get("/status")]
pub async fn get_status(_state: web::Data<AppState>) -> ActixResult<impl Responder> {
    let state_guard = ACTIVE_STREAMS.lock().await;
    let mut response: Vec<InputResponse> = Vec::new();

    for (input_id, input_info) in state_guard.iter() {
        let mut outputs_resp: Vec<OutputResponse> = Vec::new();

        // Add active outputs
        for (output_id, output_info) in input_info.output_tasks.iter() {
             let o_type = output_kind_string(&output_info.kind);

            // Extract bitrate from output stats
            let bitrate_bps = if let Some(stats) = output_info.stats.read().await.as_ref() {
                match stats {
                    InputStats::Udp(udp_stats) => Some(udp_stats.bitrate_bps),
                    InputStats::Srt(srt_stats) => {
                        // For outputs, use send rate instead of recv rate
                        Some((srt_stats.mbpsSendRate * 1_000_000.0) as u64)
                    }
                }
            } else {
                None
            };

            outputs_resp.push(OutputResponse {
                id: *output_id,
                name: output_info.name.clone(),
                input_id: *input_id,
                destination: output_info.destination.clone(),
                output_type: o_type.to_string(),
                status: output_info.status.to_string(),
                assigned_port: output_info.config.extract_assigned_port(),
                uptime_seconds: calculate_connection_uptime(&output_info.status, output_info.connected_at),
                peer_address: output_info.peer_address.clone(),
                bitrate_bps,
            });
        }

        // Add stopped outputs
        for (output_id, output_config) in input_info.stopped_outputs.iter() {
            let (destination, output_type) = match output_config {
                CreateOutputRequest::Udp { .. } => {
                    let host = output_config.get_remote_host().unwrap_or_else(|| "127.0.0.1".to_string());
                    let port = output_config.get_remote_port().unwrap_or(8000);
                    (format!("{}:{}", host, port), "udp")
                },
                CreateOutputRequest::Srt { config, .. } => {
                    let kind_str = match config {
                        SrtOutputConfig::Caller { .. } => "srt_caller",
                        SrtOutputConfig::Listener { .. } => "srt_listener",
                    };
                    let dest = match config {
                        SrtOutputConfig::Caller { .. } => {
                            let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                            let port = config.get_remote_port().unwrap_or(0);
                            format!("{}:{}", host, port)
                        },
                        SrtOutputConfig::Listener { .. } => {
                            let port = config.get_bind_port().unwrap_or(0);
                            format!(":{}", port)
                        },
                    };
                    (dest, kind_str)
                },
            };

            let name = match output_config {
                CreateOutputRequest::Udp { name, .. } => name.clone(),
                CreateOutputRequest::Srt { name, .. } => name.clone(),
            };

            outputs_resp.push(OutputResponse {
                id: *output_id,
                name,
                input_id: *input_id,
                destination,
                output_type: output_type.to_string(),
                status: "stopped".to_string(),
                assigned_port: output_config.extract_assigned_port(),
                uptime_seconds: None,
                peer_address: None,
                bitrate_bps: None, // No bitrate for stopped outputs
            });
        }

        // Extract bitrate from stats
        let bitrate_bps = if let Some(stats) = input_info.stats.read().await.as_ref() {
            match stats {
                InputStats::Udp(udp_stats) => Some(udp_stats.bitrate_bps),
                InputStats::Srt(srt_stats) => {
                    // SRT stats provide mbpsRecvRate, convert to bps
                    Some((srt_stats.mbpsRecvRate * 1_000_000.0) as u64)
                }
            }
        } else {
            None
        };

        response.push(InputResponse {
            id: *input_id,
            name: input_info.name.clone(),
            status: input_info.status.to_string(),
            assigned_port: input_info.config.extract_assigned_port(),
            outputs: outputs_resp,
            uptime_seconds: calculate_connection_uptime(&input_info.status, input_info.connected_at),
            source_address: input_info.source_address.clone(),
            bitrate_bps,
        });
    }

    Ok(HttpResponse::Ok().json(response))
}

// Helper function to calculate uptime from connected_at timestamp
fn calculate_uptime_seconds(connected_at: Option<SystemTime>) -> Option<u64> {
    connected_at.and_then(|connect_time| {
        SystemTime::now()
            .duration_since(connect_time)
            .ok()
            .map(|duration| duration.as_secs())
    })
}

// Helper function to calculate connection uptime only for connected streams
fn calculate_connection_uptime(status: &StreamStatus, connected_at: Option<SystemTime>) -> Option<u64> {
    if status.is_connected() {
        calculate_uptime_seconds(connected_at)
    } else {
        None
    }
}

// Helper function to get global state change sender
async fn get_state_change_sender() -> Option<StateChangeSender> {
    STATE_CHANGE_TX.lock().await.clone()
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

#[actix_web::get("/outputs/{id}/stats")]
async fn output_stats(
    _state: web::Data<AppState>,
    path:  web::Path<i64>,
) -> impl Responder {
    let output_id = path.into_inner();
    let state_guard = ACTIVE_STREAMS.lock().await;

    // Search for the output across all inputs
    for (_, input_info) in state_guard.iter() {
        if let Some(output_info) = input_info.output_tasks.get(&output_id) {
            if let Some(stats) = output_info.stats.read().await.clone() {
                println!("Stats obtenidos para output '{}': {:?}", output_id, stats);
                return HttpResponse::Ok().json(stats);
            } else {
                println!("No hay stats disponibles aún para output '{}'", output_id);
                return HttpResponse::NoContent().finish();                
            }
            
        }
    }
    /*let id = path.into_inner();
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
    */
    println!("Input '{}' no encontrado al solicitar stats", output_id);
    HttpResponse::NotFound().body("Output no encontrado")
}


pub async fn load_from_db(state: &AppState) -> anyhow::Result<()> {
    println!("Cargando inputs desde la base de datos...");
    let rows = database::get_all_inputs(&state.pool).await?;

    // Process inputs outside of the mutex lock first
    let mut loaded_inputs = HashMap::new();

    // Separate inputs into base inputs (UDP, SRT) and SPTS inputs
    let mut base_inputs = Vec::new();
    let mut spts_inputs = Vec::new();

    for r in &rows {
        if r.kind == "spts" {
            spts_inputs.push(r);
        } else {
            base_inputs.push(r);
        }
    }

    // Phase 1: Load base inputs first (UDP, SRT)
    println!("Phase 1: Loading {} base inputs (UDP, SRT)", base_inputs.len());
    for r in base_inputs {
        println!("Procesando input ID {} de tipo {} con status {}", r.id, r.kind, r.status);

        // Solo recrear inputs que están activos (not stopped)
        // Para inputs "stopped", crear InputInfo en estado stopped
        if r.status == "stopped" {
            println!("Input {} está en status '{}', creando entrada stopped en memoria", r.id, r.status);

            // Recrear CreateInputRequest para store en memoria
            let create_req = match r.kind.as_str() {
                "udp" => {
                    // Simply deserialize the stored configuration - it already contains the correct port
                    serde_json::from_str(&r.config_json)?
                },
                "srt_listener" | "srt_caller" => CreateInputRequest::Srt {
                    name: r.name.clone(),
                    config: serde_json::from_str(&r.config_json)?
                },
                "spts" => {
                    // Deserialize SPTS configuration
                    serde_json::from_str::<CreateInputRequest>(&r.config_json)?
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
                name: r.name.clone(),
                status: StreamStatus::Stopped,
                packet_tx: tx,
                stats: Arc::new(RwLock::new(None)),
                task_handle: None, // No task when stopped
                config: create_req,
                output_tasks: HashMap::new(),
                stopped_outputs: HashMap::new(),
                analysis_tasks: HashMap::new(),
                paused_analysis: Vec::new(),
                started_at: None, // Not started when stopped
                connected_at: None, // Not connected when stopped
                state_tx: None, // No state channel for stopped streams
                source_address: None, // No source address when stopped
            };

            loaded_inputs.insert(r.id, stopped_input_info);
            continue;
        }

        // recrear el Input según su tipo
        let create_req = match r.kind.as_str() {
            "udp" => {
                println!("Configuración UDP: {}", r.config_json);
                // Simply deserialize the stored configuration - it already contains the correct port
                serde_json::from_str(&r.config_json)?
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

        match spawn_input(create_req, r.id, r.name.clone(), None).await {
            Ok(info) => {
                println!("Input {} recreado exitosamente", r.id);
                loaded_inputs.insert(r.id, info);
            }
            Err(e) => {
                error!("Error recreating input {}: {}", r.id, e);
            }
        }
    }

    // Phase 2: Load SPTS inputs (after their source inputs exist)
    println!("Phase 2: Loading {} SPTS inputs", spts_inputs.len());
    for r in spts_inputs {
        println!("Procesando SPTS input ID {} con status {}", r.id, r.status);

        // For stopped SPTS inputs, create InputInfo in stopped state
        if r.status == "stopped" {
            println!("SPTS Input {} está en status '{}', creando entrada stopped en memoria", r.id, r.status);

            let create_req: CreateInputRequest = serde_json::from_str(&r.config_json)?;

            // Crear InputInfo en estado stopped
            let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
            let stopped_input_info = InputInfo {
                id: r.id,
                name: r.name.clone(),
                status: StreamStatus::Stopped,
                packet_tx: tx,
                stats: Arc::new(RwLock::new(None)),
                task_handle: None,
                config: create_req,
                output_tasks: HashMap::new(),
                stopped_outputs: HashMap::new(),
                analysis_tasks: HashMap::new(),
                paused_analysis: Vec::new(),
                started_at: None,
                connected_at: None,
                state_tx: None,
                source_address: None,
            };

            loaded_inputs.insert(r.id, stopped_input_info);
            continue;
        }

        // For active SPTS inputs, verify source exists and spawn
        let create_req: CreateInputRequest = serde_json::from_str(&r.config_json)?;

        // Check if source input exists
        if let CreateInputRequest::Spts { source_input_id, .. } = &create_req {
            if !loaded_inputs.contains_key(source_input_id) {
                error!("Source input {} not found for SPTS input {}. Creating in stopped state.",
                       source_input_id, r.id);

                // Create in stopped state
                let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                let stopped_input_info = InputInfo {
                    id: r.id,
                    name: r.name.clone(),
                    status: StreamStatus::Stopped,
                    packet_tx: tx,
                    stats: Arc::new(RwLock::new(None)),
                    task_handle: None,
                    config: create_req,
                    output_tasks: HashMap::new(),
                    stopped_outputs: HashMap::new(),
                    analysis_tasks: HashMap::new(),
                    paused_analysis: Vec::new(),
                    started_at: None,
                    connected_at: None,
                    state_tx: None,
                    source_address: None,
                };

                loaded_inputs.insert(r.id, stopped_input_info);
                continue;
            }
        }

        // Spawn SPTS input
        match spawn_input(create_req, r.id, r.name.clone(), None).await {
            Ok(info) => {
                println!("SPTS Input {} recreado exitosamente", r.id);
                loaded_inputs.insert(r.id, info);
            }
            Err(e) => {
                error!("Error recreating SPTS input {}: {}", r.id, e);
            }
        }
    }

    println!("Cargando outputs desde la base de datos...");
    
    let outs = database::get_all_outputs(&state.pool).await?;
    let mut outputs_by_input: HashMap<i64, Vec<OutputRow>> = HashMap::new();
    
    println!("Recreando outputs para inputs cargados...");
    // Group outputs by input_id
    for o in outs {
        outputs_by_input.entry(o.input_id).or_default().push(o);
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

                // Solo recrear outputs que están activos (not stopped)
                if o.status == "stopped" {
                    println!("Output {} está en status '{}', agregando a stopped_outputs", o.id, o.status);

                    // Recrear CreateOutputRequest para store en stopped_outputs
                    let output_config = match o.kind.as_str() {
                        "udp" => CreateOutputRequest::Udp {
                            input_id,
                            remote_host: None,
                            remote_port: None,
                            automatic_port: None,
                            name: o.name.clone(),
                            bind_host: None,
                            multicast_ttl: None,
                            multicast_interface: None,
                            destination_addr: Some(destination.clone()), // Legacy compatibility
                        },
                        "srt_caller" => {
                            let config_json = o.config_json.unwrap_or_default();
                            let config: SrtOutputConfig = serde_json::from_str(&config_json)
                                .unwrap_or(SrtOutputConfig::Caller {
                                    remote_host: None,
                                    remote_port: None,
                                    bind_host: None,
                                    destination_addr: Some(destination.clone()), // Legacy compatibility
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
                                    bind_host: None,
                                    bind_port: None,
                                    automatic_port: None,
                                    listen_port: Some(o.listen_port.unwrap_or(8000)), // Legacy compatibility
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
                            match create_udp_output(destination.clone(), input, o.id, o.name.clone(), None, None, get_state_change_sender().await).await {
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
                        let output_config = SrtOutputConfig::Caller {
                            remote_host: None,
                            remote_port: None,
                            bind_host: None,
                            destination_addr: Some(destination.clone()), // Legacy compatibility
                            common: cfg
                        };
                        match create_srt_output(input_id, output_config, input, o.id, o.name.clone(), get_state_change_sender().await) {
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

                        let output_config = SrtOutputConfig::Listener {
                            bind_host: None,
                            bind_port: None,
                            automatic_port: None,
                            listen_port: Some(listen_port), // Legacy compatibility
                            common: cfg
                        };

                        match create_srt_output(input_id, output_config, input, o.id, o.name.clone(), get_state_change_sender().await) {
                            Ok(output_info) => {
                                input.output_tasks.insert(o.id, output_info);
                                Ok(())
                            }
                            Err(e) => Err(anyhow::anyhow!("Error recreating SRT Listener output: {}", e))
                        }
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
        CreateInputRequest::Spts { name, .. } => name.clone(),
    }
}

fn generate_input_name(req: &CreateInputRequest) -> Option<String> {
    match req {
        CreateInputRequest::Udp { .. } => {
            let port = req.get_bind_port();
            Some(format!("UDP Listener {port}"))
        }
        CreateInputRequest::Srt { config, .. } => {
            let name = match config {
                SrtInputConfig::Listener { .. } => {
                    let port = config.get_bind_port();
                    format!("SRT Listener {port}")
                },
                SrtInputConfig::Caller { .. } => {
                    let host = config.get_remote_host().unwrap_or_else(|| "unknown".to_string());
                    let port = config.get_remote_port().unwrap_or(0);
                    format!("SRT Caller {}:{}", host, port)
                },
            };
            Some(name)
        }
        CreateInputRequest::Spts { source_input_id, program_number, .. } => {
            Some(format!("SPTS Program {} from Input {}", program_number, source_input_id))
        }
    }
}

async fn spawn_input(req: CreateInputRequest, id: i64, name: Option<String>, _assigned_port: Option<u16>) -> Result<InputInfo, actix_web::Error> {
    match req {
        /* ----------------------------- UDP ----------------------------- */
        CreateInputRequest::Udp { ref multicast_group, ref source_specific_multicast, .. } => {
            let port = req.get_bind_port();
            let bind_host = Some(req.get_bind_host()).filter(|h| h != "0.0.0.0");
            spawn_udp_input_with_stats(
                id,
                name,
                port,
                bind_host,
                multicast_group.clone(),
                source_specific_multicast.clone(),
                get_state_change_sender().await
            )
        }

        /* ----------------------- SRT  (caller o listener) -------------- */
        CreateInputRequest::Srt { ref config, .. } => {
            // Lanza el forwarder (con reconexión automática)
            let state_tx = get_state_change_sender().await;
            let source_with_state = SrtSourceWithState {
                config: config.clone(),
                input_id: id,
                state_tx: state_tx.clone(),
            };
            let sender =
                Forwarder::spawn_with_stats(Box::new(source_with_state), Duration::from_secs(1), id, name.clone(), state_tx);

            println!("Input SRT '{id}' creado");
            Ok(InputInfo {
                id,
                name,
                status: match config {
                    SrtInputConfig::Listener { .. } => StreamStatus::Listening,
                    SrtInputConfig::Caller { .. } => StreamStatus::Connecting,
                },
                packet_tx: sender.tx,
                stats: sender.stats,
                task_handle: Some(sender.handle),
                config: req,
                output_tasks: HashMap::new(),
                stopped_outputs: HashMap::new(),
                analysis_tasks: HashMap::new(),
                paused_analysis: Vec::new(),
                started_at: Some(SystemTime::now()),
                connected_at: None, // Will be set when connection is established
                state_tx: get_state_change_sender().await, // Use global state channel
                source_address: None, // Will be set when SRT listener accepts connection
            })
        }

        /* ----------------------------- SPTS ----------------------------- */
        CreateInputRequest::Spts { source_input_id, program_number, fill_with_nulls, .. } => {
            // Get the source MPTS input
            let streams_guard = ACTIVE_STREAMS.lock().await;
            let source_input = streams_guard.get(&source_input_id)
                .ok_or_else(|| actix_web::error::ErrorBadRequest(
                    format!("Source input {} not found or not active", source_input_id)
                ))?;

            // Create SPTS input that filters from the source
            let result = spawn_spts_input(
                source_input,
                id,
                program_number,
                fill_with_nulls.unwrap_or(false),
                name,
                get_state_change_sender().await,
            );

            // Drop the lock before returning
            drop(streams_guard);

            result
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

#[actix_web::post("/inputs/{id}/analysis/stop")]
pub async fn stop_all_analysis(
    path: web::Path<i64>,
) -> ActixResult<impl Responder> {
    let input_id = path.into_inner();

    info!("Stopping all analysis tasks for input {}", input_id);

    match analysis::stop_all_analysis(input_id).await {
        Ok(()) => {
            info!("All analysis tasks stopped successfully for input {}", input_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "All analysis tasks stopped successfully",
                "input_id": input_id
            })))
        }
        Err(e) => {
            error!("Failed to stop all analysis tasks: {}", e);
            Err(ErrorInternalServerError(format!("Failed to stop all analysis tasks: {}", e)))
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

#[actix_web::get("/inputs/{id}/analysis/{analysis_type}/data")]
pub async fn get_analysis_data(
    path: web::Path<(i64, String)>,
) -> ActixResult<impl Responder> {
    let (input_id, analysis_type_str) = path.into_inner();

    //info!("Getting {} analysis data for input {}", analysis_type_str, input_id);

    // Parse analysis type
    let analysis_type = match analysis_type_str.parse::<AnalysisType>() {
        Ok(t) => t,
        Err(e) => {
            error!("Invalid analysis type '{}': {}", analysis_type_str, e);
            return Err(ErrorInternalServerError(format!("Invalid analysis type: {}", e)));
        }
    };

    // Get the analysis data
    let guard = ACTIVE_STREAMS.lock().await;
    let input_info = guard.get(&input_id)
        .ok_or_else(|| actix_web::error::ErrorNotFound(format!("Input {} not found", input_id)))?;

    // Find the analysis task with the matching type
    let analysis_info = input_info.analysis_tasks.values()
        .find(|analysis| analysis.analysis_type == analysis_type)
        .ok_or_else(|| actix_web::error::ErrorNotFound(
            format!("No active {} analysis found for input {}", analysis_type_str, input_id)
        ))?;

    // Get the report data
    let report_data = analysis_info.report_data.read().await;

    match report_data.as_ref() {
        Some(data) => {
            //info!("Returning {} analysis data for input {}", analysis_type_str, input_id);
            Ok(HttpResponse::Ok().json(data))
        }
        None => {
            //info!("No data available yet for {} analysis on input {}", analysis_type_str, input_id);
            Err(actix_web::error::ErrorNotFound(
                format!("No data available yet for {} analysis on input {}. Analysis may be starting up.", analysis_type_str, input_id)
            ))
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
            // Get the actual status from the input after starting
            let status_str = {
                let guard = ACTIVE_STREAMS.lock().await;
                if let Some(input) = guard.get(&input_id) {
                    input.status.to_string().to_lowercase()
                } else {
                    "listening".to_string() // Default fallback
                }
            };

            // Update database status with the actual stream status
            if let Err(e) = update_input_status_in_db(&state.pool, input_id, &status_str).await {
                error!("Failed to update input status in database: {}", e);
                // Continue anyway - the stream is started in memory
            }

            info!("Input {} started successfully", input_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Input started successfully",
                "input_id": input_id,
                "status": status_str
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
            // Get the actual status from the output after starting
            let status_str = {
                let guard = ACTIVE_STREAMS.lock().await;
                if let Some(input) = guard.get(&input_id) {
                    if let Some(output) = input.output_tasks.get(&output_id) {
                        output.status.to_string().to_lowercase()
                    } else {
                        "connecting".to_string() // Default fallback
                    }
                } else {
                    "connecting".to_string() // Default fallback
                }
            };

            // Update database status with the actual stream status
            if let Err(e) = update_output_status_in_db(&state.pool, output_id, &status_str).await {
                error!("Failed to update output status in database: {}", e);
                // Continue anyway - the stream is started in memory
            }

            info!("Output {} started successfully", output_id);
            Ok(HttpResponse::Ok().json(serde_json::json!({
                "message": "Output started successfully",
                "output_id": output_id,
                "input_id": input_id,
                "status": status_str
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

#[actix_web::get("/metrics")]
pub async fn get_metrics() -> ActixResult<impl Responder> {
    match metrics::get_metrics_text() {
        Ok(metrics_text) => {
            Ok(HttpResponse::Ok()
                .content_type("text/plain; version=0.0.4; charset=utf-8")
                .body(metrics_text))
        }
        Err(e) => {
            error!("Failed to generate metrics: {}", e);
            Err(ErrorInternalServerError("Failed to generate metrics"))
        }
    }
}

#[actix_web::get("/interfaces")]
pub async fn get_interfaces(
    query: web::Query<InterfaceQueryParams>
) -> ActixResult<impl Responder> {
    let interfaces = interfaces::get_filtered_interfaces(
        query.only_up,
        query.exclude_loopback,
        query.ipv4_only,
    ).map_err(|e| ErrorInternalServerError(format!("Failed to enumerate interfaces: {}", e)))?;

    let response = interfaces::InterfacesResponse { interfaces };
    Ok(HttpResponse::Ok().json(response))
}

#[derive(serde::Deserialize)]
pub struct InterfaceQueryParams {
    only_up: Option<bool>,
    exclude_loopback: Option<bool>,
    ipv4_only: Option<bool>,
}