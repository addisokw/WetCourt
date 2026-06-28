# firmware/squirt

Squirt-gun **firing** firmware — the relay half of the turret.

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Relay** | M5Stack 3A Relay (GPIO) on the NanoC6 Grove port |
| **Owns verbs** | `FIRE <ms>`, `PING` |
| **Role** | `squirt` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

## Why this is a separate board from `turret`

The pan/tilt servos use an M5Stack 8-Servos board over I2C, which takes the
turret NanoC6's **only** Grove pins (`G1`/`G2`). A GPIO relay needs its own
signal pin, and the NanoC6 exposes no spare GPIO — so the relay gets its own
NanoC6, whose Grove port is free to drive the relay directly. The orchestrator
routes `AIM` → `turret` and `FIRE` → `squirt`.

## Wiring

The **3A Relay** plugs into this NanoC6's **Grove port**. Its control signal
lands on a Grove GPIO — `GPIO2` (SDA position) or `GPIO1` (SCL position).
`squirt.ino` defaults `RELAY_PIN = 2`; if the relay doesn't click, set it to `1`.
HIGH = fire.

## Configure before flashing

1. **Secrets** (gitignored): `cp secrets.example.h secrets.h`, then fill in
   `WIFI_SSID` / `WIFI_PASS` / `ORCH_HOST` (orchestrator LAN IP) / `ORCH_PORT`
   (`8090`).
2. **Relay pin**: confirm `RELAY_PIN` in `squirt.ino` (see Wiring).

## Build & flash

Arduino IDE or `arduino-cli` with the **ESP32 board package** (select the
*M5NanoC6* / ESP32-C6 target). No extra libraries — only `WiFi.h` from the ESP32
core.

```sh
arduino-cli compile --fqbn esp32:esp32:m5stack_nanoc6 firmware/squirt
arduino-cli upload  --fqbn esp32:esp32:m5stack_nanoc6 -p <PORT> firmware/squirt
```

## Bring-up

Flash; the board dials the orchestrator and sends `HELLO squirt`. In the operator
console, enter **maintenance** and confirm the `squirt` presence badge lights in
the Turret panel; the **Fire** buttons there target this board. Quick-fire preset
durations live in `orchestrator/calibration/squirt.toml`.
