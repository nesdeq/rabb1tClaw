//! Configuration modules.

pub mod devices;
pub mod native;
pub mod reload;

// Re-export commonly used types
pub use devices::{
    add_device, create_device, generate_token, load_devices,
    save_devices, revoke_device, generate_connection_json, get_lan_ips, print_qr_code,
    Device, DeviceStore,
};
pub use native::{
    config_dir, config_exists, config_path, devices_path, load_config, save_config,
    read_pid_file, PidGuard,
    GatewayConfig, ProviderConfig,
};
pub use reload::{setup_sighup_handler, ReloadCoordinator};
