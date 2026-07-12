# judge-face — the animated Eye (CircuitPython)

The physical presence of the Wet Court AI judge: an animated eye on a 64×32
HUB75 LED matrix, mounted **portrait** (logical 32 wide × 64 tall), driven by
an **Adafruit Matrix Portal M4**. Implements
[`docs/wet-court-eye-face-brief.md`](../../docs/wet-court-eye-face-brief.md)
on the M4, in portrait, wired into the fleet's
[device line protocol](../../protocol/README.md) rather than the brief's
suggested UDP side-channel.

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

1. **CircuitPython 10.2.1** (what this was built and tested on): double-tap
   RESET → drag the `.uf2` onto `MATRIXBOOT`. Download:
   <https://downloads.circuitpython.org/bin/matrixportal_m4/en_US/adafruit-circuitpython-matrixportal_m4-en_US-10.2.1.uf2>
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
(`../micropython/README.md`), with two platform twists: the CIRCUITPY drive
belongs to exactly one writer, and the AirLift doesn't do mDNS.

- `boot.py` arbitrates at reset: **default = OTA mode** (filesystem writable
  to `ota.py`, read-only to a USB host); **UP held at reset = USB deploy
  mode** (`deploy.sh` works, OTA answers `read_only_fs`). `boot.py` itself is
  on the OTA forbidden list, so recovery over USB can never be pushed away.
- Push from this directory to the board's IP (find it in the serial log's
  `wifi: up, <ip>` line):

  ```sh
  cd firmware/judge-face
  python3 ../micropython/otapush.py 192.168.50.77              # otafiles.txt set
  python3 ../micropython/otapush.py 192.168.50.77 eye_face.py  # just one file
  ```

  Credentials come from `./settings.toml` (`OTA_TOKEN`, `OTA_PORT`; empty
  token = OTA disabled). Files stage as `*.new`, sizes + sha256 digests are
  verified at commit, then swapped in and the board resets. `settings.toml`
  itself only travels over USB.
- The listener rides the orchestrator link's WiFi, so it works with the
  orchestrator down — but not in forced demo mode (`EYE_DEMO = 1`, no radio).

`lib/` vendors the exact `.mpy` dependencies the board runs —
`adafruit_esp32spi` (AirLift WiFi), `adafruit_connection_manager`, and
`adafruit_ticks`, byte-identical to Adafruit bundle `20260704` (10.x-mpy
series; `.mpy` files are CP-major-specific). `displayio`, `rgbmatrix`,
`framebufferio`, and `bitmaptools` are built into the M4 firmware. No
`adafruit_matrixportal` wrapper needed — `code.py` wires the HUB75 pins
directly via the board's `MTX_*` names.

With no orchestrator reachable (or `EYE_DEMO = 1`), it runs **demo mode**:
cycles idle → listening → deliberating, rotates personas each cycle, and
synthesizes a speech-like audio envelope — developable with zero
infrastructure. The demo deliberately skips the verdict phases (the guilty
strobe reads as a glitch out of context); exercise those by sending
`FACE verdict:guilty` / `FACE verdict:innocent` from the host.

## Protocol

Dials the orchestrator, `HELLO judge-face 0.3`, then services (one ack per
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

**Documented deviations from the prototype** (M4 CPU budget / displayio
limits — revisit on an S3):

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
- WiFi association + `HELLO` handshake are synchronous on the AirLift and can
  stall a few seconds; attempts are rate-limited (8 s backoff) and `dt` is
  clamped so the animation never leaps.

**Verify on-device** (bundle APIs move): `bitmaptools.arrayblit` is
feature-detected with a per-pixel fallback; the esp32spi socket-pool
non-blocking `recv_into` semantics are handled defensively in
`inputs.py:_service`. Report actual FPS from the serial console.
