# firmware/gavel

Gavel **strike** firmware for the Wet Court fleet — a beefy servo swings a gavel
for verdicts and "order in the court."

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Servo** | M5Stack 8-Servos board (I2C `0x25`) — **ch0 = gavel arm** |
| **Owns verbs** | `GAVEL`, `GJOG`, `PING` |
| **Role** | `gavel` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

One `GAVEL` is one rap: the firmware swings the arm **REST → RAISE → STRIKE →
REST** and acks `OK GAVEL` (or `ERR GAVEL no_servo_board` if the 8-Servos board
isn't on the bus).

Like the turret, the firmware is **stateless**: the host sends the full strike
geometry on every command —
`GAVEL <rest> <raise> <strike> <raise_dwell_ms> <strike_dwell_ms> <settle_dwell_ms>`
(servo µs positions + dwell ms). These live in
`orchestrator/calibration/gavel.toml` and are tuned live from the **Gavel
maintenance tab** (see below). A **bare** `GAVEL` (no args) is still accepted and
falls back to the compiled defaults in `gavel.ino`, so the board is usable
stand-alone.

`GJOG <us>` moves the servo to a raw pulse-width and holds — the console's live
position preview while tuning.

## Wiring

- **8-Servos board** on the NanoC6 **Grove port** (I2C): `SDA = GPIO2`,
  `SCL = GPIO1` (`Wire.begin(2, 1)`). Gavel servo → **channel 0**.
- Power the **servo rail from the 8-Servos board's external input**, not from the
  NanoC6 — a beefy servo's stall current will brown out the MCU. Share grounds.

## Configure before flashing

**Secrets** (gitignored): `cp secrets.example.h secrets.h`, then fill in
`WIFI_SSID` / `WIFI_PASS` / `ORCH_HOST` (orchestrator LAN IP) / `ORCH_PORT`
(`8090`). `secrets.h` is gitignored so credentials never reach the repo.

**Strike geometry** is tuned from the console, not the firmware — see *Bring-up*
below. The `GAVEL_REST` / `GAVEL_RAISE` / `GAVEL_STRIKE` and `*_DWELL_MS`
constants in `gavel.ino` are only the bare-`GAVEL` fallback; the live values live
in `orchestrator/calibration/gavel.toml`. Most builds swing the head *down* to
strike (`STRIKE < REST < RAISE`); flip if mirrored.

## Build & flash

Arduino IDE or `arduino-cli` with the **ESP32 board package** (select the
*M5NanoC6* / ESP32-C6 target). No extra libraries — only `Wire.h` and `WiFi.h`
from the ESP32 core.

```sh
arduino-cli compile --fqbn esp32:esp32:m5stack_nanoc6 firmware/gavel
arduino-cli upload  --fqbn esp32:esp32:m5stack_nanoc6 -p <PORT> firmware/gavel
```

## Bring-up

1. Flash; the gavel dials the orchestrator and sends `HELLO gavel`.
2. In the operator console, enter **maintenance** and confirm the `gavel`
   presence badge lights (`GET /maintenance/devices` lists it).
3. Open the **Gavel** tab and tune the geometry live: **Jog** each position
   (rest/raise/strike) to eyeball it, **Test strike** to feel the full rap with
   the current values, adjust the `*_dwell_ms` if the servo arrives late (slow
   servo) or you want a snappier bang, then **Save to disk** to persist them to
   `gavel.toml`. Saved values are used by real verdict strikes too.

## Known limitations / TODO

- **Blocking strike:** the rap holds `loop()` for ~0.5 s of `delay()`. Fine for a
  one-shot actuator that acks after the swing, but it won't service a second line
  mid-rap. Make it non-blocking only if the gavel ever needs to overlap commands.
- **`STRIKE_RAPS` is 1** to match the spec's "one gavel strike." If "order in the
  court" should be a flurry, that's a protocol change (a `GAVEL <n>` arg), to be
  agreed in `protocol/README.md` first — not a silent firmware divergence.
