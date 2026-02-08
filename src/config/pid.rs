//! PID file management for the gateway server process.

use super::native::config_dir;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Path to the server PID file
fn pid_path() -> PathBuf {
    config_dir().join("server.pid")
}

/// Write current process ID to PID file
fn write_pid_file() -> Result<()> {
    let path = pid_path();
    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {:?}", dir))?;
    fs::write(&path, std::process::id().to_string())
        .with_context(|| format!("Failed to write PID file {:?}", path))?;
    Ok(())
}

/// Remove PID file
fn remove_pid_file() {
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
