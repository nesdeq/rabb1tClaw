# rabb1tClaw Protocol Specification

**Protocol Version:** 3 (OpenClaw compatible)
**Implementation:** rabb1tClaw v0.3.1

## 1. Overview

Protocol spec for **rabb1tClaw**, a minimal Rust LLM gateway for the Rabbit R1. Implements the core subset of the OpenClaw WebSocket protocol v3 needed for device communication and LLM streaming.

### 1.1 What's Implemented

- Token-based device authentication with constant-time comparison
- Request/response RPC messaging
- Server-pushed events (agent streaming, chat, tick)
- LLM streaming via OpenAI, Anthropic, and OpenAI-compatible endpoints
- Background agent dispatch (code, search, advanced, memory)
- Encrypted session persistence (AES-256-GCM)
- Hot-reload config and device revocation

### 1.2 What's NOT Implemented

- Ed25519 device signatures (nonce generated but not verified)
- Node pairing and remote execution
- Execution approvals
- TTS/Voice wake
- Cron jobs, browser automation, wizard
- Channel integrations
- Config/session management methods (config.get, sessions.list, models.list)
- Chat abort (chat.abort)
- Shutdown event
- Protocol version negotiation (always responds with v3)

---

## 2. Transport Layer

### 2.1 Connection

- **Protocol:** WebSocket (RFC 6455)
- **Default Port:** 18789
- **Endpoints:**
  - `GET /` — WebSocket upgrade
  - `GET /ws` — WebSocket upgrade (alias)
  - `GET /health` — HTTP health check

### 2.2 Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `PROTOCOL_VERSION` | 3 | Protocol version in hello-ok |
| `WS_MAX_PAYLOAD` | 512 KB | Max incoming frame payload |
| `WS_MAX_BUFFERED` | 1.5 MB | Per-connection send buffer limit |
| `TICK_INTERVAL_SECS` | 30 | Tick event interval (seconds) |
| `STREAM_CHANNEL_CAPACITY` | 100 | Internal mpsc buffer for streaming |
| `CONFIG_POLL_SECS` | 2 | Config/device file poll interval |

---

## 3. Frame Format

All frames are JSON-encoded UTF-8 text frames.

### 3.1 Request Frame (Client -> Server)

```json
{
  "type": "req",
  "id": "uuid-string",
  "method": "method-name",
  "params": {}
}
```

### 3.2 Response Frame (Server -> Client)

Success:
```json
{
  "type": "res",
  "id": "uuid-string",
  "ok": true,
  "payload": {}
}
```

Error:
```json
{
  "type": "res",
  "id": "uuid-string",
  "ok": false,
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable message"
  }
}
```

`payload` and `error` are omitted when null (not sent as `null`).

### 3.3 Event Frame (Server -> Client)

```json
{
  "type": "event",
  "event": "event-name",
  "payload": {},
  "seq": 1
}
```

`payload` and `seq` are omitted when null. We do not send `stateVersion` (no presence/health state tracking).

---

## 4. Connection Lifecycle

### 4.1 Flow

```
Client                                    Server
   |                                         |
   |  -------- WebSocket CONNECT --------->  |
   |                                         |
   |  <----- connect.challenge EVENT -----   |
   |         { nonce, ts }                   |
   |                                         |
   |  -------- connect REQUEST ---------->   |
   |         { auth: { token } }             |
   |                                         |
   |  <-------- hello-ok RESPONSE --------   |
   |                                         |
   |  ========= CONNECTED STATE ==========   |
   |                                         |
   |  <---------- tick EVENT (30s) -------   |
   |  -------- method REQUEST ----------->   |
   |  <-- accepted RESPONSE (immediate) --   |
   |  <-- agent/chat EVENTS (streaming) --   |
   |  <----- final RESPONSE (complete) ---   |
```

### 4.2 Challenge Event

Sent immediately on WebSocket open:

```json
{
  "type": "event",
  "event": "connect.challenge",
  "payload": {
    "nonce": "550e8400-e29b-41d4-a716-446655440000",
    "ts": 1707148800000
  }
}
```

The nonce is a UUID v4. Not cryptographically verified in this implementation.

### 4.3 Connect Request

Client MUST send `connect` as first request. Extra fields (client info, device signatures, role, scopes, etc.) are accepted and silently ignored.

```json
{
  "type": "req",
  "id": "uuid",
  "method": "connect",
  "params": {
    "auth": {
      "token": "device-token-from-onboarding"
    }
  }
}
```

### 4.4 Hello-OK Response

```json
{
  "type": "res",
  "id": "matches-connect-id",
  "ok": true,
  "payload": {
    "type": "hello-ok",
    "protocol": 3,
    "server": {
      "version": "0.3.1",
      "connId": "a1b2c3d4"
    },
    "features": {
      "methods": ["health", "agent", "chat.send", "chat.history"],
      "events": ["agent", "chat", "tick"]
    },
    "snapshot": {
      "configPath": "~/.rabb1tclaw/config.yaml",
      "stateDir": "~/.rabb1tclaw"
    },
    "policy": {
      "maxPayload": 524288,
      "maxBufferedBytes": 1572864,
      "tickIntervalMs": 30000
    },
    "auth": {
      "deviceToken": "the-token",
      "issuedAtMs": 1707148800000
    }
  }
}
```

`auth` is omitted for local connections without a device token. `connId` is an 8-character UUID prefix, unique per connection.

### 4.5 Tick Events

Server sends every 30 seconds after successful auth. Sequence starts at 1.

```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1707148830000 },
  "seq": 1
}
```

### 4.6 WebSocket Close Codes

| Code | Meaning |
|------|---------|
| 1000 | Normal closure |
| 1008 | Policy violation (auth failed, device revoked) |

---

## 5. Authentication

### 5.1 Methods

1. **Device Token** — 32-character hex string from CLI onboarding. Compared using constant-time equality.
2. **Local Bypass** — Loopback connections (`127.x.x.x`, `::1`, `::ffff:127.x.x.x`) allowed when no devices are configured.

### 5.2 Auth Failure Reasons

| Reason | Close | Description |
|--------|-------|-------------|
| `device_revoked` | 1008 | Token matches a revoked device |
| `device_token_invalid` | 1008 | Token matches no device |
| `device_token_missing` | stays open | No token but devices exist (awaits pairing) |

### 5.3 Hot Revocation

Editing `~/.rabb1tclaw/devices.yaml` to set `revoked: true` immediately disconnects the device on the next 2-second config poll.

---

## 6. Methods

### 6.1 Summary

| Method | Description |
|--------|-------------|
| `connect` | Authentication handshake |
| `health` | Server health/uptime |
| `agent` | Invoke LLM (streaming) |
| `chat.send` | Alias for `agent` |
| `chat.history` | Get conversation history |

Unknown methods return `NOT_FOUND` error.

### 6.2 `health`

**Params:** none

**Response:**
```json
{
  "ok": true,
  "uptimeMs": 123456,
  "version": "0.3.1"
}
```

### 6.3 `agent` / `chat.send`

Primary LLM invocation with streaming response.

**Params:**
```json
{
  "message": "What's the weather?",
  "idempotencyKey": "unique-request-id"
}
```

**Immediate accepted response:**
```json
{
  "type": "res",
  "id": "request-id",
  "ok": true,
  "payload": {
    "runId": "unique-request-id",
    "status": "accepted",
    "acceptedAt": 1707148800000
  }
}
```

**Streaming:** See section 7.2 for agent and chat events.

**Final response** (after stream completes):
```json
{
  "type": "res",
  "id": "request-id",
  "ok": true,
  "payload": {
    "runId": "unique-request-id",
    "status": "ok",
    "summary": "completed",
    "result": {
      "assistantTexts": ["The complete response text..."]
    }
  }
}
```

**Idempotency:** Duplicate `idempotencyKey` while a request is in-flight blocks until the first request completes.

### 6.4 `chat.history`

**Params:** none (uses device token to identify session)

**Response:**
```json
{
  "messages": [
    {
      "role": "user",
      "content": [{ "type": "text", "text": "Hello" }],
      "timestamp": 1707148800000,
      "runId": "abc12345"
    },
    {
      "role": "assistant",
      "content": [{ "type": "text", "text": "Hi there!" }],
      "timestamp": 1707148801000,
      "runId": "abc12345"
    }
  ],
  "hasMore": false
}
```

Returns `{"messages": [], "hasMore": false}` if no device token is present.

---

## 7. Events

### 7.1 `connect.challenge`

Sent immediately on WebSocket open. See section 4.2.

### 7.2 Agent/Chat Streaming Events

Each streaming text chunk emits two events (agent + chat):

**Agent delta:**
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 1,
    "stream": "assistant",
    "ts": 1707148800100,
    "data": { "delta": "Hello" }
  }
}
```

**Chat delta:**
```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "request-id",
    "sessionKey": "default:main",
    "seq": 1,
    "state": "delta",
    "delta": "Hello"
  }
}
```

### 7.3 Agent Lifecycle Events

**Start** (emitted before streaming begins):
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 0,
    "stream": "lifecycle",
    "ts": 1707148800000,
    "data": { "phase": "start", "startedAt": 1707148800000 }
  }
}
```

**End** (emitted after last delta):
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 10,
    "stream": "lifecycle",
    "ts": 1707148801000,
    "data": {
      "phase": "end",
      "startedAt": 1707148800000,
      "endedAt": 1707148801000
    }
  }
}
```

**Error:**
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 1,
    "stream": "lifecycle",
    "ts": 1707148800500,
    "data": { "phase": "error", "error": "no LLM model configured" }
  }
}
```

### 7.4 Chat Terminal Events

**Final** (emitted after agent end):
```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "request-id",
    "sessionKey": "default:main",
    "seq": 11,
    "state": "final",
    "message": {
      "role": "assistant",
      "content": [{ "type": "text", "text": "Complete response..." }],
      "timestamp": 1707148801000
    }
  }
}
```

**Error:**
```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "request-id",
    "sessionKey": "default:main",
    "seq": 2,
    "state": "error",
    "errorMessage": "no LLM model configured"
  }
}
```

### 7.5 `tick`

See section 4.5.

### 7.6 Event Sequence

Complete sequence for a successful `agent` / `chat.send` call:

1. `res` (accepted) — immediate
2. `agent` lifecycle start (seq 0)
3. `agent` delta + `chat` delta (seq 1, 2, 3, ...) — per text chunk
4. `agent` lifecycle end (seq N)
5. `chat` final (seq N+1)
6. `res` (final, ok) — includes full response text

On error:

1. `res` (accepted) — if we got that far
2. `agent` lifecycle error (seq N)
3. `chat` error (seq N+1)
4. `res` (error) — with error code and message

---

## 8. Error Handling

### 8.1 Error Codes

| Code | Description |
|------|-------------|
| `INVALID_REQUEST` | Bad params, parse error, or auth failure reason |
| `UNAUTHORIZED` | Method called before `connect` handshake |
| `NOT_FOUND` | Unknown method |
| `UNAVAILABLE` | LLM provider error, stream failure |
| `INTERNAL_ERROR` | Unhandled server error |

### 8.2 Error Response Example

```json
{
  "type": "res",
  "id": "request-id",
  "ok": false,
  "error": {
    "code": "NOT_FOUND",
    "message": "unknown method: foo"
  }
}
```

Parse errors use id `"unknown"` since the request id couldn't be extracted.

---

## 9. Differences from OpenClaw

| Area | OpenClaw | rabb1tClaw |
|------|----------|------------|
| **Payload limits** | 25 MiB / 50 MiB | 512 KB / 1.5 MB |
| **Methods** | 92+ | 5 (connect, health, agent, chat.send, chat.history) |
| **Auth** | Token, password, Ed25519 signatures, TLS fingerprint | Token + loopback bypass |
| **HelloOk snapshot** | Full presence, health, sessions, stateVersion | configPath, stateDir only |
| **ErrorShape** | code, message, details, retryable, retryAfterMs | code, message only |
| **Handshake timeout** | 10s | None |
| **Shutdown event** | Sent before graceful stop | Not sent |
| **Protocol negotiation** | Validates minProtocol/maxProtocol range | Ignores, always responds v3 |
| **stateVersion on events** | Optional on all events | Never sent |

---

## Appendix A: HTTP Health Endpoint

```
GET /health
```

```json
{
  "ok": true,
  "version": "0.3.1"
}
```

## Appendix B: Configuration

### Config: `~/.rabb1tclaw/config.yaml`

```yaml
gateway:
  port: 18789
  bind: "0.0.0.0"

providers:
  openai:
    api: openai
    base_url: https://api.openai.com/v1
    api_key: sk-...

models:
  gpt5:
    provider: openai
    model_id: gpt-5.2
    max_tokens: 16384
    temperature: 0.7

active_model: gpt5
```

### Devices: `~/.rabb1tclaw/devices.yaml`

```yaml
devices:
  abc123def456:
    device_id: abc123def456
    display_name: "Rabbit R1"
    token: "32-char-hex-token"
    revoked: false
```

## Appendix C: CLI

```
rabb1tclaw                            Start server (init if no config)
rabb1tclaw init                       Interactive setup
rabb1tclaw server --stop              Stop running server
rabb1tclaw server --restart           Hot-reload config (SIGHUP)
rabb1tclaw server --get-ip            Print current bind IP
rabb1tclaw server --set-ip <IP>       Change bind IP
rabb1tclaw server --debug             Enable debug + protocol dump logging
rabb1tclaw devices --list             List paired devices
rabb1tclaw devices --onboard          Add device + QR code
rabb1tclaw devices --revoke <ID>      Revoke a device
rabb1tclaw devices --revoke-all       Revoke all devices
rabb1tclaw providers --list           List configured providers
rabb1tclaw providers --add            Add a new provider
rabb1tclaw providers --remove <NAME>  Remove a provider
rabb1tclaw models --list              List configured models
rabb1tclaw models --add               Add a new model
rabb1tclaw models --remove <KEY>      Remove a model
rabb1tclaw models --set-active <KEY>  Set the active model
rabb1tclaw models --edit <KEY>        Edit model parameters
```
