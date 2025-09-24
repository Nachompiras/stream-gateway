use std::net::{TcpListener, UdpSocket};
use std::collections::HashSet;
use anyhow::Result;
use crate::config::CONFIG;
use crate::database::check_port_conflict;
use sqlx::SqlitePool;

/// Check if a port is available for use by attempting to bind to it
fn is_port_available(port: u16) -> bool {
    // Check if port can be bound for TCP (for SRT listener compatibility)
    if let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) {
        drop(listener);
    } else {
        return false;
    }

    // Check if port can be bound for UDP (for UDP outputs compatibility)
    if let Ok(socket) = UdpSocket::bind(("0.0.0.0", port)) {
        drop(socket);
    } else {
        return false;
    }

    true
}

/// Find an available port in the configured range, excluding any ports already in use in the database
pub async fn find_available_port(pool: &SqlitePool, exclude_ids: Option<Vec<i64>>) -> Result<u16> {
    let port_range = CONFIG.port_range();

    // Get list of ports already in use from database
    let mut used_ports = HashSet::new();

    // Query database for ports currently in use
    let rows: Vec<(Option<i32>,)> = sqlx::query_as("SELECT listen_port FROM outputs WHERE listen_port IS NOT NULL")
        .fetch_all(pool)
        .await?;

    for (port_opt,) in rows {
        if let Some(port) = port_opt {
            used_ports.insert(port as u16);
        }
    }

    // Also query inputs for any bound ports (UDP listeners, SRT listeners)
    let input_rows: Vec<(String,)> = sqlx::query_as("SELECT config_json FROM inputs")
        .fetch_all(pool)
        .await?;

    for (config_json,) in input_rows {
        if let Ok(config_value) = serde_json::from_str::<serde_json::Value>(&config_json) {
            // Check for legacy listen_port field
            if let Some(port) = config_value.get("listen_port").and_then(|p| p.as_u64()) {
                used_ports.insert(port as u16);
            }
            // Check for new bind_port field
            if let Some(port) = config_value.get("bind_port").and_then(|p| p.as_u64()) {
                used_ports.insert(port as u16);
            }
        }
    }

    // Look for an available port in the configured range
    for port in port_range {
        // Skip if port is in database
        if used_ports.contains(&port) {
            continue;
        }

        // Check if port conflicts in database (double check with exclusions)
        let exclude_list = exclude_ids.clone().unwrap_or_default();
        if exclude_list.is_empty() {
            if check_port_conflict(pool, port, None).await? {
                continue;
            }
        } else {
            // If we have exclusions, check each one
            let mut has_conflict = false;
            for &exclude_id in &exclude_list {
                if check_port_conflict(pool, port, Some(exclude_id)).await? {
                    has_conflict = true;
                    break;
                }
            }
            if has_conflict {
                continue;
            }
        }

        // Check if port is actually available on the system
        if is_port_available(port) {
            return Ok(port);
        }
    }

    anyhow::bail!("No available ports in range {}..{}", CONFIG.auto_port_range_start, CONFIG.auto_port_range_end)
}

/// Check if automatic port assignment is requested for inputs
pub fn should_use_auto_port_input(bind_port: Option<u16>, legacy_listen_port: Option<u16>) -> bool {
    // If both are None or both are Some(0), use automatic port
    match (bind_port, legacy_listen_port) {
        (None, None) => true,                    // No port specified at all
        (Some(0), _) => true,                    // Explicitly requested auto with new field
        (None, Some(0)) => true,                 // Explicitly requested auto with legacy field
        (Some(_), None) => false,                // Explicit port with new field
        (None, Some(_)) => false,                // Explicit port with legacy field
        (Some(_), Some(_)) => false,             // Both specified (should use legacy for compatibility)
    }
}

/// Check if automatic port assignment is requested for outputs
pub fn should_use_auto_port_output(remote_port: Option<u16>, bind_port: Option<u16>, legacy_addr: Option<&String>) -> bool {
    // For outputs, we only auto-assign for SRT listeners (bind_port) or UDP outputs (remote_port)

    // Handle legacy destination_addr format first
    if let Some(addr) = legacy_addr {
        if addr == "auto" || addr.ends_with(":0") || addr == ":0" {
            return true;
        }
    }

    // Check new field format
    match (remote_port, bind_port) {
        (Some(0), _) => true,                    // UDP output with auto remote port
        (_, Some(0)) => true,                    // SRT listener output with auto bind port
        (None, None) => true,                    // No ports specified
        _ => false,                              // Explicit ports specified
    }
}