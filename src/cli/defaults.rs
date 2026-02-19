//! Smart model tier detection and default parameter population.
//!
//! All agent defaults are defined as public constants below — the single source
//! of truth. Both `populate_default_agents()` and runtime fallbacks reference these.

// ── Main agent defaults ──
pub const DEFAULT_CONTEXT_TOKENS: u32 = 200_000;
pub const DEFAULT_TASK_LOG_MAX_ENTRIES: usize = 50;

// ── Code agent defaults ──
pub const DEFAULT_CODE_TEMPERATURE: f32 = 0.0;
pub const DEFAULT_CODE_MAX_TOKENS: u32 = 16384;
pub const DEFAULT_CODE_MAX_CONCURRENT: usize = 2;
pub const DEFAULT_CODE_MAX_ITERATIONS: u32 = 5;
pub const DEFAULT_CODE_MAX_OUTPUT_TOKENS: usize = 500;
pub const DEFAULT_CODE_EXEC_TIMEOUT_SECS: u64 = 120;
// ── Memory agent defaults ──
pub const DEFAULT_MEMORY_TEMPERATURE: f32 = 0.0;
pub const DEFAULT_MEMORY_MAX_TOKENS: u32 = 1024;
pub const DEFAULT_MEMORY_TURN_INTERVAL: usize = 5;
pub const DEFAULT_MEMORY_MAX_WORDS: usize = 500;

// ── Model tier defaults (used by apply_smart_defaults + provider fallbacks) ──
pub const DEFAULT_REASONING_MAX_TOKENS_OPENAI: u32 = 16384;
pub const DEFAULT_THINKING_MAX_TOKENS_ANTHROPIC: u32 = 16384;
pub const DEFAULT_OSS_REASONING_MAX_TOKENS: u32 = 8192;
pub const DEFAULT_MODEL_MAX_TOKENS: u32 = 4096;
pub const DEFAULT_THINKING_BUDGET_TOKENS_ANTHROPIC: u32 = 10000;
pub const DEFAULT_REASONING_EFFORT_OPENAI: &str = "medium";

// ── Advanced agent defaults ──
pub const DEFAULT_ADVANCED_MAX_STEPS: u32 = 20;
pub const DEFAULT_ADVANCED_TOTAL_TIMEOUT_SECS: u64 = 900;
pub const DEFAULT_ADVANCED_CODE_MAX_ITERATIONS: u32 = 8;
pub const DEFAULT_ADVANCED_CODE_EXEC_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_ADVANCED_CODE_MAX_OUTPUT_TOKENS: usize = 500;
pub const DEFAULT_ADVANCED_MAX_CONCURRENT: usize = 1;
// ── Search agent defaults ──
pub const DEFAULT_SEARCH_MAX_CONCURRENT: usize = 3;
pub const DEFAULT_SEARCH_MAX_RESULTS: usize = 10;
pub const DEFAULT_SEARCH_MAX_NEWS: usize = 5;
pub const DEFAULT_SEARCH_MAX_PEOPLE_ALSO_ASK: usize = 5;
pub const DEFAULT_SEARCH_MAX_TOTAL_TOKENS: usize = 16000;
pub const DEFAULT_SEARCH_MAX_TOTAL_TOKENS_THOROUGH: usize = 32000;
pub const DEFAULT_SEARCH_MAX_PAGE_TOKENS: usize = 4000;
pub const DEFAULT_SEARCH_FETCH_TIMEOUT_SECS: u64 = 15;
/// Detected model capability tier.
pub(crate) enum ModelTier {
    /// `OpenAI` o-series / gpt-5: `reasoning_effort`, no temperature control
    Reasoning,
    /// Anthropic extended thinking (Claude 3.5+, Claude 4+)
    Thinking,
    /// `DeepInfra`/vLLM reasoning models (`DeepSeek` R1, `QwQ`, Kimi)
    OssReasoning,
    /// Standard chat model
    Standard,
}

/// Detect model capabilities from `api_type` + `model_id`.
pub(crate) fn detect_tier(api_type: &str, model_id: &str) -> ModelTier {
    let id = model_id.to_lowercase();

    // OpenAI reasoning: o1, o3, o4, gpt-5 (reuse provider detection)
    if api_type == "openai" && crate::provider::openai::is_reasoning_model(model_id) {
        return ModelTier::Reasoning;
    }

    // Anthropic thinking: everything except old claude-3 (non-3.5)
    if api_type == "anthropic" {
        let is_old = id.starts_with("claude-3-") && !id.starts_with("claude-3-5");
        if !is_old {
            return ModelTier::Thinking;
        }
    }

    // OSS reasoning models on DeepInfra / vLLM
    if api_type == "openai"
        && (id.contains("deepseek-r1") || id.contains("qwq") || id.contains("kimi"))
    {
        return ModelTier::OssReasoning;
    }

    ModelTier::Standard
}

/// Apply smart defaults to a `ModelConfig` based on detected tier.
/// Prints what was auto-configured so the user knows.
pub(crate) fn apply_smart_defaults(
    mc: &mut crate::config::ModelConfig,
    api_type: &str,
) {
    use crate::config::ThinkingConfig;
    match detect_tier(api_type, &mc.model_id) {
        ModelTier::Reasoning => {
            mc.max_tokens = Some(DEFAULT_REASONING_MAX_TOKENS_OPENAI);
            mc.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string());
            println!("  Auto: reasoning model — reasoning_effort: {DEFAULT_REASONING_EFFORT_OPENAI}, max_tokens: {DEFAULT_REASONING_MAX_TOKENS_OPENAI}");
        }
        ModelTier::Thinking => {
            mc.max_tokens = Some(DEFAULT_THINKING_MAX_TOKENS_ANTHROPIC);
            mc.thinking = Some(ThinkingConfig {
                enabled: true,
                budget_tokens: Some(DEFAULT_THINKING_BUDGET_TOKENS_ANTHROPIC),
            });
            println!("  Auto: thinking model — thinking: on (budget: {DEFAULT_THINKING_BUDGET_TOKENS_ANTHROPIC}), max_tokens: {DEFAULT_THINKING_MAX_TOKENS_ANTHROPIC}");
        }
        ModelTier::OssReasoning => {
            mc.max_tokens = Some(DEFAULT_OSS_REASONING_MAX_TOKENS);
            mc.thinking = Some(ThinkingConfig {
                enabled: true,
                budget_tokens: None,
            });
            println!("  Auto: reasoning model — thinking: on, max_tokens: {DEFAULT_OSS_REASONING_MAX_TOKENS}");
        }
        ModelTier::Standard => {
            mc.max_tokens = Some(DEFAULT_MODEL_MAX_TOKENS);
        }
    }
}

/// Populate the `agents` section with default operational limits.
/// Called during init so every config file ships with visible, editable values.
pub(crate) fn populate_default_agents(config: &mut crate::config::GatewayConfig) {
    let agents = config.agents.get_or_insert_with(Default::default);

    let main = agents.main.get_or_insert_with(Default::default);
    if main.context_tokens.is_none()       { main.context_tokens = Some(DEFAULT_CONTEXT_TOKENS); }
    if main.reasoning_effort.is_none()     { main.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string()); }
    if main.task_log_max_entries.is_none() { main.task_log_max_entries = Some(DEFAULT_TASK_LOG_MAX_ENTRIES); }

    let code = agents.code.get_or_insert_with(Default::default);
    if code.temperature.is_none()        { code.temperature = Some(DEFAULT_CODE_TEMPERATURE); }
    if code.max_tokens.is_none()         { code.max_tokens = Some(DEFAULT_CODE_MAX_TOKENS); }
    if code.reasoning_effort.is_none()   { code.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string()); }
    if code.max_concurrent.is_none()     { code.max_concurrent = Some(DEFAULT_CODE_MAX_CONCURRENT); }
    if code.max_iterations.is_none()     { code.max_iterations = Some(DEFAULT_CODE_MAX_ITERATIONS); }
    if code.max_output_tokens.is_none()  { code.max_output_tokens = Some(DEFAULT_CODE_MAX_OUTPUT_TOKENS); }
    if code.exec_timeout_secs.is_none()  { code.exec_timeout_secs = Some(DEFAULT_CODE_EXEC_TIMEOUT_SECS); }

    let memory = agents.memory.get_or_insert_with(Default::default);
    if memory.temperature.is_none()      { memory.temperature = Some(DEFAULT_MEMORY_TEMPERATURE); }
    if memory.max_tokens.is_none()       { memory.max_tokens = Some(DEFAULT_MEMORY_MAX_TOKENS); }
    if memory.reasoning_effort.is_none() { memory.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string()); }
    if memory.turn_interval.is_none()    { memory.turn_interval = Some(DEFAULT_MEMORY_TURN_INTERVAL); }
    if memory.max_words.is_none()        { memory.max_words = Some(DEFAULT_MEMORY_MAX_WORDS); }

    let search = agents.search.get_or_insert_with(Default::default);
    if search.reasoning_effort.is_none()     { search.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string()); }
    if search.max_concurrent.is_none()       { search.max_concurrent = Some(DEFAULT_SEARCH_MAX_CONCURRENT); }
    if search.max_results.is_none()          { search.max_results = Some(DEFAULT_SEARCH_MAX_RESULTS); }
    if search.max_news.is_none()             { search.max_news = Some(DEFAULT_SEARCH_MAX_NEWS); }
    if search.max_people_also_ask.is_none()  { search.max_people_also_ask = Some(DEFAULT_SEARCH_MAX_PEOPLE_ALSO_ASK); }
    if search.max_total_tokens.is_none()     { search.max_total_tokens = Some(DEFAULT_SEARCH_MAX_TOTAL_TOKENS); }
    if search.max_total_tokens_thorough.is_none() { search.max_total_tokens_thorough = Some(DEFAULT_SEARCH_MAX_TOTAL_TOKENS_THOROUGH); }
    if search.max_page_tokens.is_none()      { search.max_page_tokens = Some(DEFAULT_SEARCH_MAX_PAGE_TOKENS); }
    if search.fetch_timeout_secs.is_none()   { search.fetch_timeout_secs = Some(DEFAULT_SEARCH_FETCH_TIMEOUT_SECS); }

    let advanced = agents.advanced.get_or_insert_with(Default::default);
    if advanced.reasoning_effort.is_none()   { advanced.reasoning_effort = Some(DEFAULT_REASONING_EFFORT_OPENAI.to_string()); }
    if advanced.max_concurrent.is_none()     { advanced.max_concurrent = Some(DEFAULT_ADVANCED_MAX_CONCURRENT); }
    if advanced.max_iterations.is_none()     { advanced.max_iterations = Some(DEFAULT_ADVANCED_MAX_STEPS); }
    if advanced.exec_timeout_secs.is_none()  { advanced.exec_timeout_secs = Some(DEFAULT_ADVANCED_TOTAL_TIMEOUT_SECS); }
    if advanced.max_output_tokens.is_none()  { advanced.max_output_tokens = Some(DEFAULT_ADVANCED_CODE_MAX_OUTPUT_TOKENS); }
}
