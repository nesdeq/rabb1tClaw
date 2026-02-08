# Model Support Matrix

Verified against live APIs via `tests/testmodels.rs`.

## Parameter Support by Provider

| Parameter | OpenAI (standard) | OpenAI (reasoning) | Anthropic | DeepInfra / vLLM |
|---|---|---|---|---|
| `max_tokens` | yes | auto → `max_completion_tokens` | yes (required) | yes |
| `temperature` | yes | omitted (server decides) | yes (omitted when thinking on) | yes |
| `top_p` | yes | yes | yes | yes |
| `frequency_penalty` | yes | yes | ignored | yes |
| `presence_penalty` | yes | yes | ignored | yes |
| `reasoning_effort` | ignored | yes (`low`/`medium`/`high`) | ignored | ignored |
| `thinking.enabled` | ignored | ignored (always on) | yes (extended thinking) | yes (on/off toggle) |
| `thinking.budget_tokens` | ignored | ignored | yes (caps thinking tokens) | ignored |

## What `thinking` actually does per provider

### Anthropic (claude-sonnet-4-5, claude-opus-4, ...)
- `enabled: true` → sends `thinking: {type: "enabled", budget_tokens: N}`
- `budget_tokens` controls how many tokens the model can spend thinking (default 10000)
- Temperature is automatically omitted when thinking is enabled (API requirement)
- Thinking output is streamed as separate blocks and filtered out — only the final answer is returned

### DeepInfra / vLLM (Kimi-K2.5, DeepSeek R1, Qwen QwQ, ...)
- `enabled: true` → sends `chat_template_kwargs: {enable_thinking: true}`
- `enabled: false` → sends `chat_template_kwargs: {enable_thinking: false}`
- **`budget_tokens` is ignored** — DeepInfra has no thinking budget control
- **`reasoning_effort` is ignored** — DeepInfra silently drops it
- Tested: `reasoning_effort`, `thinking_budget`, `max_thinking_tokens`, `budget_tokens` in kwargs, string values for `enable_thinking` — all accepted with HTTP 200 but **none had any observable effect**
- It's a binary toggle: thinking on or thinking off, nothing in between
- When on, reasoning appears in a separate `reasoning_content` field and is automatically discarded
- When off, the model skips chain-of-thought entirely (much faster, fewer SSE chunks)

### OpenAI reasoning (o1, o3, o4-mini, gpt-5, gpt-5.2, ...)
- `thinking` is ignored — these models always reason internally
- Use `reasoning_effort` instead (`low`/`medium`/`high`) to control how hard they think
- `max_tokens` is automatically sent as `max_completion_tokens`
- Temperature is automatically omitted

### OpenAI standard (gpt-4o, etc.)
- `thinking` is ignored
- All standard params work: `max_tokens`, `temperature`, `top_p`, `frequency_penalty`, `presence_penalty`

## Reasoning Model Detection

`is_reasoning_model()` in `openai.rs` auto-detects reasoning models by prefix:
- `o1*`, `o3*`, `o4*` (o-series)
- `gpt-5*` (GPT-5.x family)

This triggers: `max_tokens` → `max_completion_tokens`, temperature omitted, `reasoning_effort` forwarded.

## Reasoning Output Handling

Three paradigms exist for how models return chain-of-thought. All are handled:

| Paradigm | Where it appears | How we handle it |
|---|---|---|
| **Anthropic thinking blocks** — `content_block_start` with `type: "thinking"`, deltas with `type: "thinking_delta"` | Anthropic API | `parse_anthropic_sse` filters thinking blocks, only forwards `text_delta` |
| **Separate `reasoning_content` field** — delta has `reasoning_content` alongside or instead of `content` | DeepInfra, vLLM providers (Kimi-K2.5, DeepSeek R1, Qwen QwQ) | `parse_openai_sse` only forwards `content`, silently discards `reasoning_content` |
| **Inline `<think>` tags** — reasoning in `<think>...</think>` within `content` | Together AI, Fireworks, some self-hosted | `check_think_block` in `stream.rs` buffers and strips the think block (called from `runner.rs`) |

## Models Tested

| Model | Provider | Type | Key findings |
|---|---|---|---|
| `claude-sonnet-4-5-20250929` | Anthropic | Standard + thinking | Thinking blocks in SSE; `budget_tokens` controls thinking length; temperature must be omitted |
| `gpt-4o` | OpenAI | Non-reasoning | All standard params accepted |
| `gpt-5.2` | OpenAI | Reasoning | `max_completion_tokens` + `reasoning_effort` work; no `reasoning_content` in delta |
| `moonshotai/Kimi-K2.5` | DeepInfra | OSS reasoning | `reasoning_content` in delta; `enable_thinking` toggle works; no budget/effort control |

## Code Paths

- `src/provider/openai.rs` — OpenAI-compatible provider (OpenAI + DeepInfra), `chat_template_kwargs` mapping
- `src/provider/anthropic.rs` — Anthropic provider, thinking block filtering
- `src/agent/stream.rs` — `<think>` tag stripping (`check_think_block`), marker filtering (`MarkerFilter`)
- `src/agent/runner.rs` — stream processing, agent spawning
- `src/config/native.rs` — `ModelConfig`, `ThinkingConfig`, config reference comments
