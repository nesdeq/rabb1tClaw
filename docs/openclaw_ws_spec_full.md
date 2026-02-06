# OpenClaw Gateway WebSocket Protocol Specification

**Version:** 1.0
**Protocol Version:** 3
**Last Updated:** 2026-02-05

## Table of Contents

1. [Overview](#1-overview)
2. [Transport Layer](#2-transport-layer)
3. [Frame Format](#3-frame-format)
4. [Connection Lifecycle](#4-connection-lifecycle)
5. [Authentication & Authorization](#5-authentication--authorization)
6. [Device Identity & Pairing](#6-device-identity--pairing)
7. [Node Pairing](#7-node-pairing)
8. [Methods Reference](#8-methods-reference)
9. [Events Reference](#9-events-reference)
10. [Error Handling](#10-error-handling)
11. [State Management](#11-state-management)
12. [Security Considerations](#12-security-considerations)

---

## 1. Overview

The OpenClaw Gateway WebSocket Protocol enables real-time bidirectional communication between clients (mobile apps, CLI, web UI) and the OpenClaw Gateway server. The protocol supports:

- **Device authentication** via ED25519 cryptographic signatures
- **Role-based access control** with scoped permissions
- **Request/response** RPC-style messaging
- **Server-pushed events** for real-time updates
- **Node registration** for remote command execution
- **Presence tracking** for connected clients

### 1.1 Terminology

| Term | Definition |
|------|------------|
| **Gateway** | The OpenClaw server that manages messaging, agents, and client connections |
| **Client** | Any application connecting to the Gateway (mobile app, CLI, web UI) |
| **Device** | A client's cryptographic identity (ED25519 keypair) |
| **Node** | A remote execution endpoint that can run commands on behalf of the Gateway |
| **Operator** | A client role with administrative capabilities |

---

## 2. Transport Layer

### 2.1 Connection

- **Protocol:** WebSocket (RFC 6455)
- **Default Port:** 18789
- **URLs:**
  - `ws://127.0.0.1:18789` (local)
  - `wss://host:port` (TLS)

### 2.2 Payload Limits

| Constant | Value | Description |
|----------|-------|-------------|
| `MAX_PAYLOAD_BYTES` | 512 KB | Maximum incoming frame size |
| `MAX_BUFFERED_BYTES` | 1.5 MB | Per-connection send buffer limit |

### 2.3 Timing Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `HANDSHAKE_TIMEOUT_MS` | 10,000 ms | Time allowed for handshake completion |
| `TICK_INTERVAL_MS` | 30,000 ms | Server heartbeat interval |
| `PENDING_TTL_MS` | 300,000 ms | Pairing request expiration (5 minutes) |
| `DEVICE_SIGNATURE_SKEW_MS` | 600,000 ms | Signature timestamp tolerance (±10 minutes) |

---

## 3. Frame Format

All frames are JSON-encoded UTF-8 strings. The protocol uses a discriminated union based on the `type` field.

### 3.1 Request Frame

Clients send requests to invoke server methods.

```typescript
{
  type: "req",
  id: string,        // UUID for correlating responses
  method: string,    // Method name to invoke
  params?: unknown   // Optional parameters (schema-validated per method)
}
```

**Example:**
```json
{
  "type": "req",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "method": "health",
  "params": {}
}
```

### 3.2 Response Frame

Server responds to client requests.

```typescript
{
  type: "res",
  id: string,           // Matches request ID
  ok: boolean,          // Success indicator
  payload?: unknown,    // Success payload (when ok=true)
  error?: ErrorShape    // Error details (when ok=false)
}
```

**Success Example:**
```json
{
  "type": "res",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "ok": true,
  "payload": { "status": "healthy" }
}
```

**Error Example:**
```json
{
  "type": "res",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "ok": false,
  "error": {
    "code": "INVALID_REQUEST",
    "message": "unknown method: foo",
    "retryable": false
  }
}
```

### 3.3 Event Frame

Server pushes events to clients.

```typescript
{
  type: "event",
  event: string,                // Event name
  payload?: unknown,            // Event-specific data
  seq?: number,                 // Sequence number for gap detection
  stateVersion?: {
    presence: number,           // Presence state version
    health: number              // Health state version
  }
}
```

**Example:**
```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1707148800000 },
  "stateVersion": { "presence": 42, "health": 17 }
}
```

### 3.4 Error Shape

```typescript
{
  code: string,              // Error code (see §10)
  message: string,           // Human-readable message
  details?: unknown,         // Additional context
  retryable?: boolean,       // Whether client should retry
  retryAfterMs?: number      // Suggested retry delay
}
```

---

## 4. Connection Lifecycle

### 4.1 Connection Flow Diagram

```
Client                                    Server
   |                                         |
   |  -------- WebSocket CONNECT --------->  |
   |                                         |
   |  <----- connect.challenge EVENT -----   |
   |         { nonce, ts }                   |
   |                                         |
   |  -------- connect REQUEST ---------->   |
   |         { ConnectParams }               |
   |                                         |
   |  <-------- hello-ok RESPONSE --------   |
   |         { HelloOk }                     |
   |                                         |
   |  ========= CONNECTED STATE ==========   |
   |                                         |
   |  <---------- tick EVENT -------------   |
   |  <---------- presence EVENT ---------   |
   |  -------- method REQUEST ----------->   |
   |  <-------- method RESPONSE ----------   |
   |                                         |
   |  <-------- shutdown EVENT -----------   |
   |  -------- WebSocket CLOSE ---------->   |
```

### 4.2 Stage 1: Challenge

Immediately upon WebSocket connection, server sends:

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

The `nonce` MUST be included in the device signature for non-local connections. This prevents replay attacks.

### 4.3 Stage 2: Connect Request

Client MUST send a `connect` request as the first message:

```typescript
{
  type: "req",
  id: string,
  method: "connect",
  params: ConnectParams
}
```

#### ConnectParams Schema

```typescript
{
  // Protocol negotiation
  minProtocol: number,           // Minimum supported protocol version (≥1)
  maxProtocol: number,           // Maximum supported protocol version (≥1)

  // Client identification
  client: {
    id: GatewayClientId,         // Client type identifier
    displayName?: string,        // Human-readable name
    version: string,             // Client version
    platform: string,            // OS platform (darwin, linux, win32, ios, android)
    deviceFamily?: string,       // Device family (iPhone, iPad, Mac, etc.)
    modelIdentifier?: string,    // Hardware model
    mode: GatewayClientMode,     // Client mode
    instanceId?: string          // Unique instance identifier
  },

  // Capabilities
  caps?: string[],               // Capability flags
  commands?: string[],           // Available commands (for nodes)
  permissions?: Record<string, boolean>,
  pathEnv?: string,              // PATH environment for command resolution

  // Role and authorization
  role?: "operator" | "node",    // Connection role (default: "operator")
  scopes?: string[],             // Requested authorization scopes

  // Device identity (ED25519)
  device?: {
    id: string,                  // Device ID (SHA256 fingerprint of public key)
    publicKey: string,           // Base64-URL encoded raw public key (32 bytes)
    signature: string,           // Base64-URL encoded ED25519 signature
    signedAt: number,            // Signature timestamp (ms since epoch)
    nonce?: string               // Challenge nonce (required for remote connections)
  },

  // Shared secret authentication (fallback)
  auth?: {
    token?: string,              // Gateway auth token
    password?: string            // Gateway auth password
  },

  // Metadata
  locale?: string,
  userAgent?: string
}
```

#### Gateway Client IDs

```typescript
const GATEWAY_CLIENT_IDS = {
  WEBCHAT_UI: "webchat-ui",
  CONTROL_UI: "openclaw-control-ui",
  WEBCHAT: "webchat",
  CLI: "cli",
  GATEWAY_CLIENT: "gateway-client",
  MACOS_APP: "openclaw-macos",
  IOS_APP: "openclaw-ios",
  ANDROID_APP: "openclaw-android",
  NODE_HOST: "node-host",
  TEST: "test",
  FINGERPRINT: "fingerprint",
  PROBE: "openclaw-probe"
};
```

#### Gateway Client Modes

```typescript
const GATEWAY_CLIENT_MODES = {
  WEBCHAT: "webchat",
  CLI: "cli",
  UI: "ui",
  BACKEND: "backend",
  NODE: "node",
  PROBE: "probe",
  TEST: "test"
};
```

### 4.4 Stage 3: Hello Response

Server responds with `hello-ok` on success:

```typescript
{
  type: "hello-ok",
  protocol: number,              // Negotiated protocol version

  server: {
    version: string,             // Server version
    commit?: string,             // Git commit hash
    host?: string,               // Server hostname
    connId: string               // Unique connection ID
  },

  features: {
    methods: string[],           // Available methods
    events: string[]             // Supported events
  },

  snapshot: {
    presence: PresenceEntry[],   // Current presence list
    health: any,                 // Health snapshot
    stateVersion: {
      presence: number,
      health: number
    },
    uptimeMs: number,
    configPath?: string,
    stateDir?: string,
    sessionDefaults?: {
      defaultAgentId: string,
      mainKey: string,
      mainSessionKey: string,
      scope?: string
    }
  },

  canvasHostUrl?: string,        // Canvas host URL for rendering

  auth?: {
    deviceToken: string,         // Token for reconnection
    role: string,
    scopes: string[],
    issuedAtMs?: number
  },

  policy: {
    maxPayload: number,          // Max incoming frame size (bytes)
    maxBufferedBytes: number,    // Max buffered data (bytes)
    tickIntervalMs: number       // Heartbeat interval (ms)
  }
}
```

### 4.5 Heartbeat (Tick)

Server sends periodic tick events:

```json
{
  "type": "event",
  "event": "tick",
  "payload": { "ts": 1707148800000 }
}
```

Clients SHOULD:
- Track `lastTick` timestamp
- Consider connection stale if `2 × tickIntervalMs` passes without tick
- Implement reconnection on tick timeout

### 4.6 Graceful Shutdown

Server broadcasts before closing:

```json
{
  "type": "event",
  "event": "shutdown",
  "payload": {
    "reason": "restart",
    "restartExpectedMs": 5000
  }
}
```

Shutdown reasons: `restart`, `stop`, `update`

### 4.7 WebSocket Close Codes

| Code | Meaning |
|------|---------|
| 1000 | Normal closure |
| 1002 | Protocol error (protocol mismatch) |
| 1006 | Abnormal closure (no close frame) |
| 1008 | Policy violation (auth failed, invalid device, pairing required) |
| 1012 | Service restart |
| 4000+ | Custom: tick timeout |

---

## 5. Authentication & Authorization

### 5.1 Authentication Methods

The Gateway supports multiple authentication methods in priority order:

1. **Device Identity** (cryptographic, preferred)
2. **Tailscale Identity** (via proxy headers)
3. **Shared Token** (fallback)
4. **Shared Password** (fallback)

### 5.2 Device Authentication

Device authentication uses ED25519 signatures:

1. Client generates or loads device identity (keypair)
2. Client constructs signature payload
3. Client signs payload with private key
4. Server verifies signature with public key

#### Signature Payload Format

```
version|deviceId|clientId|clientMode|role|scopes|signedAtMs|token|nonce
```

**Version 1 (v1):** No nonce field (legacy, local connections only)
```
v1|abc123...|openclaw-ios|ui|operator|operator.admin|1707148800000|
```

**Version 2 (v2):** Includes challenge nonce (required for remote)
```
v2|abc123...|openclaw-ios|ui|operator|operator.admin|1707148800000||550e8400-e29b-41d4-a716-446655440000
```

#### Signature Verification

1. Verify `device.id` matches SHA256 fingerprint of public key
2. Verify `signedAt` is within ±10 minutes of server time
3. For remote connections, verify `nonce` matches challenge
4. Verify ED25519 signature over payload

### 5.3 Tailscale Authentication

When Gateway is exposed via Tailscale Serve:

1. Tailscale proxy injects headers:
   - `Tailscale-User-Login`
   - `Tailscale-User-Name`
   - `Tailscale-User-Profile-Pic`
2. Gateway verifies via `tailscale whois` API
3. Login must match between header and whois result

### 5.4 Role-Based Authorization

#### Roles

| Role | Description |
|------|-------------|
| `operator` | Administrative client (default) |
| `node` | Remote execution endpoint |

#### Scopes

| Scope | Description |
|-------|-------------|
| `operator.admin` | Full administrative access |
| `operator.read` | Read-only operations |
| `operator.write` | Write operations |
| `operator.approvals` | Execution approval management |
| `operator.pairing` | Device/node pairing management |

#### Method Authorization Matrix

| Method Category | Required Scope |
|-----------------|----------------|
| Admin methods (`exec.approvals.*`, `config.*`, `wizard.*`) | `operator.admin` |
| Approval methods (`exec.approval.request`, `exec.approval.resolve`) | `operator.approvals` |
| Pairing methods (`device.pair.*`, `node.pair.*`) | `operator.pairing` |
| Read methods (`health`, `status`, `*.list`, etc.) | `operator.read` |
| Write methods (`send`, `agent`, `wake`, etc.) | `operator.write` |
| Node methods (`node.invoke.result`, `node.event`) | `node` role |

---

## 6. Device Identity & Pairing

### 6.1 Device Identity Generation

Device identity is an ED25519 keypair stored locally:

```typescript
type DeviceIdentity = {
  deviceId: string,         // SHA256 fingerprint of public key (hex)
  publicKeyPem: string,     // PEM-encoded public key (SPKI format)
  privateKeyPem: string     // PEM-encoded private key (PKCS8 format)
};
```

**Storage Location:** `~/.openclaw/identity/device.json`

```json
{
  "version": 1,
  "deviceId": "a1b2c3d4...",
  "publicKeyPem": "-----BEGIN PUBLIC KEY-----\n...",
  "privateKeyPem": "-----BEGIN PRIVATE KEY-----\n...",
  "createdAtMs": 1707148800000
}
```

### 6.2 Device ID Derivation

```
deviceId = hex(SHA256(rawPublicKey))
```

Where `rawPublicKey` is the 32-byte raw ED25519 public key extracted from SPKI DER format.

### 6.3 Pairing Flow

#### New Device Connection

```
Client                                    Server
   |                                         |
   |  -------- connect (with device) ---->   |
   |                                         |
   |  <-- NOT_PAIRED error + requestId ---   |
   |                                         |
   |           [PAIRING PENDING]             |
   |                                         |
   |  (Admin approves via another client)    |
   |                                         |
   |  -------- connect (retry) ---------->   |
   |                                         |
   |  <-------- hello-ok -----------------   |
   |         { auth.deviceToken }            |
```

#### Pairing Request Event

Broadcast to connected admin clients:

```json
{
  "type": "event",
  "event": "device.pair.requested",
  "payload": {
    "requestId": "550e8400-...",
    "deviceId": "a1b2c3d4...",
    "publicKey": "base64url-encoded",
    "displayName": "John's iPhone",
    "platform": "ios",
    "clientId": "openclaw-ios",
    "clientMode": "ui",
    "role": "operator",
    "scopes": ["operator.admin"],
    "remoteIp": "192.168.1.100",
    "silent": false,
    "isRepair": false,
    "ts": 1707148800000
  }
}
```

#### Pairing Resolution Event

```json
{
  "type": "event",
  "event": "device.pair.resolved",
  "payload": {
    "requestId": "550e8400-...",
    "deviceId": "a1b2c3d4...",
    "decision": "approved",
    "ts": 1707148800000
  }
}
```

### 6.4 Device Pairing Methods

#### `device.pair.list`

Lists pending and paired devices.

**Params:** `{}`

**Response:**
```typescript
{
  pending: DevicePairingPendingRequest[],
  paired: PairedDevice[]
}
```

#### `device.pair.approve`

Approves a pending pairing request.

**Params:** `{ requestId: string }`

**Response:** `{ requestId, device: PairedDevice }` or `null`

#### `device.pair.reject`

Rejects a pending pairing request.

**Params:** `{ requestId: string }`

**Response:** `{ requestId, deviceId }` or `null`

### 6.5 Device Token Management

#### `device.token.rotate`

Rotates device authentication token.

**Params:**
```typescript
{
  deviceId: string,
  role: string,
  scopes?: string[]
}
```

**Response:** `DeviceAuthToken` or `null`

#### `device.token.revoke`

Revokes device authentication token.

**Params:**
```typescript
{
  deviceId: string,
  role: string
}
```

**Response:** `DeviceAuthToken` or `null`

### 6.6 Silent Pairing (Auto-Approve)

Local connections (loopback) can be auto-approved:
- `silent: true` in pairing request
- Server auto-calls `approveDevicePairing`
- No admin intervention required

### 6.7 Paired Device Storage

**Location:** `~/.openclaw/state/devices/paired.json`

```typescript
type PairedDevice = {
  deviceId: string,
  publicKey: string,           // Base64-URL encoded
  displayName?: string,
  platform?: string,
  clientId?: string,
  clientMode?: string,
  role?: string,
  roles?: string[],            // Multiple roles supported
  scopes?: string[],
  remoteIp?: string,
  tokens?: Record<string, DeviceAuthToken>,
  createdAtMs: number,
  approvedAtMs: number
};
```

---

## 7. Node Pairing

Nodes are remote execution endpoints that can run commands on behalf of the Gateway.

### 7.1 Node Registration

Nodes connect with `role: "node"` and provide:
- `commands`: List of available commands
- `caps`: Capability flags
- `permissions`: Permission grants

### 7.2 Node Pairing Flow

```
Node                                      Gateway
   |                                         |
   |  -------- node.pair.request -------->   |
   |                                         |
   |  <-- "pending" response --------------  |
   |                                         |
   |  (Admin approves via device.pair.*)     |
   |                                         |
   |  -------- node.pair.verify --------->   |
   |                                         |
   |  <-------- { ok: true } -------------   |
```

### 7.3 Node Pairing Methods

#### `node.pair.request`

Initiates node pairing.

**Params:**
```typescript
{
  nodeId: string,
  displayName?: string,
  platform?: string,
  version?: string,
  coreVersion?: string,
  uiVersion?: string,
  deviceFamily?: string,
  modelIdentifier?: string,
  caps?: string[],
  commands?: string[],
  remoteIp?: string,
  silent?: boolean
}
```

**Response:** `{ status: "pending", request: NodePairingPendingRequest, created: boolean }`

#### `node.pair.list`

Lists pending and paired nodes.

**Params:** `{}`

**Response:**
```typescript
{
  pending: NodePairingPendingRequest[],
  paired: NodePairingPairedNode[]
}
```

#### `node.pair.approve`

Approves a pending node pairing request.

**Params:** `{ requestId: string }`

**Response:** `{ requestId, node: NodePairingPairedNode }` or `null`

#### `node.pair.reject`

Rejects a pending node pairing request.

**Params:** `{ requestId: string }`

**Response:** `{ requestId, nodeId }` or `null`

#### `node.pair.verify`

Verifies node pairing status.

**Params:** `{ nodeId: string, token: string }`

**Response:** `{ ok: boolean, node?: NodePairingPairedNode }`

### 7.4 Node Invocation

#### `node.invoke`

Invokes a command on a remote node.

**Params:**
```typescript
{
  nodeId: string,
  command: string,
  params?: unknown,
  timeoutMs?: number,
  idempotencyKey: string
}
```

**Response:** Command result or error

#### `node.invoke.result` (Node → Gateway)

Node sends command execution result.

**Params:**
```typescript
{
  id: string,
  nodeId: string,
  ok: boolean,
  payload?: unknown,
  payloadJSON?: string,
  error?: { code?: string, message?: string }
}
```

### 7.5 Node Events

#### `node.invoke.request` Event

Sent to node when Gateway wants to invoke a command:

```json
{
  "type": "event",
  "event": "node.invoke.request",
  "payload": {
    "id": "550e8400-...",
    "nodeId": "node-abc",
    "command": "screenshot",
    "paramsJSON": "{}",
    "timeoutMs": 30000,
    "idempotencyKey": "unique-key"
  }
}
```

---

## 8. Methods Reference

### 8.1 Core Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `health` | Get server health status | `read` |
| `status` | Get comprehensive status | `read` |
| `logs.tail` | Stream server logs | `read` |
| `system-presence` | Get current presence list | `read` |
| `system-event` | Emit system event | `admin` |
| `last-heartbeat` | Get last heartbeat timestamp | `read` |
| `set-heartbeats` | Set heartbeat configuration | `admin` |

### 8.2 Configuration Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `config.get` | Get configuration value | `admin` |
| `config.set` | Set configuration value | `admin` |
| `config.apply` | Apply configuration changes | `admin` |
| `config.patch` | Patch configuration | `admin` |
| `config.schema` | Get configuration schema | `admin` |

### 8.3 Agent Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `agent` | Run agent with message | `write` |
| `agent.identity.get` | Get agent identity | `read` |
| `agent.wait` | Wait for agent completion | `write` |

### 8.4 Chat Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `chat.send` | Send chat message | `write` |
| `chat.history` | Get chat history | `read` |
| `chat.abort` | Abort current chat | `write` |

### 8.5 Session Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `sessions.list` | List sessions | `read` |
| `sessions.preview` | Preview session content | `read` |
| `sessions.patch` | Update session | `admin` |
| `sessions.reset` | Reset session | `admin` |
| `sessions.delete` | Delete session | `admin` |
| `sessions.compact` | Compact session history | `admin` |

### 8.6 Channel Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `channels.status` | Get channel status | `read` |
| `channels.logout` | Logout from channel | `admin` |
| `send` | Send message to channel | `write` |
| `wake` | Wake/trigger processing | `write` |

### 8.7 TTS Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `tts.status` | Get TTS status | `read` |
| `tts.providers` | List TTS providers | `read` |
| `tts.enable` | Enable TTS | `write` |
| `tts.disable` | Disable TTS | `write` |
| `tts.convert` | Convert text to speech | `write` |
| `tts.setProvider` | Set TTS provider | `write` |

### 8.8 Voice Wake Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `voicewake.get` | Get voice wake config | `read` |
| `voicewake.set` | Set voice wake config | `write` |

### 8.9 Cron Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `cron.list` | List cron jobs | `read` |
| `cron.status` | Get cron status | `read` |
| `cron.runs` | Get cron run history | `read` |
| `cron.add` | Add cron job | `admin` |
| `cron.update` | Update cron job | `admin` |
| `cron.remove` | Remove cron job | `admin` |
| `cron.run` | Run cron job immediately | `admin` |

### 8.10 Model/Agent Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `models.list` | List available models | `read` |
| `agents.list` | List agents | `read` |
| `agents.files.list` | List agent files | `admin` |
| `agents.files.get` | Get agent file | `admin` |
| `agents.files.set` | Set agent file | `admin` |

### 8.11 Skill Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `skills.status` | Get skills status | `read` |
| `skills.bins` | Get available binaries | `node` |
| `skills.install` | Install skill | `admin` |
| `skills.update` | Update skill | `admin` |

### 8.12 Execution Approval Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `exec.approvals.get` | Get approval settings | `admin` |
| `exec.approvals.set` | Set approval settings | `admin` |
| `exec.approvals.node.get` | Get node approval settings | `admin` |
| `exec.approvals.node.set` | Set node approval settings | `admin` |
| `exec.approval.request` | Request execution approval | `approvals` |
| `exec.approval.resolve` | Resolve approval request | `approvals` |

### 8.13 Node Management Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `node.list` | List connected nodes | `read` |
| `node.describe` | Describe node capabilities | `read` |
| `node.invoke` | Invoke command on node | `write` |
| `node.rename` | Rename node | `pairing` |
| `node.invoke.result` | Report invocation result | `node` |
| `node.event` | Report node event | `node` |

### 8.14 Wizard Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `wizard.start` | Start onboarding wizard | `admin` |
| `wizard.next` | Advance wizard step | `admin` |
| `wizard.cancel` | Cancel wizard | `admin` |
| `wizard.status` | Get wizard status | `admin` |

### 8.15 Miscellaneous Methods

| Method | Description | Scope |
|--------|-------------|-------|
| `talk.mode` | Set talk mode | `write` |
| `usage.status` | Get usage status | `read` |
| `usage.cost` | Get usage cost | `read` |
| `update.run` | Run update | `admin` |
| `browser.request` | Browser automation request | `write` |

---

## 9. Events Reference

### 9.1 Connection Events

#### `connect.challenge`

Sent immediately on WebSocket connection.

```json
{
  "event": "connect.challenge",
  "payload": {
    "nonce": "uuid",
    "ts": 1707148800000
  }
}
```

#### `shutdown`

Sent before server shutdown.

```json
{
  "event": "shutdown",
  "payload": {
    "reason": "restart|stop|update",
    "restartExpectedMs": 5000
  }
}
```

### 9.2 Heartbeat Events

#### `tick`

Periodic heartbeat (every 30 seconds).

```json
{
  "event": "tick",
  "payload": { "ts": 1707148800000 }
}
```

#### `heartbeat`

System heartbeat marker.

### 9.3 State Events

#### `presence`

Presence list updated.

```json
{
  "event": "presence",
  "payload": {
    "presence": [
      {
        "host": "John's MacBook",
        "ip": "192.168.1.100",
        "version": "2024.1.15",
        "platform": "darwin",
        "mode": "ui",
        "deviceId": "abc123",
        "roles": ["operator"],
        "scopes": ["operator.admin"],
        "reason": "connect",
        "ts": 1707148800000
      }
    ]
  },
  "stateVersion": { "presence": 42, "health": 17 }
}
```

#### `health`

Health snapshot updated.

```json
{
  "event": "health",
  "payload": { /* health data */ },
  "stateVersion": { "presence": 42, "health": 18 }
}
```

### 9.4 Agent Events

#### `agent`

Agent execution events (streaming).

```json
{
  "event": "agent",
  "payload": {
    "type": "text|tool_use|tool_result|error|done",
    "sessionKey": "...",
    "agentId": "...",
    "text": "...",
    "toolName": "...",
    "toolInput": {}
  }
}
```

#### `chat`

WebChat streaming events.

### 9.5 Pairing Events

#### `device.pair.requested` / `device.pair.resolved`

See §6.3.

#### `node.pair.requested` / `node.pair.resolved`

Similar structure for node pairing.

### 9.6 Execution Events

#### `exec.approval.requested`

Execution approval requested.

```json
{
  "event": "exec.approval.requested",
  "payload": {
    "requestId": "...",
    "toolName": "...",
    "toolInput": {},
    "ts": 1707148800000
  }
}
```

#### `exec.approval.resolved`

Execution approval resolved.

```json
{
  "event": "exec.approval.resolved",
  "payload": {
    "requestId": "...",
    "decision": "approved|denied",
    "ts": 1707148800000
  }
}
```

### 9.7 Mode Events

#### `talk.mode`

Talk mode changed.

#### `voicewake.changed`

Voice wake configuration changed.

### 9.8 Cron Events

#### `cron`

Cron job execution event.

### 9.9 Node Events

#### `node.invoke.request`

See §7.5.

---

## 10. Error Handling

### 10.1 Error Codes

| Code | Description |
|------|-------------|
| `NOT_LINKED` | Service not linked/configured |
| `NOT_PAIRED` | Device/node not paired |
| `AGENT_TIMEOUT` | Agent execution timed out |
| `INVALID_REQUEST` | Invalid request (bad params, unknown method, unauthorized) |
| `UNAVAILABLE` | Server temporarily unavailable |

### 10.2 Connection Rejection Reasons

| Reason | Description |
|--------|-------------|
| `unauthorized` | Authentication failed |
| `protocol-mismatch` | Protocol version negotiation failed |
| `invalid-role` | Unknown role requested |
| `origin-mismatch` | Browser origin not allowed |
| `control-ui-insecure-auth` | Control UI requires secure context |
| `device-required` | Device identity required |
| `device-id-mismatch` | Device ID doesn't match public key |
| `device-signature-stale` | Signature timestamp too old |
| `device-nonce-missing` | Challenge nonce required |
| `device-nonce-mismatch` | Nonce doesn't match challenge |
| `device-signature` | Signature verification failed |
| `pairing-required` | Device not in paired list |
| `handshake-timeout` | Handshake took too long |

### 10.3 Authentication Failure Messages

Gateway provides descriptive error messages:

- `unauthorized: gateway token missing (set gateway.remote.token...)`
- `unauthorized: gateway token mismatch (...)`
- `unauthorized: gateway password missing (...)`
- `unauthorized: tailscale identity missing (...)`

---

## 11. State Management

### 11.1 Presence Tracking

Each connected client is tracked with:

```typescript
type PresenceEntry = {
  host?: string,
  ip?: string,
  version?: string,
  platform?: string,
  deviceFamily?: string,
  modelIdentifier?: string,
  mode?: string,
  lastInputSeconds?: number,
  reason?: string,             // "connect", "disconnect", "self"
  tags?: string[],
  text?: string,
  ts: number,
  deviceId?: string,
  roles?: string[],
  scopes?: string[],
  instanceId?: string
};
```

- **TTL:** 5 minutes per entry
- **Max entries:** 200
- **Self entry:** Gateway always present
- **Version tracking:** Incremented on presence changes

### 11.2 State Versioning

Events include `stateVersion` for optimistic concurrency:

```typescript
{
  presence: number,   // Incremented on presence changes
  health: number      // Incremented on health updates
}
```

Clients can detect missed events by tracking sequence numbers.

### 11.3 Sequence Numbers

Events may include `seq` for gap detection:

```json
{
  "type": "event",
  "event": "agent",
  "seq": 42,
  "payload": { ... }
}
```

If `received_seq !== expected_seq`, client should:
1. Request full state refresh
2. Report gap via `onGap` callback

### 11.4 Persistence Paths

| Data | Path |
|------|------|
| Device identity | `~/.openclaw/identity/device.json` |
| Device pairing (pending) | `~/.openclaw/state/devices/pending.json` |
| Device pairing (paired) | `~/.openclaw/state/devices/paired.json` |
| Node pairing (pending) | `~/.openclaw/state/nodes/pending.json` |
| Node pairing (paired) | `~/.openclaw/state/nodes/paired.json` |

All files use atomic writes (temp file + rename) and 0600 permissions.

---

## 12. Security Considerations

### 12.1 Cryptographic Requirements

- **Signature Algorithm:** ED25519 (RFC 8032)
- **Hash Algorithm:** SHA-256 for device ID derivation
- **Key Format:** SPKI DER for public keys, PKCS8 for private keys
- **Encoding:** Base64-URL (no padding) for signatures and public keys

### 12.2 Replay Attack Prevention

1. **Nonce challenge:** Server sends random nonce on connect
2. **Timestamp validation:** Signatures valid ±10 minutes
3. **Nonce binding:** Remote clients MUST include challenge nonce in signature

### 12.3 Local vs Remote Connections

| Security Check | Local | Remote |
|----------------|-------|--------|
| Nonce required | No | Yes |
| Legacy v1 signature | Allowed | Rejected |
| Auto-approve pairing | Allowed | Requires admin |

Local is determined by:
- Loopback address (127.0.0.1, ::1)
- Local hostname (localhost)
- No untrusted proxy headers

### 12.4 Timing-Safe Comparisons

All token/password comparisons use constant-time algorithms to prevent timing attacks:

```typescript
function safeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  return timingSafeEqual(Buffer.from(a), Buffer.from(b));
}
```

### 12.5 Proxy Header Trust

When behind a reverse proxy:
1. Configure `gateway.trustedProxies` with proxy addresses
2. Gateway validates `X-Forwarded-For` / `X-Real-IP` only from trusted sources
3. Untrusted proxy headers cause connection to be treated as remote

### 12.6 Browser Security

Control UI connections require:
- HTTPS or localhost (secure context)
- Origin validation against `gateway.controlUi.allowedOrigins`
- Device identity or shared secret authentication

### 12.7 Token Storage

Device tokens:
- UUID-based (32 chars, no dashes)
- Stored server-side with device pairing data
- Support rotation (`device.token.rotate`)
- Support revocation (`device.token.revoke`)
- Track `lastUsedAtMs` for audit

---

## Appendix A: Complete Connect Example

### Client Connecting with Device Identity

```javascript
// 1. Load or create device identity
const identity = loadOrCreateDeviceIdentity();

// 2. Build signature payload
const payload = buildDeviceAuthPayload({
  deviceId: identity.deviceId,
  clientId: "openclaw-ios",
  clientMode: "ui",
  role: "operator",
  scopes: ["operator.admin"],
  signedAtMs: Date.now(),
  token: null,
  nonce: challengeNonce,  // from connect.challenge event
  version: "v2"
});

// 3. Sign payload
const signature = signDevicePayload(identity.privateKeyPem, payload);

// 4. Send connect request
ws.send(JSON.stringify({
  type: "req",
  id: uuid(),
  method: "connect",
  params: {
    minProtocol: 3,
    maxProtocol: 3,
    client: {
      id: "openclaw-ios",
      displayName: "John's iPhone",
      version: "2024.1.15",
      platform: "ios",
      deviceFamily: "iPhone",
      mode: "ui",
      instanceId: "unique-instance-id"
    },
    role: "operator",
    scopes: ["operator.admin"],
    device: {
      id: identity.deviceId,
      publicKey: publicKeyRawBase64UrlFromPem(identity.publicKeyPem),
      signature: signature,
      signedAt: Date.now(),
      nonce: challengeNonce
    }
  }
}));
```

---

## Appendix B: Protocol Version History

| Version | Changes |
|---------|---------|
| 1 | Initial protocol |
| 2 | Added nonce-based device authentication |
| 3 | Current version |

---

## Appendix C: TypeScript Type Definitions

See source files:
- `src/gateway/protocol/schema/frames.ts` - Frame schemas
- `src/gateway/protocol/schema/devices.ts` - Device schemas
- `src/gateway/protocol/schema/nodes.ts` - Node schemas
- `src/gateway/protocol/schema/snapshot.ts` - State schemas
- `src/gateway/protocol/schema/error-codes.ts` - Error codes
- `src/infra/device-identity.ts` - Device identity utilities
- `src/infra/device-pairing.ts` - Device pairing logic
- `src/infra/node-pairing.ts` - Node pairing logic
