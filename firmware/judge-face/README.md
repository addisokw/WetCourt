# judge-face — the animated Eye (CircuitPython)

The physical presence of the Wet Court AI judge: an animated eye on a 64×32
HUB75 LED matrix, mounted **portrait** (logical 32 wide × 64 tall), driven by
an **Adafruit Matrix Portal M4**. Implements
[`docs/wet-court-eye-face-brief.md`](../../docs/wet-court-eye-face-brief.md)
on the M4, in portrait, wired into the fleet's
[device line protocol](../../protocol/README.md) rather than the brief's
suggested UDP side-channel.

The eye drifts its gaze, blinks, dilates its pupil with the defendant's voice
level, narrows its lids while deliberating, strobes red on a guilty verdict
and blooms green on an innocent one. Five judge personas (palette + motion
speed) are switchable at runtime.

## Files

| File | Role |
|---|---|
| `code.py` | entry point: display bring-up (portrait), main loop, FPS report |
| `eye_face.py` | `EyeFace` — layered-displayio renderer (the core deliverable) |
| `personas.py` | the 5 persona ramps + tone sampling |
| `inputs.py` | `OrchestratorLink` (TCP line protocol) + `DemoSource` (fake inputs) |
| `config.py` | display constants + settings.toml accessors |
| `settings.toml.example` | template for WiFi/orchestrator config (copy → `settings.toml`) |

## Setup

1. **CircuitPython**: install the latest stable CircuitPython for
   *Matrix Portal M4* (double-tap RESET → drag the `.uf2` onto `MATRIXBOOT`).
   This retires the previous Arduino sketch (git history has it) — flashing
   CP replaces it on the board.
2. **Libraries** (from the Adafruit CircuitPython Bundle matching your CP
   version, into `CIRCUITPY/lib/`):
   - `adafruit_esp32spi/` (AirLift WiFi)
   - `adafruit_connection_manager.mpy` (socket pool for the AirLift)
   - `adafruit_ticks.mpy` (wrap-safe timing)
   `displayio`, `rgbmatrix`, `framebufferio`, `bitmaptools` are built into the
   M4 firmware. No `adafruit_matrixportal` wrapper needed — `code.py` wires
   the HUB75 pins directly via the board's `MTX_*` names.
3. **Config**: copy `settings.toml.example` → `settings.toml`, fill in WiFi +
   orchestrator host (same values as the old gitignored `secrets.h`).
4. Copy the five `.py` files + `settings.toml` to the `CIRCUITPY` drive. It
   reboots and runs; the serial console prints FPS every 5 s and link status.

With no orchestrator reachable (or `EYE_DEMO = 1`), it runs **demo mode**:
cycles idle → listening → deliberating → verdict, rotates personas each
cycle, and synthesizes a speech-like audio envelope — the full acceptance
loop with zero infrastructure.

## Protocol

Dials the orchestrator, `HELLO judge-face 0.2`, then services (one ack per
command):

| Command | Effect |
|---|---|
| `FACE <phase>` | `idle` · `listening` · `deliberating` · `verdict:guilty` · `verdict:innocent` |
| `AUDIO <0.0–1.0>` | mic envelope; drives pupil dilation while `listening` (send ~20–30 Hz) |
| `PERSONA <slug>` | `honorable` · `magistrate` · `cosmic` · `nullpointer` · `petunia` |
| `PANEL <pattern>` | legacy alias: `idle`→idle, `thinking`→deliberating, `verdict`→verdict:guilty |
| `PING` | keepalive |

The orchestrator currently only sends `PANEL`/`PING`; the richer verbs are
spec'd in `protocol/README.md` and ready for the host to adopt.

## Architecture (brief §4a, layered displayio)

Per-frame Python work is: move one TileGrid (gaze), nudge two lid bars
(blink), and occasionally rewrite a 19×19 pupil box (dilation) — everything
else composites in C. The iris tile (halo + striations + limbal ring + pupil
+ catchlight) is built per persona and cached; verdict effects recolor the
palettes instead of touching bitmaps.

**Documented deviations from the prototype** (M4 CPU budget / displayio
limits — revisit on an S3):

- **Portrait orientation** (user decision; the brief's geometry is
  parameterized, so `W=32, H=64` flows through).
- Iris **striations are static** per persona — no slow per-frame re-texturing.
- Guilty **glitch** = whole-face ±2 px horizontal jitter + ~10 Hz red palette
  strobe, not per-row shifts.
- Innocent **bloom** = palette lerp toward green easing out over 2 s
  (displayio has no alpha blending).
- `deliberating` (lids to ~0.55, faster darts) is the brief's *recommended
  extension*, not prototype behavior.
- WiFi association + `HELLO` handshake are synchronous on the AirLift and can
  stall a few seconds; attempts are rate-limited (8 s backoff) and `dt` is
  clamped so the animation never leaps.

**Verify on-device** (bundle APIs move): `bitmaptools.arrayblit` is
feature-detected with a per-pixel fallback; the esp32spi socket-pool
non-blocking `recv_into` semantics are handled defensively in
`inputs.py:_service`. Report actual FPS from the serial console.
