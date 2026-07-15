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
| `main.py` | slew-limited servo driver + `AIM` handler (runs at boot) |
| `test_motion_desktop.py` | CPython test of the motion-safety layer (soft-home, slew limits, warm-reset readback, overcurrent watchdog) — `python3 test_motion_desktop.py`, no hardware |
| [`../micropython/wetline.py`](../micropython/wetline.py) | shared protocol client: WiFi, dial, `HELLO`, line loop, status LED (one copy for all NanoC6 boards) |
| `secrets.example.py` | template for WiFi/orchestrator config (copy → `secrets.py`, gitignored) |
| `deploy.sh` | copy `main.py` + `secrets.py` + the shared `wetline.py` to the board via `mpremote` |

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
4. **OTA (optional)**: set `OTA_TOKEN` in `secrets.py` and redeploy once;
   afterwards `python3 ../micropython/otapush.py <role>.local` updates over WiFi,
   no cable (see [`../micropython/`](../micropython/README.md)).

## Protocol

Dials the orchestrator, `HELLO judge-neck 0.5`, then services (one ack per
command, per [the device protocol](../../protocol/README.md)):

| Command | Effect |
|---|---|
| `AIM <pan_us> <tilt_us>` | set slew-limited targets for both servos — values are **already-calibrated µs** from the host (`judge_neck.toml`); firmware clamps to 500–2500 as a backstop. `OK AIM` acks the accepted target, not arrival |
| `PING` | keepalive |

It also emits one unsolicited event: `OVERCURRENT <amps>` when the servo
board's total current stays above `OC_LIMIT_A` for `OC_HOLD_MS` — the
firmware has already eased tilt to the top of the working range,
`TILT_SAFE` (stall/jam guard; one trip per excursion, re-arms when current
recovers). It deliberately does NOT go to full droop: droop only clears the
booth with pan centered. The threshold is **unverified on hardware** — tune
it on the rig, or set `OC_LIMIT_A = None` to disable.

Current is polled **only while moving** (and `OC_TAIL_MS` after the last
step): the STM32 renders servo PWM in software and servicing an I2C read
can stretch an in-flight pulse — 0.4's always-on 4 Hz poll made the parked
head twitch every few seconds. Parked means a silent bus.

## Motion safety

These servos have no position feedback and slew to a commanded pulse at full
speed — fast enough that a cold-boot "go to center" once snapped the tilt
mount under the head's weight. So the commanded pulse never jumps: `AIM`
sets targets and the wetline tick walks the output toward them at a bounded
rate (`AIM_RATE`, µs/s — tuned so moves read as smooth on the rig, not just
survivable). Boot is a slow soft-home (`HOME_RATE`) from where the head
actually is:

- **power-on**: tilt is at gravity droop — `TILT_DROOP` (~2167 µs on this
  rig; the counter-spring puts droop *above* the 1500–1967 working range,
  and 1500 is the spring crash-limit). A wrong droop estimate fails
  downward into the spring, never upward through the mount.
- **warm reset** (OTA `machine.reset()`, soft reset): the 5 V rail never
  dropped, so the STM32 held its last pulse — the firmware reads it back
  (`0x60`/`0x62`) and ramps from there. The readback is trusted only if the
  channel still reads servo mode `3`: a power-cycled STM32 (registers at
  defaults) can't spoof a held pulse, so a brownout degrades to the droop
  assumption.

**Droop-zone pan lock.** The drooped head only clears the booth with pan
centered, so while tilt is beyond `TILT_SAFE` (the working-range max,
toward droop) pan is locked — frozen mid-move if needed, and on boot not
even powered. Pan's true position is unknowable without feedback; its
power-up snap to the assumed pose is taken only once tilt is back inside
the working range.

If the head is remounted or the spring changes, re-measure `TILT_DROOP`:
with servos unpowered, let the head settle, then find the pulse that matches
the resting pose (over a REPL, step `_write_pulse(CH_TILT, us)` toward it
while supporting the head).

If the servo board isn't answering on I2C, the firmware still serves the
protocol and acks `ERR AIM i2c_fail` until it appears; targets are kept and
the believed position is never advanced past a failed write, so motion
resumes without a jump.

The host mirrors every judge-neck `AIM` to the judge-face (in degrees) for
the eye's catchlight parallax — nothing this board needs to do about it.

## Status

Logic is desktop-tested (`test_motion_desktop.py`: soft-home ramp, slew
limits, dt cap, warm-reset readback, overcurrent trip/re-arm) and the 0.3
wire contract was **verified on a physical NanoC6 + servo board**
(2026-07-11). The 0.4 motion-safety layer (added after the tilt mount
snapped on a cold-boot jolt, 2026-07-14) is **pending a hardware pass**:
confirm the STM32 pulse readback at `0x60`, the `0xA0` current units, and
re-measure `TILT_DROOP` on the repaired mount.
