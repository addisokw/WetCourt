// Copy this file to `wifi_config.rs` and fill in your network + host.
// `wifi_config.rs` is gitignored.

pub const WIFI_SSID: &str = "your-ssid";
pub const WIFI_PASS: &str = "your-password";

// Orchestrator's TCP hardware bind address (matches `hardware.bind_addr` in
// orchestrator/config.toml). Use the host's LAN IP — mDNS is unreliable on
// this network per project notes.
pub const ORCH_HOST: &str = "192.168.1.100";
pub const ORCH_PORT: u16 = 8090;
