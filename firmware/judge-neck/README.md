# judge-neck — pan/tilt gaze (MicroPython)

The servos that aim the judge's LED-matrix head. The face itself is the
separate `judge-face` board (Matrix Portal M4); this NanoC6 + 8-Servos board
owns the pan/tilt only — the turret recipe, rescoped. MicroPython
reimplementation of the original Arduino sketch (git history has it): same
hardware, same wire contract.

## Hardware

- **Board:** M5Stack NanoC6 (ESP32-C6)
- **Accessory:** M5Stack 8-Servos board on Grove I2C (`0x25`) — **ch0 = pan,
  ch1 = tilt**
- **Onboard RGB LED** shows link status: **red** = WiFi down · **amber** =
  dialing orchestrator · **green** = connected

## Files

| File | Role |
|---|---|
| `main.py` | servo driver + `AIM` handler (runs at boot) |
| `wetline.py` | role-agnostic protocol client: WiFi, dial, `HELLO`, line loop, status LED — reusable by the other NanoC6 boards |
| `secrets.example.py` | template for WiFi/orchestrator config (copy → `secrets.py`, gitignored) |
| `deploy.sh` | copy the three files to the board via `mpremote` |

## Setup

1. **Flash MicroPython** (one-time). Tested build: **v1.28.0
   `ESP32_GENERIC_C6`** (`ESP32_GENERIC_C6-20260406-v1.28.0.bin`), download
   from <https://micropython.org/download/ESP32_GENERIC_C6/>.

   ```sh
   pip3 install esptool mpremote
   python3 -m esptool --port /dev/cu.usbmodem* erase-flash
   python3 -m esptool --port /dev/cu.usbmodem* --baud 460800 \
       write-flash 0x0 ESP32_GENERIC_C6-20260406-v1.28.0.bin
   ```

   > ESP32-C6 is RISC-V: flash offset is **`0x0`**, not `0x1000`. If esptool
   > can't enter the bootloader, hold the BTN (G9) while plugging in.

2. **Config**: `cp secrets.example.py secrets.py`, fill in WiFi +
   orchestrator host.
3. **Deploy**: `./deploy.sh` — copies `wetline.py`, `secrets.py`, `main.py`
   and resets; the board boots into `main.py`. Watch it with
   `mpremote repl`.

## Protocol

Dials the orchestrator, `HELLO judge-neck 0.2`, then services (one ack per
command, per [the device protocol](../../protocol/README.md)):

| Command | Effect |
|---|---|
| `AIM <pan_us> <tilt_us>` | set both servo pulse widths — values are **already-calibrated µs** from the host (`judge_neck.toml`, 1000–2000, center 1500); firmware clamps to 1000–2000 as a backstop |
| `PING` | keepalive |

On boot both channels are put in servo mode and centered (1500 µs). If the
servo board isn't answering on I2C, the firmware still serves the protocol
and acks `ERR AIM i2c_fail` until it appears.

The host mirrors every judge-neck `AIM` to the judge-face (in degrees) for
the eye's catchlight parallax — nothing this board needs to do about it.

## Status

Logic is stub-tested host-side (dispatch, clamping, ack shapes); **not yet
verified on a physical NanoC6 + servo board** — deploy with `./deploy.sh`
and check the LED goes red → amber → green, then `AIM 1500 1500` from the
maintenance console.
