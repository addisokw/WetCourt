# Wet Court firmware

Rust firmware for the M5Stack NanoC6 (ESP32-C6). Connects to the orchestrator
over WiFi via plain TCP and speaks the line protocol from §5.2 of
`../wet-court-architecture.md`.

First-cut hardware mapping:
- **Built-in SK6812 RGB LED** stands in for the squirt valve. `FIRE <ms>`
  paints it red for the requested duration. `LIGHTS <state>` colors it dim
  when no FIRE is in flight.
- **Built-in BOOT button (GPIO9)** is the lectern trial-start button. On
  press the firmware sends `BUTTON\n`, which the orchestrator routes to
  `Event::OperatorStart`.

## One-time toolchain setup

```powershell
# Rust ESP toolchain (RISC-V variant is upstream nightly; espup grabs the
# matching ESP-IDF + LLVM clang for the build):
cargo install espup
espup install
# Add the espup env-vars to your shell (or use the powershell export file
# espup printed). On Windows, espup writes ~\export-esp.ps1.
. $HOME\export-esp.ps1

cargo install ldproxy espflash
```

## Configure the network and host

```powershell
Copy-Item src\wifi_config.example.rs src\wifi_config.rs
```

Edit `src/wifi_config.rs` and fill in `WIFI_SSID`, `WIFI_PASS`, and the
orchestrator's LAN IP (the host running `cargo run -- --config config.toml`
with `hardware.driver = "tcp"`). `wifi_config.rs` is gitignored.

## Build and flash

Plug the NanoC6 into USB-C. From `firmware/`:

```powershell
cargo run --release
```

`cargo run` invokes `espflash flash --monitor` per `.cargo/config.toml`, so
this both flashes and opens the serial monitor. Expected boot log:

```
I (xxx) wet_court_firmware: connecting to wifi: <your-ssid>
I (xxx) wet_court_firmware: wifi up: 192.168.x.y
I (xxx) wet_court_firmware: connecting to orchestrator: 192.168.x.z:8090
I (xxx) wet_court_firmware: connected
```

The LED blinks green when the TCP session opens.

## Pairing with the orchestrator

In `orchestrator/config.toml` (or `config.dev.toml` for laptop dev), set:

```toml
[hardware]
driver = "tcp"
bind_addr = "0.0.0.0:8090"
ack_timeout_ms = 3000
```

Start the orchestrator, then power up the NanoC6 — the MCU will dial in and
the orchestrator logs `tcp_hw: MCU connected from <ip>`.
