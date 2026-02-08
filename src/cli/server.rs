//! `rabb1tclaw server` — start, stop, restart, IP management.

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::config::{
    config_exists, config_path, devices_path, load_config, load_devices,
    save_config, read_pid_file, PidGuard, ReloadCoordinator, setup_sighup_handler,
};
use crate::connection::{create_router, ServerState};
use crate::state::GatewayState;
use super::init_logging;

#[derive(clap::Args)]
pub(crate) struct ServerArgs {
    /// Update bind IP address and SIGHUP if running
    #[arg(long)]
    set_ip: Option<String>,

    /// Print current bind IP address
    #[arg(long)]
    get_ip: bool,

    /// Stop a running server (via PID file)
    #[arg(long)]
    stop: bool,

    /// Restart a running server (SIGHUP via PID file)
    #[arg(long)]
    restart: bool,
}

pub(crate) async fn dispatch(args: ServerArgs) -> Result<()> {
    if args.stop {
        return super::send_signal_to_server("", "SIGTERM");
    }
    if args.restart {
        return super::send_signal_to_server("-HUP", "SIGHUP");
    }
    if args.get_ip {
        let config = load_config()?;
        println!("{}", config.gateway.bind);
        return Ok(());
    }
    if let Some(ip) = args.set_ip {
        return cmd_server_set_ip(&ip);
    }
    init_logging();
    cmd_server_start().await
}

pub(crate) async fn cmd_server_start() -> Result<()> {
    if !config_exists() {
        println!("\n=== Welcome to rabb1tClaw ===\n");
        println!("No configuration found. Running initial setup...\n");
        super::init::cmd_init().await?;
        if !config_exists() {
            println!("Setup incomplete. Run 'rabb1tclaw init' to try again.");
            return Ok(());
        }
    }

    // Load .env from binary directory so SERP_API_KEY (etc.) are available
    let _ = dotenvy::from_path(super::binary_env_path());

    let config = load_config()?;
    let device_store = load_devices()?;

    let _pid_guard = PidGuard::new().context("Failed to write PID file")?;

    println!(r#"
          _    _    _ _    ___ _
 _ _ __ _| |__| |__/ | |_ / __| |__ ___ __ __
| '_/ _` | '_ \ '_ \ |  _| (__| / _` \ V  V /
|_| \__,_|_.__/_.__/_|\__|\___|_\__,_|\_/\_/
  v{}
"#,
        env!("CARGO_PKG_VERSION"),
    );

    let bind_ip: std::net::IpAddr = config
        .gateway
        .bind
        .parse()
        .unwrap_or_else(|_| std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let port = config.gateway.port;

    let model_info = config.active_model.as_ref()
        .and_then(|key| config.models.get(key).map(|m| {
            format!("{} ({}@{})", key, m.model_id, m.provider)
        }))
        .unwrap_or_else(|| "none".to_string());

    info!("config  {}", config_path().display());
    info!("devices {}", devices_path().display());
    info!("bind    {}", bind_ip);
    info!("model   {}", model_info);

    let gateway_state = Arc::new(
        GatewayState::new(config, device_store)
            .context("Failed to initialize gateway state")?,
    );

    {
        let ds = gateway_state.device_store.read().await;
        gateway_state.session_manager.load_from_disk(&ds).await;
    }

    let reload_coordinator = ReloadCoordinator::new(gateway_state.clone());
    let sighup_notify = reload_coordinator.sighup_notifier();
    setup_sighup_handler(sighup_notify);
    tokio::spawn(reload_coordinator.run());
    info!("hot-reload enabled (2s poll + SIGHUP)");

    let server_state = Arc::new(ServerState { gateway: gateway_state });
    let app = create_router(server_state);

    let addr = SocketAddr::new(bind_ip, port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("Failed to bind to address")?;

    info!("listening ws://{}", addr);
    info!("health   http://{}/health", addr);

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        info!("Shutting down");
    });

    server.await.context("Server error")?;

    // _pid_guard drops here, removing the PID file
    Ok(())
}

fn cmd_server_set_ip(ip: &str) -> Result<()> {
    ip.parse::<std::net::IpAddr>()
        .with_context(|| format!("Invalid IP address: {}", ip))?;

    let mut config = load_config()?;
    config.gateway.bind = ip.to_string();
    save_config(&config)?;
    println!("Bind IP updated to {}", ip);

    // SIGHUP if server is running
    if read_pid_file().is_some() {
        let _ = super::send_signal_to_server("-HUP", "SIGHUP");
    }

    Ok(())
}
