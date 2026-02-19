# Agent Workflow

## Overview

Background agents that run autonomously after the main conversation agent responds. Three kinds:

- **Code agent** — runs Python in a hakoniwa-sandboxed shell, spawned via `@@dispatch` blocks with `{"type":"code","desc":"..."}`. Multi-turn and self-healing: writes code, executes it, captures errors, feeds errors back to the LLM, and retries. Each device gets a persistent workspace with a reusable `.venv`.
- **Search agent** — 5-phase LLM-powered web search pipeline, spawned via `@@dispatch` blocks with `{"type":"search","desc":"..."}`. Phase 1: LLM analyzes query → refined Serper params (locale, language, time filter) + depth decision. Phase 2: multi-type Serper fetch with deduplication. Phase 3: enrich all results via trafilatura page content extraction. Phase 4: token-budgeted context assembly. Phase 5: LLM synthesis with sources. Gracefully degrades to raw snippets if no search model is configured.
- **Advanced agent** — LLM orchestrator with its own conversation context, spawned via `@@dispatch` blocks with `{"type":"advanced","desc":"..."}`. Plans multi-step tasks, delegates to code and search agents, observes results, compresses its own context when needed, and iterates until done. Can ask the user questions mid-task via `{"id":N,"answer":"..."}` answer dispatch relay through the main agent. Logs every step to an admin-visible log file.

All agents run concurrently in the background. Results are delivered on the user's next interaction via system prompt injection. All operational limits are configurable via `agents.code.*`, `agents.search.*`, and `agents.advanced.*` in config (see defaults below).

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
│  2. Build system prompt (compiled-in, src/prompts/system.md)            │
│     Replace: {code_max_concurrent}, {search_max_concurrent},            │
│              {advanced_max_concurrent}                                  │
│  3. Inject awareness context (date/time)                                │
│  4. Inject session memory (memory.rs :: load_session_memory)            │
│  5. ┌─────────────────────────────────────────────────────────┐         │
│     │  INJECT TASK CONTEXT (if any tasks exist)               │         │
│     │  tasklog.rs :: build_task_context(state, prefix, max)   │         │
│     │  → reads persisted task log (tasks.md per device)       │         │
│     │  → reads live running tasks from all 3 trackers         │         │
│     │  → emits @@task ... @@end text block prepended to msg   │         │
│     │  Events: dispatched, completed, failed, asking, [live]  │         │
│     └─────────────────────────────────────────────────────────┘         │
│  6. Build messages from session history + new user message              │
│  7. Trim pairs to context budget (token-based FIFO)                     │
│  8. Spawn streaming LLM call                                            │
└─────────────────────────────┬───────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  runner.rs :: stream_agent_response()                                   │
│                                                                         │
│  Stream chunks → emit deltas to client → accumulate full_response       │
│  (think-block stripping for reasoning models)                           │
│  (MarkerFilter strips all marker types from streaming deltas)           │
│                                                                         │
│  On StreamChunk::Done:                                                  │
│  ┌─────────────────────────────────────────────────────────────┐        │
│  │  A. STRIP ALL DISPATCH BLOCKS from response for session     │        │
│  │     clean = strip_task_markers(full_response)               │        │
│  │     → removes all @@dispatch ... @@end blocks               │        │
│  │     → session history gets clean text (no blocks)           │        │
│  │                                                             │        │
│  │  B. RECORD assistant message (clean_response)               │        │
│  │                                                             │        │
│  │  C. DISPATCH BACKGROUND AGENTS (from full_response):        │        │
│  │     dispatch_background_agents(state, token,                │        │
│  │                                full_response)               │        │
│  │                                                             │        │
│  │  D. Emit stream done event (with clean_response)            │        │
│  └─────────────────────────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────┐
│  runner.rs :: dispatch_background_agents()                              │
│                                                                         │
│  Parses unified markers from full_response and spawns agents:           │
│                                                                         │
│  markers = parse_task_markers(full_response)                            │
│  → returns Vec<TaskMarker> with Dispatch and Answer variants            │
│                                                                         │
│  For each TaskMarker::Dispatch { task_type, desc }:                     │
│    match task_type:                                                     │
│      "code"     → tracker.register + tokio::spawn(run_agent(...))       │
│      "search"   → tracker.register + tokio::spawn(run_search(...))      │
│      "advanced" → tracker.register + tokio::spawn(run_advanced_task())  │
│    Registration checks max_concurrent; logs warning if at capacity      │
│                                                                         │
│  For each TaskMarker::Answer { id, answer }:                            │
│    answer_pending_question(state, prefix, id, answer)                   │
│    → finds PendingQuestion matching prefix + task_id                    │
│    → fires oneshot::Sender to unblock the waiting advanced agent        │
│                                                                         │
│  Fire memory subagent (background, non-blocking)                        │
└─────────────────────────────────────────────────────────────────────────┘
```

## Code Agent

```
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
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  SELF-HEALING LOOP (iteration 1..=max_iterations)             │      │
│  │                                                               │      │
│  │  ┌─────────────────────────────────────────────────────┐      │      │
│  │  │  LLM CALL                                           │      │      │
│  │  │  ChatRequest {                                      │      │      │
│  │  │    model: resolved model_id                         │      │      │
│  │  │    system: src/prompts/system_code.md               │      │      │
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
│  │  │  env_vars: provider API keys from config            │      │      │
│  │  │  timeout: exec_timeout_secs                         │      │      │
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
│  │  │  return Ok((stdout, iteration))                            │      │
│  │  └────────────────────────────────────────────────────────────┘      │
│  │                                                               │      │
│  │  If all iterations exhausted:                                 │      │
│  │    return Err((last_error, max_iterations))                   │      │
│  └───────────────────────────────────────────────────────────────┘      │
│                                                                         │
│  tracker.complete(Completed { output })                                 │
│  or tracker.complete(Failed { error })                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

## Search Agent

```
┌─────────────────────────────────────────────────────────────────────────┐
│  search/agent.rs :: run_search() — BACKGROUND TOKIO TASK                │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 1: SEARCH PLAN (1 LLM call)                            │      │
│  │  resolve_agent_model(state, AgentKind::Search)                │      │
│  │  call_llm(SEARCH_ANALYZE_PROMPT, raw_query)                   │      │
│  │  → JSON: depth (quick/thorough) + refined queries             │      │
│  │  Each query has: q, type, gl, hl, tbs, location               │      │
│  │  Fallback: thorough + raw query if LLM unavailable or fails   │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 2: MULTI-TYPE SERPER FETCH (parallel API calls)        │      │
│  │                                                               │      │
│  │  Quick:    1 type per query (web past-day)                    │      │
│  │  Thorough: 3 types per query (web all-time + past-day + news) │      │
│  │                                                               │      │
│  │  fetch_serper() for each variant (all in parallel)            │      │
│  │  → organic + news + knowledge graph + PAA                     │      │
│  │  → collect_and_dedup() — deduplicate by URL across all        │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 3: ENRICH (parallel URL fetch + trafilatura)           │      │
│  │  Fetches ALL result URLs in parallel                          │      │
│  │  fetch_and_extract(url) → trafilatura content extraction      │      │
│  │  Each page truncated to max_page_tokens                       │      │
│  │  KG/PAA entries pass through with snippet as content          │      │
│  │  Failed fetches gracefully degrade to snippet-only            │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 4: TOKEN-BUDGETED CONTEXT ASSEMBLY                     │      │
│  │  create_context() — Title/URL/Snippet/Content per result      │      │
│  │  Stops adding when max_total_tokens (quick) or                │      │
│  │  max_total_tokens_thorough (thorough) would be exceeded       │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  PHASE 5: LLM SYNTHESIS                                       │      │
│  │  call_llm(SEARCH_SYNTHESIZE_PROMPT, query + raw_context)      │      │
│  │  LLM synthesizes comprehensive answer with sources            │      │
│  │  Skipped if no search model configured (raw context returned) │      │
│  └───────────────────────────────────────────────────────────────┘      │
│                                                                         │
│  tracker.complete(Completed { context })                                │
│  or tracker.complete(Failed { error })                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

## Advanced Agent

```
┌─────────────────────────────────────────────────────────────────────────┐
│  advanced/agent.rs :: run_advanced_task() — BACKGROUND TOKIO TASK       │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  SETUP                                                        │      │
│  │  1. Open TaskLog at ~/.rabb1tclaw/<prefix>/advanced_<id>.log  │      │
│  │  2. resolve_agent_model(state, AgentKind::Advanced)           │      │
│  │  3. Read limits: max_steps, total_timeout, code limits        │      │
│  │  4. Collect API env vars from GatewayConfig.providers         │      │
│  │     → formatted as <PROVIDER_NAME>_API_KEY                    │      │
│  │     → also checks OPENAI_API_KEY, ANTHROPIC_API_KEY,          │      │
│  │       SERP_API_KEY from environment                           │      │
│  │  5. Build system prompt (src/prompts/system_advanced.md)      |      │      
│  │     → replace {available_apis} with env var listing           │      │
│  │  6. messages = [user: "## Task\n\n{description}"]             │      │
│  │  7. pinned_count = 1 (task message)                           │      │
│  └───────────────────────────┬───────────────────────────────────┘      │
│                              │                                          │
│                              ▼                                          │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  ORCHESTRATION LOOP (step 1..=max_steps)                      │      │
│  │                                                               │      │
│  │  Check total timeout (pauses during question wait)            │      │
│  │  Update tracker → Running { step, detail }                    │      │
│  │                                                               │      │
│  │  ┌──────────────────────────────────────────────────────┐     │      │
│  │  │  LLM CALL                                            │     │      │
│  │  │  resolved.chat_request(messages, system_prompt)      │     │      │
│  │  │  → provider.chat_stream() → collect_stream()         │     │      │
│  │  │                                                      │     │      │
│  │  │  Push response as assistant message.                 │     │      │
│  │  │  After step 1: pinned_count = 2 (task + plan)        │     │      │
│  │  └──────────────────────────┬───────────────────────────┘     │      │
│  │                             │                                 │      │
│  │                             ▼                                 │      │
│  │  ┌──────────────────────────────────────────────────────┐     │      │
│  │  │  PARSE DIRECTIVES from response                      │     │      │
│  │  │  Scan for fenced blocks: ```code, ```search,         │     │      │
│  │  │  ```question, ```done                                │     │      │
│  │  │  Unknown fence types (```python, etc.) ignored.      │     │      │
│  │  │  Process FIRST directive only (one per turn).        │     │      │
│  │  │  If no directive found → prompt LLM to emit one.     │     │      │
│  │  └──────────────────────────┬───────────────────────────┘     │      │
│  │                             │                                 │      │
│  │            ┌────────────────┼────────────────┐                │      │
│  │            ▼                ▼                ▼                │      │
│  │     ┌───────────┐  ┌────────────┐  ┌──────────────┐           │      │
│  │     │ ```done   │  │ ```code    │  │ ```search    │           │      │
│  │     └─────┬─────┘  └──────┬─────┘  └──────┬───────┘           │      │
│  │           │               │               │                   │      │
│  │           ▼               ▼               ▼                   │      │
│  │     Return Ok(       run_code_        run_search_             │      │
│  │       summary,       subtask()        subtask()               │      │
│  │       step)          (see below)      (see below)             │      │
│  │                           │               │                   │      │
│  │            ┌──────────────┘               │                   │      │
│  │            │    ┌─────────────────────────┘                   │      │
│  │            ▼    ▼                                             │      │
│  │     Push result as user message                               │      │
│  │     (success or failure feedback)                             │      │
│  │                                                               │      │
│  │            ▼                                                  │      │
│  │     ┌──────────────┐                                          │      │
│  │     │ ```question  │                                          │      │
│  │     └──────┬───────┘                                          │      │
│  │            │                                                  │      │
│  │            ▼                                                  │      │
│  │     Update tracker → NeedsInput { question }                  │      │
│  │     Create oneshot channel                                    │      │
│  │     Store PendingQuestion { prefix, task_id, answer_tx }      │      │
│  │       in state.advanced_questions                             │      │
│  │     AWAIT answer_rx (blocks indefinitely)                     │      │
│  │     → timeout clock pauses during wait                        │      │
│  │     Push user answer as message                               │      │
│  │                                                               │      │
│  │  ┌──────────────────────────────────────────────────────┐     │      │
│  │  │  CONTEXT COMPRESSION CHECK                           │     │      │
│  │  │  If total message chars > 80,000:                    │     │      │
│  │  │    compress_context(messages, pinned_count,          │     │      │
│  │  │                     keep_recent=4)                   │     │      │
│  │  │    → LLM summarizes older working-zone messages      │     │      │
│  │  │    → Pinned zone (task + plan) NEVER compressed      │     │      │
│  │  │    → Last 4 messages kept uncompressed               │     │      │
│  │  │    → Boundary adjusted to avoid consecutive same-    │     │      │
│  │  │      role messages                                   │     │      │
│  │  └──────────────────────────────────────────────────────┘     │      │
│  │                                                               │      │
│  │  If max_steps exhausted: return Err("max steps exceeded")     │      │
│  └───────────────────────────────────────────────────────────────┘      │
│                                                                         │
│  tracker.complete(Completed { summary })                                │
│  or tracker.complete(Failed { error })                                  │
│  Clean up any PendingQuestions for this task_id                         │
└─────────────────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────┐
│  run_code_subtask() — INLINE (not a separate tracked task)              │
│                                                                         │
│  Uses AgentKind::Code model but advanced agent's operational limits.    │
│  Shares the same workspace as the normal code agent.                    │
│                                                                         │
│  1. resolve_agent_model(state, AgentKind::Code)                         │
│  2. ensure_venv(workspace) [spawn_blocking]                             │
│  3. Self-healing loop (1..=code_max_iters):                             │
│     a. LLM call (system: system_code.md)                                │
│     b. Extract packages → pip install [spawn_blocking]                  │
│     c. Extract ```python block → write adv_<id>.py                      │
│     d. execute_in_sandbox(workspace, script, timeout, env_vars)         │
│        → env_vars from provider API keys (NOT the LLM context)          │
│     e. Success → return output                                          │
│     f. Failure → feed error to LLM → continue                           │
│  4. No verification step (orchestrator evaluates results itself)        │
│                                                                         │
│  Note: env vars are injected via hakoniwa Command::env(), never         │
│  passed through any LLM context. Python accesses via os.environ[].      │
└─────────────────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────┐
│  run_search_subtask() — INLINE (not a separate tracked task)            │
│                                                                         │
│  Calls run_search_inner() directly with Search agent's limits.          │
│  Uses AgentKind::Search model. Same 5-phase pipeline as tracked search. │
│  Returns the synthesized text result to the orchestration loop.         │
└─────────────────────────────────────────────────────────────────────────┘
```

## Question/Answer Flow (Advanced Agent ↔ User)

```
Advanced agent emits ```question directive
        │
        ▼
Tracker status → NeedsInput { question }
Store PendingQuestion { prefix, task_id, answer_tx }
Advanced agent loop blocks (awaits oneshot receiver)
Timeout clock paused during wait.
        │
        ▼
Main agent task context injection shows:
  @@task
  asking #N advanced — task desc
  Question: <question text>
  @@end
        │
        ▼
Main agent relays question to user naturally
        │
        ▼
User answers → main agent emits:
  @@dispatch
  [{"id": N, "answer": "the user's answer text"}]
  @@end
        │
        ▼
dispatch_background_agents() parses unified marker
  → answer_pending_question(state, prefix, id, answer)
  → finds first PendingQuestion matching this device prefix
  → fires oneshot sender
        │
        ▼
Advanced agent resumes with answer in context
  messages.push(user: "**User answer:** {answer}")
  → next orchestration step
```

## Task Context Injection (Next User Message)

```
build_task_context() (tasklog.rs) reads the persisted task log (tasks.md)
and live running tasks from all three trackers, emitting a text block
prepended to the user message:

  @@task
  dispatched #1 code — analyze sales data
  completed #2 code — generate chart
  Output: Created chart.png ...
  failed #3 code — broken task
  Error: SyntaxError on line 5
  completed #4 search — latest rust news
  Context: ...
  asking #5 advanced — Build report
  Question: PDF or markdown?
  [live] running #6 code — compute stats (executing iteration 2)
  @@end

  <actual user message>

Task log is persistent (FIFO, capped at task_log_max_entries).
Live running tasks are appended from BackgroundTracker get_running().
Main LLM sees results → weaves them into its response naturally.
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

  # Advanced agent code subtasks additionally inject env vars:
  cmd.env("PROVIDER_API_KEY", value)  # API keys from GatewayConfig.providers
```

## Filesystem Layout

```
~/.rabb1tclaw/
  config.yaml
  devices.yaml
  <token_prefix>/                   # one per device — all device data grouped here
    conversation.enc                # encrypted conversation session
    memory.md                       # session memory
    workspace/                      # persistent workspace for code agent
      .venv/                        # Python venv (created on first code task)
      task_a1b2c3d4.py              # scripts from normal code agent runs
      adv_e5f6g7h8.py              # scripts from advanced agent code subtasks
      output.csv                    # any files the code creates
      chart.png
    advanced_<task_id>.log          # admin-visible log for each advanced task
```

## Dispatch Format

All agent dispatch and answer relay use `@@dispatch` / `@@end` blocks (`markers.rs`):

```
@@dispatch
[{"type":"code","desc":"specific description of what to implement"}]
@@end

@@dispatch
[{"type":"search","desc":"specific search query"},{"type":"code","desc":"compute something"}]
@@end

@@dispatch
[{"id":3,"answer":"the user's answer to an advanced agent question"}]
@@end
```

- `@@dispatch` must be on its own line, `@@end` must be on its own line
- Content is a JSON array validated by serde
- Parsed by `parse_task_markers()` → `Vec<TaskMarker>` with `Dispatch` and `Answer` variants
- Stripped by `strip_task_markers()` before recording to session history
- `Dispatch` entries spawn background agents (up to `max_concurrent` each)
- `Answer` entries relay user answers to pending advanced agent questions
- Blocks are also filtered from streaming deltas by `MarkerFilter` in `stream.rs`

## Advanced Agent Directive Format

The advanced agent LLM emits fenced blocks in its responses:
```
```code
Natural language task description for the code agent.
\```

```search
Search query keywords.
\```

```question
Question for the user.
\```

```done
Summary of what was accomplished and where results can be found.
\```
```

Only `code`, `search`, `question`, `done` are recognized. Other fence types (e.g. ` ```python `) are ignored. One directive per turn — only the first is processed.

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

All values are configurable via `agents.<kind>.*` in config.yaml.

### Code Agent (`agents.code.*`)

| Config key | Default | Purpose |
|---|---|---|
| `max_iterations` | 5 | Self-healing retry limit |
| `max_concurrent` | 2 | Parallel code agents per device |
| `exec_timeout_secs` | 120 | Sandbox execution timeout |
| `max_output_tokens` | 500 | Stdout/stderr truncation (tiktoken tokens) |

### Search Agent (`agents.search.*`)

| Config key | Default | Purpose |
|---|---|---|
| `model` | (active_model) | LLM for query analysis and synthesis |
| `max_concurrent` | 3 | Parallel searches per device |
| `max_results` | 10 | Serper organic results to fetch |
| `max_news` | 5 | Serper news results to fetch |
| `max_people_also_ask` | 5 | "People Also Ask" entries to include |
| `max_total_tokens` | 16000 | Quick-depth search context budget (tiktoken tokens) |
| `max_total_tokens_thorough` | 32000 | Thorough-depth search context budget (tiktoken tokens) |
| `max_page_tokens` | 4000 | Per-page token budget for enriched content |
| `fetch_timeout_secs` | 15 | HTTP timeout for fetching pages |

If no search model is configured (no `agents.search.model` and no `active_model`), the search agent degrades gracefully to raw Serper snippets without LLM processing.

### Advanced Agent (`agents.advanced.*`)

| Config key | Default | Purpose |
|---|---|---|
| `model` | (active_model) | LLM for orchestration planning and reasoning |
| `max_concurrent` | 1 | Parallel advanced tasks per device |
| `max_iterations` | 20 | Max orchestration loop steps |
| `exec_timeout_secs` | 900 | Total wall-clock timeout for entire task |
| `max_output_tokens` | 500 | Stdout/stderr truncation for code subtasks (tokens) |

Code subtask operational limits (within the advanced agent):
- `code_max_iterations`: 8 (hardcoded — advanced tasks get more retries than normal)
- `code_exec_timeout_secs`: 300 (hardcoded — longer timeout for complex subtasks)

The code subtask uses `AgentKind::Code` model for code generation, but the advanced agent's own limits for iterations/timeouts/output truncation. Search subtasks use `AgentKind::Search` model and search agent's limits for the pipeline.

## System Prompts

| File | Used by | Purpose |
|---|---|---|
| `src/prompts/system.md` | Main agent | Conversation, delegation rules, marker format |
| `src/prompts/system_code.md` | Code agent | Python generation, response format |
| `src/prompts/system_memory.md` | Memory subagent | Extract explicit facts/instructions only |
| `src/prompts/system_search_analyze.md` | Search Phase 1 | Query → refined queries + depth + Serper params |
| `src/prompts/system_search_synthesize.md` | Search Phase 5 | Enriched content → final synthesis with sources |
| `src/prompts/system_advanced.md` | Advanced agent | Directive format, planning, delegation guidelines |

All loaded via `include_str!()` and defined as constants in `config/native.rs`.

## Integration Points in runner.rs

1. **System prompt build** (before message building):
   - Replace `{code_max_concurrent}`, `{search_max_concurrent}`, `{advanced_max_concurrent}`
   - Inject awareness, memory, code status, search results, advanced status
2. **Streaming**: `MarkerFilter` in `stream.rs` hides `@@dispatch` blocks from device in real-time
3. **On stream done** (in `stream_agent_response()`):
   - `strip_task_markers()` removes all markers for session storage
   - Record clean_response to session
   - `dispatch_background_agents()` parses + spawns from full_response
4. **Background dispatch** (in `dispatch_background_agents()`):
   - `parse_task_markers()` → `Vec<TaskMarker>` with `Dispatch` and `Answer` variants
   - `Dispatch` markers spawn code/search/advanced agents by type
   - `Answer` markers relay user answers to pending advanced questions
   - Fire memory subagent

## Dependencies

- `hakoniwa` 1.3 — Linux sandboxing (namespaces, bind mounts, rlimits)
- `passt` — must be installed on host for sandbox network access (pip, HTTP)
- `python3` (or `python`) — any installation works: system, pyenv, virtualenv, etc.
- `rs-trafilatura` 0.1 — content extraction from HTML pages (search agent Phase 3 deep reading)
- `tiktoken-rs` — token counting for output truncation and context budgets (o200k_base encoding)
