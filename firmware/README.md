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
| Turret aim | M5Stack NanoC6 (esp32c6) + 8-Servos | `AIM`, `PING` | [`turret/`](turret/) | **in progress** |
| Squirt fire | M5Stack NanoC6 (esp32c6) + relay | `FIRE`, `PING` | [`squirt/`](squirt/) | **in progress** |
| Judge face | Adafruit Matrix Portal M4 (SAMD51 + AirLift), CircuitPython | `FACE`, `AUDIO`, `PERSONA`, `AIM`, `PANEL` *(legacy)*, `PING` | [`judge-face/`](judge-face/) | **in progress** |
| Judge neck (gaze) | M5Stack NanoC6 (esp32c6) + 8-Servos | `AIM`, `PING` | [`judge-neck/`](judge-neck/) | scaffolded |
| Gavel | M5Stack NanoC6 (esp32c6) + 8-Servos | `GAVEL`, `GJOG`, `PING` | [`gavel/`](gavel/) | **in progress** |
| Swear-in object *(future)* | TBD micro | `BUTTON` (start trigger) | `swear-in/` | future |

Two subsystems are each split across two boards for a hardware reason. The
turret's **aim** and **fire**: the servo board takes the NanoC6's only Grove I2C
pins, leaving no GPIO for the relay, so the relay gets its own board (role
`squirt`). The judge head's **face** and **neck**: the HUB75 panel + its
refresh timing fully occupy the Matrix Portal M4, so the pan/tilt gaze reuses the
turret's NanoC6 + 8-servo recipe on its own board (role `judge-neck`) — that
firmware is the turret's, rescoped.

Each device dials the orchestrator over TCP and identifies with `HELLO <role>`;
the host routes commands per role. `LIGHTS` is deferred (no owner); e-stop is the
operator panel + hardware power, not a device.

The turret's **vision** process (camera person-tracking + aim feed) is a non-MCU
host process and lives in `vision/`, not here — see
[`../docs/hardware-architecture.md`](../docs/hardware-architecture.md).
