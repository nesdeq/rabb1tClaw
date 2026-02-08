# Agent Workflow

## Overview

Background agents that run autonomously after the main conversation agent responds. Two kinds:

- **Code agent** — runs Python in a hakoniwa-sandboxed shell, spawned via `<!--code_task:-->` markers. Multi-turn and self-healing: writes code, executes it, captures errors, feeds errors back to the LLM, and retries. Each device gets a persistent workspace with a reusable `.venv`.
- **Search agent** — 3-phase LLM-powered web search pipeline, spawned via `<!--web_search:-->` markers. Phase 1: LLM analyzes query → refined Serper params (locale, language, time filter). Phase 2: Serper API calls + LLM evaluates snippet sufficiency. Phase 3 (conditional): deep-reads top URLs via trafilatura + LLM synthesizes final answer with sources and dates. Gracefully degrades to raw snippets if no search model is configured.

Both agents run concurrently in the background. Results are delivered on the user's next interaction via system prompt injection. All operational limits are configurable via `agents.code.*` and `agents.search.*` in config (see defaults below).

## Activity Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         USER SENDS MESSAGE                              │
└─────────────────────────────┬───────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  runner.rs :: handle_agent()                                            │
│                                                                         │
│  1. Resolve active_model → ModelConfig → ProviderConfig                 │
│  2. Build system prompt (compiled-in)                                   │
│  3. Inject awareness context (date/time)                                │
│  4. Inject session memory (memory.rs :: load_session_memory)            │
│  5. ┌─────────────────────────────────────────────────────────┐         │
│     │  INJECT CODE TASK STATUS (if any)                       │         │
│     │  code_agent :: build_task_status_block(tracker, prefix) │         │
│     │  → appends <!-- Background Tasks --> block              │         │
│     │  tracker.mark_delivered(prefix)                         │         │
│     └─────────────────────────────────────────────────────────┘         │
│  5b.┌─────────────────────────────────────────────────────────┐         │
│     │  INJECT SEARCH RESULTS (if any)                         │         │
│     │  search :: build_search_results_block(tracker, prefix)  │         │
│     │  → appends <!-- Web Search Results --> block            │         │
│     │  tracker.mark_delivered(prefix)                         │         │
│     └─────────────────────────────────────────────────────────┘         │
│  6. Build messages from session history + new user message              │
│  7. Trim pairs to context budget                                        │
│  8. Spawn streaming LLM call                                            │
└─────────────────────────────┬───────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  runner.rs :: stream_agent_response()                                   │
│                                                                         │
│  Stream chunks → emit deltas to client → accumulate full_response       │
│  (think-block stripping for reasoning models)                           │
│                                                                         │
│  On StreamChunk::Done:                                                  │
│  ┌─────────────────────────────────────────────────────────────┐        │
│  │  A. STRIP MARKERS from response                             │        │
│  │     clean = strip_web_search_markers(                       │        │
│  │               strip_code_task_markers(full_response))       │        │
│  │     → session history gets clean text (no markers)          │        │
│  │                                                             │        │
│  │  B. RECORD assistant message (clean_response)               │        │
│  │                                                             │        │
│  │  C. PARSE CODE MARKERS from original full_response          │        │
│  │     tasks = parse_code_task_markers(full_response)          │        │
│  │     For each task description:                              │        │
│  │       task_id = uuid[..8]                                   │        │
│  │       tracker.register(prefix, task_id, desc, max_conc)     │        │
│  │       if registered (< max_concurrent running):             │        │
│  │         tokio::spawn(run_agent(...))  ─────────────────┐    │        │
│  │       else:                                            │    │        │
│  │         log warning (at capacity)                      │    │        │
│  │                                                        │    │        │
│  │  D. PARSE SEARCH MARKERS from original full_response   │    │        │
│  │     searches = parse_web_search_markers(full_response) │    │        │
│  │     For each query:                                    │    │        │
│  │       query_id = uuid[..8]                             │    │        │
│  │       tracker.register(prefix, query_id, query, max_c) │    │        │
│  │       if registered: tokio::spawn(run_search(...))     │    │        │
│  │                                                        │    │        │
│  │  E. Fire memory subagent (background)                  │    │        │
│  │  F. Emit stream done events                            │    │        │
│  └─────────────────────────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────────────────────────┘
                                                            │
                    ┌───────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  code/agent.rs :: run_agent() — BACKGROUND TOKIO TASK                   │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  SETUP                                                        │      │
│  │  1. resolve_agent_model(state, AgentKind::Code)               │      │
│  │  2. workspace = ~/.rabb1tclaw/<token_prefix>/workspace/       │      │
│  │  3. find_python()                                             │      │
│  │     → sys.executable + sys.prefix resolution                  │      │
│  │     → works with system python, pyenv, virtualenvs            │      │
│  │  4. ensure_venv(workspace) [spawn_blocking]                   │      │
│  │     → if .venv missing: python -m venv /workspace/.venv       │      │
│  │  5. list_workspace() → context for LLM                        │      │
│  │  6. messages = ["Task: {desc}\nWorkspace:\n{listing}"]        │      │
│  └───────────────────────────────┬───────────────────────────────┘      │
│                                  │                                      │
│                                  ▼                                      │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  SELF-HEALING LOOP (iteration 1..=5)                          │      │
│  │                                                               │      │
│  │  ┌─────────────────────────────────────────────────────┐      │      │
│  │  │  LLM CALL (via code_agent_request())                │      │      │
│  │  │  ChatRequest {                                      │      │      │
│  │  │    model: resolved model_id                         │      │      │
│  │  │    system: docs/system_code.md                      │      │      │
│  │  │    messages: conversation so far                    │      │      │
│  │  │    params: merged (agent override > model config)   │      │      │
│  │  │  }                                                  │      │      │
│  │  │  → provider.chat_stream() → collect_stream()        │      │      │
│  │  └────────────────────┬────────────────────────────────┘      │      │
│  │                       │                                       │      │
│  │                       ▼                                       │      │
│  │  ┌─────────────────────────────────────────────────────┐      │      │
│  │  │  EXTRACT PACKAGES (### Packages section)            │      │      │
│  │  │  If packages found:                                 │      │      │
│  │  │    pip_install(workspace, packages) [spawn_blocking]│      │      │
│  │  │    → /workspace/.venv/bin/pip install --quiet ...   │      │      │
│  │  │    If pip fails → feed error to LLM → continue      │      │      │
│  │  └────────────────────┬────────────────────────────────┘      │      │
│  │                       │                                       │      │
│  │                       ▼                                       │      │
│  │  ┌─────────────────────────────────────────────────────┐      │      │
│  │  │  EXTRACT CODE (```python block)                     │      │      │
│  │  │  If no code block → feed error to LLM → continue    │      │      │
│  │  │  Write to workspace/task_<id>.py                    │      │      │
│  │  └────────────────────┬────────────────────────────────┘      │      │
│  │                       │                                       │      │
│  │                       ▼                                       │      │
│  │  ┌─────────────────────────────────────────────────────┐      │      │
│  │  │  EXECUTE IN SANDBOX [spawn_blocking]                │      │      │
│  │  │  /workspace/.venv/bin/python /workspace/task_<id>.py│      │      │
│  │  │  timeout: 120s                                      │      │      │
│  │  │                                                     │      │      │
│  │  │  ┌─────────┐     ┌──────────┐                       │      │      │
│  │  │  │ SUCCESS │     │  FAILED  │                       │      │      │
│  │  │  └────┬────┘     └─────┬────┘                       │      │      │
│  │  │       │                │                            │      │      │
│  │  │       │                ▼                            │      │      │
│  │  │       │    messages.push(assistant: response)       │      │      │
│  │  │       │    messages.push(user: "Failed:\n{stderr}   │      │      │
│  │  │       │                        \nFix it.")          │      │      │
│  │  │       │                │                            │      │      │
│  │  │       │                └──► next iteration ─────────┘      │      │
│  │  │       │                                                    │      │
│  │  │       ▼                                                    │      │
│  │  │  VERIFY (unless last iteration)                            │      │
│  │  │    Ask LLM: "Does output satisfy task? LGTM or fix."       │      │
│  │  │    If "LGTM" → accept result                               │      │
│  │  │    If fix code in response → execute fix directly          │      │
│  │  │    Otherwise → continue loop                               │      │
│  │  │                                                            │      │
│  │  │  tracker.complete(Completed { stdout, iteration })         │      │
│  │  │  BREAK                                                     │      │
│  │  └────────────────────────────────────────────────────────────┘      │
│  │                                                               │      │
│  │  If all 5 exhausted:                                          │      │
│  │    tracker.complete(Failed { last_error, 5 })                 │      │
│  └───────────────────────────────────────────────────────────────┘      │
└─────────────────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────┐
│  search/agent.rs :: run_search() — BACKGROUND TOKIO TASK                │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 1: QUERY ANALYSIS (1 LLM call)                         │      │
│  │  resolve_agent_model(state, AgentKind::Search)                │      │
│  │  call_llm(SEARCH_ANALYZE_PROMPT, raw_query)                   │      │
│  │  → JSON: refined queries + Serper params (gl, hl, tbs, type)  │      │
│  │  Fallback: raw query if LLM unavailable or fails              │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 2a: SERPER FETCH (parallel API calls)                  │      │
│  │  fetch_serper(client, api_key, analyzed_query, num)           │      │
│  │  → organic + news + knowledge graph + PAA results             │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 2b: EVALUATE (1 LLM call)                              │      │
│  │  call_llm(SEARCH_EVALUATE_PROMPT, results_text)               │      │
│  │  → JSON verdict: "sufficient" or "need_deep_read"             │      │
│  │                                                               │      │
│  │  ┌──────────────┐     ┌──────────────────┐                    │      │
│  │  │  sufficient  │     │  need_deep_read  │                    │      │
│  │  └──────┬───────┘     └────────┬─────────┘                    │      │
│  │         │                      │                              │      │
│  │         ▼                      ▼                              │      │
│  │  format_eval_results()   PHASE 3: DEEP READ + SYNTHESIZE      │      │
│  │  → done                  fetch_and_extract(urls) [max 3]      │      │
│  │                          → trafilatura content extraction     │      │
│  │                          call_llm(SEARCH_SYNTHESIZE_PROMPT)   │      │
│  │                          → final text with sources + dates    │      │
│  └───────────────────────────────────────────────────────────────┘      │
│                                                                         │
│  tracker.complete(Completed { context })                                │
└─────────────────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────┐
│                      NEXT USER MESSAGE                                  │
│                                                                         │
│  build_task_status_block() injects code results:                        │
│                                                                         │
│  <!-- Background Tasks -->                                              │
│  - [running] "analyze sales data" (started 45s ago)                     │
│  - [completed] "generate chart": Created chart.png ... (2 iterations)   │
│  - [failed] "parse dataset": FileNotFoundError ... (5 iterations)       │
│  <!-- End Background Tasks -->                                          │
│                                                                         │
│  build_search_results_block() injects search results:                   │
│                                                                         │
│  <!-- Web Search Results -->                                            │
│  **Title:** ... **URL:** ... **Snippet:** ... **Content:** ...          │
│  <!-- End Web Search Results -->                                        │
│                                                                         │
│  Main LLM sees results → weaves into response naturally                 │
│  tracker.mark_delivered() → prevents re-injection                       │
│  Old delivered tasks pruned after prune_age_secs (default 1 hour)       │
└─────────────────────────────────────────────────────────────────────────┘
```

## Sandbox Configuration

```
hakoniwa::Container::new()            # MOUNT + USER + PID namespaces, procfs at /proc
  bindmount_ro(python_prefix, same)   # Python installation (RO, same host path for prefix resolution)
  bindmount_ro("/usr", "/usr")        # shared libs and binaries
  symlink/bindmount /lib /bin etc     # distro-aware: symlinks on merged-usr, bind on traditional
  runctl(MountFallback)               # handle cross-filesystem bind mounts in user namespaces
  bindmount_ro(resolv_conf, /etc/resolv.conf)  # real upstream DNS (not systemd-resolved stub)
  bindmount_ro(selective /etc)        # hosts, nsswitch, ssl, ca-certificates, ld.so.*, localtime
  bindmount_rw(workspace, /workspace) # device workspace (RW)
  devfsmount(/dev)                    # minimal device nodes
  tmpfsmount(/tmp)                    # volatile tmpfs
  setrlimit(AS, 2GB)                  # virtual memory cap
  setrlimit(NPROC, 64)               # process limit
  setrlimit(NOFILE, 256)             # file descriptor limit
  unshare(Network)                    # isolated network namespace (only when needed)
  network(Pasta::default())           # user-mode networking via passt (pip, HTTP)
```

## Filesystem Layout

```
~/.rabb1tclaw/
  config.yaml
  devices.yaml
  <token_prefix>/                   # one per device — all device data grouped here
    session/
      <key>.enc                     # encrypted conversation sessions
      <key>.memory.md               # session memory
    workspace/                      # persistent workspace for code agent
      .venv/                        # Python venv (created on first code task)
      task_a1b2c3d4.py              # scripts from code agent runs
      task_e5f6g7h8.py
      output.csv                    # any files the code creates
      chart.png
```

## Marker Format

Main LLM emits in its response:
```
<!--code_task: specific description of what to implement -->
<!--web_search: specific search query -->
```

- **Code markers** parsed by `parse_code_task_markers()`, stripped by `strip_code_task_markers()`
- **Search markers** parsed by `parse_web_search_markers()`, stripped by `strip_web_search_markers()`
- Both stripped before recording to session history
- Multiple markers of either type spawn independent agents (up to `max_concurrent` each)
- Markers are also filtered from streaming deltas by `MarkerFilter` in `stream.rs`

## Code Agent System Prompt Response Format

The code agent LLM must respond with:
```
### Plan
Brief description.

### Packages (optional)
\```
numpy
matplotlib
\```

### Code
\```python
# complete script
\```

### Expected Output
Brief description.
```

## Configurable Defaults

All values are configurable via `agents.code.*` and `agents.search.*` in config.yaml.

### Code Agent (`agents.code.*`)

| Config key | Default | Purpose |
|---|---|---|
| `max_iterations` | 5 | Self-healing retry limit |
| `max_concurrent` | 2 | Parallel code agents per device |
| `exec_timeout_secs` | 120 | Sandbox execution timeout |
| `max_output_chars` | 2000 | Stdout/stderr truncation |
| `prune_age_secs` | 3600 | Remove delivered results after N seconds |

### Search Agent (`agents.search.*`)

| Config key | Default | Purpose |
|---|---|---|
| `model` | (active_model) | LLM for query analysis, evaluation, synthesis |
| `max_concurrent` | 3 | Parallel searches per device |
| `max_results` | 10 | Serper organic results to fetch |
| `max_news` | 5 | Serper news results to fetch |
| `max_people_also_ask` | 5 | "People Also Ask" entries to include |
| `max_total_tokens` | 8000 | Total search context budget (in tokens, ~4 chars/token) |
| `prune_age_secs` | 3600 | Remove delivered results after N seconds |

If no search model is configured (no `agents.search.model` and no `active_model`), the search agent degrades gracefully to raw Serper snippets without LLM processing.

## Integration Points in runner.rs

1. **System prompt injection** (before message building): `build_task_status_block()` + `build_search_results_block()` + `mark_delivered()`
2. **Marker parsing + agent spawn** (in `StreamChunk::Done`):
   - `parse_code_task_markers()` → `tracker.register()` → `tokio::spawn(run_agent())`
   - `parse_web_search_markers()` → `tracker.register()` → `tokio::spawn(run_search())`
3. **Marker stripping** (before session recording): `strip_code_task_markers()` + `strip_web_search_markers()`
4. **Streaming marker filter**: `MarkerFilter` in `stream.rs` hides markers from device in real-time

## Dependencies

- `hakoniwa` 1.3 — Linux sandboxing (namespaces, bind mounts, rlimits)
- `passt` — must be installed on host for sandbox network access (pip, HTTP)
- `python3` (or `python`) — any installation works: system, pyenv, virtualenv, etc.
- `rs-trafilatura` 0.1 — content extraction from HTML pages (search agent Phase 3 deep reading)
