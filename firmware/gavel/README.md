# firmware/gavel

Gavel **strike** firmware for the Wet Court fleet — a beefy servo swings a gavel
for verdicts and "order in the court."

| | |
|---|---|
| **Board** | M5Stack NanoC6 (ESP32-C6) |
| **Servo** | signal **directly on Grove `G2`** (50 Hz PWM, adapter cable) — no 8-Servos board; both units are taken by the turret and judge-neck pan/tilts |
| **Owns verbs** | `GAVEL`, `GJOG`, `PING` |
| **Role** | `gavel` (sent in the `HELLO` handshake) |
| **Protocol** | [`../../protocol/README.md`](../../protocol/README.md) (v2) |

One `GAVEL` is one rap: the firmware swings the arm **REST → RAISE → STRIKE →
REST** and acks `OK GAVEL` after the swing completes.

Like the turret, the firmware is **stateless**: the host sends the full strike
geometry on every command —
`GAVEL <rest> <raise> <strike> <raise_dwell_ms> <strike_dwell_ms> <settle_dwell_ms>`
(servo µs positions + dwell ms). These live in
`orchestrator/calibration/gavel.toml` and are tuned live from the **Gavel
maintenance tab** (see below). A **bare** `GAVEL` (no args) is still accepted and
falls back to the compiled defaults in `main.py`, so the board is usable
stand-alone.

`GJOG <us>` moves the servo to a raw pulse-width and holds — the console's live
position preview while tuning.

## Wiring

- **Servo signal** → Grove **`G2`** (the yellow / SDA-position wire) via an
  adapter cable; the firmware drives it as 50 Hz PWM (`PWM(Pin(2), freq=50)`,
  pulse width in µs via `duty_ns`).
- **Power the servo from an external 5 V supply, NOT the Grove 5 V pin** — a
  beefy servo's stall current will brown out the NanoC6. Share grounds
  (servo GND ↔ Grove GND ↔ supply GND).

## Runtime & files (MicroPython)

MicroPython reimplementation of the retired Arduino sketch (git history has
it): same hardware and wire contract. `main.py` is the servo driver + the
`GAVEL`/`GJOG` handlers; the protocol client (WiFi, dial, `HELLO`, line loop,
RGB status LED: red = no WiFi · amber = dialing · green = connected) is the
shared [`../micropython/wetline.py`](../micropython/wetline.py).

**Strike geometry** is tuned from the console, not the firmware — see *Bring-up*
below. The `GAVEL_REST` / `GAVEL_RAISE` / `GAVEL_STRIKE` and `*_DWELL_MS`
constants in `main.py` are only the bare-`GAVEL` fallback; the live values live
in `orchestrator/calibration/gavel.toml`. Most builds swing the head *down* to
strike (`STRIKE < REST < RAISE`); flip if mirrored. Dwells are clamped to
2000 ms each as a backstop, since the rap blocks the loop until it completes.

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
   afterwards `python3 ../micropython/otapush.py <role>.local` updates over WiFi,
   no cable (see [`../micropython/`](../micropython/README.md)).

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

- **Blocking strike:** the rap holds the serve loop for ~0.5 s. Fine for a
  one-shot actuator that acks after the swing, but it won't service a second line
  mid-rap. Make it non-blocking only if the gavel ever needs to overlap commands.
- **One rap per `GAVEL`** to match the spec's "one gavel strike." If "order in the
  court" should be a flurry, that's a protocol change (a `GAVEL <n>` arg), to be
  agreed in `protocol/README.md` first — not a silent firmware divergence.
- **No servo presence detection**: direct PWM can't tell whether a servo is
  attached or powered (the old 8-Servos board could be probed on I2C), so
  `GAVEL`/`GJOG` always ack `OK` — watch the mech, not just the ack.
- **Servo motion not yet verified on hardware** (the adapter cable doesn't
  exist yet); the board itself is commissioned — flashed, on WiFi, OTA-updated
  by name, acks verified over the wire.
