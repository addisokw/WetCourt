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
| AI judge (face + gaze) | Adafruit Matrix Portal M4 (SAMD51) | `PANEL`, gaze `AIM` | `ai-judge/` | planned |
| Gavel | M5Stack NanoC6 (esp32c6) + 8-Servos | `GAVEL`, `GJOG`, `PING` | [`gavel/`](gavel/) | **in progress** |
| Swear-in object *(future)* | TBD micro | `BUTTON` (start trigger) | `swear-in/` | future |

The turret's **aim** and **fire** are two separate NanoC6 boards: the servo board
takes the NanoC6's only Grove I2C pins, leaving no GPIO for the relay, so the
relay gets its own board (role `squirt`).

Each device dials the orchestrator over TCP and identifies with `HELLO <role>`;
the host routes commands per role. `LIGHTS` is deferred (no owner); e-stop is the
operator panel + hardware power, not a device.

The turret's **vision** process (camera person-tracking + aim feed) is a non-MCU
host process and lives in `vision/`, not here — see
[`../docs/hardware-architecture.md`](../docs/hardware-architecture.md).
