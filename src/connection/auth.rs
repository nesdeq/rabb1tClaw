//! Gateway authentication.

use crate::config::DeviceStore;
use crate::protocol::ConnectAuth;

/// Auth failure reason (protocol-relevant)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// Device token is revoked - device should clear credentials
    Revoked,
    /// Token doesn't match any device
    InvalidToken,
    /// No token provided but devices exist - needs pairing
    NeedsPairing,
}

impl AuthFailure {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthFailure::Revoked => "device_revoked",
            AuthFailure::InvalidToken => "device_token_invalid",
            AuthFailure::NeedsPairing => "device_token_missing",
        }
    }
}

/// Authentication result
#[derive(Debug)]
pub enum AuthResult {
    Ok(AuthMethod),
    Failed(AuthFailure),
}

/// Authentication method used
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Local,
    DeviceToken,
}

/// Constant-time byte comparison (prevents timing attacks)
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    constant_time_eq::constant_time_eq(a, b)
}

/// Check if an IP address is a loopback address
pub fn is_loopback(addr: &str) -> bool {
    addr == "127.0.0.1"
        || addr.starts_with("127.")
        || addr == "::1"
        || addr.starts_with("::ffff:127.")
        || addr == "localhost"
}

/// Validate a token against device store
fn validate_device_token(device_store: &DeviceStore, provided_token: &str) -> AuthResult {
    for device in device_store.devices.values() {
        if constant_time_eq(device.token.as_bytes(), provided_token.as_bytes()) {
            if device.revoked {
                tracing::warn!(device = %device.display_name, "Auth failed: device revoked");
                return AuthResult::Failed(AuthFailure::Revoked);
            }

            tracing::info!(device = %device.display_name, "Auth ok");
            return AuthResult::Ok(AuthMethod::DeviceToken);
        }
    }

    tracing::warn!("Auth failed: invalid token");
    AuthResult::Failed(AuthFailure::InvalidToken)
}

/// Authorize connection
pub fn authorize_connect(
    device_store: &DeviceStore,
    connect_auth: Option<&ConnectAuth>,
    is_local: bool,
) -> AuthResult {
    // Local connections can bypass auth if no devices configured
    if is_local && device_store.devices.is_empty() {
        return AuthResult::Ok(AuthMethod::Local);
    }

    // Check for token in auth
    if let Some(auth) = connect_auth {
        if let Some(ref token) = auth.token {
            return validate_device_token(device_store, token);
        }
    }

    // No devices configured, allow connection
    if device_store.devices.is_empty() {
        return AuthResult::Ok(AuthMethod::Local);
    }

    // No token provided but devices exist - require pairing
    tracing::info!("Auth: needs pairing (no token provided)");
    AuthResult::Failed(AuthFailure::NeedsPairing)
}
