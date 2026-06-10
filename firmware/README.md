# Wet Court firmware

Rust firmware for the M5Stack NanoC6 (ESP32-C6). Connects to the orchestrator
over WiFi via plain TCP and speaks the line protocol from §5.2 of
`../docs/architecture.md`.

First-cut hardware mapping:
- **Built-in BOOT button (GPIO9)** is the lectern trial-start button and is
  fully functional. On press (50 ms debounce) the firmware sends `BUTTON\n`,
  which the orchestrator routes to `Event::OperatorStart`.
- **Actuation commands are log-only stubs.** `FIRE <ms>` tracks the fire
  window with a timer and `GAVEL` / `LIGHTS <state>` / `PANEL <pattern>` are
  acknowledged with `OK <cmd>`, but nothing is driven yet — the built-in
  SK6812 RGB LED needs an RMT driver whose ecosystem versioning is currently
  a mess (the ws2812 crate was dropped over a `links =` conflict). Wiring
  real GPIO actuation is a follow-up.
- `PING` is answered with `PONG`. On disconnect the firmware retries the
  orchestrator every 2 s indefinitely.

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

## Known issue (2026-05-15, still unresolved)

The firmware builds and flashes cleanly, but boots into a panic loop:

```
assert failed: mmu_hal_map_region /IDF/components/hal/mmu_hal.c:84 (paddr % page_size_in_bytes == 0)
```

The chip *did* briefly connect to the orchestrator once after a full
erase + bootloader + partition table + app flash, before crashing — so
WiFi + TCP + the protocol all work in principle. The panic is in the
ESP-IDF early-boot path, not our `main` body.

Most likely cause: bootloader / partition-table / app version mismatch
from using plain `espflash flash` (writes app only, leaves stale
bootloader). Next session should:

1. Install `cargo-espflash` (`cargo install cargo-espflash`), which
   bundles the bootloader + partition table + app from the same
   ESP-IDF build in one image, and use `cargo espflash flash --monitor`
   instead of `cargo run`.
2. If that still panics, suspect ESP_IDF_VERSION pinning: the runtime
   boot log reported `v5.5.1` even though `.cargo/config.toml` requests
   `v5.2.2`. Embuild may be ignoring the pin.

The toolchain wrestling to get here is captured in commit history
(rust-toolchain.toml pinned to `nightly-2025-09-15`, esp-idf-svc bumped
to 0.52, ws2812 driver dropped due to `links =` conflict, USB-Serial-JTAG
console enabled via `sdkconfig.defaults`).
