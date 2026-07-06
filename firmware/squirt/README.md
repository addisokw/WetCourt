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
`main.py` defaults `RELAY_PIN = 2`; if the relay doesn't click, set it to `1`.
HIGH = fire.

## Runtime & files (MicroPython)

MicroPython reimplementation of the retired Arduino sketch (git history has
it): same hardware and wire contract. `main.py` is the relay pin + `FIRE`
handler — off at boot, off in a `finally` after every pulse, duration clamped
to 1000 ms. The protocol client (WiFi, dial, `HELLO`, line loop, RGB status
LED: red = no WiFi · amber = dialing · green = connected) is the shared
[`../micropython/wetline.py`](../micropython/wetline.py).

## Setup

1. **Flash MicroPython** (one-time): **v1.28.0 `ESP32_GENERIC_C6`** from
   <https://micropython.org/download/ESP32_GENERIC_C6/>.

   ```sh
   pip3 install esptool mpremote
   python3 -m esptool --port /dev/cu.usbmodem* erase-flash
   python3 -m esptool --port /dev/cu.usbmodem* --baud 460800 \
       write-flash 0x0 ESP32_GENERIC_C6-*.bin     # C6 is RISC-V: offset 0x0
   ```

2. **Secrets** (gitignored): `cp secrets.example.py secrets.py`, then fill in
   `WIFI_SSID` / `WIFI_PASS` / `ORCH_HOST` (orchestrator LAN IP) / `ORCH_PORT`
   (`8090`). Confirm `RELAY_PIN` in `main.py` (see Wiring).
3. **Deploy**: `./deploy.sh` — copies `main.py`, `secrets.py`, and the shared
   `wetline.py`, then resets. Watch it with `mpremote repl`.

> The MicroPython port is not yet verified on hardware (the Arduino version
> was); logic is stub-tested host-side.

## Bring-up

Flash; the board dials the orchestrator and sends `HELLO squirt`. In the operator
console, enter **maintenance** and confirm the `squirt` presence badge lights in
the Turret panel; the **Fire** buttons there target this board. Quick-fire preset
durations live in `orchestrator/calibration/squirt.toml`.
