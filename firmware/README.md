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
cargo espflash flash --release --port COM4 --monitor
```

Use `cargo espflash` (not `cargo run` / plain `espflash flash`) so the
bootloader, partition table, and app are written together from the same
ESP-IDF build. Flashing the app alone over a stale bootloader produces the
`mmu_hal_map_region` panic noted below. Install once with
`cargo install cargo-espflash`. Expected boot log:

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

## Resolved: MMU boot panic (fixed 2026-05-16)

The earlier panic loop on boot —

```
assert failed: mmu_hal_map_region /IDF/components/hal/mmu_hal.c:84 (paddr % page_size_in_bytes == 0)
```

— was caused by `espflash flash` (and `cargo run` via `.cargo/config.toml`)
writing only the app over a stale bootloader. Flashing with
`cargo espflash flash --release --port COM4` writes the bootloader,
partition table, and app from the same ESP-IDF v5.2.2 build, and the chip
now boots cleanly, joins WiFi, and dials the orchestrator.

The toolchain wrestling to get here is captured in commit history
(rust-toolchain.toml pinned to `nightly-2025-09-15`, esp-idf-svc bumped
to 0.52, ws2812 driver dropped due to `links =` conflict, USB-Serial-JTAG
console enabled via `sdkconfig.defaults`).

App partition headroom is tight (1,047,088 / 1,048,576 bytes — 99.86%
used at `--release`). New code or larger ESP-IDF features may require
growing the `factory` partition in `sdkconfig.defaults` /
`partitions.csv`.

## Resolved: WiFi auth fail on WPA2/WPA3 mixed APs (fixed 2026-05-17)

After the MMU fix the chip booted but failed association the moment the
test AP was reconfigured to WPA2/WPA3-PSK mixed mode (`dot11_authmode:0x6`):

```
wifi:state: init -> auth (b0)
wifi:state: auth -> init (600)
... wifi associate failed: ESP_ERR_TIMEOUT
```

Two changes in `src/main.rs` fixed it:

1. **`AuthMethod::None`** for the `ClientConfiguration` — this sets the
   ESP-IDF scan auth-*threshold* to `WIFI_AUTH_OPEN`, letting the driver
   pick whichever auth the AP actually advertises (WPA2-PSK *or* WPA3-SAE
   on mixed APs). Hardcoding `WPA2Personal` made the threshold reject
   the AP's announced WPA2/WPA3 mode; hardcoding `WPA2WPA3Personal` then
   broke fallback to pure WPA2 APs. `None` works for both.
2. **`PmfConfiguration::Capable { required: false }`** — WPA3 SAE
   requires PMF (Protected Management Frames). `Capable, not required`
   lets WPA3 complete on a mixed AP and still permits non-PMF WPA2.

Also wrapped `wifi.connect()` + `wait_netif_up()` in a retry loop so a
transient associate failure no longer kicks the firmware out of
`app_main` with `ESP_ERR_TIMEOUT`. Previously the `?` on those calls
propagated to `main`, which returned and ended the program.

Sanity-check log after the fix:

```
wet_court_firmware: connecting to wifi: test_lan
wifi:(connect)dot11_authmode:0x6, pairwise_cipher:0x3, group_cipher:0x3
wifi:state: init -> auth -> assoc -> run
wifi:connected with test_lan, aid = 7, channel 1
wet_court_firmware: wifi up: 192.168.50.182
wet_court_firmware: connecting to orchestrator: 192.168.50.179:8090
```

## Operator gotcha: orchestrator must run with `driver = "tcp"`

If the chip joins WiFi but logs `session error: connection timed out`
in a loop, the orchestrator is probably running in mock mode. `booth`'s
TCP hardware listener only binds when `[hardware] driver = "tcp"`; with
`driver = "mock"` (the default in `config.dev.toml`) port 8090 is never
opened and the only listener is the frontend on 8080. Start `booth` with
`cargo run -- --config config.toml`, or flip `driver` to `"tcp"` in the
dev config. Successful pair-up logs `tcp_hw: listening on 0.0.0.0:8090`
on the host and `connected` on the MCU. On Windows, allow the listener
on the **Private** profile when the firewall prompts.
