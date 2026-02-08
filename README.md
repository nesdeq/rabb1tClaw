```
          _    _    _ _    ___ _
 _ _ __ _| |__| |__/ | |_ / __| |__ ___ __ __
| '_/ _` | '_ \ '_ \ |  _| (__| / _` \ V  V /
|_| \__,_|_.__/_.__/_|\__|\___|_\__,_|\_/\_/
```

<img src="rabb1tClaw.gif" alt="rabb1tClaw onboarding in under 30 seconds" width="400" align="right">

**A native Rust LLM gateway for the Rabbit R1.** Sub-30-second setup. No cloud middleman. Your hardware, your models.

rabb1tClaw implements the [OpenClaw](https://github.com/openclaw/openclaw) WebSocket protocol v3, giving your R1 a direct line to OpenAI, Anthropic, DeepInfra, or any OpenAI-compatible endpoint. Built on async Rust (Tokio + Axum), it handles hundreds of concurrent device connections with streaming responses that start arriving on the first token. Runs comfortably on a Raspberry Pi.

38 Rust source files. ~7k lines. That's the whole thing.

<br clear="both">

## How It Works

Everything here was designed for the Rabbit R1 first -- a voice-first device with a small screen and no keyboard. There's no room for "Searching..." spinners, tool-call confirmations, or multi-step UI flows. You talk to it. It talks back. That's the interface.

The main conversation loop is the first-class citizen. The LLM streams its response directly to the device, word by word. When it decides it needs to run code or search the web, it emits lightweight markers inline. rabb1tClaw intercepts these in real-time, strips them from the stream so the device never sees them, and dispatches concurrent background agents:

- **Code agent** -- sandboxed Python execution (Linux namespaces via hakoniwa), self-healing retry loop, persistent per-device workspace
- **Search agent** -- 3-phase LLM-powered web search via [Serper.dev](https://serper.dev) (query analysis, evaluate, deep-read + synthesize)
- **Memory agent** -- extracts facts worth remembering, persists them to disk, injects them into future conversations

The main loop always knows what's running. On the next user message, completed results are injected into the system prompt and the LLM weaves them into its response naturally. No tool-use UI, no status indicators, no interruptions. The conversation just flows -- exactly how a voice-first device should work.

See [docs/workflow.md](docs/workflow.md) for the full activity diagrams and agent pipelines.

## Supported Providers

| Provider | Models | Notes |
|----------|--------|-------|
| **OpenAI** | gpt-4o, gpt-5.2, o3, o4-mini, ... | Reasoning model auto-detection (`max_completion_tokens`, `reasoning_effort`) |
| **Anthropic** | claude-sonnet-4-5, claude-opus-4, ... | Extended thinking with budget control |
| **DeepInfra / vLLM** | Kimi-K2.5, DeepSeek R1, Qwen QwQ, ... | Thinking toggle via `chat_template_kwargs` |
| **Any OpenAI-compatible** | Your endpoint + model | Works out of the box |

Multiple providers and models configured simultaneously. Switch active model with one command. Per-model parameter tuning. Reasoning output from all three paradigms (Anthropic thinking blocks, `reasoning_content` field, inline `<think>` tags) is handled and stripped automatically.

See [docs/modelsupport.md](docs/modelsupport.md) for the full parameter support matrix and per-provider behavior.

## Quick Start

> **First time?** See [docs/install.md](docs/install.md) for full prerequisites (Rust, Python 3, passt, kernel config). The commands below assume you're already set up.

```bash
git clone https://github.com/nesdeq/rabb1tClaw.git && cd rabb1tClaw
cargo build --release
cp .env.example target/release/.env   # fill in your API key(s)
./target/release/rabb1tclaw
```

Add your LLM provider key(s) to `.env`. For web search, add `SERP_API_KEY` from [serper.dev](https://serper.dev) -- free tier gives 2,500 searches, no payment info required.

First run auto-detects your keys, fetches available models, applies smart defaults per model tier (reasoning, thinking, standard), onboards your first device with a QR code, and starts the server. Watch the gif -- it takes about 30 seconds.

Use the CLI commands below from another terminal while the server is running.

## Configuration

All config lives under `~/.rabb1tclaw/` with `0600` permissions:

| File | Purpose |
|------|---------|
| `config.yaml` | Gateway, providers, models, agents, active model |
| `devices.yaml` | Paired device tokens |
| `<token_prefix>/session/` | Encrypted conversation sessions and memory |
| `<token_prefix>/workspace/` | Persistent code agent workspace + venv |

Models are configured separately from providers, referencing a provider by key:

```yaml
providers:
  openai:
    api: openai
    base_url: https://api.openai.com/v1
    api_key: sk-...

models:
  gpt-4o:
    provider: openai
    model_id: gpt-4o
    max_tokens: 4096
    temperature: 0.7
    context_tokens: 200000

active_model: gpt-4o
```

Agent behavior is fully tunable: concurrency limits, iteration counts, token budgets, timeouts, and more.

See [docs/config.md](docs/config.md) for the complete parameter reference with defaults.

## Session Encryption

Conversation sessions are AES-256-GCM encrypted at rest. The key is derived from SHA-256 of the device token -- each device's history is keyed to its own credentials. Sessions load and decrypt on server start. Revoked devices have their sessions orphaned (unreadable without the token).

## CLI Reference

### Server

```
rabb1tclaw                            Start (runs init if no config exists)
rabb1tclaw server --stop              Stop running server
rabb1tclaw server --restart           Hot-reload config (SIGHUP)
rabb1tclaw server --get-ip            Print current bind IP
rabb1tclaw server --set-ip <IP>       Change bind IP
```

### Devices

```
rabb1tclaw devices --list             List paired devices
rabb1tclaw devices --onboard          Add device + QR code
rabb1tclaw devices --revoke <ID>      Revoke a device
rabb1tclaw devices --revoke-all       Revoke all devices
```

### Providers

```
rabb1tclaw providers --list           List configured providers
rabb1tclaw providers --add            Add a new provider interactively
rabb1tclaw providers --remove <NAME>  Remove a provider (and orphaned models)
```

### Models

```
rabb1tclaw models --list              List configured models
rabb1tclaw models --add               Add a new model interactively
rabb1tclaw models --remove <KEY>      Remove a model
rabb1tclaw models --set-active <KEY>  Set the active model
rabb1tclaw models --edit <KEY>        Edit model parameters
```

### Setup

```
rabb1tclaw init                       Interactive setup (re-run anytime)
```

## Hot Reload

The server watches config files every 2 seconds. Edit them and changes apply live. Revoking a device disconnects it immediately. `rabb1tclaw server --restart` sends SIGHUP for instant reload.

## Roadmap

- ClawHub skill compatibility with Rust hakoniwa namespace isolation

## License

[MIT](LICENSE)

---

## Changelog

### v0.2.0

**Background agent architecture.** The main conversation loop can now dispatch concurrent background agents and incorporate their results organically into the next response.

- **Code agent** -- sandboxed Python execution via hakoniwa (Linux namespaces, rlimits, passt networking). Self-healing loop: LLM generates code, sandbox executes, errors feed back to LLM, up to 5 iterations. LLM verifies its own output before accepting. Persistent per-device workspace with reusable venv.
- **Search agent** -- 3-phase LLM-powered web search. Phase 1: query analysis and refinement. Phase 2: Serper API fetch + LLM evaluates snippet sufficiency. Phase 3: deep-read top URLs via trafilatura + LLM synthesizes answer with sources. Graceful degradation to raw snippets when no search model is configured.
- **Memory agent** -- extracts facts from conversation every N turns, persists to disk, injects into future system prompts. Merges with existing memory on each run. Configurable interval and word limit.
- **Generic background tracker** -- unified `BackgroundTracker<S>` with atomic get-and-mark-delivered, concurrency limiting, and time-based pruning. Shared by code and search agents.
- **Streaming marker filter** -- `<!--code_task:-->` and `<!--web_search:-->` markers stripped from the live stream in real-time so the device never sees dispatch internals.
- **Token-based truncation** -- tiktoken `o200k_base` encoding for accurate token counting. Stdout/stderr, search context, and status blocks all truncated by token count, not character count.
- **Smart model tier detection** -- `rabb1tclaw init` auto-detects Reasoning (o-series, gpt-5), Thinking (Anthropic), OSS Reasoning (DeepSeek R1, QwQ, Kimi), and Standard tiers. Applies appropriate defaults (max_tokens, reasoning_effort, thinking budget).
- **Agent-level model overrides** -- each agent kind (main, code, memory, search) can target a different model with independent parameter tuning.
- **Configurable operational limits** -- max_concurrent, max_iterations, exec_timeout, token budgets, prune ages, and more. All exposed in config.yaml with sensible defaults.

### v0.1.0

Initial release. OpenClaw WebSocket protocol v3, multi-provider support (OpenAI, Anthropic, DeepInfra/vLLM), encrypted sessions, token-based context FIFO, hot reload, QR onboarding, full CLI.
