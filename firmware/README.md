# firmware

One self-contained project per independently-flashed board. The boards span
different MCUs, toolchains, and languages, so this is a **container of islands,
not a shared build** — the unifying artifacts are this board map and the
[device protocol spec](../protocol/README.md), not a common workspace. Adding
hardware = a new sibling dir + one orchestrator registry entry + a role in the
spec.

## Board map

| Subsystem | Board / MCU | Owns (verbs / events) | Dir | Status |
|---|---|---|---|---|
| Turret aim | M5Stack NanoC6 (esp32c6) + 8-Servos, MicroPython | `AIM`, `PING` | [`turret/`](turret/) | **in progress** |
| Squirt fire | M5Stack NanoC6 (esp32c6) + relay, MicroPython | `FIRE`, `PING` | [`squirt/`](squirt/) | **in progress** |
| Judge face | Adafruit Matrix Portal M4 (SAMD51 + AirLift), CircuitPython | `FACE`, `AUDIO`, `PERSONA`, `AIM`, `PANEL` *(legacy)*, `PING` | [`judge-face/`](judge-face/) | **in progress** |
| Judge neck (gaze) | M5Stack NanoC6 (esp32c6) + 8-Servos, MicroPython | `AIM`, `PING` | [`judge-neck/`](judge-neck/) | scaffolded |
| Gavel | M5Stack NanoC6 (esp32c6), servo direct on G2, MicroPython | `GAVEL`, `GJOG`, `PING` | [`gavel/`](gavel/) | **in progress** |
| Swear-in button | M5Stack NanoC6 (esp32c6), arcade button on G1 + its lamp on G2, MicroPython | `LED`, `PING` (emits `BUTTON`) | [`swear-in/`](swear-in/) | **in progress** |

Two subsystems are each split across two boards for a hardware reason. The
turret's **aim** and **fire**: the servo board takes the NanoC6's only Grove I2C
pins, leaving no GPIO for the relay, so the relay gets its own board (role
`squirt`). The judge head's **face** and **neck**: the HUB75 panel + its
refresh timing fully occupy the Matrix Portal M4, so the pan/tilt gaze reuses the
turret's NanoC6 + 8-servo recipe on its own board (role `judge-neck`).

All five NanoC6 boards run **MicroPython** (v1.28.0 `ESP32_GENERIC_C6`), each a
thin `main.py` of hardware glue over shared support code in
[`micropython/`](micropython/) — the single exception to the islands rule,
deployed per board by its `deploy.sh`: `wetline.py` (the protocol client;
also advertises the board over mDNS as `<role>.local`) and `ota.py`
(token-gated, staged, sha256-verified **WiFi updates** via `otapush.py
<role>.local` — no cable after first deploy). The judge face stays
CircuitPython for displayio; it carries its own `ota.py` port speaking the
same protocol (same `otapush.py` client, credentials from `settings.toml`,
push to the board's IP — no mDNS on the AirLift). Its `boot.py` hands the
CIRCUITPY drive to on-device code by default so OTA can write; **hold UP at
reset** for a host-writable drive (`deploy.sh` / recovery).

Each device dials the orchestrator over TCP and identifies with `HELLO <role>`;
the host routes commands per role. `LIGHTS` is deferred (no owner); e-stop is the
operator panel + hardware power, not a device.

The turret's **vision** process (camera person-tracking + aim feed) is a non-MCU
host process and lives in `vision/`, not here — see
[`../docs/hardware-architecture.md`](../docs/hardware-architecture.md).
