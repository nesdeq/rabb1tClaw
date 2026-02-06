//! Native YAML-based configuration for the Rust gateway.
//!
//! Config location: ~/.rabb1tclaw/config.yaml

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

// ============================================================================
// Paths
// ============================================================================

const CONFIG_DIR: &str = ".rabb1tclaw";
const CONFIG_FILE: &str = "config.yaml";
const DEVICES_FILE: &str = "devices.yaml";

pub fn config_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(CONFIG_DIR))
        .unwrap_or_else(|| PathBuf::from(CONFIG_DIR))
}

pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

pub fn devices_path() -> PathBuf {
    config_dir().join(DEVICES_FILE)
}

// ============================================================================
// Config Types
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Gateway settings
    #[serde(default)]
    pub gateway: GatewaySettings,

    /// LLM providers (keyed by provider name)
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Active provider name
    #[serde(default)]
    pub active_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySettings {
    /// Port to listen on (default: 18789)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Bind IP address (e.g., "127.0.0.1", "0.0.0.0")
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_port() -> u16 {
    18789
}

fn default_bind() -> String {
    "127.0.0.1".to_string()
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind: default_bind(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// API type: "openai" or "anthropic"
    pub api: String,

    /// Base URL for the API
    pub base_url: String,

    /// API key
    pub api_key: String,

    /// Model to use
    pub model: String,

    /// Optional display name
    pub name: Option<String>,
}

// ============================================================================
// Config Loading/Saving
// ============================================================================

/// Check if config exists
pub fn config_exists() -> bool {
    config_path().exists()
}

/// Load config
pub fn load_config() -> Result<GatewayConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(GatewayConfig::default());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {:?}", path))?;

    let config: GatewayConfig = serde_yml::from_str(&content)
        .with_context(|| format!("Failed to parse config from {:?}", path))?;

    Ok(config)
}

/// Write content to a file with 0o600 permissions, creating parent dirs as needed.
pub fn write_secure(path: &std::path::Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create dir {:?}", dir))?;
    }
    fs::write(path, content)
        .with_context(|| format!("Failed to write {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Save config
pub fn save_config(config: &GatewayConfig) -> Result<()> {
    let path = config_path();
    let content = serde_yml::to_string(config).context("Failed to serialize config")?;
    write_secure(&path, &content)?;
    info!("config saved");
    Ok(())
}

// ============================================================================
// PID File
// ============================================================================

/// Path to the server PID file
pub fn pid_path() -> PathBuf {
    config_dir().join("server.pid")
}

/// Write current process ID to PID file
pub fn write_pid_file() -> Result<()> {
    let path = pid_path();
    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {:?}", dir))?;
    fs::write(&path, std::process::id().to_string())
        .with_context(|| format!("Failed to write PID file {:?}", path))?;
    Ok(())
}

/// Remove PID file
pub fn remove_pid_file() {
    let _ = fs::remove_file(pid_path());
}

/// Read PID from PID file
pub fn read_pid_file() -> Option<u32> {
    fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// RAII guard that removes the PID file on drop
pub struct PidGuard;

impl PidGuard {
    pub fn new() -> Result<Self> {
        write_pid_file()?;
        Ok(Self)
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        remove_pid_file();
    }
}
