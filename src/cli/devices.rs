//! `rabb1tclaw devices` — list, onboard, revoke.

use anyhow::Result;

use crate::config::{
    add_device, create_device, devices_path, generate_connection_json, generate_token,
    get_lan_ips, load_config, load_devices, print_qr_code, revoke_device, save_devices,
    Device, DeviceStore, GatewayConfig,
};

#[derive(clap::Args)]
pub(crate) struct DevicesArgs {
    /// List all paired devices
    #[arg(long)]
    list: bool,

    /// Add a new device (generates token + QR code)
    #[arg(long)]
    onboard: bool,

    /// Revoke a device by ID or token
    #[arg(long)]
    revoke: Option<String>,

    /// Revoke ALL devices
    #[arg(long)]
    revoke_all: bool,
}

pub(crate) fn dispatch(args: &DevicesArgs) -> Result<()> {
    if args.onboard {
        let config = load_config()?;
        let mut store = load_devices()?;
        cmd_onboard(&config, &mut store)?;
        return Ok(());
    }
    if let Some(ref id_or_token) = args.revoke {
        return cmd_revoke_device(id_or_token);
    }
    if args.revoke_all {
        return cmd_revoke_all();
    }
    if args.list {
        return cmd_list_devices();
    }
    println!("No action specified. Use --list, --onboard, --revoke, or --revoke-all.");
    println!("Run 'rabb1tclaw devices --help' for details.");
    Ok(())
}

fn cmd_list_devices() -> Result<()> {
    let store = load_devices()?;

    if store.devices.is_empty() {
        println!("No devices configured.");
        println!("Run 'rabb1tclaw devices --onboard' to add a device.");
        return Ok(());
    }

    println!(
        "\n{:<16} {:<32} STATUS",
        "NAME", "TOKEN"
    );
    println!("{}", "-".repeat(62));

    for device in store.devices.values() {
        let status = if device.revoked { "REVOKED" } else { "active" };
        println!(
            "{:<16} {:<32} {}",
            &device.display_name[..device.display_name.len().min(15)],
            device.token,
            status
        );
    }

    println!("\nDevices file: {}", devices_path().display());
    Ok(())
}

fn cmd_revoke_device(id_or_token: &str) -> Result<()> {
    let mut store = load_devices()?;

    if let Some(name) = revoke_device(&mut store, id_or_token) {
        save_devices(&store)?;
        println!("Revoked device: {name}");
    } else {
        println!("Device not found: {id_or_token}");
        println!("Use 'rabb1tclaw devices --list' to see available devices.");
    }

    Ok(())
}

fn cmd_revoke_all() -> Result<()> {
    let mut store = load_devices()?;

    if store.devices.is_empty() {
        println!("No devices to revoke.");
        return Ok(());
    }

    let mut revoked_count = 0;
    let device_ids: Vec<String> = store.devices.keys().cloned().collect();

    for device_id in device_ids {
        if let Some(name) = revoke_device(&mut store, &device_id) {
            println!("Revoked: {name}");
            revoked_count += 1;
        }
    }

    save_devices(&store)?;
    println!("\nRevoked {revoked_count} device(s).");

    Ok(())
}

pub(super) fn cmd_onboard(config: &GatewayConfig, store: &mut DeviceStore) -> Result<Device> {
    println!("\n=== Device Onboarding ===\n");

    let name = super::ask("Device name (e.g., 'Rabbit R1', 'iPhone'): ")?;
    let name = name.trim();

    let name = if name.is_empty() {
        format!("Device-{}", &generate_token()[..6])
    } else {
        name.to_string()
    };

    let device = create_device(&name);
    add_device(store, device.clone());
    save_devices(store)?;

    println!("\nDevice created!");
    println!("  Name:  {}", device.display_name);
    println!("  Token: {}", device.token);

    println!("\n=== Connection Info ===\n");
    let ips = get_lan_ips();
    println!("LAN IP addresses:");
    for ip in &ips {
        println!("  ws://{}:{}", ip, config.gateway.port);
    }
    println!("\nToken: {}", device.token);

    println!("\n=== QR Code ===\n");
    let qr_data = generate_connection_json(config, &device.token);
    print_qr_code(&qr_data);

    println!();
    println!("Scan QR with your device, or enter the");
    println!("connection URL and token manually.\n");

    Ok(device)
}
