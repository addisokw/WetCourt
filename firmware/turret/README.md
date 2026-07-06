# firmware/turret

Squirt-gun turret **aim** firmware (pan/tilt) for the Wet Court fleet.

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Servos** | M5Stack 8-Servos board (I2C `0x25`) — **ch0 = pan, ch1 = tilt** |
| **Owns verbs** | `AIM <pan_us> <tilt_us>`, `PING` |
| **Role** | `turret` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

**Firing is a separate board** — `firmware/squirt/` (role `squirt`) drives the
relay. The servo board takes the NanoC6's only Grove pins for I2C, leaving no
GPIO for a relay, so the gun's trigger gets its own NanoC6. The orchestrator
routes `AIM` here and `FIRE` to `squirt`.

The firmware is intentionally "dumb": `AIM` values are servo pulse-width
**microseconds** that the orchestrator has already calibrated (degrees → µs via
`orchestrator/calibration/turret.toml`, range 1000–2000, center 1500). The
firmware just clamps and writes them.

## Wiring

- **8-Servos board** on the NanoC6 **Grove port** (I2C): `SDA = GPIO2`,
  `SCL = GPIO1` (`I2C(0, scl=Pin(1), sda=Pin(2))`). Pan servo → channel 0,
  tilt → channel 1.

## Runtime & files (MicroPython)

MicroPython reimplementation of the retired Arduino sketch (git history has
it): same hardware and wire contract. `main.py` is the servo driver + `AIM`
handler; the protocol client (WiFi, dial, `HELLO`, line loop, RGB status LED:
red = no WiFi · amber = dialing · green = connected) is the shared
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
   (`8090`).
3. **Deploy**: `./deploy.sh` — copies `main.py`, `secrets.py`, and the shared
   `wetline.py`, then resets. Watch it with `mpremote repl`.
4. **OTA (optional)**: set `OTA_TOKEN` in `secrets.py` and redeploy once;
   afterwards `python3 ../micropython/otapush.py <board-ip>` updates over WiFi,
   no cable (see [`../micropython/`](../micropython/README.md)).

## Bring-up / calibration

1. Flash; the turret dials the orchestrator and sends `HELLO turret`.
2. In the operator console, enter **maintenance** and confirm the `turret`
   presence badge lights (`GET /maintenance/devices` lists it).
3. Use the **Turret panel** to aim (sliders/gamepad). Tune `turret.toml` pan/tilt
   center/limits and establish the **boresight** (the pulse widths that point the
   gun at a marker at the seating distance), then save the calibration. (Firing
   is exercised from the same panel but targets the `squirt` board.)

## Known limitations / TODO

- **No AIM slew** yet: the firmware sets the target pulse directly. If the mech
  slams on large moves, add a stepped slew in `handle_aim()`.
- **MicroPython port not yet verified on hardware** — the Arduino version was;
  logic is stub-tested host-side. First bench session: `./deploy.sh`, check the
  LED goes red → amber → green, re-run the turret panel bring-up.
