# firmware/swear-in

Defendant's **arcade button** for the Wet Court fleet — the one physical
control on their side of the bench. A press starts the trial (from Idle) or
serves as a non-verbal acknowledgement mid-trial; the button's built-in lamp
is host-driven, so the light itself cues *when* pressing does something.

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Button** | arcade pushbutton with built-in LED — switch on Grove **`G1`** ↔ GND, lamp on Grove **`G2`** ↔ GND |
| **Owns verbs** | `LED`, `PING` |
| **Emits** | `BUTTON` (unsolicited, on each debounced press) |
| **Role** | `swear-in` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

The firmware is a debounced switch scan plus a lamp animator, both run from
the shared client's `tick` hook (this board motivated `wetline.run(...,
tick=)` — the first device that *emits* rather than only answering).

**`BUTTON` semantics live in the host**, not here: the firmware reports every
debounced press (rate-limited to one per 250 ms) and the trial FSM decides
what a press means in the current state. Presses while the orchestrator link
is down are dropped, and the lamp is forced dark — a dark button honestly
reads as "not accepting input."

`LED <mode>` sets the lamp animation:

| Mode | Look | Intended cue |
|---|---|---|
| `off` | dark | pressing does nothing |
| `on` | steady | held state / press registered |
| `blink` | square flash (800 ms period) | **press me** — attract / start-trial prompt |
| `pulse` | slow breathe (2.4 s period) | armed / an acknowledgement window is open |

Independent of mode, every debounced press flashes the lamp full-bright for
150 ms — instant local feedback even before the host reacts.

## Wiring

The Grove pigtail's data wires, used as plain GPIO (no I2C on this board):

- **Switch** → Grove **`G1`** (white / SCL-position wire) and **GND** (black).
  Firmware uses the internal pull-up; pressed = low. Polarity doesn't matter.
- **Lamp** → Grove **`G2`** (yellow / SDA-position wire, +) and **GND** (−).
  Driven as 1 kHz PWM, active high. LED polarity matters — if the lamp stays
  dark in `on` mode, swap its two wires first.

**Lamp voltage note:** arcade buttons commonly ship with a 12 V or 5 V lamp
module. A 5 V-rated LED module (built-in resistor) will light from the 3.3 V
GPIO, just dimmer; a 12 V module will barely glow or stay dark. If it's too
dim, either swap the module's resistor for ~3.3 V operation, or drive the lamp
from the Grove 5 V pin through a small NPN/MOSFET switched by `G2`. A bare LED
(no built-in resistor) needs a series resistor (~100–220 Ω) — don't connect it
raw. GPIO source current is fine for a single LED (< 20 mA) but not more.

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
   `WIFI_SSID` / `WIFI_PASS`. Leave `ORCH_HOST` unset to auto-discover the
   orchestrator from its UDP beacon (the usual mode); set it only as a hard
   override (show rigs, or two orchestrators on one LAN).
3. **Deploy**: `./deploy.sh` — copies `main.py`, `secrets.py`, and the shared
   `wetline.py`/`ota.py`, then resets. Watch it with `mpremote repl`.
4. **OTA (optional)**: set `OTA_TOKEN` in `secrets.py` and redeploy once;
   afterwards `python3 ../micropython/otapush.py swear-in.local` updates over
   WiFi, no cable (see [`../micropython/`](../micropython/README.md)).

## Bench test (no orchestrator)

The lamp only animates while a link is up, so for a pure hardware check use
the REPL directly:

```sh
mpremote exec "
from machine import Pin, PWM
import time
p = PWM(Pin(2), freq=1000); p.duty_u16(65535)   # lamp full on
b = Pin(1, Pin.IN, Pin.PULL_UP)
for _ in range(50): print('pressed' if b.value()==0 else 'up'); time.sleep_ms(100)
"
```

## Bring-up

1. Flash + deploy; the board dials the orchestrator and sends
   `HELLO swear-in 0.1`.
2. In the operator console, enter **maintenance** and confirm the `swear_in`
   presence badge lights (`GET /maintenance/devices` lists it).
3. Drive `LED blink` / `pulse` / `on` / `off` from the console (or netcat) and
   watch the lamp; press the button and confirm the host logs `BUTTON` (from
   Idle this starts a trial — same path as the console's start button).

## Known limitations / TODO

- **Presses are momentary edges only** — no long-press or double-press vocab.
  If a hold gesture is ever wanted (e.g. hold-to-confirm), add a `BUTTON long`
  variant to the protocol first.
- **Offline presses are dropped by design** (no queueing): a queued stale
  press replaying after reconnect could start a trial nobody asked for.
- **Verified on hardware** (2026-07-13): flashed, on WiFi, LED modes and
  press reporting exercised from the console's Swear-in maintenance tab.
