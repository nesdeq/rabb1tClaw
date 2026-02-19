//! Native YAML-based configuration for the Rust gateway.
//!
//! Config location: ~/.rabb1tclaw/config.yaml

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

// ============================================================================
// Paths
// ============================================================================

const CONFIG_DIR: &str = ".rabb1tclaw";
const CONFIG_FILE: &str = "config.yaml";
const DEVICES_FILE: &str = "devices.yaml";

pub fn config_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map_or_else(|| PathBuf::from(CONFIG_DIR), |d| d.home_dir().join(CONFIG_DIR))
}

pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

pub fn devices_path() -> PathBuf {
    config_dir().join(DEVICES_FILE)
}

/// Per-device root directory: `~/.rabb1tclaw/<token_prefix>/`
pub fn device_dir(token_prefix: &str) -> PathBuf {
    config_dir().join(token_prefix)
}

// ============================================================================
// Config Types
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Gateway settings
    #[serde(default)]
    pub gateway: GatewaySettings,

    /// LLM providers — API connections (keyed by provider name)
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Model configurations (keyed by model key)
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Active model key
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<String>,

    /// Per-agent model + parameter overrides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentsSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySettings {
    /// Port to listen on (default: 18789)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Bind IP address (e.g., "127.0.0.1", "0.0.0.0")
    #[serde(default = "default_bind")]
    pub bind: String,
}

const fn default_port() -> u16 {
    18789
}

fn default_bind() -> String {
    "127.0.0.1".to_string()
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind: default_bind(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// API type: "openai" or "anthropic"
    pub api: String,

    /// Base URL for the API
    pub base_url: String,

    /// API key
    pub api_key: String,

    /// Optional display name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Which provider key this model uses
    pub provider: String,

    /// Model ID sent to the API (e.g. "gpt-4o", "claude-opus-4-6")
    pub model_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// "low"/"medium"/"high" — `OpenAI` o-series reasoning effort
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,

    /// Max context tokens for conversation history FIFO (default 200 000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,

    /// Anthropic extended thinking / OSS <think> control
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    // ── Model parameter overrides ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,

    // ── Operational limits (agent-kind-specific; ignored when N/A) ──
    /// Max concurrent background tasks/queries per device (code, search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,
    /// Max self-healing iterations before giving up (code)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Max tokens of stdout/stderr kept per execution (code)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    /// Sandbox execution timeout in seconds (code)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_timeout_secs: Option<u64>,
    /// Run memory extraction every N user turns (memory)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_interval: Option<usize>,
    /// Max words in persisted session memory (memory)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<usize>,
    /// Max organic search results from Serper (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    /// Max news results from Serper (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_news: Option<usize>,
    /// Max "People Also Ask" entries from Serper (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_people_also_ask: Option<usize>,
    /// Total token budget for all search context — quick depth (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<usize>,
    /// Total token budget for thorough depth (search, defaults to 32000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens_thorough: Option<usize>,
    /// Per-page token budget for deep reads (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_page_tokens: Option<usize>,
    /// HTTP timeout for fetching deep-read URLs in seconds (search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_timeout_secs: Option<u64>,
    /// Conversation history FIFO in tokens (main)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// Max entries in the persistent task log file (main)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_log_max_entries: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<AgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<AgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<AgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<AgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced: Option<AgentConfig>,
}

// ============================================================================
// Agent Kind + Config Accessors
// ============================================================================

/// Which agent is being resolved — controls default params and inheritance.
#[derive(Debug, Clone, Copy)]
pub enum AgentKind { Main, Code, Memory, Search, Advanced }

impl GatewayConfig {
    /// Quick typed access to a specific agent's config block.
    pub fn agent_config(&self, kind: AgentKind) -> Option<&AgentConfig> {
        self.agents.as_ref().and_then(|a| match kind {
            AgentKind::Main => a.main.as_ref(),
            AgentKind::Code => a.code.as_ref(),
            AgentKind::Memory => a.memory.as_ref(),
            AgentKind::Search => a.search.as_ref(),
            AgentKind::Advanced => a.advanced.as_ref(),
        })
    }
}

// ============================================================================
// Agent Assignment Query
// ============================================================================

/// Return which agent roles (e.g. "main", "code", "memory") use a given model key.
pub fn model_agent_roles(config: &GatewayConfig, model_key: &str) -> Vec<&'static str> {
    const AGENTS: &[(AgentKind, &str)] = &[
        (AgentKind::Main, "main"),
        (AgentKind::Code, "code"),
        (AgentKind::Memory, "memory"),
        (AgentKind::Search, "search"),
        (AgentKind::Advanced, "advanced"),
    ];
    let is_active = config.active_model.as_deref() == Some(model_key);
    AGENTS.iter().filter_map(|&(kind, label)| {
        let model = config.agent_config(kind).and_then(|a| a.model.as_ref());
        model.map_or(is_active, |m| m == model_key).then_some(label)
    }).collect()
}

// ============================================================================
// Config Loading/Saving
// ============================================================================

/// Check if config exists
pub fn config_exists() -> bool {
    config_path().exists()
}

/// Load config
pub fn load_config() -> Result<GatewayConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(GatewayConfig::default());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;

    let config: GatewayConfig = serde_yml::from_str(&content)
        .with_context(|| format!("Failed to parse config from {}", path.display()))?;

    Ok(config)
}

/// Write data to a file with 0o600 permissions, creating parent dirs as needed.
pub fn write_secure(path: &std::path::Path, content: impl AsRef<[u8]>) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create dir {}", dir.display()))?;
    }
    fs::write(path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Save config with commented parameter reference appended
pub fn save_config(config: &GatewayConfig) -> Result<()> {
    let path = config_path();
    let mut content = serde_yml::to_string(config).context("Failed to serialize config")?;
    content.push_str(CONFIG_REFERENCE);
    write_secure(&path, &content)?;

    info!("config saved");
    Ok(())
}

/// Main conversation system prompt (compiled-in, not user-overridable).
pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

/// System prompt for the sandboxed code execution agent.
pub const CODE_AGENT_SYSTEM_PROMPT: &str = include_str!("../prompts/system_code.md");

/// System prompt for the memory extraction subagent.
pub const MEMORY_AGENT_SYSTEM_PROMPT: &str = include_str!("../prompts/system_memory.md");

/// System prompt for search plan generation (depth + query analysis).
pub const SEARCH_ANALYZE_PROMPT: &str = include_str!("../prompts/system_search_analyze.md");

/// System prompt for deep-read synthesis (final step).
pub const SEARCH_SYNTHESIZE_PROMPT: &str = include_str!("../prompts/system_search_synthesize.md");

/// System prompt for the advanced orchestrator agent.
pub const ADVANCED_AGENT_SYSTEM_PROMPT: &str = include_str!("../prompts/system_advanced.md");

/// Commented-out reference appended to every saved config.yaml
const CONFIG_REFERENCE: &str = r#"
# ─── Parameter Reference ─────────────────────────────────────────────
#
# providers:
#   my-provider:
#     api: openai              # "openai" or "anthropic"
#     base_url: https://api.openai.com/v1
#     api_key: sk-...
#     name: My Provider        # optional display name
#
# models:
#   my-model:
#     provider: my-provider    # must match a key in providers
#     model_id: gpt-4o         # model ID sent to the API
#     max_tokens: 4096         # max output tokens (optional)
#     temperature: 0.7         # 0.0–2.0 (optional)
#     top_p: 1.0               # nucleus sampling (optional)
#     frequency_penalty: 0.0   # -2.0–2.0 (optional, OpenAI only)
#     presence_penalty: 0.0    # -2.0–2.0 (optional, OpenAI only)
#     reasoning_effort: medium # low/medium/high (OpenAI o-series & gpt-5.x only)
#     context_tokens: 200000   # conversation history FIFO in tokens (default: 200000)
#     thinking:                # thinking / chain-of-thought control
#       enabled: true
#       budget_tokens: 10000   # Anthropic only — max thinking tokens
#
# active_model: my-model       # which model to use by default
#
# agents:                          # per-agent model overrides + operational limits
#   main:
#     model: my-model              # falls back to active_model if omitted
#     temperature: 0.9             # override model config params
#     reasoning_effort: medium     # baseline for reasoning models
#     context_tokens: 200000       # conversation history FIFO (tokens, tiktoken)
#     task_log_max_entries: 50     # max entries in persistent task log file
#   code:
#     model: cheap-model           # use a different model for code execution
#     temperature: 0.0
#     max_tokens: 16384
#     reasoning_effort: medium
#     max_concurrent: 2            # max parallel code tasks per device
#     max_iterations: 5            # self-healing retry limit
#     max_output_tokens: 500       # stdout/stderr truncation per execution (tokens, tiktoken)
#     exec_timeout_secs: 120       # sandbox timeout
#   memory:
#     model: cheap-model           # use a different model for memory extraction
#     temperature: 0.0
#     max_tokens: 1024
#     reasoning_effort: medium
#     turn_interval: 5             # extract memory every N user turns
#     max_words: 500               # word limit for persisted memory
#   search:
#     model: cheap-model           # model for query analysis / evaluation / synthesis
#     reasoning_effort: medium
#     max_concurrent: 3            # max parallel searches per device
#     max_results: 10              # Serper organic results to fetch
#     max_news: 5                  # Serper news results to fetch
#     max_people_also_ask: 5       # "People Also Ask" entries to include
#     max_total_tokens: 16000      # quick search context budget (tokens, tiktoken)
#     max_total_tokens_thorough: 32000  # thorough search context budget
#     max_page_tokens: 4000        # per-page budget for deep reads (tokens, tiktoken)
#     fetch_timeout_secs: 15       # HTTP timeout for fetching deep-read pages
#   advanced:
#     model: smart-model            # model for orchestration planning
#     reasoning_effort: medium
#     max_concurrent: 1             # max parallel advanced tasks per device
#     max_iterations: 20            # max orchestration loop steps
#     exec_timeout_secs: 600        # total wall-clock timeout for entire task
#     max_output_tokens: 500        # stdout/stderr truncation for code subtasks
#
# ─── How parameters behave per provider ──────────────────────────────
#
# OpenAI (gpt-4o, etc.):
#   All params sent as-is. frequency_penalty & presence_penalty supported.
#
# OpenAI reasoning (o1, o3, o4-mini, gpt-5, gpt-5.2, ...):
#   max_tokens      → sent as max_completion_tokens (automatic)
#   temperature     → omitted (server decides)
#   reasoning_effort→ sent as-is (low/medium/high)
#   thinking        → ignored (reasoning is always on for these models)
#
# Anthropic (claude-sonnet-4-5, claude-opus-4, ...):
#   thinking.enabled      → enables extended thinking
#   thinking.budget_tokens→ max tokens the model can use for thinking
#   temperature           → omitted when thinking is enabled (API requirement)
#   frequency/presence_penalty → not supported, ignored
#   reasoning_effort      → not supported, ignored
#
# DeepInfra / vLLM (Kimi-K2.5, DeepSeek R1, Qwen QwQ, ...):
#   thinking.enabled → on/off toggle (sent as chat_template_kwargs)
#   thinking.budget_tokens → ignored (no budget control on DeepInfra)
#   reasoning_effort → ignored (DeepInfra silently drops it)
#   When thinking is on, reasoning output appears in a separate field
#     and is automatically discarded — only the final answer is returned.
#   When thinking is off, the model skips chain-of-thought (faster).
#   frequency_penalty & presence_penalty are supported.
# ──────────────────────────────────────────────────────────────────────
"#;

