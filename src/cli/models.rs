//! `rabb1tclaw models` — list, add, remove, set-active, edit.

use anyhow::Result;

use crate::config::{
    load_config, save_config,
    ModelConfig, ThinkingConfig,
};
use super::{apply_smart_defaults, ask, pick_model, sanitize_model_key};

#[derive(clap::Args)]
pub(crate) struct ModelsArgs {
    /// List configured models
    #[arg(long)]
    list: bool,

    /// Interactively add a new model
    #[arg(long)]
    add: bool,

    /// Remove a model by key
    #[arg(long)]
    remove: Option<String>,

    /// Set the active model by key
    #[arg(long)]
    set_active: Option<String>,

    /// Edit parameters of an existing model
    #[arg(long)]
    edit: Option<String>,

}

pub(crate) async fn dispatch(args: ModelsArgs) -> Result<()> {
    if args.add {
        return cmd_models_add().await;
    }
    if let Some(ref key) = args.remove {
        return cmd_models_remove(key);
    }
    if let Some(ref key) = args.set_active {
        return cmd_models_set_active(key);
    }
    if let Some(ref key) = args.edit {
        return cmd_models_edit(key);
    }
    if args.list {
        return cmd_models_list();
    }
    println!("No action specified. Use --list, --add, --remove, --set-active, or --edit.");
    println!("Run 'rabb1tclaw models --help' for details.");
    Ok(())
}

fn cmd_models_list() -> Result<()> {
    let config = load_config()?;

    if config.models.is_empty() {
        println!("No models configured.");
        println!("Run 'rabb1tclaw models --add' or 'rabb1tclaw init' to add one.");
        return Ok(());
    }

    println!(
        "\n{:<18} {:<30} {:<12} AGENTS",
        "KEY", "MODEL_ID", "PROVIDER"
    );
    println!("{}", "-".repeat(78));

    for (key, model) in &config.models {
        let is_active = config.active_model.as_deref() == Some(key.as_str());
        let agents = crate::config::native::model_agent_roles(&config, key);
        let agents_str = agents.join(", ");

        let active = if is_active { " <-- active" } else { "" };
        println!(
            "{:<18} {:<30} {:<12} {}{}",
            key, model.model_id, model.provider, agents_str, active
        );
    }

    Ok(())
}

async fn cmd_models_add() -> Result<()> {
    let mut config = load_config()?;

    if config.providers.is_empty() {
        println!("No providers configured. Add a provider first:");
        println!("  rabb1tclaw providers --add");
        return Ok(());
    }

    println!("\n=== Add Model ===\n");

    // 1. Pick provider
    let provider_keys: Vec<String> = config.providers.keys().cloned().collect();
    println!("Choose a provider:");
    for (i, key) in provider_keys.iter().enumerate() {
        let p = &config.providers[key];
        let name = p.name.as_deref().unwrap_or(key);
        println!("  {}) {} ({})", i + 1, name, p.api);
    }

    let choice = ask("\nChoice [1]: ")?;
    let idx: usize = choice.parse().unwrap_or(1);
    if idx < 1 || idx > provider_keys.len() {
        println!("Invalid choice.");
        return Ok(());
    }
    let provider_key = &provider_keys[idx - 1];
    let provider_config = &config.providers[provider_key];

    // 2. Fetch and pick model
    let model_id = match pick_model(
        &provider_config.api, &provider_config.base_url, &provider_config.api_key, "Enter manually"
    ).await {
        Some(id) => id,
        None => ask("Enter model ID manually: ")?,
    };

    if model_id.is_empty() {
        println!("No model selected, aborting.");
        return Ok(());
    }

    // 3. Choose key
    let default_key = sanitize_model_key(&model_id);
    let key_input = ask(&format!("Model key [{default_key}]: "))?;
    let model_key = if key_input.is_empty() {
        default_key
    } else {
        key_input
    };

    if config.models.contains_key(&model_key) {
        println!("A model with key '{model_key}' already exists.");
        return Ok(());
    }

    // 4. Smart defaults based on model capabilities
    let is_first = config.models.is_empty();

    let mut mc = ModelConfig {
        provider: provider_key.clone(),
        model_id: model_id.clone(),
        ..Default::default()
    };
    apply_smart_defaults(&mut mc, &provider_config.api);

    config.models.insert(model_key.clone(), mc);

    // If first model, set as active
    if is_first {
        config.active_model = Some(model_key.clone());
    }

    save_config(&config)?;
    println!(
        "\nAdded model '{}' ({} via {}){}",
        model_key,
        model_id,
        provider_key,
        if is_first { " (active)" } else { "" }
    );

    Ok(())
}

fn cmd_models_remove(key: &str) -> Result<()> {
    let mut config = load_config()?;

    if !require_model(&config, key) {
        return Ok(());
    }
    config.models.remove(key);

    if config.active_model.as_deref() == Some(key) {
        config.active_model = config.models.keys().next().cloned();
        if let Some(ref new_active) = config.active_model {
            println!("Active model changed to '{new_active}'");
        } else {
            println!("No models remaining.");
        }
    }

    save_config(&config)?;
    println!("Removed model '{key}'");

    Ok(())
}

fn cmd_models_set_active(key: &str) -> Result<()> {
    let mut config = load_config()?;

    if !require_model(&config, key) {
        return Ok(());
    }

    config.active_model = Some(key.to_string());
    save_config(&config)?;

    let m = &config.models[key];
    println!("Active model set to '{}' ({} via {})", key, m.model_id, m.provider);

    Ok(())
}

fn cmd_models_edit(key: &str) -> Result<()> {
    fn show<T: std::fmt::Display>(label: &str, val: Option<T>) {
        println!("  {:<18} {}", label, val.map_or_else(|| "(default)".to_string(), |v| v.to_string()));
    }

    let mut config = load_config()?;

    if !require_model(&config, key) {
        return Ok(());
    }
    let model = config.models.get_mut(key).unwrap();

    println!("\n=== Edit Model '{key}' ===\n");
    println!("Current settings:");
    println!("  {:<18} {}", "model_id:", model.model_id);
    println!("  {:<18} {}", "provider:", model.provider);
    show("max_tokens:", model.max_tokens);
    show("temperature:", model.temperature);
    show("top_p:", model.top_p);
    show("frequency_penalty:", model.frequency_penalty);
    show("presence_penalty:", model.presence_penalty);
    show("context_tokens:", model.context_tokens);
    println!("  {:<18} {}", "reasoning_effort:", model.reasoning_effort.as_deref().unwrap_or("(none)"));
    println!("  {:<18} {}", "thinking:", model.thinking.as_ref().map_or_else(
        || "(disabled)".to_string(),
        |t| format!("enabled={}, budget={}", t.enabled,
            t.budget_tokens.map_or_else(|| "default".to_string(), |v| v.to_string()))
    ));
    println!("\nEnter new values (empty to keep current, 'none' to clear):\n");

    if let Some(v) = prompt_edit("temperature", model.temperature.as_ref())? {
        model.temperature = v;
    }
    if let Some(v) = prompt_edit("max_tokens", model.max_tokens.as_ref())? {
        model.max_tokens = v;
    }
    if let Some(v) = prompt_edit("top_p", model.top_p.as_ref())? {
        model.top_p = v;
    }
    if let Some(v) = prompt_edit("frequency_penalty", model.frequency_penalty.as_ref())? {
        model.frequency_penalty = v;
    }
    if let Some(v) = prompt_edit("presence_penalty", model.presence_penalty.as_ref())? {
        model.presence_penalty = v;
    }
    if let Some(v) = prompt_edit("context_tokens", model.context_tokens.as_ref())? {
        model.context_tokens = v;
    }
    if let Some(v) = prompt_edit("reasoning_effort", model.reasoning_effort.as_ref())? {
        model.reasoning_effort = v;
    }

    // Thinking config
    let answer = ask("Enable thinking? (y/n, empty to keep): ")?;
    match answer.to_lowercase().as_str() {
        "y" | "yes" => {
            let budget: Option<u32> = prompt_optional("  Budget tokens (e.g. 10000, empty for default)")?;
            model.thinking = Some(ThinkingConfig {
                enabled: true,
                budget_tokens: budget,
            });
        }
        "n" | "no" => {
            model.thinking = None;
        }
        _ => {} // keep current
    }

    save_config(&config)?;
    println!("\nModel '{key}' updated.");

    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// Check if a model key exists in config; print error + available list if not.
fn require_model(config: &crate::config::GatewayConfig, key: &str) -> bool {
    if config.models.contains_key(key) {
        return true;
    }
    println!("Model '{key}' not found.");
    if !config.models.is_empty() {
        println!("Available models:");
        for k in config.models.keys() {
            println!("  {k}");
        }
    }
    false
}

/// Prompt for an optional value. Returns None if empty, or parses the input.
fn prompt_optional<T: std::str::FromStr>(label: &str) -> Result<Option<T>> {
    let input = ask(&format!("{label}: "))?;
    if input.is_empty() {
        Ok(None)
    } else {
        Ok(Some(input.parse().map_err(|_| anyhow::anyhow!("invalid value"))?))
    }
}

/// Prompt for editing a value. Returns `Some(new_value)` if changed, None if kept.
/// "none" clears the value, empty keeps current.
#[allow(clippy::option_option)]
fn prompt_edit<T: std::str::FromStr + std::fmt::Display>(
    name: &str,
    current: Option<&T>,
) -> Result<Option<Option<T>>> {
    let cur = current.map_or_else(|| "(default)".to_string(), ToString::to_string);
    let input = ask(&format!("{name} [{cur}]: "))?;
    if input.is_empty() {
        Ok(None) // keep current
    } else if input == "none" {
        Ok(Some(None)) // clear
    } else if let Ok(v) = input.parse::<T>() {
        Ok(Some(Some(v)))
    } else {
        println!("  Invalid value, keeping current.");
        Ok(None)
    }
}
