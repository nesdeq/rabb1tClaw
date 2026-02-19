> **Source:** [github.com/openclaw/openclaw](https://github.com/openclaw/openclaw)
> Synthesized from TypeBox schemas in `src/gateway/protocol/schema/`, method/event registries, server constants, and connection handler source code.
> Last synced: 2026-02-19

# OpenClaw Gateway WebSocket Protocol v3

## 1. Overview

The Gateway WS protocol is the single control plane and node transport for OpenClaw. All clients (CLI, web UI, macOS app, iOS/Android nodes, headless nodes) connect over WebSocket and declare their role + scope at handshake time.

- **Protocol Version:** `3`
- **Transport:** WebSocket, text frames with JSON payloads
- **Default endpoint:** `ws://127.0.0.1:18789`

## 2. Frame Format

All communication uses JSON-encoded WebSocket text frames. Three frame types, discriminated on the `type` field:

### 2.1 Request Frame (Client -> Server)

```json
{
  "type": "req",
  "id": "<non-empty-uuid>",
  "method": "<method-name>",
  "params": {}
}
```

### 2.2 Response Frame (Server -> Client)

```json
{
  "type": "res",
  "id": "<matches-request-id>",
  "ok": true,
  "payload": {},
  "error": null
}
```

A response may carry `status: "accepted"` as an intermediate acknowledgment for streaming methods; the final response arrives later.

### 2.3 Event Frame (Server -> Client)

```json
{
  "type": "event",
  "event": "<event-name>",
  "payload": {},
  "seq": 0,
  "stateVersion": { "presence": 0, "health": 0 }
}
```

### 2.4 Error Shape

```json
{
  "code": "<error-code>",
  "message": "<human-readable>",
  "details": {},
  "retryable": false,
  "retryAfterMs": 5000
}
```

Error codes: `NOT_LINKED`, `NOT_PAIRED`, `AGENT_TIMEOUT`, `INVALID_REQUEST`, `UNAVAILABLE`.

## 3. Connection Handshake

### 3.1 Server Challenge

Immediately upon WebSocket open:

```json
{
  "type": "event",
  "event": "connect.challenge",
  "payload": { "nonce": "<uuid-v4>", "ts": 1737264000000 }
}
```

Client must complete handshake within `DEFAULT_HANDSHAKE_TIMEOUT_MS` (10,000ms) or the connection is closed with cause `"handshake-timeout"`.

### 3.2 Client Connect

```json
{
  "type": "req",
  "id": "<uuid>",
  "method": "connect",
  "params": {
    "minProtocol": 3,
    "maxProtocol": 3,
    "client": {
      "id": "<client-id>",
      "displayName": "optional",
      "version": "1.2.3",
      "platform": "macos",
      "deviceFamily": "optional",
      "modelIdentifier": "optional",
      "mode": "<client-mode>",
      "instanceId": "optional"
    },
    "caps": ["camera", "canvas", "screen", "location", "voice"],
    "commands": ["camera.snap", "canvas.navigate"],
    "permissions": { "camera.capture": true },
    "role": "operator",
    "scopes": ["operator.read", "operator.write"],
    "device": {
      "id": "device_fingerprint",
      "publicKey": "<base64url>",
      "signature": "<base64url>",
      "signedAt": 1737264000000,
      "nonce": "<must-match-challenge>"
    },
    "auth": {
      "token": "optional gateway token",
      "password": "optional gateway password"
    },
    "locale": "en-US",
    "userAgent": "openclaw-cli/1.2.3"
  }
}
```

### 3.3 Server Hello-OK

```json
{
  "type": "res",
  "id": "<matches-connect-request-id>",
  "ok": true,
  "payload": {
    "type": "hello-ok",
    "protocol": 3,
    "server": {
      "version": "1.2.3",
      "commit": "abc1234",
      "host": "hostname",
      "connId": "<connection-id>"
    },
    "features": {
      "methods": ["health", "agent", "chat.send"],
      "events": ["connect.challenge", "agent", "chat"]
    },
    "snapshot": {
      "presence": [],
      "health": {},
      "stateVersion": { "presence": 0, "health": 0 },
      "uptimeMs": 123456,
      "configPath": "~/.openclaw/config.json5",
      "stateDir": "~/.openclaw",
      "sessionDefaults": {
        "defaultAgentId": "main",
        "mainKey": "main",
        "mainSessionKey": "agent:main:main",
        "scope": "per-sender"
      },
      "authMode": "token",
      "updateAvailable": null
    },
    "canvasHostUrl": "http://127.0.0.1:18789/__openclaw__/canvas/",
    "auth": {
      "deviceToken": "<issued-token>",
      "role": "operator",
      "scopes": ["operator.read", "operator.write"],
      "issuedAtMs": 1737264000000
    },
    "policy": {
      "maxPayload": 26214400,
      "maxBufferedBytes": 52428800,
      "tickIntervalMs": 30000
    }
  }
}
```

## 4. Authentication

### 4.1 Auth Modes

| Mode | Description |
|------|-------------|
| `none` | No authentication required |
| `token` | Bearer token validation |
| `password` | Shared password |
| `trusted-proxy` | Proxy-delegated identity |

### 4.2 Device Signatures

Device signature payload is pipe-delimited:
```
v2|<deviceId>|<clientId>|<clientMode>|<role>|<scope1,scope2>|<signedAt>|<token>|<nonce>
```

- **v1** (legacy, loopback only): no nonce
- **v2**: includes server-provided challenge nonce
- **Timestamp skew tolerance:** 10 minutes

### 4.3 Device Token Lifecycle

After pairing, the gateway issues a device token in `hello-ok.auth.deviceToken`. Tokens can be rotated (`device.token.rotate`) or revoked (`device.token.revoke`).

### 4.4 Rate Limiting

Two scopes: shared secret attempts and device token verification. Failures increment counters; successes reset them.

## 5. Keepalive

### 5.1 Server Tick

```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1737264030000 },
  "seq": 1
}
```

**Default interval:** 30,000ms. Communicated in `hello-ok.policy.tickIntervalMs`.

### 5.2 Client Tick Watch

If no tick within 2x `tickIntervalMs`, the client closes with close code **4000**.

### 5.3 Shutdown Event

```json
{
  "type": "event",
  "event": "shutdown",
  "payload": { "reason": "restart", "restartExpectedMs": 5000 }
}
```

## 6. Server Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `MAX_PAYLOAD_BYTES` | 26,214,400 (25 MiB) | Max single frame size |
| `MAX_BUFFERED_BYTES` | 52,428,800 (50 MiB) | Max per-connection send buffer |
| `DEFAULT_HANDSHAKE_TIMEOUT_MS` | 10,000 | Handshake deadline |
| `TICK_INTERVAL_MS` | 30,000 | Server tick interval |
| `DEDUPE_TTL_MS` | 300,000 (5 min) | Idempotency key TTL |
| `DEDUPE_MAX` | 1,000 | Max tracked idempotency keys |

## 7. WebSocket Close Codes

| Code | Meaning |
|------|---------|
| 1002 | Protocol error (frame validation) |
| 1008 | Semantic error (auth failure, invalid role) |
| 4000 | Client tick timeout |

## 8. Device Pairing

1. Device connects with unknown `device.id`
2. Gateway stores pending request, emits `device.pair.requested` event
3. Operator approves via `device.pair.approve` or rejects via `device.pair.reject`
4. On approval, device token issued; device reconnects with token
5. Pending requests expire after 5 minutes
6. Local clients get silent auto-approval

## 9. Chat Events

### 9.1 Chat Delta

```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "<run-id>",
    "sessionKey": "<session-key>",
    "seq": 1,
    "state": "delta",
    "delta": "incremental text"
  }
}
```

### 9.2 Chat Final

```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "<run-id>",
    "sessionKey": "<session-key>",
    "seq": 10,
    "state": "final",
    "message": {},
    "usage": {},
    "stopReason": "end_turn"
  }
}
```

### 9.3 Chat Error

```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "<run-id>",
    "sessionKey": "<session-key>",
    "seq": 11,
    "state": "error",
    "errorMessage": "something went wrong"
  }
}
```

### 9.4 Chat Aborted

```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "<run-id>",
    "sessionKey": "<session-key>",
    "seq": 11,
    "state": "aborted"
  }
}
```

States: multiple `delta` events, then exactly one terminal event (`final`, `aborted`, or `error`).

## 10. Agent Events

```json
{
  "type": "event",
  "event": "agent",
  "payload": {
    "runId": "<run-id>",
    "seq": 0,
    "stream": "lifecycle|assistant",
    "ts": 1737264000000,
    "data": { "phase": "start|end|error", "delta": "..." }
  }
}
```

## 11. Method Registry (92+ methods)

### Core

| Method | Description |
|--------|-------------|
| `health` | Gateway health check |
| `agent` | Invoke agent with full control |
| `chat.send` | Send a chat message (streaming) |
| `chat.history` | Get chat history |
| `chat.abort` | Abort an active chat run |

### Sessions

`sessions.list`, `sessions.preview`, `sessions.patch`, `sessions.reset`, `sessions.delete`, `sessions.compact`

### Models & Agents

`models.list`, `agents.list`, `agents.create`, `agents.update`, `agents.delete`, `agents.files.list`, `agents.files.get`, `agents.files.set`

### Configuration

`config.get`, `config.set`, `config.apply`, `config.patch`, `config.schema`

### Device Pairing

`device.pair.list`, `device.pair.approve`, `device.pair.reject`, `device.pair.remove`, `device.token.rotate`, `device.token.revoke`

### Node Management

`node.pair.request`, `node.pair.list`, `node.pair.approve`, `node.pair.reject`, `node.pair.verify`, `node.rename`, `node.list`, `node.describe`, `node.invoke`, `node.invoke.result`, `node.event`

### Execution Approvals

`exec.approval.request`, `exec.approval.waitDecision`, `exec.approval.resolve`, `exec.approvals.get`, `exec.approvals.set`, `exec.approvals.node.get`, `exec.approvals.node.set`

### TTS / Voice

`tts.status`, `tts.providers`, `tts.enable`, `tts.disable`, `tts.convert`, `tts.setProvider`, `talk.config`, `talk.mode`, `voicewake.get`, `voicewake.set`

### Other

`status`, `usage.status`, `usage.cost`, `logs.tail`, `channels.status`, `channels.logout`, `wizard.start`, `wizard.next`, `wizard.cancel`, `wizard.status`, `cron.list`, `cron.status`, `cron.add`, `cron.update`, `cron.remove`, `cron.run`, `cron.runs`, `update.run`, `last-heartbeat`, `set-heartbeats`, `wake`, `send`, `agent.identity.get`, `agent.wait`, `browser.request`, `system-presence`, `system-event`

## 12. Event Registry (19 events)

| Event | Description |
|-------|-------------|
| `connect.challenge` | Pre-handshake challenge |
| `agent` | Agent streaming events |
| `chat` | Chat streaming events |
| `presence` | Presence snapshot updated |
| `tick` | Server keepalive tick |
| `talk.mode` | Talk mode toggled |
| `shutdown` | Graceful shutdown |
| `health` | Health state changed |
| `heartbeat` | Heartbeat completed |
| `cron` | Cron job event |
| `node.pair.requested` | Node pairing requested |
| `node.pair.resolved` | Node pairing resolved |
| `node.invoke.request` | Command invocation to node |
| `device.pair.requested` | Device pairing requested |
| `device.pair.resolved` | Device pairing resolved |
| `voicewake.changed` | Voice wake config changed |
| `exec.approval.requested` | Exec approval needed |
| `exec.approval.resolved` | Exec approval decided |
| `update.available` | Software update available |

## 13. Idempotency

Side-effecting methods require `idempotencyKey` in params. TTL: 5 minutes, max 1,000 tracked keys.

## 14. Reconnection

Exponential backoff: 1s start, 30s cap. On device token mismatch, client clears stored credentials. Sequence gap detection via `lastSeq` tracking.

## 15. Scope Guards

| Scope | Guards |
|-------|--------|
| `operator.read` | Health, logs, status queries |
| `operator.write` | Send messages, chat, TTS |
| `operator.admin` | Config changes, updates |
| `operator.approvals` | Exec approval methods/events |
| `operator.pairing` | Pairing methods |
