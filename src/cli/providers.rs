//! `rabb1tclaw providers` — list, add, remove, set-active.

use anyhow::Result;
use std::io::{self, Write};

use crate::config::{load_config, save_config, ProviderConfig};
use super::{
    discover_api_keys, mask_key, pick_model,
    print_provider_not_found, prompt_line, KNOWN_PROVIDERS,
};

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

    /// Set the active (default) provider by name
    #[arg(long)]
    set_active: Option<String>,
}

pub(crate) async fn dispatch(args: ProvidersArgs) -> Result<()> {
    if args.add {
        return cmd_providers_add().await;
    }
    if let Some(ref name) = args.remove {
        return cmd_providers_remove(name);
    }
    if let Some(ref name) = args.set_active {
        return cmd_providers_set_active(name);
    }
    if args.list {
        return cmd_providers_list();
    }
    println!("No action specified. Use --list, --add, --remove, or --set-active.");
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
        "\n{:<15} {:<12} {:<35}",
        "NAME", "API", "MODEL"
    );
    println!("{}", "-".repeat(70));

    for (key, provider) in &config.providers {
        let active = if config.active_provider.as_deref() == Some(key) {
            " <-- active"
        } else {
            ""
        };
        println!(
            "{:<15} {:<12} {:<35}{}",
            key, provider.api, provider.model, active
        );
    }

    Ok(())
}

async fn cmd_providers_add() -> Result<()> {
    println!("\n=== Add Provider ===\n");

    println!("Choose provider type:");
    for (i, kp) in KNOWN_PROVIDERS.iter().enumerate() {
        println!("  {}) {} ({})", i + 1, kp.display_name, kp.api_type);
    }
    println!("  {}) Custom (OpenAI-compatible URL)", KNOWN_PROVIDERS.len() + 1);

    print!("\nChoice: ");
    io::stdout().flush()?;
    let choice = prompt_line()?;
    let choice: usize = choice.trim().parse().unwrap_or(0);

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
        print!("Provider name: ");
        io::stdout().flush()?;
        let name = prompt_line()?.trim().to_string();
        print!("Base URL: ");
        io::stdout().flush()?;
        let url = prompt_line()?.trim().to_string();
        print!("API type (openai/anthropic) [openai]: ");
        io::stdout().flush()?;
        let api = prompt_line()?;
        let api = if api.trim().is_empty() { "openai".to_string() } else { api.trim().to_string() };
        let key = name.to_lowercase().replace(' ', "-");
        (api, url, key, name)
    };

    // Get API key — try discovered keys first, then prompt
    let api_key = resolve_api_key(&provider_key)?;

    if api_key.is_empty() {
        println!("No API key provided, aborting.");
        return Ok(());
    }

    // Pick model
    let model_id = match pick_model(&api_type, &base_url, &api_key).await {
        Some(id) => id,
        None => {
            print!("Enter model ID manually: ");
            io::stdout().flush()?;
            prompt_line()?.trim().to_string()
        }
    };

    if model_id.is_empty() {
        println!("No model selected, aborting.");
        return Ok(());
    }

    let mut config = load_config()?;
    let is_first = config.providers.is_empty();

    config.providers.insert(
        provider_key.clone(),
        ProviderConfig {
            api: api_type,
            base_url,
            api_key,
            model: model_id.clone(),
            name: Some(display_name.clone()),
        },
    );

    if is_first {
        config.active_provider = Some(provider_key.clone());
    }

    save_config(&config)?;
    println!(
        "\nAdded provider '{}' with model '{}'{}",
        provider_key,
        model_id,
        if is_first { " (active)" } else { "" }
    );

    Ok(())
}

fn cmd_providers_remove(name: &str) -> Result<()> {
    let mut config = load_config()?;

    if config.providers.remove(name).is_none() {
        print_provider_not_found(name, &config);
        return Ok(());
    }

    if config.active_provider.as_deref() == Some(name) {
        config.active_provider = config.providers.keys().next().cloned();
        if let Some(ref new_active) = config.active_provider {
            println!("Active provider changed to '{}'", new_active);
        } else {
            println!("No providers remaining.");
        }
    }

    save_config(&config)?;
    println!("Removed provider '{}'", name);

    Ok(())
}

fn cmd_providers_set_active(name: &str) -> Result<()> {
    let mut config = load_config()?;

    if !config.providers.contains_key(name) {
        print_provider_not_found(name, &config);
        return Ok(());
    }

    config.active_provider = Some(name.to_string());
    save_config(&config)?;

    let p = &config.providers[name];
    println!("Active provider set to '{}' ({})", name, p.model);

    Ok(())
}

/// Resolve an API key for a provider: check discovered keys first, then prompt.
fn resolve_api_key(provider_key: &str) -> Result<String> {
    // Reuse shared discovery logic (checks env vars + .env file)
    let discovered = discover_api_keys();
    if let Some((kp, key)) = discovered.into_iter().find(|(kp, _)| kp.key == provider_key) {
        println!("Using {} = {}", kp.env_var, mask_key(&key));
        return Ok(key);
    }

    print!("API Key: ");
    io::stdout().flush()?;
    Ok(prompt_line()?.trim().to_string())
}
