//! `rabb1tclaw init` — interactive first-time setup.

use anyhow::Result;

use crate::config::{
    config_exists, config_path, get_lan_ips, load_config, save_config,
    GatewayConfig, ProviderConfig, ModelConfig,
};
use super::{apply_smart_defaults, ask, discover_api_keys, mask_key, pick_model, print_quick_reference, sanitize_model_key};
use super::devices::cmd_onboard;

pub async fn cmd_init() -> Result<()> {
    println!("\n=== rabb1tClaw Init ===\n");

    // Read .env from binary directory
    let env_path = super::binary_env_path();
    println!("Reading {}", env_path.display());

    let found_keys = discover_api_keys();

    if found_keys.is_empty() {
        println!("No API keys found in .env or environment.");
        println!("\nCreate a .env file with one or more of:");
        println!("  OPENAI_API_KEY=sk-...");
        println!("  ANTHROPIC_API_KEY=sk-ant-...");
        println!("  DEEPINFRA_API_KEY=...");
        return Ok(());
    }

    println!("Found API keys:");
    for (kp, key) in &found_keys {
        println!("  {} = {}", kp.env_var, mask_key(key));
    }

    // Choose bind IP
    println!("\n--- Bind IP ---\n");
    let lan_ips = get_lan_ips();
    println!("Available IPs:");
    println!("  0) 127.0.0.1 (loopback only)");
    println!("  1) 0.0.0.0 (all interfaces)");
    for (i, ip) in lan_ips.iter().enumerate() {
        println!("  {}) {}", i + 2, ip);
    }
    let bind_ip = {
        let choice = ask("\nChoice [0]: ")?;
        match choice.as_str() {
            "1" => "0.0.0.0".to_string(),
            s if s.parse::<usize>().ok().filter(|&n| n >= 2 && n < lan_ips.len() + 2).is_some() => {
                lan_ips[s.parse::<usize>().unwrap() - 2].clone()
            }
            _ => "127.0.0.1".to_string(),
        }
    };
    println!("Bind: {}", bind_ip);

    let mut config = if config_exists() {
        load_config()?
    } else {
        GatewayConfig::default()
    };
    config.gateway.bind = bind_ip;

    // 1. Create provider entries (no model selection yet)
    for (kp, api_key) in &found_keys {
        config.providers.insert(
            kp.key.to_string(),
            ProviderConfig {
                api: kp.api_type.to_string(),
                base_url: kp.base_url.to_string(),
                api_key: api_key.clone(),
                name: Some(kp.display_name.to_string()),
            },
        );
        println!("Added provider: {}", kp.display_name);
    }

    // 2. For each provider, offer to add a model
    let mut first_model = true;
    for (kp, api_key) in &found_keys {
        println!("\n--- {} — Pick a model ---\n", kp.display_name);

        if let Some(model_id) = pick_model(kp.api_type, kp.base_url, api_key, "Skip this provider").await {
            let model_key = sanitize_model_key(&model_id);
            let mut mc = ModelConfig {
                provider: kp.key.to_string(),
                model_id: model_id.clone(),
                ..Default::default()
            };
            apply_smart_defaults(&mut mc, kp.api_type);
            config.models.insert(model_key.clone(), mc);
            if first_model {
                config.active_model = Some(model_key.clone());
                first_model = false;
            }
            println!("Added model '{}' ({})", model_key, model_id);
        } else {
            println!("Skipping model for {}.", kp.display_name);
        }
    }

    // 3. Choose active model if multiple were added
    let model_keys: Vec<String> = config.models.keys().cloned().collect();
    if model_keys.len() > 1 {
        println!("\n--- Active Model ---\n");
        println!("Choose the default (active) model:");
        for (i, key) in model_keys.iter().enumerate() {
            let m = &config.models[key];
            let current = if config.active_model.as_deref() == Some(key.as_str()) {
                " (current)"
            } else {
                ""
            };
            println!("  {}) {} — {} via {}{}", i + 1, key, m.model_id, m.provider, current);
        }
        let choice = ask("\nChoice [1]: ")?;
        let idx: usize = choice.parse().unwrap_or(1);
        if idx >= 1 && idx <= model_keys.len() {
            config.active_model = Some(model_keys[idx - 1].clone());
        }
        println!("Active model: {}", config.active_model.as_deref().unwrap_or("none"));
    }

    super::populate_default_agents(&mut config);
    save_config(&config)?;

    // Offer device onboarding
    println!("\n--- Device Onboarding ---\n");
    let answer = ask("Add a device now? [Y/n]: ")?;
    if !answer.to_lowercase().starts_with('n') {
        let mut store = crate::config::load_devices()?;
        cmd_onboard(&mut config, &mut store)?;
    }

    // Summary
    println!("\n=== Setup Complete ===\n");
    println!("Config: {}", config_path().display());
    print_quick_reference();

    Ok(())
}
