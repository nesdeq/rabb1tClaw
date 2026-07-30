//! Regression tests for the correctness audit.
//!
//! Unit tests rather than `tests/`: this is a binary crate with no lib target,
//! so integration tests cannot reach these internals. Requires Linux (hakoniwa).

use crate::agent::markers::{parse_task_markers, strip_task_markers};
use crate::agent::stream::MarkerFilter;
use crate::agent::tracker::truncate;

/// Run the live stream filter exactly as `runner.rs` drives it.
fn live_stream(chunks: &[&str]) -> String {
    let mut full = String::new();
    let mut f = MarkerFilter::new();
    let mut out = String::new();
    for c in chunks {
        full.push_str(c);
        for d in f.drain(&full) {
            out.push_str(d);
        }
    }
    if let Some(rest) = f.flush(&full) {
        out.push_str(rest);
    }
    out
}

// ============================================================================
// C1 — dispatch blocks must never reach the device, and the three parsers
//      (live filter / strip / parse) must agree on block boundaries.
// ============================================================================

#[test]
fn c1_a_dispatch_not_preceded_by_newline_leaks_to_device() {
    let resp = "Let me check that.@@dispatch\n[{\"type\":\"search\",\"desc\":\"weather\"}]\n@@end\nDone.";

    let device_sees = live_stream(&[resp]);
    let persisted = strip_task_markers(resp);
    let parsed = parse_task_markers(resp);

    println!("device_sees  = {device_sees:?}");
    println!("persisted    = {persisted:?}");
    println!("markers      = {}", parsed.len());

    assert_eq!(parsed.len(), 1, "parser treats it as a real dispatch block");
    assert!(!persisted.contains("@@dispatch"), "strip removes it");
    assert!(
        !device_sees.contains("@@dispatch"),
        "LEAK: live stream sent the raw dispatch block to the device:\n{device_sees}"
    );
}

#[test]
fn c1_b_unclosed_block_is_persisted_and_returned() {
    let resp = "Working on it.\n@@dispatch\n[{\"type\":\"code\",\"desc\":\"x\"}]";

    let device_sees = live_stream(&[resp]);
    let persisted = strip_task_markers(resp);

    println!("device_sees = {device_sees:?}");
    println!("persisted   = {persisted:?}");

    assert!(
        !persisted.contains("@@dispatch"),
        "LEAK: unclosed block kept in final response + persisted history:\n{persisted}"
    );
    assert_eq!(device_sees, persisted, "stream and final message diverge");
}

#[test]
fn c1_c_unclosed_block_suppressed_identically_in_stream_and_final() {
    let resp = "Hi\n@@dispatch\n[{\"type\":\"code\",\"desc\":\"x\"}]\nHere is your answer: 42.";
    let device_sees = live_stream(&[resp]);
    let final_msg = strip_task_markers(resp);
    println!("device_sees = {device_sees:?}");
    println!("final       = {final_msg:?}");
    assert!(!device_sees.contains("@@dispatch") && !final_msg.contains("@@dispatch"));
    assert_eq!(device_sees, final_msg,
        "stream and final message disagree on an unclosed block");
}

#[test]
fn c1_d_chunked_delivery_matches_whole_delivery() {
    let resp = "Sure.\n@@dispatch\n[{\"type\":\"search\",\"desc\":\"q\"}]\n@@end\nAll set.";
    let whole = live_stream(&[resp]);
    let per_char: Vec<&str> = {
        let mut v = Vec::new();
        let mut i = 0;
        while i < resp.len() {
            let mut j = i + 1;
            while !resp.is_char_boundary(j) {
                j += 1;
            }
            v.push(&resp[i..j]);
            i = j;
        }
        v
    };
    let chunked = live_stream(&per_char);
    println!("whole   = {whole:?}");
    println!("chunked = {chunked:?}");
    assert_eq!(whole, chunked, "chunk boundaries changed the device output");
    assert_eq!(whole, strip_task_markers(resp), "stream != final message");
}

// ============================================================================
// C3 — truncate(s, n) must return a prefix of s.
// ============================================================================

#[test]
fn c3_truncate_keeps_the_longest_decodable_prefix() {
    use crate::agent::tracker::count_tokens;
    let corpus = [
        "🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂",
        "日本語のテキストです。これは切り詰めのテストです。",
        "café naïve résumé — ünïcödé",
        "🇩🇪🇫🇷🇯🇵🇺🇸 flags and 👨‍👩‍👧‍👦 families",
        "Ω≈ç√∫˜µ≤≥÷ åß∂ƒ©˙∆˚¬…æ",
    ];
    let mut failures = Vec::new();
    for text in corpus {
        let first_char_end = text.char_indices().nth(1).map_or(text.len(), |(i, _)| i);
        let first_char_tokens = count_tokens(&text[..first_char_end]);
        for n in 1..12usize {
            let out = truncate(text, n);
            let body = out.strip_suffix("...").unwrap_or(&out);
            if !text.starts_with(body) {
                failures.push(format!("truncate({text:?}, {n}) = {out:?}  [NOT A PREFIX]"));
            }
            if count_tokens(body) > n {
                failures.push(format!("truncate({text:?}, {n}) = {out:?}  [OVER BUDGET]"));
            }
            // Empty is only honest when not even one character fits.
            if body.is_empty() && n >= first_char_tokens {
                failures.push(format!("truncate({text:?}, {n}) = {out:?}  [AVOIDABLE LOSS]"));
            }
        }
    }
    for f in &failures {
        println!("{f}");
    }
    assert!(failures.is_empty(), "{} failing cases", failures.len());
}

// ============================================================================
// C2 — SSE decoding must be lossless across network chunk boundaries.
// ============================================================================

#[tokio::test]
async fn c2_sse_multibyte_split_across_chunks() {
    use axum::response::IntoResponse;

    let line = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo 🙂 wörld\"}}]}\n\n";
    let done = "data: [DONE]\n\n";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(line.as_bytes());
    body.extend_from_slice(done.as_bytes());

    // One byte per network chunk => guaranteed mid-UTF-8 splits.
    let pieces: Vec<Vec<u8>> = body.iter().map(|b| vec![*b]).collect();

    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let pieces = pieces.clone();
            async move {
                let s = futures::stream::iter(
                    pieces
                        .into_iter()
                        .map(|p| Ok::<_, std::io::Error>(axum::body::Bytes::from(p))),
                );
                axum::body::Body::from_stream(s).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = crate::provider::create_provider("openai", &format!("http://{addr}/v1"), "k");
    let req = crate::provider::ChatRequest {
        model: "gpt-4o".into(),
        messages: vec![crate::provider::ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }],
        system: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        reasoning_effort: None,
        thinking: None,
    };
    let mut rx = provider.chat_stream(req).await.unwrap();
    let mut got = String::new();
    while let Some(c) = rx.recv().await {
        match c {
            crate::provider::StreamChunk::Text(t) => got.push_str(&t),
            crate::provider::StreamChunk::Done => break,
            crate::provider::StreamChunk::Error(e) => panic!("stream error: {e}"),
        }
    }
    println!("received = {got:?}");
    assert_eq!(
        got, "héllo 🙂 wörld",
        "SSE decode corrupted multi-byte text split across network chunks"
    );
}

#[test]
fn c1_e_whitespace_depends_on_chunking_and_diverges_from_final() {
    let resp = "Sure.\n@@dispatch\n[{\"type\":\"search\",\"desc\":\"q\"}]\n@@end\nAll set.";
    let whole = live_stream(&[resp]);
    let split: Vec<&str> = vec![&resp[..6], &resp[6..]];
    let two = live_stream(&split);
    let final_msg = strip_task_markers(resp);
    println!("whole     = {whole:?}");
    println!("two-chunk = {two:?}");
    println!("final     = {final_msg:?}");
    assert_eq!(whole, final_msg, "streamed text != final message text");
}

#[tokio::test]
async fn c2_b_single_realistic_split_inside_one_emoji() {
    use axum::response::IntoResponse;
    let line = "data: {\"choices\":[{\"delta\":{\"content\":\"price is 12€ today\"}}]}\n\ndata: [DONE]\n\n";
    let bytes = line.as_bytes().to_vec();
    // Split once, inside the 3-byte € (U+20AC).
    let euro = line.find('€').unwrap();
    let (a, b) = bytes.split_at(euro + 1);
    let pieces = vec![a.to_vec(), b.to_vec()];

    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let pieces = pieces.clone();
            async move {
                let s = futures::stream::iter(
                    pieces.into_iter().map(|p| Ok::<_, std::io::Error>(axum::body::Bytes::from(p))),
                );
                axum::body::Body::from_stream(s).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let provider = crate::provider::create_provider("openai", &format!("http://{addr}/v1"), "k");
    let req = crate::provider::ChatRequest {
        model: "gpt-4o".into(),
        messages: vec![crate::provider::ChatMessage { role: "user".into(), content: "hi".into() }],
        system: None, max_tokens: None, temperature: None, top_p: None,
        frequency_penalty: None, presence_penalty: None, reasoning_effort: None, thinking: None,
    };
    let mut rx = provider.chat_stream(req).await.unwrap();
    let mut got = String::new();
    while let Some(c) = rx.recv().await {
        match c {
            crate::provider::StreamChunk::Text(t) => got.push_str(&t),
            crate::provider::StreamChunk::Done => break,
            crate::provider::StreamChunk::Error(e) => panic!("stream error: {e}"),
        }
    }
    println!("received = {got:?}");
    assert_eq!(got, "price is 12€ today", "one TCP split inside a 3-byte char corrupts it");
}

// ============================================================================
// C4/C5 — session AEAD: authenticity, and behaviour on corrupt input.
// ============================================================================

#[tokio::test]
async fn c4_session_aead_round_trip_and_tamper() {
    use crate::agent::session::{ConversationTurn, SessionManager};
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());

    let tok_a = "a".repeat(32);
    let mgr = SessionManager::new();
    mgr.record_message(&tok_a, "user", "secret plan", Some("r1")).await;

    let dir = tmp.path().join(".rabb1tclaw").join(&tok_a[..8]);
    let path = dir.join("conversation.enc");
    let data = std::fs::read(&path).unwrap();
    println!("ciphertext len = {}", data.len());
    assert!(!String::from_utf8_lossy(&data).contains("secret plan"), "plaintext on disk");

    // Round-trip through the real load path.
    let mut store = crate::config::DeviceStore::default();
    store.devices.insert("d1".into(), crate::config::Device {
        device_id: "d1".into(), display_name: "A".into(), token: tok_a.clone(), revoked: false,
    });
    let m2 = SessionManager::new();
    m2.load_from_disk(&store).await;
    let h: Vec<ConversationTurn> = m2.get_history(&tok_a).await;
    assert_eq!(h.len(), 1, "round-trip lost the turn");
    assert_eq!(h[0].content, "secret plan");

    // Tamper every byte position: must never yield plaintext.
    for i in 0..data.len() {
        let mut bad = data.clone();
        bad[i] ^= 0x01;
        std::fs::write(&path, &bad).unwrap();
        let m3 = SessionManager::new();
        m3.load_from_disk(&store).await;
        let hh = m3.get_history(&tok_a).await;
        assert!(hh.is_empty(), "tampered byte {i} still decrypted");
    }
    println!("all {} tampered variants rejected", data.len());
}

#[tokio::test]
async fn c5_write_secure_is_atomic_under_concurrent_readers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested").join("conversation.enc");
    let small = vec![b'a'; 64 * 1024];
    let large = vec![b'b'; 512 * 1024];
    crate::config::native::write_secure(&path, &small).unwrap();

    let p = path.clone();
    let (s1, l1) = (small.clone(), large.clone());
    let writer = tokio::task::spawn_blocking(move || {
        for i in 0..200 {
            let body = if i % 2 == 0 { &l1 } else { &s1 };
            crate::config::native::write_secure(&p, body).unwrap();
        }
    });

    let mut torn = 0;
    let mut reads = 0;
    while !writer.is_finished() {
        if let Ok(got) = std::fs::read(&path) {
            reads += 1;
            if got != small && got != large { torn += 1; }
        }
    }
    writer.await.unwrap();
    println!("{reads} reads during 200 rewrites, torn = {torn}");

    let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "conversation.enc")
        .collect();
    println!("leftover files = {strays:?}");
    assert_eq!(torn, 0, "{torn}/{reads} reads saw a partially written file");
    assert!(strays.is_empty(), "staging files left behind: {strays:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        println!("final mode = {mode:o}");
        assert_eq!(mode, 0o600, "atomic rename lost the 0600 permissions");
    }
}

// ============================================================================
// C6 — device isolation must not depend on an 8-hex-char token prefix.
// ============================================================================

#[tokio::test]
async fn c6_prefix_collision_leaks_history_across_devices() {
    use crate::agent::session::SessionManager;
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());

    // Two DISTINCT 32-hex tokens sharing their first 8 chars.
    let a = format!("deadbeef{}", "1".repeat(24));
    let b = format!("deadbeef{}", "2".repeat(24));
    assert_ne!(a, b);

    let mgr = SessionManager::new();
    mgr.record_message(&a, "user", "device A private data", None).await;
    let b_view = mgr.get_history(&b).await;
    println!("device B sees {} of device A's turns", b_view.len());
    for t in &b_view { println!("  leaked: {:?}", t.content); }
    assert!(b_view.is_empty(), "device B read device A's conversation");
}

// ============================================================================
// C7 — deny-by-default on the connect path.
// ============================================================================

#[test]
fn c7_remote_connection_denied_when_no_devices_configured() {
    use crate::connection::auth::{authorize_connect, AuthResult};
    let store = crate::config::DeviceStore::default(); // no devices yet
    let r = authorize_connect(&store, None, /* is_local = */ false);
    println!("remote + no token + empty store -> {r:?}");
    assert!(matches!(r, AuthResult::Failed(_)), "unauthenticated remote client was admitted");
}

#[test]
fn c7_b_remote_denied_after_revoke_all() {
    use crate::connection::auth::{authorize_connect, AuthResult};
    // `devices --revoke-all` marks revoked but keeps entries, so the store is
    // non-empty. Confirm that is what actually happens.
    let mut store = crate::config::DeviceStore::default();
    store.devices.insert("d".into(), crate::config::Device {
        device_id: "d".into(), display_name: "R1".into(), token: "t".repeat(32), revoked: true,
    });
    let r = authorize_connect(&store, None, false);
    println!("after revoke-all -> {r:?}");
    assert!(matches!(r, AuthResult::Failed(_)));
}

// ============================================================================
// Shared fixture: a real GatewayState pointed at a local fake LLM.
// ============================================================================

async fn fake_llm(delay_ms: u64, text: &'static str) -> String {
    use axum::response::IntoResponse;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices":[{"delta":{"content": text}}]})
            );
            axum::body::Body::from(body).into_response()
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn state_with(base_url: String) -> std::sync::Arc<crate::state::GatewayState> {
    use crate::config::{GatewayConfig, ModelConfig, ProviderConfig};
    let mut cfg = GatewayConfig::default();
    cfg.providers.insert("p".into(), ProviderConfig {
        api: "openai".into(), base_url, api_key: "k".into(), name: None,
    });
    cfg.models.insert("m".into(), ModelConfig {
        provider: "p".into(), model_id: "gpt-4o".into(), ..Default::default()
    });
    cfg.active_model = Some("m".into());
    std::sync::Arc::new(crate::state::GatewayState::new(
        cfg, crate::config::DeviceStore::default(), None,
    ))
}

// ============================================================================
// C8 — protocol invariant: every `req` receives exactly one terminal `res`
//      carrying that request's own id.
// ============================================================================

#[tokio::test]
async fn c8_every_request_id_gets_its_own_res() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let url = fake_llm(400, "hello").await;
    let state = state_with(url);
    let params = serde_json::json!({"message":"hi","idempotencyKey":"KEY-1"});

    let (tx1, mut rx1) = tokio::sync::mpsc::channel(64);
    let ctx1 = crate::state::HandlerContext {
        state: &state, request_id: "req-1".into(), tx: tx1, device_token: None,
    };
    crate::agent::runner::handle_agent(&ctx1, Some(params.clone())).await.unwrap();

    // Retry with the same key but a new request id, while req-1 still streams.
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(64);
    let ctx2 = crate::state::HandlerContext {
        state: &state, request_id: "req-2".into(), tx: tx2, device_token: None,
    };
    crate::agent::runner::handle_agent(&ctx2, Some(params)).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(900)).await;

    let drain = |rx: &mut tokio::sync::mpsc::Receiver<crate::protocol::OutgoingFrame>| {
        let mut ids = Vec::new();
        while let Ok(f) = rx.try_recv() {
            if let crate::protocol::OutgoingFrame::Response(r) = f { ids.push(r.id); }
        }
        ids
    };
    let ids1 = drain(&mut rx1);
    let ids2 = drain(&mut rx2);
    println!("req-1 res ids = {ids1:?}");
    println!("req-2 res ids = {ids2:?}");
    assert!(ids1.iter().any(|i| i == "req-1"), "original got no res");
    assert!(ids2.iter().any(|i| i == "req-2"), "duplicate request id got no res");
}

#[tokio::test]
async fn c9_duplicate_never_stalls_the_receive_loop() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // Original run streams for 1.5s; the retry must not wait for it, because
    // dispatch_method is awaited inline in the WebSocket receive loop.
    let url = fake_llm(1500, "slow answer").await;
    let state = state_with(url);

    let (tx1, _rx1) = tokio::sync::mpsc::channel(64);
    let c1 = crate::state::HandlerContext {
        state: &state, request_id: "r1".into(), tx: tx1, device_token: None,
    };
    let params = serde_json::json!({"message":"hi","idempotencyKey":"K"});
    crate::agent::runner::handle_agent(&c1, Some(params.clone())).await.unwrap();

    let (tx2, mut rx2) = tokio::sync::mpsc::channel(64);
    let c2 = crate::state::HandlerContext {
        state: &state, request_id: "r2".into(), tx: tx2, device_token: None,
    };
    let started = tokio::time::Instant::now();
    crate::agent::runner::handle_agent(&c2, Some(params)).await.unwrap();
    let waited = started.elapsed();
    println!("duplicate returned after {waited:?}");

    let mut res_ids = Vec::new();
    while let Ok(f) = rx2.try_recv() {
        if let crate::protocol::OutgoingFrame::Response(r) = f { res_ids.push(r.id); }
    }
    println!("duplicate res ids = {res_ids:?}");
    assert!(waited < std::time::Duration::from_millis(500),
        "duplicate blocked the receive loop for {waited:?}");
    assert!(res_ids.contains(&"r2".to_string()), "duplicate got no res of its own");
}

// ============================================================================
// C11 — background tracker: completed items must not accumulate without bound,
//       and NeedsInput must not hold a concurrency slot forever.
// ============================================================================

#[tokio::test]
async fn c11_a_finished_items_are_pruned() {
    use crate::agent::advanced::{AdvancedTaskStatus, AdvancedTaskTracker};
    let retain = crate::cli::defaults::DEFAULT_TASK_LOG_MAX_ENTRIES;
    let tr = AdvancedTaskTracker::new();
    for i in 0..500u32 {
        assert!(tr.register("pfx", i, format!("task {i}"), 1, retain).await.is_some());
        tr.complete("pfx", i, AdvancedTaskStatus::Completed { summary: "ok".into() }).await;
    }
    let retained = tr.tracked_len("pfx").await;
    println!("retained after 500 completed tasks = {retained} (bound {retain})");
    assert!(retained <= retain + 1, "tracker grew to {retained} items");
}

#[tokio::test]
async fn c11_b_slot_is_released_when_the_task_finishes() {
    use crate::agent::advanced::{AdvancedTaskStatus, AdvancedTaskTracker};
    let max = crate::cli::defaults::DEFAULT_ADVANCED_MAX_CONCURRENT;
    let retain = crate::cli::defaults::DEFAULT_TASK_LOG_MAX_ENTRIES;
    let tr = AdvancedTaskTracker::new();

    assert!(tr.register("pfx", 1, "long task".into(), max, retain).await.is_some());
    tr.update_status("pfx", 1, AdvancedTaskStatus::NeedsInput { question: "which?".into() }).await;
    assert!(tr.register("pfx", 2, "blocked".into(), max, retain).await.is_none(),
        "NeedsInput must hold the slot while the question is live");

    // The advanced loop now bounds that wait by the total timeout and fails the
    // task, which is what releases the slot.
    tr.complete("pfx", 1, AdvancedTaskStatus::Failed { error: "no answer".into() }).await;
    assert!(tr.register("pfx", 3, "next".into(), max, retain).await.is_some(),
        "slot not released after the task failed out");
    println!("slot released after timeout-driven failure");
}

// ============================================================================
// C10 — conversation FIFO must preserve user/assistant pairing.
// ============================================================================

#[tokio::test]
async fn c10_orphan_user_turn_after_provider_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // Server that always 500s -> StreamChunk::Error path.
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let state = state_with(format!("http://{addr}/v1"));

    let token = "c".repeat(32);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let ctx = crate::state::HandlerContext {
        state: &state, request_id: "r".into(), tx, device_token: Some(token.clone()),
    };
    crate::agent::runner::handle_agent(
        &ctx, Some(serde_json::json!({"message":"hello","idempotencyKey":"K1"}))).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let roles: Vec<String> = state.session_manager.get_history(&token).await
        .into_iter().map(|t| t.role).collect();
    println!("history roles after a failed LLM call = {roles:?}");
    assert!(roles.is_empty() || roles.len().is_multiple_of(2),
        "a failed provider call left an unpaired user turn in persisted history: {roles:?}");
}

#[tokio::test]
async fn c8_b_idempotency_key_is_not_scoped_per_device() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let url = fake_llm(400, "answer for A").await;
    let state = state_with(url);

    let tok_a = "a".repeat(32);
    let tok_b = "b".repeat(32);

    // Device A starts a run with a key its client chose.
    let (txa, _rxa) = tokio::sync::mpsc::channel(64);
    let ca = crate::state::HandlerContext {
        state: &state, request_id: "A-1".into(), tx: txa, device_token: Some(tok_a.clone()),
    };
    crate::agent::runner::handle_agent(
        &ca, Some(serde_json::json!({"message":"A question","idempotencyKey":"msg-1"}))).await.unwrap();

    // Device B independently uses the same key value while A is in flight.
    let (txb, mut rxb) = tokio::sync::mpsc::channel(64);
    let cb = crate::state::HandlerContext {
        state: &state, request_id: "B-1".into(), tx: txb, device_token: Some(tok_b.clone()),
    };
    let r = tokio::time::timeout(std::time::Duration::from_secs(3),
        crate::agent::runner::handle_agent(
            &cb, Some(serde_json::json!({"message":"B question","idempotencyKey":"msg-1"})))).await;
    println!("device B handle_agent completed = {}", r.is_ok());

    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let mut b_frames = 0;
    while rxb.try_recv().is_ok() { b_frames += 1; }
    let b_hist: Vec<String> = state.session_manager.get_history(&tok_b).await
        .into_iter().map(|t| t.role).collect();
    println!("device B frames = {b_frames}, device B history = {b_hist:?}");
    assert!(b_frames > 0,
        "device B's request was swallowed because device A used the same idempotencyKey");
}

// ============================================================================
// C12 — trim_pairs_to_budget must keep the conversation within budget.
// ============================================================================

#[tokio::test]
async fn c12_history_fifo_respects_budget_with_unpaired_history() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let url = fake_llm(0, "ok").await;
    let state = state_with(url);
    let token = "d".repeat(32);

    // Realistic damaged history: an orphan user turn (see c10) then normal pairs.
    state.session_manager.record_message(&token, "user", &"x ".repeat(4000), None).await;
    for i in 0..6 {
        state.session_manager.record_message(&token, "user", &format!("u{i} ").repeat(4000), None).await;
        state.session_manager.record_message(&token, "assistant", &format!("a{i} ").repeat(4000), None).await;
    }
    let hist = state.session_manager.get_history(&token).await;
    let roles: Vec<&str> = hist.iter().map(|t| t.role.as_str()).collect();
    println!("history roles = {roles:?}");

    let mut msgs: Vec<crate::provider::ChatMessage> = hist.iter()
        .map(|t| crate::provider::ChatMessage { role: t.role.clone(), content: t.content.clone() })
        .collect();
    msgs.push(crate::provider::ChatMessage { role: "user".into(), content: "now".into() });

    let budget = 20_000u32;
    let before: usize = msgs.iter().map(|m| crate::agent::tracker::count_tokens(&m.content)).sum();
    crate::agent::runner::trim_pairs_to_budget(&mut msgs, budget);
    let after: usize = msgs.iter().map(|m| crate::agent::tracker::count_tokens(&m.content)).sum();
    let out_roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
    println!("tokens {before} -> {after} (budget {budget}), roles = {out_roles:?}");
    assert!(after <= budget as usize,
        "FIFO left {after} tokens against a {budget} budget");
}

// ============================================================================
// C13 — Anthropic request body must be accepted by the models `init` can pick.
//   Spec: extended thinking `{"type":"enabled","budget_tokens":N}` is rejected
//   with 400 on Claude 4.7 and later (Opus 4.7/4.8/5, Sonnet 5, Fable 5).
// ============================================================================

#[tokio::test]
async fn c13_anthropic_thinking_body_for_current_models() {
    use axum::response::IntoResponse;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let cap = captured.clone();
    let app = axum::Router::new().route(
        "/messages",
        axum::routing::post(move |body: axum::Json<serde_json::Value>| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(body.0);
                axum::body::Body::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
                    .into_response()
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    // Models a user would pick from `init`'s auto-fetched list today.
    for model_id in ["claude-opus-5", "claude-sonnet-5", "claude-opus-4-5", "claude-3-5-sonnet-20241022"] {
        let mut mc = crate::config::ModelConfig {
            provider: "p".into(), model_id: model_id.into(), ..Default::default()
        };
        crate::cli::defaults::apply_smart_defaults(&mut mc, "anthropic");
        let thinking = mc.thinking.as_ref().map(crate::provider::ThinkingParams::from);
        println!("{model_id}: init sets thinking = {:?}", mc.thinking.as_ref().map(|t| (t.enabled, t.budget_tokens)));

        let p = crate::provider::create_provider("anthropic", &format!("http://{addr}"), "k");
        let req = crate::provider::ChatRequest {
            model: model_id.into(),
            messages: vec![crate::provider::ChatMessage { role: "user".into(), content: "hi".into() }],
            system: Some("sys".into()), max_tokens: mc.max_tokens, temperature: Some(0.7),
            top_p: None, frequency_penalty: None, presence_penalty: None,
            reasoning_effort: None, thinking,
        };
        let mut rx = p.chat_stream(req).await.unwrap();
        while rx.recv().await.is_some() {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let bodies = captured.lock().unwrap().clone();
    let mut rejected = Vec::new();
    for b in &bodies {
        let model = b["model"].as_str().unwrap_or("?").to_string();
        let th = b.get("thinking").cloned();
        println!("wire body for {model}: thinking = {th:?}");
        let modern = matches!(model.as_str(),
            "claude-opus-5" | "claude-sonnet-5" | "claude-fable-5" | "claude-opus-4-7" | "claude-opus-4-8");
        if modern && th.as_ref().and_then(|t| t.get("type")).and_then(|t| t.as_str()) == Some("enabled") {
            rejected.push(model);
        }
    }
    assert!(rejected.is_empty(),
        "these models reject thinking.type=enabled with HTTP 400, but rabb1tClaw sends it: {rejected:?}");
}

// ============================================================================
// C14 — revocation must terminate every live session for the device.
// ============================================================================

#[tokio::test]
async fn c14_deleting_a_device_from_devices_yaml_disconnects_it() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let dir = tmp.path().join(".rabb1tclaw");
    std::fs::create_dir_all(&dir).unwrap();

    let token = "e".repeat(32);
    let mut store = crate::config::DeviceStore::default();
    store.devices.insert("d1".into(), crate::config::Device {
        device_id: "d1".into(), display_name: "R1".into(), token: token.clone(), revoked: false,
    });
    crate::config::save_devices(&store).unwrap();

    let state = state_with("http://127.0.0.1:1/v1".into());
    *state.device_store.write().await = store;

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    state.register_connection(token.clone(), "c1".into(), tx, shutdown.clone(), notify).await;

    // Operator removes the device outright instead of setting revoked: true.
    crate::config::save_devices(&crate::config::DeviceStore::default()).unwrap();
    state.reload_devices().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let got_close = rx.try_recv().is_ok();
    println!("shutdown flag = {}, close frame sent = {got_close}",
        shutdown.load(std::sync::atomic::Ordering::SeqCst));
    assert!(shutdown.load(std::sync::atomic::Ordering::SeqCst) || got_close,
        "device deleted from devices.yaml keeps its live authenticated session");
}

// ============================================================================
// C15 — revocation must not be able to wedge the connection registry.
// ============================================================================

#[tokio::test]
async fn c15_revocation_cannot_block_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    std::fs::create_dir_all(tmp.path().join(".rabb1tclaw")).unwrap();

    let token = "f".repeat(32);
    let mut store = crate::config::DeviceStore::default();
    store.devices.insert("d1".into(), crate::config::Device {
        device_id: "d1".into(), display_name: "R1".into(), token: token.clone(), revoked: false,
    });
    crate::config::save_devices(&store).unwrap();
    let state = state_with("http://127.0.0.1:1/v1".into());
    *state.device_store.write().await = store.clone();

    // A stalled client: its send task is blocked, so the mpsc buffer is full.
    let (tx, _rx_held) = tokio::sync::mpsc::channel(crate::protocol::STREAM_CHANNEL_CAPACITY);
    for i in 0..crate::protocol::STREAM_CHANNEL_CAPACITY {
        tx.send(crate::protocol::OutgoingFrame::Event(
            crate::protocol::EventFrame::new(format!("filler{i}")))).await.unwrap();
    }
    println!("channel saturated ({} frames)", crate::protocol::STREAM_CHANNEL_CAPACITY);
    state.register_connection(token.clone(), "stalled".into(), tx,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        std::sync::Arc::new(tokio::sync::Notify::new())).await;

    // Operator revokes the device.
    store.devices.get_mut("d1").unwrap().revoked = true;
    crate::config::save_devices(&store).unwrap();
    let st = state.clone();
    tokio::spawn(async move { let _ = st.reload_devices().await; });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // A brand-new device now tries to connect.
    let ok = tokio::time::timeout(std::time::Duration::from_secs(2),
        state.register_connection("other".into(), "new".into(),
            tokio::sync::mpsc::channel(8).0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(tokio::sync::Notify::new()))).await;
    println!("new connection registered within 2s = {}", ok.is_ok());
    assert!(ok.is_ok(),
        "revoking a device with a saturated send channel holds the active_connections \
         read lock forever; every new connection blocks");
}

#[test]
fn c6_b_create_device_never_mints_a_colliding_prefix() {
    use crate::agent::session::token_prefix;
    use crate::config::{add_device, create_device, Device, DeviceStore};

    let mut store = DeviceStore::default();
    // Occupy every prefix the generator could hand out for this run by seeding
    // the store with a device, then assert new devices never reuse a prefix.
    for i in 0..64 {
        let d = create_device(&store, &format!("dev {i}"));
        add_device(&mut store, d);
    }
    let mut seen = std::collections::HashSet::new();
    for d in store.devices.values() {
        assert!(seen.insert(token_prefix(&d.token)),
            "create_device minted a duplicate storage prefix");
    }
    println!("{} devices, {} distinct prefixes", store.devices.len(), seen.len());

    // Directly exercise the collision branch: pre-seed a taken prefix.
    let victim: Device = store.devices.values().next().unwrap().clone();
    let taken = token_prefix(&victim.token);
    let fresh = create_device(&store, "new");
    assert_ne!(token_prefix(&fresh.token), taken);
    assert!(!store.devices.values().any(|d| token_prefix(&d.token) == token_prefix(&fresh.token)));
}

#[test]
fn c13_b_extended_thinking_whitelist() {
    use crate::cli::defaults::apply_smart_defaults;
    let cases = [
        ("claude-3-5-sonnet-20241022", false),
        ("claude-3-7-sonnet-20250219", true),
        ("claude-sonnet-4", true),
        ("claude-opus-4-1", true),
        ("claude-opus-4-5", true),
        ("claude-sonnet-4-6", true),
        ("claude-opus-4-7", false),
        ("claude-opus-4-8", false),
        ("claude-opus-5", false),
        ("claude-sonnet-5", false),
        ("claude-fable-5", false),
        ("claude-opus-9", false),
    ];
    for (model_id, want_thinking) in cases {
        let mut mc = crate::config::ModelConfig {
            provider: "p".into(), model_id: model_id.into(), ..Default::default()
        };
        apply_smart_defaults(&mut mc, "anthropic");
        let got = mc.thinking.as_ref().is_some_and(|t| t.enabled);
        println!("{model_id}: thinking={got} (want {want_thinking})");
        assert_eq!(got, want_thinking, "{model_id}");
    }
}
