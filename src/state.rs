//! Shared gateway state and handler context.

use crate::agent::advanced::AdvancedTaskTracker;
use crate::agent::code::CodeTaskTracker;
use crate::agent::search::SearchQueryTracker;
use crate::agent::session::SessionManager;
use crate::config::{DeviceStore, GatewayConfig};
use crate::protocol::{now_ms, ErrorShape, OutgoingFrame, ResponseFrame};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// Optional debug protocol log — every WS frame written here when `--debug` is active.
pub type DebugLog = Arc<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>;

/// Write a timestamped debug line to a protocol log file.
pub fn write_debug_line(log: &DebugLog, direction: &str, conn_id: &str, data: &str) {
    use std::io::Write;
    let now = chrono::Local::now().format("%H:%M:%S%.3f");
    if let Ok(mut w) = log.lock() {
        let _ = writeln!(w, "{now} {direction} [{conn_id}] {data}");
        let _ = w.flush();
    }
}

// ============================================================================
// Active Connection Tracking
// ============================================================================

/// Info about an active WebSocket connection
#[derive(Clone)]
pub struct ActiveConnection {
    pub conn_id: String,
    pub tx: mpsc::Sender<OutgoingFrame>,
    /// Set to true to force-close the connection
    pub shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Notify to wake the receive loop on shutdown
    pub shutdown_notify: Arc<tokio::sync::Notify>,
}

/// Registry of active connections by device token
pub type ConnectionRegistry = HashMap<String, Vec<ActiveConnection>>;

// ============================================================================
// State
// ============================================================================

/// Shared gateway state
pub struct GatewayState {
    pub started_at: u64,
    /// Active agent requests by `idempotency_key` (for request deduplication)
    pub active_requests: RwLock<HashMap<String, Arc<tokio::sync::Notify>>>,
    /// Gateway config - wrapped in `RwLock` for hot-reload
    pub gateway_config: RwLock<GatewayConfig>,
    /// Native device store - wrapped in `RwLock` for hot-reload
    pub device_store: RwLock<DeviceStore>,
    /// Active WebSocket connections by device token (for immediate revocation)
    pub active_connections: RwLock<ConnectionRegistry>,
    /// Conversation session manager
    pub session_manager: SessionManager,
    /// Background code task tracker
    pub code_task_tracker: Arc<CodeTaskTracker>,
    /// Background web search query tracker
    pub search_query_tracker: Arc<SearchQueryTracker>,
    /// Background advanced task tracker
    pub advanced_task_tracker: Arc<AdvancedTaskTracker>,
    /// Pending questions from advanced tasks awaiting user answers
    pub advanced_questions: RwLock<Vec<crate::agent::advanced::PendingQuestion>>,
    /// Monotonic task ID counter (shared across all agent types)
    pub next_task_id: AtomicU32,
    /// Optional debug protocol log (active when server started with `--debug`)
    pub debug_log: Option<DebugLog>,
}

impl GatewayState {
    pub fn new(config: GatewayConfig, device_store: DeviceStore, debug_log: Option<DebugLog>) -> Self {
        Self {
            started_at: now_ms(),
            active_requests: RwLock::new(HashMap::new()),
            gateway_config: RwLock::new(config),
            device_store: RwLock::new(device_store),
            active_connections: RwLock::new(HashMap::new()),
            session_manager: SessionManager::new(),
            code_task_tracker: Arc::new(CodeTaskTracker::new()),
            search_query_tracker: Arc::new(SearchQueryTracker::new()),
            advanced_task_tracker: Arc::new(AdvancedTaskTracker::new()),
            advanced_questions: RwLock::new(Vec::new()),
            next_task_id: AtomicU32::new(1),
            debug_log,
        }
    }

    /// Get the next monotonic task ID.
    pub fn next_id(&self) -> u32 {
        self.next_task_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register an active connection for a device token
    pub async fn register_connection(
        &self,
        token: String,
        conn_id: String,
        tx: mpsc::Sender<OutgoingFrame>,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
        shutdown_notify: Arc<tokio::sync::Notify>,
    ) {
        let conn = ActiveConnection { conn_id, tx, shutdown, shutdown_notify };
        let mut connections = self.active_connections.write().await;
        connections.entry(token).or_default().push(conn);
    }

    /// Unregister a connection when it closes
    pub async fn unregister_connection(&self, token: &str, conn_id: &str) {
        let mut connections = self.active_connections.write().await;
        if let Some(conns) = connections.get_mut(token) {
            conns.retain(|c| c.conn_id != conn_id);
            if conns.is_empty() {
                connections.remove(token);
            }
        }
    }

    /// Reload config from disk
    pub async fn reload_config(&self) -> Result<()> {
        let new_config = crate::config::load_config()?;
        *self.gateway_config.write().await = new_config;
        tracing::info!("Config reloaded");
        Ok(())
    }

    /// Reload device store from disk and disconnect revoked devices
    pub async fn reload_devices(&self) -> Result<()> {
        let new_devices = crate::config::load_devices()?;
        let count = new_devices.devices.len();

        // Find newly revoked device tokens
        let old_devices = self.device_store.read().await;
        let mut revoked_tokens: Vec<String> = Vec::new();

        for (device_id, new_device) in &new_devices.devices {
            if new_device.revoked {
                // Check if this is newly revoked (wasn't revoked before)
                if let Some(old_device) = old_devices.devices.get(device_id) {
                    if !old_device.revoked {
                        tracing::info!(
                            device_id = %device_id,
                            device_name = %new_device.display_name,
                            "Device newly revoked, will disconnect active sessions"
                        );
                        revoked_tokens.push(new_device.token.clone());
                    }
                }
            }
        }
        drop(old_devices);

        // Update the device store
        *self.device_store.write().await = new_devices;
        tracing::info!("Device store reloaded ({} devices)", count);

        // Disconnect any active connections for revoked tokens
        if !revoked_tokens.is_empty() {
            self.disconnect_revoked_devices(&revoked_tokens).await;
        }

        Ok(())
    }

    /// Write a timestamped line to the debug protocol log (if active).
    pub fn log_frame(&self, conn_id: &str, direction: &str, data: &str) {
        if let Some(ref log) = self.debug_log {
            write_debug_line(log, direction, conn_id, data);
        }
    }

    /// Disconnect all active connections for the given device tokens
    async fn disconnect_revoked_devices(&self, tokens: &[String]) {
        let connections = self.active_connections.read().await;

        for token in tokens {
            if let Some(conns) = connections.get(token) {
                for conn in conns {
                    tracing::info!(
                        conn_id = %conn.conn_id,
                        "Disconnecting revoked device"
                    );

                    // Signal the receive loop to stop
                    conn.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                    conn.shutdown_notify.notify_one();

                    // Send close frame with policy violation code
                    let _ = conn.tx.send(OutgoingFrame::Close {
                        code: 1008,
                        reason: "device_revoked".to_string(),
                    }).await;
                }
            }
        }
    }
}

// ============================================================================
// Handler Context
// ============================================================================

/// Context for handling a single request
pub struct HandlerContext<'a> {
    pub state: &'a Arc<GatewayState>,
    pub request_id: String,
    pub tx: mpsc::Sender<OutgoingFrame>,
    pub device_token: Option<String>,
}

impl HandlerContext<'_> {
    /// Send a response frame
    pub async fn respond(&self, payload: serde_json::Value) -> Result<()> {
        let frame = OutgoingFrame::Response(ResponseFrame::ok(self.request_id.clone(), payload));
        self.tx.send(frame).await?;
        Ok(())
    }

    /// Send an error response
    pub async fn respond_error(&self, error: ErrorShape) -> Result<()> {
        let frame = OutgoingFrame::Response(ResponseFrame::error(self.request_id.clone(), error));
        self.tx.send(frame).await?;
        Ok(())
    }
}
