//! Hot-reload support for config and device files.
//!
//! Polls files every few seconds and reloads on change.
//! Also supports SIGHUP for immediate reload.

use crate::state::GatewayState;
use super::native::{config_path, devices_path};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;
use tracing::{error, info, warn};

/// Reload coordinator - tracks file mtimes and triggers reloads
pub struct ReloadCoordinator {
    state: Arc<GatewayState>,
    config_mtime: Option<SystemTime>,
    devices_mtime: Option<SystemTime>,
    sighup_notify: Arc<Notify>,
}

impl ReloadCoordinator {
    pub fn new(state: Arc<GatewayState>) -> Self {
        Self {
            state,
            config_mtime: get_mtime(&config_path()),
            devices_mtime: get_mtime(&devices_path()),
            sighup_notify: Arc::new(Notify::new()),
        }
    }

    pub fn sighup_notifier(&self) -> Arc<Notify> {
        self.sighup_notify.clone()
    }

    /// Run the reload loop - polls periodically, also wakes on SIGHUP
    pub async fn run(mut self) {
        let poll_interval = Duration::from_secs(crate::protocol::CONFIG_POLL_SECS);
        info!("reload watcher started ({:?} poll)", poll_interval);

        loop {
            tokio::select! {
                () = tokio::time::sleep(poll_interval) => {}
                () = self.sighup_notify.notified() => {
                    info!("SIGHUP received, forcing reload");
                }
            }

            check_and_reload(
                "config", &config_path(), &mut self.config_mtime,
                || self.state.reload_config(),
            ).await;

            check_and_reload(
                "devices", &devices_path(), &mut self.devices_mtime,
                || self.state.reload_devices(),
            ).await;
        }
    }
}

/// Check if a file has changed and reload if so.
async fn check_and_reload<F, Fut>(
    label: &str,
    path: &std::path::Path,
    cached_mtime: &mut Option<SystemTime>,
    reload_fn: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let new_mtime = get_mtime(path);
    if new_mtime == *cached_mtime {
        return;
    }
    info!("{} file changed, reloading: {:?}", label, path);
    match reload_fn().await {
        Ok(()) => {
            *cached_mtime = new_mtime;
            info!("{} reloaded successfully", label);
        }
        Err(e) => {
            error!("Failed to reload {} (keeping old): {}", label, e);
        }
    }
}

/// Get file modification time, or None if file doesn't exist
fn get_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
}

/// Setup SIGHUP handler (Unix only)
#[cfg(unix)]
pub fn setup_sighup_handler(notify: Arc<Notify>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to setup SIGHUP handler: {}", e);
                return;
            }
        };

        loop {
            sighup.recv().await;
            info!("Received SIGHUP signal");
            notify.notify_one();
        }
    });
}

/// Setup SIGHUP handler (no-op on non-Unix)
#[cfg(not(unix))]
pub fn setup_sighup_handler(_notify: Arc<Notify>) {
    warn!("SIGHUP handler not available on this platform");
}
