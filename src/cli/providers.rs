//! `rabb1tclaw providers` — list, add, remove.
//!
//! Providers are API connections only (key + URL + api type).

use anyhow::Result;

use crate::config::{load_config, save_config, ProviderConfig};
use super::{ask, discover_api_keys, mask_key, KNOWN_PROVIDERS};

#[derive(clap::Args)]
pub(crate) struct ProvidersArgs {
    /// List configured providers
    #[arg(long)]
    list: bool,

    /// Interactively add a new provider
    #[arg(long)]
    add: bool,

    /// Remove a provider by name
    #[arg(long)]
    remove: Option<String>,
}

pub(crate) fn dispatch(args: &ProvidersArgs) -> Result<()> {
    if args.add {
        return cmd_providers_add();
    }
    if let Some(ref name) = args.remove {
        return cmd_providers_remove(name);
    }
    if args.list {
        return cmd_providers_list();
    }
    println!("No action specified. Use --list, --add, or --remove.");
    println!("Run 'rabb1tclaw providers --help' for details.");
    Ok(())
}

fn cmd_providers_list() -> Result<()> {
    let config = load_config()?;

    if config.providers.is_empty() {
        println!("No providers configured.");
        println!("Run 'rabb1tclaw providers --add' or 'rabb1tclaw init' to add one.");
        return Ok(());
    }

    println!(
        "\n{:<15} {:<12} BASE_URL",
        "NAME", "API"
    );
    println!("{}", "-".repeat(70));

    for (key, provider) in &config.providers {
        let display = provider.name.as_deref().unwrap_or(key);
        println!(
            "{:<15} {:<12} {}",
            display, provider.api, provider.base_url
        );
    }

    Ok(())
}

fn cmd_providers_add() -> Result<()> {
    println!("\n=== Add Provider ===\n");

    println!("Choose provider type:");
    for (i, kp) in KNOWN_PROVIDERS.iter().enumerate() {
        println!("  {}) {} ({})", i + 1, kp.display_name, kp.api_type);
    }
    println!("  {}) Custom (OpenAI-compatible URL)", KNOWN_PROVIDERS.len() + 1);

    let choice = ask("\nChoice: ")?;
    let choice: usize = choice.parse().unwrap_or(0);

    if choice == 0 || choice > KNOWN_PROVIDERS.len() + 1 {
        println!("Invalid choice.");
        return Ok(());
    }

    let (api_type, base_url, provider_key, display_name) = if choice <= KNOWN_PROVIDERS.len() {
        let kp = &KNOWN_PROVIDERS[choice - 1];
        (
            kp.api_type.to_string(),
            kp.base_url.to_string(),
            kp.key.to_string(),
            kp.display_name.to_string(),
        )
    } else {
        let name = ask("Provider name: ")?;
        let url = ask("Base URL: ")?;
        let api = ask("API type (openai/anthropic) [openai]: ")?;
        let api = if api.is_empty() { "openai".to_string() } else { api };
        let key = name.to_lowercase().replace(' ', "-");
        (api, url, key, name)
    };

    // Get API key — try discovered keys first, then prompt
    let api_key = resolve_api_key(&provider_key)?;

    if api_key.is_empty() {
        println!("No API key provided, aborting.");
        return Ok(());
    }

    let mut config = load_config()?;

    config.providers.insert(
        provider_key.clone(),
        ProviderConfig { api: api_type, base_url, api_key, name: Some(display_name) },
    );

    save_config(&config)?;
    println!(
        "\nAdded provider '{provider_key}'",
    );
    println!("Now add a model: rabb1tclaw models --add");

    Ok(())
}

fn cmd_providers_remove(name: &str) -> Result<()> {
    let mut config = load_config()?;

    if config.providers.remove(name).is_none() {
        println!("Provider '{name}' not found.");
        if !config.providers.is_empty() {
            println!("Available providers:");
            for key in config.providers.keys() {
                println!("  {key}");
            }
        }
        return Ok(());
    }

    // Remove any models that reference this provider
    let orphaned_models: Vec<String> = config.models.iter()
        .filter(|(_, m)| m.provider == name)
        .map(|(k, _)| k.clone())
        .collect();

    for key in &orphaned_models {
        config.models.remove(key);
        println!("Removed orphaned model '{key}'");
    }

    // Fix active model if it was removed
    if let Some(ref active) = config.active_model {
        if !config.models.contains_key(active) {
            config.active_model = config.models.keys().next().cloned();
            if let Some(ref new_active) = config.active_model {
                println!("Active model changed to '{new_active}'");
            }
        }
    }

    save_config(&config)?;
    println!("Removed provider '{name}'");

    Ok(())
}

/// Resolve an API key for a provider: check discovered keys first, then prompt.
fn resolve_api_key(provider_key: &str) -> Result<String> {
    let discovered = discover_api_keys();
    if let Some((kp, key)) = discovered.into_iter().find(|(kp, _)| kp.key == provider_key) {
        println!("Using {} = {}", kp.env_var, mask_key(&key));
        return Ok(key);
    }

    ask("API Key: ")
}
