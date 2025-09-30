use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub ip: String,
    pub is_loopback: bool,
    pub is_up: bool,
    pub interface_type: String,
}

#[derive(Debug, Serialize)]
pub struct InterfacesResponse {
    pub interfaces: Vec<InterfaceInfo>,
}

/// Get all network interfaces available on the system
pub fn get_system_interfaces() -> Result<Vec<InterfaceInfo>, String> {
    let interfaces = if_addrs::get_if_addrs()
        .map_err(|e| format!("Failed to enumerate network interfaces: {}", e))?;

    let mut result = Vec::new();

    for interface in interfaces {

        let interface_type = if interface.is_loopback() {
            "loopback".to_string()
        } else {
            match &interface.addr {
                if_addrs::IfAddr::V4(_) => "ethernet".to_string(),
                if_addrs::IfAddr::V6(_) => "ethernet".to_string(),
            }
        };

        let ip_addr = match &interface.addr {
            if_addrs::IfAddr::V4(v4_addr) => IpAddr::V4(v4_addr.ip),
            if_addrs::IfAddr::V6(v6_addr) => IpAddr::V6(v6_addr.ip),
        };

        let is_loopback = interface.is_loopback();
        let name = interface.name;

        result.push(InterfaceInfo {
            name,
            ip: ip_addr.to_string(),
            is_loopback,
            is_up: true, // Assume all enumerated interfaces are up
            interface_type,
        });
    }

    Ok(result)
}

/// Check if the given IP address exists on any system interface
pub fn is_valid_bind_address(bind_addr: &str) -> bool {
    // Special cases that are always valid
    if bind_addr == "0.0.0.0" || bind_addr == "::" {
        return true;
    }

    // Parse the address
    let target_addr = match bind_addr.parse::<IpAddr>() {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    // Get system interfaces
    let interfaces = match get_system_interfaces() {
        Ok(interfaces) => interfaces,
        Err(_) => return false,
    };

    // Check if any interface matches the target address
    for interface in interfaces {
        if let Ok(interface_addr) = interface.ip.parse::<IpAddr>() {
            if interface_addr == target_addr {
                return true;
            }
        }
    }

    false
}

/// Get available interfaces with optional filters
pub fn get_filtered_interfaces(
    only_up: Option<bool>,
    exclude_loopback: Option<bool>,
    ipv4_only: Option<bool>,
) -> Result<Vec<InterfaceInfo>, String> {
    let mut interfaces = get_system_interfaces()?;

    // Apply filters
    if let Some(true) = exclude_loopback {
        interfaces.retain(|iface| !iface.is_loopback);
    }

    if let Some(true) = ipv4_only {
        interfaces.retain(|iface| {
            iface.ip.parse::<std::net::Ipv4Addr>().is_ok()
        });
    }

    if let Some(true) = only_up {
        interfaces.retain(|iface| iface.is_up);
    }

    Ok(interfaces)
}

/// Get a list of available bind addresses as strings
pub fn get_available_bind_addresses() -> Result<Vec<String>, String> {
    let interfaces = get_system_interfaces()?;
    let mut addresses = vec!["0.0.0.0".to_string(), "::".to_string()]; // Always include wildcard addresses

    for interface in interfaces {
        addresses.push(interface.ip);
    }

    // Remove duplicates and sort
    addresses.sort();
    addresses.dedup();

    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wildcard_addresses_are_valid() {
        assert!(is_valid_bind_address("0.0.0.0"));
        assert!(is_valid_bind_address("::"));
    }

    #[test]
    fn test_invalid_address_format() {
        assert!(!is_valid_bind_address("invalid"));
        assert!(!is_valid_bind_address("999.999.999.999"));
    }

    #[test]
    fn test_get_system_interfaces() {
        // This test will depend on the system it runs on
        let interfaces = get_system_interfaces();
        assert!(interfaces.is_ok());

        let interfaces = interfaces.unwrap();
        assert!(!interfaces.is_empty()); // Every system should have at least loopback

        // Should have at least one loopback interface
        assert!(interfaces.iter().any(|i| i.is_loopback));
    }

    #[test]
    fn test_localhost_is_valid() {
        // 127.0.0.1 should be available on all systems
        assert!(is_valid_bind_address("127.0.0.1"));
    }
}