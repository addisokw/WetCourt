# firmware/turret

Squirt-gun turret firmware for the Wet Court fleet.

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Servos** | M5Stack 8-Servos board (I2C `0x25`) — **ch0 = pan, ch1 = tilt** |
| **Relay** | M5Stack single relay (fires the squirt gun) |
| **Owns verbs** | `FIRE <ms>`, `AIM <pan_us> <tilt_us>`, `PING` |
| **Role** | `turret` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

The firmware is intentionally "dumb": `AIM` values are servo pulse-width
**microseconds** that the orchestrator has already calibrated (degrees → µs via
`orchestrator/calibration/turret.toml`, range 1000–2000, center 1500). The
firmware just clamps and writes them.

## Wiring

- **8-Servos board** on the NanoC6 **Grove port** (I2C): `SDA = GPIO2`,
  `SCL = GPIO1` (`Wire.begin(2, 1)`). Pan servo → channel 0, tilt → channel 1.
- **Relay** — ⚠️ **confirm before flashing.** The sketch defaults to a **GPIO
  relay** (`#define RELAY_GPIO`, `RELAY_PIN = 6`, HIGH = fire). If yours is an
  I2C relay unit instead, comment out `RELAY_GPIO` and set `RELAY_ADDR`/`RELAY_REG`
  (it can share the Grove I2C bus with the servo board). Set `RELAY_PIN` to the
  actual GPIO you wired.

## Configure before flashing

1. **Secrets** (gitignored): `cp secrets.example.h secrets.h`, then fill in
   `WIFI_SSID` / `WIFI_PASS` / `ORCH_HOST` (orchestrator LAN IP) / `ORCH_PORT`
   (`8090`). `secrets.h` is gitignored so credentials never reach the repo.
2. **Relay**: set the **RELAY CONFIG** path in `turret.ino` (see Wiring).

## Build & flash

Arduino IDE or `arduino-cli` with the **ESP32 board package** (select the
*M5NanoC6* / ESP32-C6 target). No extra libraries — only `Wire.h` and `WiFi.h`
from the ESP32 core.

```sh
arduino-cli compile --fqbn esp32:esp32:m5stack_nanoc6 firmware/turret
arduino-cli upload  --fqbn esp32:esp32:m5stack_nanoc6 -p <PORT> firmware/turret
```

## Bring-up / calibration

1. Flash; the turret dials the orchestrator and sends `HELLO turret`.
2. In the operator console, enter **maintenance** and confirm the `turret`
   presence badge lights (`GET /maintenance/devices` lists it).
3. Use the **Turret panel** to aim (sliders/gamepad) and fire presets. Tune
   `turret.toml` pan/tilt center/limits and establish the **boresight** (the
   pulse widths that point the gun at a marker at the seating distance), then
   save the calibration.

## Known limitations / TODO

- **Relay interface is unconfirmed** (GPIO vs I2C) — see Wiring. This is the
  one hardware detail to verify against your actual board.
- **No AIM slew** yet: the firmware sets the target pulse directly. If the mech
  slams on large moves, add a stepped slew in `doAim()`.
- `FIRE` blocks for its duration (≤ `FIRE_MAX_MS`, default 1 s) before acking;
  fine for the booth's short squirts, well within the host's ack timeout.
