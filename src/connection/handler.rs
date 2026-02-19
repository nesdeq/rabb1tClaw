//! WebSocket connection lifecycle: handshake, auth, frame routing, tick, cleanup.

use super::auth::{authorize_connect, is_loopback, AuthFailure, AuthResult};
use super::server::ServerState;
use crate::agent::dispatch_method;
use crate::config::{config_dir, config_path};
use crate::protocol::{
    now_ms, AuthInfo, ConnectParams, ErrorShape, EventFrame, Features, HelloOk,
    IncomingFrame, OutgoingFrame, Policy, ResponseFrame, ServerInfo, Snapshot,
    PROTOCOL_VERSION,
};
use crate::state::HandlerContext;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

#[allow(clippy::too_many_lines)]
pub async fn handle_socket(socket: WebSocket, state: Arc<ServerState>, client_ip: String) {
    let conn_id = crate::protocol::short_id();
    let is_local = is_loopback(&client_ip);

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut authenticated_token: Option<String> = None;

    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<OutgoingFrame>(crate::protocol::STREAM_CHANNEL_CAPACITY);

    // Spawn task to forward outgoing messages
    let debug_log = state.gateway.debug_log.clone();
    let send_conn_id = conn_id.clone();
    let send_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            match frame {
                OutgoingFrame::Close { code, reason } => {
                    log_outgoing(debug_log.as_ref(), &send_conn_id, &format!("CLOSE {code} {reason}"));
                    let _ = sender.send(Message::Close(Some(CloseFrame {
                        code,
                        reason: reason.into(),
                    }))).await;
                    break;
                }
                frame => match serde_json::to_string(&frame) {
                    Ok(json) => {
                        log_outgoing(debug_log.as_ref(), &send_conn_id, &json);
                        if sender.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => error!("Failed to serialize frame: {}", e),
                },
            }
        }
    });

    let mut connected = false;

    // Send connect.challenge event (protocol spec section 4.2)
    let challenge_event = EventFrame::new("connect.challenge")
        .with_payload(json!({ "nonce": uuid::Uuid::new_v4().to_string(), "ts": now_ms() }));
    let _ = tx.send(OutgoingFrame::Event(challenge_event)).await;
    state.gateway.log_frame(&conn_id, "---", &format!("connected from {client_ip}"));
    debug!(conn_id = %conn_id, "Sent connect.challenge");

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let shutdown_notify_clone = shutdown_notify.clone();

    // Main message loop
    loop {
        let msg = tokio::select! {
            msg = receiver.next() => msg,
            () = shutdown_notify_clone.notified() => {
                info!(conn_id = %conn_id, "Shutdown signal received");
                break;
            }
        };

        let msg = match msg {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                debug!(conn_id = %conn_id, "WebSocket error: {}", e);
                break;
            }
            None => break,
        };

        match msg {
            Message::Text(text) => {
                state.gateway.log_frame(&conn_id, "IN ", text.as_ref());
                let frame: IncomingFrame = match serde_json::from_str(text.as_ref()) {
                    Ok(f) => f,
                    Err(e) => {
                        debug!(conn_id = %conn_id, "Parse error: {}", e);
                        let error = ResponseFrame::error(
                            "unknown".to_string(),
                            ErrorShape::invalid_request(format!("failed to parse frame: {e}")),
                        );
                        let _ = tx.send(OutgoingFrame::Response(error)).await;
                        continue;
                    }
                };

                let IncomingFrame::Request { id, method, params } = frame;

                if method == "connect" {
                    match handle_connect(&state, &conn_id, &id, &tx, &shutdown, &shutdown_notify, params, is_local).await {
                        ConnectOutcome::Connected(token) => {
                            authenticated_token = token;
                            connected = true;
                        }
                        ConnectOutcome::NeedsPairing => {}
                        ConnectOutcome::Rejected => {
                            let _ = tx.send(OutgoingFrame::Close {
                                code: 1008,
                                reason: "auth_failed".to_string(),
                            }).await;
                            break;
                        }
                    }
                } else if !connected {
                    let error = ResponseFrame::error(
                        id,
                        ErrorShape::unauthorized("not connected, send 'connect' first"),
                    );
                    let _ = tx.send(OutgoingFrame::Response(error)).await;
                } else {
                    let ctx = HandlerContext {
                        state: &state.gateway,
                        request_id: id.clone(),
                        tx: tx.clone(),
                        device_token: authenticated_token.clone(),
                    };

                    if let Err(e) = dispatch_method(&ctx, &method, params).await {
                        error!(conn_id = %conn_id, method = %method, "Handler error: {}", e);
                        let error = ResponseFrame::error(id, ErrorShape::internal(e.to_string()));
                        let _ = tx.send(OutgoingFrame::Response(error)).await;
                    }
                }
            }
            Message::Binary(_) => debug!(conn_id = %conn_id, "Binary message ignored"),
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => {
                debug!(conn_id = %conn_id, "Closed by client");
                break;
            }
        }
    }

    // Clean up
    drop(tx);
    let _ = send_task.await;

    if let Some(ref token) = authenticated_token {
        state.gateway.unregister_connection(token, &conn_id).await;
    }

    state.gateway.log_frame(&conn_id, "---", "disconnected");
    let disc_prefix = authenticated_token.as_ref()
        .map_or_else(|| conn_id.clone(), |t| crate::agent::session::token_prefix(t));
    info!("[{}] disconnected", disc_prefix);
}

// ============================================================================
// Connect Handshake
// ============================================================================

enum ConnectOutcome {
    Connected(Option<String>),
    NeedsPairing,
    Rejected,
}

#[allow(clippy::too_many_arguments)]
async fn handle_connect(
    state: &Arc<ServerState>,
    conn_id: &str,
    request_id: &str,
    tx: &mpsc::Sender<OutgoingFrame>,
    shutdown: &Arc<AtomicBool>,
    shutdown_notify: &Arc<tokio::sync::Notify>,
    params: Option<serde_json::Value>,
    is_local: bool,
) -> ConnectOutcome {
    let connect_params: Option<ConnectParams> =
        params.and_then(|p| serde_json::from_value(p).ok());

    let connect_auth = connect_params.as_ref().and_then(|p| p.auth.as_ref());

    let device_store = state.gateway.device_store.read().await;
    let auth_result = authorize_connect(&device_store, connect_auth, is_local);
    drop(device_store);

    match auth_result {
        AuthResult::Ok(method) => {
            let device_token = connect_auth.and_then(|a| a.token.clone());
            let prefix = device_token.as_ref()
                .map_or_else(|| conn_id.to_string(), |t| crate::agent::session::token_prefix(t));
            info!("[{}] connected method={:?}", prefix, method);

            // Register connection for revocation tracking
            if let Some(ref token) = device_token {
                state.gateway.register_connection(
                    token.clone(), conn_id.to_string(), tx.clone(),
                    shutdown.clone(), shutdown_notify.clone(),
                ).await;
            }

            let hello = create_hello_ok(
                conn_id.to_string(),
                config_path().display().to_string(),
                config_dir().display().to_string(),
                device_token.clone(),
            );
            let hello_json = serde_json::to_value(&hello).unwrap();
            let response = ResponseFrame::ok(request_id.to_string(), hello_json);
            let _ = tx.send(OutgoingFrame::Response(response)).await;

            // Start tick events
            spawn_tick_task(tx.clone(), conn_id.to_string(), shutdown.clone());

            ConnectOutcome::Connected(device_token)
        }
        AuthResult::Failed(failure) => {
            let reason = failure.as_str();
            warn!(conn_id = %conn_id, reason = %reason, "Auth failed");

            let error = ErrorShape::invalid_request(format!("unauthorized: {reason}"));
            let _ = tx.send(OutgoingFrame::Response(
                ResponseFrame::error(request_id.to_string(), error),
            )).await;

            if failure == AuthFailure::NeedsPairing {
                info!(conn_id = %conn_id, "Waiting for pairing");
                ConnectOutcome::NeedsPairing
            } else {
                ConnectOutcome::Rejected
            }
        }
    }
}

fn spawn_tick_task(tx: mpsc::Sender<OutgoingFrame>, conn_id: String, shutdown: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut tick_interval = interval(Duration::from_secs(crate::protocol::TICK_INTERVAL_SECS));
        tick_interval.tick().await; // Skip first immediate tick
        let mut seq = 1u64;
        loop {
            tick_interval.tick().await;

            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            let event = EventFrame::new("tick")
                .with_payload(json!({ "ts": now_ms() }))
                .with_seq(seq);

            if tx.send(OutgoingFrame::Event(event)).await.is_err() {
                debug!(conn_id = %conn_id, "Tick task ended");
                break;
            }
            seq += 1;
        }
    });
}

// ============================================================================
// Debug logging
// ============================================================================

fn log_outgoing(debug_log: Option<&crate::state::DebugLog>, conn_id: &str, msg: &str) {
    if let Some(log) = debug_log {
        crate::state::write_debug_line(log, "OUT", conn_id, msg);
    }
}

// ============================================================================
// Hello-OK Response
// ============================================================================

fn create_hello_ok(
    conn_id: String,
    config_path: String,
    state_dir: String,
    device_token: Option<String>,
) -> HelloOk {
    HelloOk {
        frame_type: "hello-ok",
        protocol: PROTOCOL_VERSION,
        server: ServerInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            conn_id,
        },
        features: Features::default(),
        snapshot: Snapshot {
            config_path: Some(config_path),
            state_dir: Some(state_dir),
        },
        policy: Policy::default(),
        auth: device_token.map(|token| AuthInfo {
            device_token: token,
            issued_at_ms: Some(now_ms()),
        }),
    }
}
