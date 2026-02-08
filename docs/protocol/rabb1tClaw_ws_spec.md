# rabb1tClaw Protocol Specification

**Version:** 1.0
**Protocol Version:** 3
**Implementation:** rabb1tClaw

## Table of Contents

1. [Overview](#1-overview)
2. [Transport Layer](#2-transport-layer)
3. [Frame Format](#3-frame-format)
4. [Connection Lifecycle](#4-connection-lifecycle)
5. [Authentication](#5-authentication)
6. [Methods Reference](#6-methods-reference)
7. [Events Reference](#7-events-reference)
8. [Error Handling](#8-error-handling)

---

## 1. Overview

This is the protocol specification for **rabb1tClaw**, a minimal Rust LLM gateway for Rabbit R1 and other devices. It supports:

- **Token-based device authentication**
- **Request/response** RPC-style messaging
- **Server-pushed events** for real-time updates
- **LLM streaming** via OpenAI and Anthropic APIs

### 1.1 What's NOT Implemented

The following features from the full OpenClaw spec are **not implemented**:

- ED25519 cryptographic signatures
- Node pairing and remote execution
- Tailscale authentication
- Execution approvals
- TTS/Voice wake
- Cron jobs
- Browser automation
- Wizard onboarding
- Channel integrations
- Most config/session management

---

## 2. Transport Layer

### 2.1 Connection

- **Protocol:** WebSocket (RFC 6455)
- **Default Port:** 18789
- **URLs:**
  - `ws://127.0.0.1:18789/` or `/ws` (local)
  - `ws://host:18789/` (LAN)

### 2.2 Payload Limits

| Constant | Value | Description |
|----------|-------|-------------|
| `MAX_PAYLOAD_BYTES` | 512 KB | Maximum incoming frame size |
| `MAX_BUFFERED_BYTES` | 1.5 MB | Per-connection send buffer limit |

### 2.3 Timing Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `TICK_INTERVAL_MS` | 30,000 ms | Server heartbeat interval |

---

## 3. Frame Format

All frames are JSON-encoded UTF-8 strings.

### 3.1 Request Frame (Client → Server)

```json
{
  "type": "req",
  "id": "uuid-string",
  "method": "method-name",
  "params": { }
}
```

### 3.2 Response Frame (Server → Client)

**Success:**
```json
{
  "type": "res",
  "id": "uuid-string",
  "ok": true,
  "payload": { }
}
```

**Error:**
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

### 3.3 Event Frame (Server → Client)

```json
{
  "type": "event",
  "event": "event-name",
  "payload": { },
  "seq": 1,
  "stateVersion": {
    "presence": 0,
    "health": 0
  }
}
```

---

## 4. Connection Lifecycle

### 4.1 Flow Diagram

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
   |  <---------- tick EVENT -------------   |
   |  -------- method REQUEST ----------->   |
   |  <-------- method RESPONSE ----------   |
```

### 4.2 Challenge Event

Sent immediately on WebSocket connection:

```json
{
  "type": "event",
  "event": "connect.challenge",
  "payload": {
    "nonce": "uuid",
    "ts": 1707148800000
  }
}
```

**Note:** The nonce is generated but not cryptographically verified in this implementation.

### 4.3 Connect Request

Client MUST send `connect` as first request:

```json
{
  "type": "req",
  "id": "uuid",
  "method": "connect",
  "params": {
    "minProtocol": 3,
    "maxProtocol": 3,
    "client": {
      "id": "client-id",
      "displayName": "Device Name",
      "version": "1.0.0",
      "platform": "android",
      "mode": "ui"
    },
    "auth": {
      "token": "device-token-from-onboarding"
    }
  }
}
```

### 4.4 Hello-Ok Response

```json
{
  "type": "hello-ok",
  "protocol": 3,
  "server": {
    "version": "0.1.0",
    "connId": "abc12345"
  },
  "features": {
    "methods": ["health", "config.get", "agent", "chat.send", "chat.history", "chat.abort", "sessions.list", "models.list"],
    "events": ["agent", "chat", "tick", "presence", "health"]
  },
  "snapshot": {
    "presence": [],
    "health": {},
    "stateVersion": { "presence": 0, "health": 0 },
    "uptimeMs": 0,
    "configPath": "~/.rustgw/config.yaml",
    "stateDir": "~/.rustgw",
    "sessionDefaults": {
      "defaultAgentId": "default",
      "mainKey": "main",
      "mainSessionKey": "default:main"
    }
  },
  "policy": {
    "maxPayload": 524288,
    "maxBufferedBytes": 1572864,
    "tickIntervalMs": 30000
  },
  "auth": {
    "deviceToken": "the-token",
    "role": "operator",
    "scopes": ["operator.admin"]
  }
}
```

### 4.5 Tick Events

Server sends every 30 seconds:

```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1707148800000 },
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

### 5.1 Authentication Methods

This implementation supports:

1. **Device Token** - 32-character hex string from onboarding
2. **Local Bypass** - Loopback connections allowed if no devices configured

### 5.2 Token Validation

- Tokens compared using constant-time comparison (timing attack prevention)
- Revoked devices rejected with close code 1008
- Hot-reload: editing `~/.rustgw/devices.yaml` immediately disconnects revoked devices

### 5.3 Local Connection Detection

Loopback addresses bypass auth when no devices exist:
- `127.0.0.1`
- `127.x.x.x`
- `::1`
- `::ffff:127.x.x.x`

---

## 6. Methods Reference

### 6.1 Implemented Methods

| Method | Description |
|--------|-------------|
| `connect` | Authentication handshake |
| `health` | Server health/uptime |
| `config.get` | Read config values |
| `agent` | Invoke LLM (streaming) |
| `chat.send` | Alias for agent |
| `chat.history` | Get chat history (stub - returns empty) |
| `chat.abort` | Abort running request |
| `sessions.list` | List sessions |
| `models.list` | List available models |

### 6.2 Method: `health`

**Params:** `{}`

**Response:**
```json
{
  "ok": true,
  "uptimeMs": 123456,
  "version": "0.1.0"
}
```

### 6.3 Method: `config.get`

**Params:** `{ "key": "gateway.port" }` (optional)

**Response:**
```json
{
  "key": "gateway.port",
  "value": 18789
}
```

### 6.4 Method: `agent`

Primary method for LLM invocation with streaming.

**Params:**
```json
{
  "message": "Hello, how are you?",
  "idempotencyKey": "unique-request-id",
  "agentId": "default",
  "extraSystemPrompt": "You are helpful."
}
```

**Immediate Response:**
```json
{
  "runId": "unique-request-id",
  "status": "accepted",
  "acceptedAt": 1707148800000
}
```

**Streaming Events:** See §7.2

**Final Response:**
```json
{
  "runId": "unique-request-id",
  "status": "ok",
  "summary": "completed",
  "result": {
    "assistantTexts": ["The complete response text..."]
  }
}
```

### 6.5 Method: `chat.send`

Alias for `agent` method.

### 6.6 Method: `chat.history`

**Params:** `{}`

**Response:**
```json
{
  "messages": [],
  "hasMore": false
}
```

### 6.7 Method: `chat.abort`

**Params:** `{ "runId": "the-run-id" }`

**Response:**
```json
{
  "runId": "the-run-id",
  "aborted": true
}
```

### 6.8 Method: `sessions.list`

**Params:** `{}`

**Response:**
```json
{
  "sessions": []
}
```

### 6.9 Method: `models.list`

**Params:** `{}`

**Response:**
```json
{
  "models": [
    {
      "id": "gpt-5.2",
      "name": "GPT-5.2",
      "provider": "openai",
      "reasoning": false,
      "contextWindow": 128000,
      "maxTokens": 4096
    }
  ]
}
```

---

## 7. Events Reference

### 7.1 Connection Events

#### `connect.challenge`

Sent immediately on connection.

```json
{
  "type": "event",
  "event": "connect.challenge",
  "payload": {
    "nonce": "uuid",
    "ts": 1707148800000
  }
}
```

### 7.2 Agent/Chat Events

#### `agent` (lifecycle)

Start:
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

End:
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 10,
    "stream": "lifecycle",
    "ts": 1707148800000,
    "data": { "phase": "end", "startedAt": 1707148800000, "endedAt": 1707148801000 }
  }
}
```

#### `agent` (streaming)

Text delta:
```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "request-id",
    "seq": 1,
    "stream": "assistant",
    "ts": 1707148800000,
    "data": { "delta": "Hello" }
  }
}
```

#### `chat` (streaming)

Delta:
```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "request-id",
    "sessionKey": "default:main",
    "seq": 1,
    "state": "delta",
    "message": {
      "role": "assistant",
      "content": [{ "type": "text", "text": "Hello..." }],
      "timestamp": 1707148800000
    }
  }
}
```

Final:
```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "request-id",
    "sessionKey": "default:main",
    "seq": 10,
    "state": "final",
    "message": {
      "role": "assistant",
      "content": [{ "type": "text", "text": "Complete response..." }],
      "timestamp": 1707148801000
    }
  }
}
```

### 7.3 Heartbeat Events

#### `tick`

```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1707148800000 },
  "seq": 1
}
```

---

## 8. Error Handling

### 8.1 Error Codes

| Code | Description |
|------|-------------|
| `INVALID_REQUEST` | Bad params, parse error, or unauthorized |
| `UNAUTHORIZED` | Not connected (send `connect` first) |
| `NOT_FOUND` | Unknown method |
| `UNAVAILABLE` | LLM provider error |
| `INTERNAL_ERROR` | Server error |

### 8.2 Auth Failure Reasons

| Reason | Description |
|--------|-------------|
| `device_revoked` | Device token was revoked |
| `device_token_invalid` | Token doesn't match any device |
| `device_token_missing` | No token provided but devices exist |

### 8.3 Example Error Response

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

---

## Appendix A: Configuration

### Native Config: `~/.rustgw/config.yaml`

```yaml
gateway:
  port: 18789
  bind: lan  # or "loopback"

providers:
  openai:
    api: openai
    base_url: https://api.openai.com/v1
    api_key: sk-...
    model: gpt-5.2
    name: OpenAI

active_provider: openai
```

### Devices: `~/.rustgw/devices.yaml`

```yaml
devices:
  abc123def456:
    device_id: abc123def456
    display_name: "Rabbit R1"
    token: "32-char-hex-token"
    role: operator
    created_at_ms: 1707148800000
    last_connected_ms: null
    revoked: false
```

---

## Appendix B: CLI Commands

```
rabb1tclaw [OPTIONS]

--onboard, -n       Add new device (generates QR code)
--list-devices, -l  List paired devices
--revoke, -r <ID>   Revoke device by ID or token
--revoke-all, -R    Revoke all devices
--override, -o      Bypass config, use OpenAI gpt-5.2 directly
--help, -h          Show help
```

---

## Appendix C: Health Endpoint

HTTP health check available at:

```
GET http://host:18789/health
```

Response:
```json
{
  "ok": true,
  "version": "0.1.0"
}
```
