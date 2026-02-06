//! HTTP routes and WebSocket upgrade handler.

use super::handler::handle_socket;
use crate::state::GatewayState;
use axum::{
    extract::{
        connect_info::ConnectInfo,
        ws::WebSocketUpgrade,
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

// ============================================================================
// Server State
// ============================================================================

pub struct ServerState {
    pub gateway: Arc<GatewayState>,
}

// ============================================================================
// Routes
// ============================================================================

pub fn create_router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/", get(ws_handler))
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let client_ip = addr.ip().to_string();
    info!(client_ip = %client_ip, "WebSocket connection");

    ws.on_upgrade(move |socket| handle_socket(socket, state, client_ip))
}
