//! `rabb1tclaw init` — interactive first-time setup.

use anyhow::Result;
use std::io::{self, Write};

use crate::config::{
    config_exists, config_path, get_lan_ips, load_config, save_config,
    GatewayConfig, ProviderConfig,
};
use super::{discover_api_keys, mask_key, pick_model, print_quick_reference, prompt_line};
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
    print!("\nChoice [0]: ");
    io::stdout().flush()?;
    let bind_ip = {
        let choice = prompt_line()?;
        match choice.trim() {
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

    // For each found key, list models and let user pick
    let mut first_provider = true;
    for (kp, api_key) in &found_keys {
        println!("\n--- {} ---\n", kp.display_name);

        if let Some(model_id) = pick_model(kp.api_type, kp.base_url, api_key).await {
            config.providers.insert(
                kp.key.to_string(),
                ProviderConfig {
                    api: kp.api_type.to_string(),
                    base_url: kp.base_url.to_string(),
                    api_key: api_key.clone(),
                    model: model_id.clone(),
                    name: Some(kp.display_name.to_string()),
                },
            );
            if first_provider {
                config.active_provider = Some(kp.key.to_string());
                first_provider = false;
            }
            println!("Added {} with model {}", kp.display_name, model_id);
        } else {
            println!("Skipping {}.", kp.display_name);
        }
    }

    // Choose default provider if multiple were added
    let provider_keys: Vec<String> = config.providers.keys().cloned().collect();
    if provider_keys.len() > 1 {
        println!("\n--- Default Provider ---\n");
        println!("Choose the default (active) provider:");
        for (i, key) in provider_keys.iter().enumerate() {
            let p = &config.providers[key];
            let current = if config.active_provider.as_deref() == Some(key.as_str()) {
                " (current)"
            } else {
                ""
            };
            println!("  {}) {} - {}{}", i + 1, key, p.model, current);
        }
        print!("\nChoice [1]: ");
        io::stdout().flush()?;
        let choice = prompt_line()?;
        let idx: usize = choice.trim().parse().unwrap_or(1);
        if idx >= 1 && idx <= provider_keys.len() {
            config.active_provider = Some(provider_keys[idx - 1].clone());
        }
        println!("Active provider: {}", config.active_provider.as_deref().unwrap_or("none"));
    }

    save_config(&config)?;

    // Offer device onboarding
    println!("\n--- Device Onboarding ---\n");
    print!("Add a device now? [Y/n]: ");
    io::stdout().flush()?;
    let answer = prompt_line()?;
    if !answer.trim().to_lowercase().starts_with('n') {
        let mut store = crate::config::load_devices()?;
        cmd_onboard(&mut config, &mut store)?;
    }

    // Summary
    println!("\n=== Setup Complete ===\n");
    println!("Config: {}", config_path().display());
    print_quick_reference();

    Ok(())
}
