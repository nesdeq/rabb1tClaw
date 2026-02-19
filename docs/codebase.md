# Codebase Summary — rabb1tClaw

**7,543 lines of Rust** across 43 `.rs` files + **598 lines of system prompts** across 6 `.md` files in src/prompts/.

## What It Is
A minimal Rust LLM gateway for the Rabbit R1 and other devices. WebSocket server (protocol v3) that sits between devices and LLM APIs (OpenAI, Anthropic, DeepInfra/vLLM). It runs a main conversational agent plus four background agents: code execution, memory extraction, web search, and advanced orchestration.

## Architecture Layers

**Config** (`config/`) — YAML-based at `~/.rabb1tclaw/config.yaml`. Providers (API connections) are separate from models (config + params). `AgentKind` enum (Main/Code/Memory/Search/Advanced) with `agent_config()` typed accessor. Hot-reload via file polling (2s) + SIGHUP.

**Protocol** (`protocol.rs`) — v3 WebSocket frames. Incoming: tagged `Request`. Outgoing: `Response`, `Event`, `Close`. Infrastructure constants (tick, payload limits, channel capacity).

**Connection** (`connection/`) — Axum HTTP server with WebSocket upgrade. Handler does connect handshake with auth (loopback bypass, device tokens with constant-time comparison, revocation). After auth, frames route to `dispatch_method`.

**State** (`state.rs`) — `GatewayState` holds everything: config (RwLock), device store (RwLock), active connections (for revocation), session manager, code task tracker, search query tracker, advanced task tracker. `HandlerContext` bundles per-request state.

**Providers** (`provider/`) — `LlmProvider` trait with `chat_stream()`. OpenAI provider handles reasoning models (o1/o3/o4/gpt-5 → `max_completion_tokens`, no temp), OSS reasoning (chat_template_kwargs), and thinking output. Anthropic provider handles extended thinking blocks. Shared SSE processor with line-by-line parsing.

**Agent Runner** (`agent/runner.rs`) — `resolve_agent_model()` merges agent override > model config > defaults. `handle_agent()` builds system prompt with live awareness, session memory, code task status, and search results injections. Token-based FIFO trims oldest pairs. Spawns `stream_agent_response()` which handles think-block stripping, marker filtering, and dispatches background agents on completion.

**Background Agents:**
- **Code** (`agent/code/`) — Hakoniwa 1.3 sandboxed Python execution. Self-healing loop (5 iterations): LLM generates code → sandbox executes → errors fed back → retry. Per-device workspace with persistent `.venv`. Selective mounts (no host rootfs). Pasta networking for pip/HTTP.
- **Memory** (`agent/memory.rs`) — Runs every N conversation turns (pairs). Extracts durable facts/instructions from conversation. Writes `memory.md` per device. Strict no-inference policy.
- **Search** (`agent/search/`) — 5-phase pipeline: (1) LLM query analysis → refined Serper params + depth decision, (2) multi-type Serper fetch with deduplication, (3) enrich all URLs via trafilatura content extraction, (4) token-budgeted context assembly, (5) LLM synthesis with sources. Graceful degradation without search model.
- **Advanced** (`agent/advanced/`) — LLM orchestrator with multi-step planning. Can delegate to code and search agents, ask user questions via main agent relay, compress its own context, and log all steps to per-task log files.

**Dispatch System** — Main LLM emits `@@dispatch` / `@@end` blocks containing JSON arrays of dispatch entries (`{"type":"code","desc":"..."}`) and answer relays (`{"id":N,"answer":"..."}`). Parsed by `parse_task_markers()` post-stream for agent dispatch, filtered from streaming deltas by `MarkerFilter`, stripped from session history by `strip_task_markers()`. Task context is injected into user messages via `@@task` / `@@end` text blocks (persisted task log + live running tasks).

**Session** (`agent/session.rs`) — One session per device (no multi-session support). AES-256-GCM encrypted at rest (key = SHA-256 of device token). Stored as `~/.rabb1tclaw/<prefix>/conversation.enc`. Record pattern: clone snapshot under write lock, release, persist to disk without holding lock. Turn counting: 1 turn = 1 user message + 1 assistant response pair.

**CLI** (`cli/`) — Clap 4 derive. Subcommands: `init` (interactive setup with smart defaults), `server` (start/stop/restart/IP), `devices` (list/onboard/revoke), `providers` (list/add/remove), `models` (list/add/remove/edit/set-active). Smart defaults via `ModelTier` detection (Reasoning/Thinking/OssReasoning/Standard).

**Tracker** (`agent/tracker.rs`) — Generic `BackgroundTracker<S>` parameterized over status enum. Register/complete/update_status/get_running. Tiktoken o200k_base tokenizer (singleton) for token counting and truncation.

**Task Log** (`agent/tasklog.rs`) — Persistent FIFO task log (`tasks.md` per device) + live running task aggregation. `build_task_context()` combines persisted log entries with live tracker state into `@@task` / `@@end` blocks injected into user messages.

## Key Patterns
- `record_message()` — clone-then-release lock pattern for persistence
- `fail!(iteration, error)` macro in code agent
- `model_agent_roles()` with const table — shared by dispatch and CLI
- `is_reasoning_model()` pub in openai.rs — reused by CLI's `detect_tier()`
- All defaults centralized in `cli/defaults.rs`
- Per-device isolation via token prefix (first 8 chars of token)