# Configuration Reference

All parameters are optional. Defaults are applied during `rabb1tclaw init`.
Token limits use [tiktoken](https://github.com/zurawiki/tiktoken-rs) `o200k_base` encoding for accurate counting.

---

## Gateway

| Parameter | Default | Description |
|-----------|---------|-------------|
| `gateway.port` | `18789` | WebSocket listen port |
| `gateway.bind` | `127.0.0.1` | Bind address |

## Providers

| Parameter | Required | Description |
|-----------|----------|-------------|
| `providers.<key>.api` | yes | `"openai"` or `"anthropic"` |
| `providers.<key>.base_url` | yes | API endpoint URL |
| `providers.<key>.api_key` | yes | API key |
| `providers.<key>.name` | no | Display name |

## Models

| Parameter | Default | Description |
|-----------|---------|-------------|
| `models.<key>.provider` | *required* | Must match a key in `providers` |
| `models.<key>.model_id` | *required* | Model identifier sent to API (e.g. `gpt-5.2`, `claude-sonnet-4-5-20250929`) |
| `models.<key>.max_tokens` | tier-dependent | Max output tokens. Auto-set by tier detection (see below) |
| `models.<key>.temperature` | `None` | Sampling temperature (0.0-2.0). Omitted for reasoning models |
| `models.<key>.top_p` | `None` | Nucleus sampling |
| `models.<key>.frequency_penalty` | `None` | OpenAI only (-2.0 to 2.0) |
| `models.<key>.presence_penalty` | `None` | OpenAI only (-2.0 to 2.0) |
| `models.<key>.reasoning_effort` | `None` | `"low"` / `"medium"` / `"high"` (o-series & gpt-5.x only) |
| `models.<key>.context_tokens` | `200000` | Conversation history FIFO budget (tokens, tiktoken) |
| `models.<key>.thinking.enabled` | `false` | Enable extended thinking / chain-of-thought |
| `models.<key>.thinking.budget_tokens` | `None` | Max thinking tokens (Anthropic only). Fallback: `10000` |

### Model Tier Auto-Detection

Applied during `rabb1tclaw init` based on `api` type and `model_id`:

| Tier | Detection | Auto-Applied |
|------|-----------|-------------|
| **Reasoning** | OpenAI + `o1*`/`o3*`/`o4*`/`gpt-5*` | `max_tokens: 16384`, `reasoning_effort: "medium"` |
| **Thinking** | Anthropic + Claude 3.5+ / Claude 4+ | `max_tokens: 16384`, `thinking: {enabled: true, budget_tokens: 10000}` |
| **OSS Reasoning** | OpenAI + `deepseek-r1`/`qwq`/`kimi` | `max_tokens: 8192`, `thinking: {enabled: true}` |
| **Standard** | Everything else | `max_tokens: 4096` |

## Top Level

| Parameter | Default | Description |
|-----------|---------|-------------|
| `active_model` | `None` | Which model key to use by default |

## Agents

All agent parameters override the parent model config. Resolution order:
**agent override** > **model config** > **hardcoded fallback**

### Main Agent (`agents.main`)

| Parameter | Default | Unit | Description |
|-----------|---------|------|-------------|
| `model` | inherits `active_model` | | Agent-specific model key |
| `temperature` | inherits model | | |
| `reasoning_effort` | `"medium"` | | |
| `context_tokens` | `200000` | tokens (tiktoken) | Conversation history FIFO. Messages trimmed oldest-first when exceeded |
| `min_context_messages` | `3` | count | Minimum messages kept during context trimming (never trim below this) |

### Code Agent (`agents.code`)

| Parameter | Default | Unit | Description |
|-----------|---------|------|-------------|
| `model` | inherits `active_model` | | |
| `temperature` | `0.0` | | Deterministic code generation |
| `max_tokens` | `16384` | tokens | LLM output limit per code generation call |
| `reasoning_effort` | `"medium"` | | |
| `max_concurrent` | `2` | count | Max parallel code tasks per device |
| `max_iterations` | `5` | count | Self-healing retry limit. Each iteration: LLM generates code, sandbox executes, if error → retry |
| `max_output_tokens` | `500` | tokens (tiktoken) | Truncation limit for stdout/stderr captured from sandbox execution. Injected into LLM context for self-healing |
| `max_status_tokens` | `256` | tokens (tiktoken) | Truncation limit per task in the status block injected into the main agent's system prompt |
| `exec_timeout_secs` | `120` | seconds | Sandbox execution timeout per iteration |
| `prune_age_secs` | `3600` | seconds | Cleanup delivered task results after this many seconds |

### Memory Agent (`agents.memory`)

| Parameter | Default | Unit | Description |
|-----------|---------|------|-------------|
| `model` | inherits `active_model` | | |
| `temperature` | `0.0` | | Deterministic memory extraction |
| `max_tokens` | `1024` | tokens | LLM output limit per memory extraction call |
| `reasoning_effort` | `"medium"` | | |
| `turn_interval` | `5` | count | Run memory extraction every N user turns. Set to `0` to disable |
| `max_words` | `500` | words | Word limit for persisted session memory file |

### Search Agent (`agents.search`)

| Parameter | Default | Unit | Description |
|-----------|---------|------|-------------|
| `model` | inherits `active_model` | | Model for query analysis, evaluation, and synthesis (all 3 phases) |
| `reasoning_effort` | `"medium"` | | |
| `max_concurrent` | `3` | count | Max parallel search queries per device |
| `max_results` | `10` | count | Serper organic results to fetch per query |
| `max_news` | `5` | count | Serper news results to fetch per query |
| `max_people_also_ask` | `5` | count | "People Also Ask" entries to include |
| `max_total_tokens` | `16000` | tokens (tiktoken) | Total token budget for the final search context injected into the main agent |
| `max_deep_read_urls` | `5` | count | Max URLs to fetch full page content for in Phase 3 (deep read). Also passed to the evaluate LLM so it knows the limit |
| `max_page_tokens` | `2500` | tokens (tiktoken) | Per-page token budget for deep-read content. Each fetched page is truncated to this limit before synthesis |
| `fetch_timeout_secs` | `15` | seconds | HTTP timeout for fetching deep-read pages |
| `prune_age_secs` | `3600` | seconds | Cleanup delivered search results after this many seconds |

---

## Search Pipeline Phases

The search agent runs a 3-phase pipeline:

1. **Phase 1 — Query Analysis**: LLM refines the raw query into 1-3 optimized Serper API queries with locale, language, and time filters
2. **Phase 2 — Evaluate**: Serper results fetched, formatted, and sent to LLM. LLM decides: `"sufficient"` (snippets answer the query) or `"need_deep_read"` (fetch full pages). Up to `max_deep_read_urls` URLs selected proportionally across result categories (organic, news, PAA)
3. **Phase 3 — Deep Read + Synthesize**: Selected URLs fetched via HTTP (trafilatura text extraction), each truncated to `max_page_tokens`. LLM synthesizes a comprehensive answer from snippets + full page content

Final output truncated to `max_total_tokens` before injection into the main agent's system prompt.

---

## Provider-Specific Behavior

### OpenAI / OpenAI-compatible

- Reasoning models (`o1*`, `o3*`, `o4*`, `gpt-5*`): `max_tokens` sent as `max_completion_tokens`, `temperature` omitted, `reasoning_effort` sent
- OSS reasoning models on vLLM (DeepSeek R1, QwQ, Kimi): `chat_template_kwargs.enable_thinking` sent when `thinking.enabled: true`
- `frequency_penalty` and `presence_penalty` sent for non-reasoning models only

### Anthropic

- When `thinking.enabled: true`: `temperature` omitted (API requirement), `thinking.budget_tokens` sent (fallback: `10000`)
- `max_tokens` fallback: `4096` when unspecified
- `frequency_penalty` and `presence_penalty` not supported (ignored)

---

## File Paths

| Path | Description |
|------|-------------|
| `~/.rabb1tclaw/config.yaml` | Main configuration file |
| `~/.rabb1tclaw/devices.yaml` | Registered device tokens |
| `~/.rabb1tclaw/<token_prefix>/` | Per-device session data |
| `~/.rabb1tclaw/<token_prefix>/<session>.memory.md` | Persisted session memory |
