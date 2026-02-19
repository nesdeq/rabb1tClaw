//! Device management: types, CRUD, QR code, LAN IP.

use super::native::{devices_path, GatewayConfig};
use crate::protocol::now_ms;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

// ============================================================================
// Device Types
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStore {
    #[serde(default)]
    pub devices: HashMap<String, Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Device {
    /// Device ID (hash of public key or random)
    pub device_id: String,

    /// Human-readable name
    pub display_name: String,

    /// Authentication token
    pub token: String,

    /// Whether the token is revoked
    #[serde(default)]
    pub revoked: bool,
}

// ============================================================================
// Device Loading/Saving
// ============================================================================

/// Load device store
pub fn load_devices() -> Result<DeviceStore> {
    let path = devices_path();
    if !path.exists() {
        return Ok(DeviceStore::default());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read devices from {}", path.display()))?;

    let store: DeviceStore = serde_yml::from_str(&content)
        .with_context(|| format!("Failed to parse devices from {}", path.display()))?;

    Ok(store)
}

/// Save device store
pub fn save_devices(store: &DeviceStore) -> Result<()> {
    let path = devices_path();
    let content = serde_yml::to_string(store).context("Failed to serialize devices")?;
    super::native::write_secure(&path, &content)
}

// ============================================================================
// Device Management
// ============================================================================

/// Generate a new device token
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

/// Generate a new device ID
fn generate_device_id() -> String {
    format!("{:x}{}", now_ms(), &generate_token()[..16])
}

/// Create a new device
pub fn create_device(display_name: &str) -> Device {
    Device {
        device_id: generate_device_id(),
        display_name: display_name.to_string(),
        token: generate_token(),
        revoked: false,
    }
}

/// Add a device to the store
pub fn add_device(store: &mut DeviceStore, device: Device) {
    store.devices.insert(device.device_id.clone(), device);
}

/// Revoke a device by ID or token
pub fn revoke_device(store: &mut DeviceStore, id_or_token: &str) -> Option<String> {
    // Try to find by device ID first
    if let Some(device) = store.devices.get_mut(id_or_token) {
        device.revoked = true;
        return Some(device.display_name.clone());
    }

    // Try to find by token
    for device in store.devices.values_mut() {
        if device.token == id_or_token {
            device.revoked = true;
            return Some(device.display_name.clone());
        }
    }

    None
}

// ============================================================================
// QR Code Generation
// ============================================================================

/// Generate connection info as JSON for QR code
pub fn generate_connection_json(config: &GatewayConfig, token: &str) -> String {
    serde_json::json!({
        "type": "clawdbot-gateway",
        "version": 1,
        "ips": get_lan_ips(),
        "port": config.gateway.port,
        "token": token,
        "protocol": "ws"
    }).to_string()
}

/// Get LAN IP addresses (including Tailscale if available)
pub fn get_lan_ips() -> Vec<String> {
    let mut ips = Vec::new();

    // Try Tailscale IP first (100.x.y.z range)
    if let Ok(ts_output) = std::process::Command::new("tailscale")
        .arg("ip")
        .arg("-4")
        .output()
    {
        if ts_output.status.success() {
            if let Ok(ts_ip) = String::from_utf8(ts_output.stdout) {
                let ts_ip = ts_ip.trim();
                if !ts_ip.is_empty() {
                    ips.push(ts_ip.to_string());
                }
            }
        }
    }

    // Try to get network interfaces
    if let Ok(interfaces) = std::process::Command::new("hostname")
        .arg("-I")
        .output()
    {
        if interfaces.status.success() {
            let output = String::from_utf8_lossy(&interfaces.stdout);
            for ip in output.split_whitespace() {
                if !ip.starts_with("127.") && !ip.starts_with("169.254.") && !ip.contains(':') {
                    ips.push(ip.to_string());
                }
            }
        }
    }

    // Fallback: try ip command
    if ips.is_empty() {
        if let Ok(output) = std::process::Command::new("ip")
            .args(["-4", "addr", "show"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    if line.contains("inet ") {
                        if let Some(ip) = line.split_whitespace().nth(1) {
                            let ip = ip.split('/').next().unwrap_or(ip);
                            if !ip.starts_with("127.") && !ip.starts_with("169.254.") {
                                ips.push(ip.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    if ips.is_empty() {
        ips.push("127.0.0.1".to_string());
    }

    ips
}

/// Print QR code to terminal using Unicode block characters
pub fn print_qr_code(data: &str) {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    match QrCode::new(data) {
        Ok(code) => {
            let image = code.render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Light)
                .light_color(unicode::Dense1x2::Dark)
                .quiet_zone(true)
                .build();
            println!("{image}");
        }
        Err(e) => {
            eprintln!("Failed to generate QR code: {e}");
            println!("Connection data:");
            println!("{data}");
        }
    }
}
