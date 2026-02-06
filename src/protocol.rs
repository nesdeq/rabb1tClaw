//! WebSocket protocol v3 frame types for OpenClaw gateway.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Protocol version
pub const PROTOCOL_VERSION: u32 = 3;

/// Get current timestamp in milliseconds since Unix epoch
#[inline]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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
#[allow(dead_code)] // Fields deserialized from client but not all accessed in server logic
pub struct ConnectParams {
    #[serde(rename = "minProtocol")]
    pub min_protocol: u32,
    #[serde(rename = "maxProtocol")]
    pub max_protocol: u32,
    pub client: ClientInfo,
    pub auth: Option<ConnectAuth>,
    pub device: Option<DeviceAuth>,
    pub caps: Option<Vec<String>>,
    pub role: Option<String>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ClientInfo {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    pub version: String,
    pub platform: String,
    pub mode: String,
    #[serde(rename = "instanceId")]
    pub instance_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields deserialized from client auth payload
pub struct ConnectAuth {
    pub token: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeviceAuth {
    pub id: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub signature: String,
    #[serde(rename = "signedAt")]
    pub signed_at: u64,
    pub nonce: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Protocol fields deserialized but not all used yet
pub struct AgentParams {
    pub message: String,
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    pub to: Option<String>,
    #[serde(rename = "replyTo")]
    pub reply_to: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "sessionKey")]
    pub session_key: Option<String>,
    pub thinking: Option<String>,
    pub deliver: Option<bool>,
    pub channel: Option<String>,
    #[serde(rename = "replyChannel")]
    pub reply_channel: Option<String>,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(rename = "threadId")]
    pub thread_id: Option<String>,
    pub timeout: Option<u64>,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
    pub label: Option<String>,
    #[serde(rename = "extraSystemPrompt")]
    pub extra_system_prompt: Option<String>,
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
    pub fn ok(id: String, payload: serde_json::Value) -> Self {
        Self {
            frame_type: "res",
            id,
            ok: true,
            payload: Some(payload),
            error: None,
        }
    }

    pub fn error(id: String, error: ErrorShape) -> Self {
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
    #[serde(rename = "stateVersion", skip_serializing_if = "Option::is_none")]
    pub state_version: Option<StateVersion>,
}

impl EventFrame {
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            frame_type: "event",
            event: event.into(),
            payload: None,
            seq: None,
            state_version: None,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorShape {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(rename = "retryAfterMs", skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ErrorShape {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable: None,
            retry_after_ms: None,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateVersion {
    pub presence: u64,
    pub health: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(rename = "connId")]
    pub conn_id: String,
}

#[derive(Debug, Serialize)]
pub struct Features {
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

impl Features {
    pub fn default_features() -> Self {
        Self {
            methods: vec![
                "health".into(),
                "config.get".into(),
                "agent".into(),
                "chat.send".into(),
                "chat.history".into(),
                "chat.abort".into(),
                "sessions.list".into(),
                "models.list".into(),
            ],
            events: vec![
                "agent".into(),
                "chat".into(),
                "tick".into(),
                "presence".into(),
                "health".into(),
            ],
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub presence: Vec<serde_json::Value>,
    pub health: serde_json::Value,
    #[serde(rename = "stateVersion")]
    pub state_version: StateVersion,
    #[serde(rename = "uptimeMs")]
    pub uptime_ms: u64,
    #[serde(rename = "configPath", skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(rename = "stateDir", skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
    #[serde(rename = "sessionDefaults", skip_serializing_if = "Option::is_none")]
    pub session_defaults: Option<SessionDefaults>,
}

#[derive(Debug, Serialize)]
pub struct SessionDefaults {
    #[serde(rename = "defaultAgentId")]
    pub default_agent_id: String,
    #[serde(rename = "mainKey")]
    pub main_key: String,
    #[serde(rename = "mainSessionKey")]
    pub main_session_key: String,
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
            max_payload: 512 * 1024,              // 512KB (matches TypeScript)
            max_buffered_bytes: 1572864,          // 1.5MB (matches TypeScript)
            tick_interval_ms: 30_000,             // 30s
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuthInfo {
    #[serde(rename = "deviceToken")]
    pub device_token: String,
    pub role: String,
    pub scopes: Vec<String>,
    #[serde(rename = "issuedAtMs", skip_serializing_if = "Option::is_none")]
    pub issued_at_ms: Option<u64>,
}

