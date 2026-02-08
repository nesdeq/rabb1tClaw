//! CLI subcommands, clap structs, and shared helpers.

pub(crate) mod defaults;
mod devices;
mod init;
mod models;
mod providers;
mod server;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{self, Write};

use crate::provider::ModelInfo;

// ============================================================================
// Clap Structs
// ============================================================================

#[derive(Parser)]
#[command(name = "rabb1tclaw", version, about = "Minimal Rust LLM gateway for Rabbit R1 and other devices")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive first-time setup (reads .env from CWD)
    Init,
    /// Start or manage the gateway server
    Server(server::ServerArgs),
    /// Manage paired devices
    Devices(devices::DevicesArgs),
    /// Manage LLM providers (API connections)
    Providers(providers::ProvidersArgs),
    /// Manage model configurations
    Models(models::ModelsArgs),
}

// ============================================================================
// Known Providers
// ============================================================================

pub(crate) struct KnownProvider {
    pub env_var: &'static str,
    pub key: &'static str,
    pub api_type: &'static str,
    pub base_url: &'static str,
    pub display_name: &'static str,
}

pub(crate) const KNOWN_PROVIDERS: &[KnownProvider] = &[
    KnownProvider {
        env_var: "OPENAI_API_KEY",
        key: "openai",
        api_type: "openai",
        base_url: "https://api.openai.com/v1",
        display_name: "OpenAI",
    },
    KnownProvider {
        env_var: "ANTHROPIC_API_KEY",
        key: "anthropic",
        api_type: "anthropic",
        base_url: "https://api.anthropic.com/v1",
        display_name: "Anthropic",
    },
    KnownProvider {
        env_var: "DEEPINFRA_API_KEY",
        key: "deepinfra",
        api_type: "openai",
        base_url: "https://api.deepinfra.com/v1/openai",
        display_name: "DeepInfra",
    },
];

// ============================================================================
// Entry Point
// ============================================================================

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            init_logging();
            server::cmd_server_start().await
        }
        Some(Command::Init) => init::cmd_init().await,
        Some(Command::Server(args)) => server::dispatch(args).await,
        Some(Command::Devices(args)) => devices::dispatch(args),
        Some(Command::Providers(args)) => providers::dispatch(args).await,
        Some(Command::Models(args)) => models::dispatch(args).await,
    }
}

pub(crate) fn init_logging() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rabb1tclaw=info".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false),
        )
        .init();
}

// ============================================================================
// Shared Helpers
// ============================================================================

/// Path to .env next to the binary
pub(crate) fn binary_env_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(".env")))
        .unwrap_or_else(|| std::path::PathBuf::from(".env"))
}

/// Mask an API key for display: show first 7 + last 4 chars
pub(crate) fn mask_key(key: &str) -> String {
    if key.len() <= 11 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..7], &key[key.len() - 4..])
}

/// Print a prompt, flush stdout, and read a line from stdin (trimmed)
pub(crate) fn ask(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Filter models to only include chat-relevant ones
pub(crate) fn filter_relevant_models(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    models
        .into_iter()
        .filter(|m| {
            let id = m.id.to_lowercase();
            !id.contains("embed")
                && !id.contains("whisper")
                && !id.contains("tts")
                && !id.contains("dall-e")
                && !id.contains("moderation")
                && !id.contains("davinci")
                && !id.contains("babbage")
                && !id.contains("curie")
                && !id.contains("ada")
        })
        .collect()
}

/// Discover API keys from .env (binary dir) and environment variables.
pub(crate) fn discover_api_keys() -> Vec<(&'static KnownProvider, String)> {
    let mut found: Vec<(&KnownProvider, String)> = Vec::new();
    let env_path = binary_env_path();

    if let Ok(iter) = dotenvy::from_path_iter(&env_path) {
        for item in iter {
            if let Ok((k, v)) = item {
                for kp in KNOWN_PROVIDERS {
                    if k == kp.env_var && !v.is_empty() {
                        found.push((kp, v.clone()));
                    }
                }
            }
        }
    }

    // Also check environment variables directly
    for kp in KNOWN_PROVIDERS {
        if found.iter().any(|(p, _)| p.key == kp.key) {
            continue;
        }
        if let Ok(v) = std::env::var(kp.env_var) {
            if !v.is_empty() {
                found.push((kp, v));
            }
        }
    }

    found
}

/// Fetch models from a provider, filter, sort, and let user pick one.
/// `skip_label` is shown as the "0)" option (e.g. "Skip this provider" or "Enter manually").
/// Returns None if user picks 0, no models found, or fetch fails.
pub(crate) async fn pick_model(api_type: &str, base_url: &str, api_key: &str, skip_label: &str) -> Option<String> {
    println!("Fetching models...");
    let models = crate::provider::list_models(api_type, base_url, api_key).await;

    match models {
        Ok(mut models) => {
            models = filter_relevant_models(models);
            models.sort_by(|a, b| a.id.cmp(&b.id));

            if models.is_empty() {
                println!("No chat models found.");
                return None;
            }

            println!("Models:");
            for (i, m) in models.iter().enumerate() {
                println!("  {:>3}) {}", i + 1, m.id);
            }
            println!("    0) {}", skip_label);

            let choice = ask("\nPick a model [1]: ").ok()?;
            let idx: usize = choice.parse().unwrap_or(1);

            if idx == 0 {
                return None;
            }

            let model_id = if idx >= 1 && idx <= models.len() {
                models[idx - 1].id.clone()
            } else {
                models[0].id.clone()
            };
            Some(model_id)
        }
        Err(e) => {
            println!("Failed to list models: {}", e);
            None
        }
    }
}

// Re-export from defaults module
pub(crate) use defaults::{apply_smart_defaults, populate_default_agents};

/// Sanitize a model ID into a config-safe key (lowercase alphanumeric + hyphens).
pub(crate) fn sanitize_model_key(model_id: &str) -> String {
    let id = model_id
        .rsplit('/')
        .next()
        .unwrap_or(model_id)
        .to_lowercase();
    id.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Print quick reference command table (fits 80 cols)
pub(crate) fn print_quick_reference() {
    println!("\nQuick reference:");
    println!("{}", "-".repeat(56));
    println!("  rabb1tclaw                   Start the server");
    println!("  rabb1tclaw server --stop     Stop running server");
    println!("  rabb1tclaw server --restart  Reload config (SIGHUP)");
    println!("  rabb1tclaw server --get-ip   Show bind IP");
    println!("  rabb1tclaw server --set-ip X Change bind IP");
    println!("{}", "-".repeat(56));
    println!("  rabb1tclaw devices --list    List paired devices");
    println!("  rabb1tclaw devices --onboard Add device + QR code");
    println!("  rabb1tclaw devices --revoke  Revoke a device");
    println!("{}", "-".repeat(56));
    println!("  rabb1tclaw providers --list  List providers");
    println!("  rabb1tclaw providers --add   Add a provider");
    println!("  rabb1tclaw providers --remove NAME");
    println!("{}", "-".repeat(56));
    println!("  rabb1tclaw models --list     List models");
    println!("  rabb1tclaw models --add      Add a model");
    println!("  rabb1tclaw models --remove KEY");
    println!("  rabb1tclaw models --set-active KEY");
    println!("  rabb1tclaw models --edit KEY Edit model params");
    println!("{}", "-".repeat(56));
    println!("  rabb1tclaw init              Re-run setup");
    println!("  rabb1tclaw --help            Full help");
}

/// Send a signal to the server process identified by PID file.
/// Returns Ok if PID file found, prints appropriate message.
#[cfg(unix)]
pub(crate) fn send_signal_to_server(signal: &str, signal_name: &str) -> Result<()> {
    match crate::config::read_pid_file() {
        Some(pid) => {
            let mut args = vec![pid.to_string()];
            if !signal.is_empty() {
                args.insert(0, signal.to_string());
            }
            let status = std::process::Command::new("kill")
                .args(&args)
                .status()
                .with_context(|| format!("Failed to send {}", signal_name))?;
            if status.success() {
                println!("Sent {} to server (PID {})", signal_name, pid);
            } else {
                eprintln!("Failed to signal PID {} (process may have exited)", pid);
            }
            Ok(())
        }
        None => {
            println!("No running server found (no PID file).");
            Ok(())
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn send_signal_to_server(_signal: &str, signal_name: &str) -> Result<()> {
    eprintln!("Server {} is only supported on Unix", signal_name);
    Ok(())
}
