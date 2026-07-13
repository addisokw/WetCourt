# judge-face — the animated Eye (CircuitPython)

The physical presence of the Wet Court AI judge: an animated eye on a 64×32
HUB75 LED matrix, mounted **portrait** (logical 32 wide × 64 tall), driven by
an **Adafruit Matrix Portal S3**. Implements
[`docs/wet-court-eye-face-brief.md`](../../docs/wet-court-eye-face-brief.md)
in portrait, wired into the fleet's
[device line protocol](../../protocol/README.md) rather than the brief's
suggested UDP side-channel.

(Ported from the Matrix Portal M4 on 2026-07-12 — same panel plug, same
protocol, native WiFi instead of the AirLift. The last M4-flavored firmware
is commit `74330dc`.)

The eye drifts its gaze vertically (the `judge-neck` pan/tilt mech that the
panel is mounted on owns horizontal gaze), dilates its pupil with the
defendant's voice level, darts faster while deliberating, strobes red on a
guilty verdict and blooms green on an innocent one. Five judge personas
(palette + motion speed) are switchable at runtime.

## Files

| File | Role |
|---|---|
| `code.py` | entry point: display bring-up (portrait), main loop, FPS report |
| `eye_face.py` | `EyeFace` — layered-displayio renderer (the core deliverable) |
| `personas.py` | the 5 persona ramps + tone sampling |
| `inputs.py` | `OrchestratorLink` (TCP line protocol) + `DemoSource` (fake inputs) |
| `config.py` | display constants + settings.toml accessors |
| `ota.py` | WiFi firmware updates (CircuitPython port of `../micropython/ota.py`) |
| `boot.py` | filesystem arbitration: code-writable (OTA) vs host-writable (USB) |
| `otafiles.txt` | default file set `otapush.py` sends from this dir |
| `settings.toml.example` | template for WiFi/orchestrator config (copy → `settings.toml`) |
| `lib/` | vendored CircuitPython libraries (exact `.mpy` files the board runs) |
| `deploy.sh` | copy firmware + libs + settings onto a mounted `CIRCUITPY` drive |

## Setup

1. **CircuitPython 10.2.1** (matching the version the M4 port was tested
   on): double-tap RESET → drag the `.uf2` onto `MATRXS3BOOT`. Download:
   <https://downloads.circuitpython.org/bin/adafruit_matrixportal_s3/en_US/adafruit-circuitpython-adafruit_matrixportal_s3-en_US-10.2.1.uf2>
   (A newer stable should also work — the deps are vendored, but re-verify.)
2. **Config**: copy `settings.toml.example` → `settings.toml`, fill in WiFi +
   orchestrator host.
3. **Deploy**: `./deploy.sh` (with the `CIRCUITPY` drive mounted). It copies
   the `.py` files, `settings.toml`, and `lib/`, then the board
   auto-reloads; the serial console prints FPS every 5 s and link status.
   **After the first deploy that includes `boot.py`, the drive defaults to
   OTA mode (read-only to your Mac) — hold UP while pressing reset to get a
   writable drive back for `deploy.sh`.**

## OTA updates (no cable)

Same staged/verified protocol and push client as the NanoC6 fleet
(`../micropython/README.md`), with one platform twist: the CIRCUITPY drive
belongs to exactly one writer.

- `boot.py` arbitrates at reset: **default = OTA mode** (filesystem writable
  to `ota.py`, read-only to a USB host); **UP held at reset = USB deploy
  mode** (`deploy.sh` works, OTA answers `read_only_fs`). `boot.py` itself is
  on the OTA forbidden list, so recovery over USB can never be pushed away.
- Push from this directory. The board answers to `judge-face.local` (mDNS,
  also its DHCP hostname on the booth router); a raw IP from the serial
  log's `wifi: up, <ip>` line works too:

  ```sh
  cd firmware/judge-face
  python3 ../micropython/otapush.py judge-face.local              # otafiles.txt set
  python3 ../micropython/otapush.py judge-face.local eye_face.py  # just one file
  ```

  Credentials come from `./settings.toml` (`OTA_TOKEN`, `OTA_PORT`; empty
  token = OTA disabled). Files stage as `*.new`, sizes + sha256 digests are
  verified at commit, then swapped in and the board resets. `settings.toml`
  itself only travels over USB.
- The listener rides the orchestrator link's WiFi, so it works with the
  orchestrator down — but not in forced demo mode (`EYE_DEMO = 1`, no radio).

`lib/` vendors the exact `.mpy` dependencies the board runs — just
`adafruit_ticks`, byte-identical to Adafruit bundle `20260704` (10.x-mpy
series; `.mpy` files are CP-major-specific). Everything else is built into
the S3 firmware: `displayio`, `rgbmatrix`, `framebufferio`, `bitmaptools`,
and — new with the S3 — native `wifi`, `socketpool`, `mdns`, and `hashlib`
(the AirLift-era `adafruit_esp32spi` + `adafruit_connection_manager` are
gone). No `adafruit_matrixportal` wrapper needed — `code.py` wires the
HUB75 pins directly via the board's `MTX_*` names.

With no orchestrator reachable (or `EYE_DEMO = 1`), it runs **demo mode**:
cycles idle → listening → deliberating, rotates personas each cycle, and
synthesizes a speech-like audio envelope — developable with zero
infrastructure. The demo deliberately skips the verdict phases (the guilty
strobe reads as a glitch out of context); exercise those by sending
`FACE verdict:guilty` / `FACE verdict:innocent` from the host.

## Protocol

Dials the orchestrator, `HELLO judge-face 0.4`, then services (one ack per
command):

| Command | Effect |
|---|---|
| `FACE <phase>` | `idle` · `listening` · `deliberating` · `verdict:guilty` · `verdict:innocent` |
| `AUDIO <0.0–1.0>` | mic envelope; drives pupil dilation while `listening` (send ~20–30 Hz) |
| `PERSONA <slug>` | `honorable` · `magistrate` · `cosmic` · `nullpointer` · `petunia` |
| `AIM <pan> <tilt>` | neck pose in **degrees** (the host mirrors judge-neck `AIM`); counter-moves the catchlight for specular parallax — moves no hardware |
| `PANEL <pattern>` | legacy alias: `idle`→idle, `thinking`→deliberating, `verdict`→verdict:guilty |
| `PING` | keepalive |

The orchestrator currently only sends `PANEL`/`PING`; the richer verbs are
spec'd in `protocol/README.md` and ready for the host to adopt.

## Architecture (brief §4a, layered displayio)

Per-frame Python work is: move two TileGrids (gaze + catchlight) and
occasionally rewrite a 19×19 pupil box (dilation) — everything else
composites in C. The iris tile (halo + striations + limbal ring + pupil) is
built per persona and cached; verdict effects recolor the palettes instead
of touching bitmaps.

The **catchlight is its own 2×2 layer**, not baked into the iris: a
catchlight is a reflection of a fixed light source, so it counter-moves
against the neck pose (`AIM` mirror, ~0.12 px/deg, smoothed ~0.25 s to match
servo swing), clamped to stay on the iris. It rides the eye's own vertical
micro-drift rigidly — counter-moving there too was tried and read badly at
this pixel scale. The tunable is `_GLINT_PX_PER_DEG` at the top of
`eye_face.py`; flip its sign there if the slide direction reads wrong on
hardware.

**Documented deviations from the prototype** (originally M4 CPU budget /
displayio limits; the S3 has the CPU headroom to revisit the budget-driven
ones — static striations, the glitch style — but none have been yet, and
the displayio limits still stand):

- **Portrait orientation** (user decision; the brief's geometry is
  parameterized, so `W=32, H=64` flows through).
- **No blink/eyelids** (operator preference): the prototype's lid bars read
  as the frame shrinking on the physical portrait panel. The `deliberating`
  narrowed-lids behavior went with them; its faster gaze darts remain.
- **Background is pure black** (operator preference) instead of the
  prototype's (8,6,8) — off pixels stay off; the sclera halo fades to black.
- **Eye sized to fit the narrow axis**: iris ratio 0.34 + 4 px halo (brief:
  0.38 + 6 px), so the full disc + glow fits the 32 px width instead of
  clipping flat at the edges.
- **Gaze drifts vertically only**: the panel rides the `judge-neck` pan/tilt
  mech, which owns horizontal gaze; portrait width leaves no sideways room.
- Iris **striations are static** per persona — no slow per-frame re-texturing.
- Guilty **glitch** = whole-face ±2 px horizontal jitter + ~10 Hz red palette
  strobe, not per-row shifts.
- Innocent **bloom** = palette lerp toward green easing out over 2 s
  (displayio has no alpha blending).
- `deliberating` (faster gaze darts) is the brief's *recommended extension*,
  not prototype behavior.
- WiFi association + `HELLO` handshake are synchronous (native `wifi` too,
  though quicker than the AirLift was) and can stall a couple of seconds;
  attempts are rate-limited with backoff and `dt` is clamped so the
  animation never leaps.

**Verify on-device** (core APIs move): `bitmaptools.arrayblit` is
feature-detected with a per-pixel fallback; the native socketpool
non-blocking `recv_into` semantics (EAGAIN = no data, 0 = peer closed) are
handled defensively in `inputs.py:_service`. Report actual FPS from the
serial console — the S3 should beat the M4's numbers.

## Troubleshooting a dead face

What the blank-vs-not distinction tells you before you touch anything:

- **Panel animating but no orchestrator registration** → the board is fine,
  the *link* is down. Query `OTALOG <token>` (raw TCP line to
  `<face-ip>:8266`) for the link's last error + layered net probe, and see
  the socketpool note below.
- **Panel frozen on one frame** → `code.py` is wedged mid-loop (it drives
  `display.refresh()` manually). Serial console shows where.
- **Panel showing tiny text** → `code.py` crashed with a Python exception;
  CircuitPython paints the traceback to the framebuffer. Read it there or
  over serial.
- **Panel completely blank AND absent from the LAN** → `code.py` never ran:
  **safe mode**, no power, or a corrupted filesystem. A Python bug can't
  produce this combination — don't start by reading firmware diffs.

The quickest network check runs from the orchestrator host (it's on the
booth LAN): `ping judge-face.local` (mDNS) or look for `judge-face.lan` in
the router's client list next to the NanoC6 fixtures (`squirt.lan`,
`gavel.lan`, …). If the name doesn't resolve, fall back to probing candidate
IPs on `:8266` — `OTALOG` now also reports live RSSI, since weak booth WiFi
looks exactly like a dead host.

**Recovery ladder** (proven 2026-07-12: booth trial found the panel blank
and off-network; step 1 fixed it):

1. **Press RESET once.** Safe mode is CircuitPython's response to a hard
   fault, watchdog, or brownout — it deliberately doesn't run `code.py`
   (blank panel, no WiFi) until a manual reset. The status NeoPixel blinks
   **yellow** in safe mode. The HUB75 panel's current draw makes brownout
   the usual suspect here — and the S3's WiFi TX bursts draw *more* than
   the M4+AirLift did, so a marginal 5 V supply gets less forgiving with
   this board, not more. **An OTA push is peak load** (sustained flash
   writes + WiFi + panel — a push mid-transfer is a proven brownout
   trigger on a weak supply). A one-off safe-mode trip is expected booth
   life, not a firmware bug. After any surprise reboot, `OTALOG` reports
   `boot=<reset reason>` — query it *before* pressing RESET if you can,
   since the button press overwrites the reason.
2. **If it recurs**: USB to a computer, `screen /dev/cu.usbmodem* 115200` —
   CircuitPython prints the safe-mode reason at the top of the console.
   Repeated brownouts = fix the 5 V supply, not the code.
3. **If CIRCUITPY looks damaged** (missing/garbled files — possible after a
   power cut mid-OTA-write, since OTA mode keeps the drive code-writable):
   hold **UP while pressing RESET** (USB deploy mode) and re-run
   `./deploy.sh`. `boot.py` is on the OTA forbidden list, so this escape
   hatch always survives.
4. **Confirm recovery**: the eye animates (demo mode counts), the
   orchestrator logs `registry: judge_face connected`, and `OTALOG` answers
   on `:8266`.
