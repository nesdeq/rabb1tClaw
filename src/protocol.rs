//! WebSocket protocol v3 frame types for `OpenClaw` gateway.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Protocol version
pub const PROTOCOL_VERSION: u32 = 3;

// ── Infrastructure constants ──
/// WebSocket tick / keep-alive interval
pub const TICK_INTERVAL_SECS: u64 = 30;
/// Max WebSocket frame payload
pub const WS_MAX_PAYLOAD: u64 = 512 * 1024; // 512KB
/// Max buffered WebSocket bytes before backpressure
pub const WS_MAX_BUFFERED: u64 = 1_572_864; // 1.5MB
/// mpsc channel buffer for streaming chunks
pub const STREAM_CHANNEL_CAPACITY: usize = 100;
/// Config/devices file poll interval for hot reload
pub const CONFIG_POLL_SECS: u64 = 2;

/// Get current timestamp in milliseconds since Unix epoch
#[inline]
pub fn now_ms() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs() * 1000 + u64::from(d.subsec_millis())
}

/// Generate a short (8-char) random identifier from UUID v4.
#[inline]
pub fn short_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// Get current timestamp in seconds since Unix epoch
#[inline]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ============================================================================
// Incoming Frames (Client → Server)
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum IncomingFrame {
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        params: Option<serde_json::Value>,
    },
}

#[derive(Debug, Deserialize)]
pub struct ConnectParams {
    pub auth: Option<ConnectAuth>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectAuth {
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentParams {
    pub message: String,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
}

// ============================================================================
// Outgoing Frames (Server → Client)
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum OutgoingFrame {
    Response(ResponseFrame),
    Event(EventFrame),
    #[serde(skip)]
    Close { code: u16, reason: String },
}

#[derive(Debug, Serialize)]
pub struct ResponseFrame {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorShape>,
}

impl ResponseFrame {
    pub const fn ok(id: String, payload: serde_json::Value) -> Self {
        Self {
            frame_type: "res",
            id,
            ok: true,
            payload: Some(payload),
            error: None,
        }
    }

    pub const fn error(id: String, error: ErrorShape) -> Self {
        Self {
            frame_type: "res",
            id,
            ok: false,
            payload: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EventFrame {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

impl EventFrame {
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            frame_type: "event",
            event: event.into(),
            payload: None,
            seq: None,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub const fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorShape {
    pub code: String,
    pub message: String,
}

impl ErrorShape {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("INVALID_REQUEST", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("UNAUTHORIZED", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("NOT_FOUND", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("UNAVAILABLE", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL_ERROR", message)
    }
}

#[derive(Debug, Serialize)]
pub struct HelloOk {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub protocol: u32,
    pub server: ServerInfo,
    pub features: Features,
    pub snapshot: Snapshot,
    pub policy: Policy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthInfo>,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub version: String,
    #[serde(rename = "connId")]
    pub conn_id: String,
}

#[derive(Debug, Serialize)]
pub struct Features {
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            methods: vec![
                "health".into(),
                "agent".into(),
                "chat.send".into(),
                "chat.history".into(),
            ],
            events: vec![
                "agent".into(),
                "chat".into(),
                "tick".into(),
            ],
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    #[serde(rename = "configPath", skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(rename = "stateDir", skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Policy {
    #[serde(rename = "maxPayload")]
    pub max_payload: u64,
    #[serde(rename = "maxBufferedBytes")]
    pub max_buffered_bytes: u64,
    #[serde(rename = "tickIntervalMs")]
    pub tick_interval_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_payload: WS_MAX_PAYLOAD,
            max_buffered_bytes: WS_MAX_BUFFERED,
            tick_interval_ms: TICK_INTERVAL_SECS * 1000,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuthInfo {
    #[serde(rename = "deviceToken")]
    pub device_token: String,
    #[serde(rename = "issuedAtMs", skip_serializing_if = "Option::is_none")]
    pub issued_at_ms: Option<u64>,
}
