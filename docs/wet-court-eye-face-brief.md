# Wet Court — "Eye" Judge Face → CircuitPython (Adafruit Matrix Portal)

**Type:** implementation brief to initiate a Claude Code session
**Target:** a 64×32 HUB75 RGB LED matrix driven by an Adafruit Matrix Portal, running CircuitPython
**Source of truth:** the web prototype `wet-court-led-lab.html` (attach it to the session). This doc restates the essentials so it is self-contained, but the prototype's `eye:(t,a)` function and `PERSONAS` table are the canonical behavior — match them.

---

## 1. Mission

Port the **"Eye"** face effect from the browser prototype to run natively on the Matrix Portal as the physical presence of the Wet Court AI judge. The eye must feel *alive and reactive*: it drifts its gaze, blinks, dilates its pupil in response to the defendant's voice level, and reacts at the verdict. It should read as "this thing is looking at me and forming an opinion."

**In scope**
- A self-contained `EyeFace` module that renders the animated eye on the matrix at a smooth frame rate.
- The 5 judge personas (palette + motion character), switchable at runtime.
- Reaction to two live inputs: a **phase** (idle / listening / deliberating / verdict) and an **audio level** (0.0–1.0).
- A **standalone demo mode** that fakes those inputs so the eye can be built and tuned with no Spark attached.
- A thin input layer that receives real phase/audio from the DGX Spark (see §5), cleanly separated from rendering.

**Out of scope** (runs on the Spark, not the Portal)
- The LLM judge, TTS, STT, the crime dashboard, the squirt-gun tracking/servo, and the pan/tilt mechs. The Portal only renders the face and consumes state messages.

---

## 2. Hardware & platform

- **Board:** Adafruit Matrix Portal (**confirm M4 vs S3** — see §9; S3 strongly preferred for CPU headroom and built-in WiFi).
- **Panel:** 64×32 RGB LED matrix, HUB75. Landscape orientation (this brief is for the landscape Eye; the portrait HAL variant is a separate task).
- **Firmware:** CircuitPython (latest stable for the board). Verify module/library APIs against the installed version rather than trusting this doc verbatim.
- **Display stack:** `rgbmatrix` + `framebufferio` + `displayio`. Use the Matrix Portal convenience wrapper to avoid hand-wiring pins:

```python
import displayio
from adafruit_matrixportal.matrix import Matrix

displayio.release_displays()
matrix  = Matrix(width=64, height=32, bit_depth=5)   # bit_depth 4–6: gradients vs refresh cost
display = matrix.display                              # a displayio display; use display.root_group
```

Higher `bit_depth` gives smoother iris gradients but costs RAM and refresh rate — treat it as a tunable and note the chosen value.

---

## 3. Reference behavior — how the Eye works

All coordinates below are for the **64×32** panel. In the prototype the eye derives everything from panel geometry, so keep it parameterized (`W`, `H`, `CX=W/2`, `CY=H/2`, `IR = min(W,H)*0.38 ≈ 12.2`) rather than hardcoding.

Per-frame the prototype computes, from a time `t` (seconds, scaled by persona speed) and audio level `a` (0–1):

1. **Gaze drift.** The eye center wanders slowly:
   - `lx = (noise(t*0.25) - 0.5) * (W*0.11)`  → roughly ±3.5 px horizontal
   - `ly = (noise'(t*0.25) - 0.5) * (H*0.11)` → roughly ±1.8 px vertical
   - Eye center `cx = CX + lx`, `cy = CY + ly`. The whole iris+pupil disc moves together over a static sclera (like a real eye).
2. **Pupil dilation.** Pupil radius `dil = 3 + a*4.5` (px). Louder voice → bigger pupil. This is the single most important reactive signal — make it visibly track audio.
3. **Blink.** A cycle `bc = (t*0.7) % 6.0`; openness `open` = 1.0 normally, dipping to ~0.05 during a short blink (~0.5 s) roughly every ~8.5 s. Eyelids are two dark bars from the top and bottom edges whose inner edges sit at `CY ± open*(H*0.47)`; at `open=1` they're off-panel (invisible), during a blink they sweep to center and back.
4. **Iris.** For pixels within `IR` of center (`k = r/IR`):
   - Radial striations: `stri = 0.5 + 0.5*sin(angle*9 + noise(...)*6)`, animated slowly.
   - Color mixes persona **primary → secondary** by `m = 0.35 + stri*0.5*(1-k)` (brighter, more textured toward the center).
   - **Limbal ring:** darken the outer rim — multiply by `limb = 1 - (k-0.82)/0.18` for `k>0.82` (a dark ring at the iris edge; reads as authority).
5. **Pupil.** For `r < dil`, near-black `(4,3,5)`.
6. **Catchlight.** A tiny white specular dot near the upper-left of the pupil (center `(cx-3, cy-3)`, radius ~1.3). Small but it's what makes it look wet/alive — keep it.
7. **Sclera halo.** Just outside the iris, a soft dim glow: `s = 1 - (r-IR)/6`, colored from the persona **dim** tone, fading over ~6 px.
8. **Background / off pixels.** Very dark `(8,6,8)`.

### Personas (palette + character)

Each persona is a color ramp plus a `speed` multiplier and a `fold` (unused by the Eye). Build a lookup from these stops and derive named tones by sampling the ramp:

- `bg = ramp(0.0)`, `dim = ramp(0.16)`, `primary = ramp(0.55)`, `secondary = ramp(0.80)`, `accent = ramp(0.98)`

Stops are `(position, [r,g,b])`, linearly interpolated:

```
The Honorable    speed 0.55  [(0,[6,4,8]),(.25,[40,8,12]),(.5,[110,20,22]),(.72,[190,120,40]),(.9,[214,170,80]),(1,[245,228,170])]
MAGISTRATE.exe   speed 1.25  [(0,[2,6,4]),(.3,[4,40,20]),(.55,[10,120,50]),(.78,[40,220,90]),(1,[170,255,185])]
Cosmic Arbiter   speed 0.40  [(0,[4,4,14]),(.28,[20,12,60]),(.5,[60,20,110]),(.72,[40,110,170]),(.9,[80,200,220]),(1,[200,240,255])]
NULLPOINTER      speed 1.60  [(0,[10,2,16]),(.3,[120,0,90]),(.5,[255,0,170]),(.7,[120,40,200]),(.85,[0,220,255]),(1,[235,255,255])]
Justice Petunia  speed 0.70  [(0,[12,6,2]),(.3,[60,28,4]),(.55,[160,86,8]),(.78,[235,150,40]),(.92,[255,190,90]),(1,[255,238,200])]
```

### Phase reactions

The prototype's Eye reacts to audio directly; the phase-specific drama lived in the trial harness. Implement these phase behaviors on top of the base eye:

| Phase | Eye behavior |
|---|---|
| `idle` | Calm. Slow drift, occasional blinks, pupil at rest (`a≈0`). |
| `listening` | Pupil dilates with incoming `audio`; gaze darts a little more. |
| `deliberating` | *(recommended extension)* Narrow the lids toward `open≈0.5` (suspicion), dart the pupil faster, quicken the drift. |
| `verdict:guilty` | Override to a red strobe (~8–12 Hz) with small random horizontal row shifts (glitch), synced to the squirt-gun fire. |
| `verdict:innocent` | A calm green bloom that eases out over ~2 s. |

Mark which behaviors are faithful to the prototype vs. new; the guilty/innocent overrides mirror the prototype's `renderVerdict`.

---

## 4. Recommended architecture on hardware

**Reality check:** writing all 2048 pixels every frame in pure Python (`bitmap[x,y] = idx`) will run at single-digit FPS. Don't ship that. Two viable paths, in order of preference:

### 4a. Layered `displayio` (recommended)

Let displayio do the compositing in C and keep Python doing as little per frame as possible. Structure the face as a `Group`:

- **Layer 0 — sclera:** a static 64×32 bitmap (dim halo + dark background). Rebuild only on persona change.
- **Layer 1 — iris disc:** a small bitmap (~26×26) containing the striated iris, limbal ring, pupil hole, and catchlight, precomputed **per persona** and cached at a few **pupil-dilation sizes** (e.g. 5 steps). Gaze = move this `TileGrid`'s `.x/.y` each frame (cheap). Dilation = swap to the nearest cached size (or redraw its ~256 px only when the target radius changes by ≥1). Blink lids composite on top.
- **Layer 2 — eyelids:** two dark bars anchored to the top/bottom edges (bitmaps moved via `.y`, or `Rect` from `adafruit_display_shapes`) whose inner edge animates only during a blink. Idle cost ≈ zero.
- **Verdict overlay:** a full-panel bitmap toggled on for the guilty strobe / innocent bloom, so the base layers don't need touching.

This gets you smooth motion because the only per-frame Python work is moving a TileGrid and occasionally swapping/redrawing a tiny bitmap.

### 4b. Full per-pixel with `ulab` (fallback / if you need exact fidelity)

If the layered look isn't faithful enough, compute the whole frame vectorized with `ulab.numpy` (radius/angle/field as arrays) and blit the resulting index array into the bitmap in one shot. Investigate `bitmaptools.arrayblit(...)` for the fast copy — **verify it exists and its signature in the installed CircuitPython**; if not, fall back to a tight assignment loop over only changed pixels (dirty-rect the disc + lids, not the whole panel). Precompute the static `r`, `angle`, and `limb` arrays once and only recompute the pupil/lid overlay each frame.

Start with 4a. Reach for 4b only if the layered version can't reproduce the iris texture or catchlight to your satisfaction.

### Suggested module interface

```python
class EyeFace:
    def __init__(self, display, persona="The Honorable"): ...
    def set_persona(self, name): ...          # rebuild cached iris/sclera bitmaps
    def set_phase(self, phase): ...           # "idle" | "listening" | "deliberating" | "verdict:guilty" | "verdict:innocent"
    def set_audio(self, level): ...           # 0.0–1.0, smoothed internally
    def tick(self, dt): ...                   # advance drift/blink/dilation, update TileGrids; call every frame
```

Keep rendering pure/side-effect-free w.r.t. inputs: the input layer only calls `set_*`, the loop only calls `tick(dt)`.

---

## 5. Inputs from the DGX Spark

Rendering must not block on the network. Poll for messages, update state, keep animating regardless.

- **Transport (S3):** UDP over WiFi is the natural fit (`wifi` + `socketpool`, non-blocking recv). **Serial/UART** is a fine fallback if the Portal is tethered to the Spark.
- **Message schema (suggested):** newline-delimited JSON, small and forgiving:

```json
{"phase": "listening", "audio": 0.42, "persona": "MAGISTRATE.exe"}
```

- `phase`: one of the phase strings above (verdict as `"verdict:guilty"` / `"verdict:innocent"`).
- `audio`: 0.0–1.0 mic envelope, sent at ~20–30 Hz during `listening`.
- `persona` (optional): switch the active judge.
- The Spark should send the **verdict event slightly before** it triggers the squirt-gun servo/pump, so the red strobe and the water land together.

**Standalone demo mode** (no input source): synthesize an audio envelope (a smooth pseudo-noise "speech" signal like the prototype's speech sim) and cycle phases idle → listening → deliberating → verdict on a timer, so the eye is fully developable without the Spark. Select it via a config flag or auto-fallback when no messages arrive for N seconds.

---

## 6. Suggested file layout

```
code.py                 # entry point: init display, wire inputs, run the loop
eye_face.py             # EyeFace class + rendering (the core deliverable)
personas.py             # persona stop tables, ramp(), palette/tone builders
inputs.py               # UDP/serial receiver + demo-mode fake source (same interface)
config.py               # board model, bit_depth, transport choice, demo flag, WiFi creds ref
secrets / settings.toml # WiFi creds (do not hardcode)
```

---

## 7. Milestones & acceptance criteria

1. **Bring-up:** panel lights, correct dimensions/orientation, a static test pattern renders.
2. **Static eye:** one persona's iris + pupil + catchlight + sclera rendered correctly, centered.
3. **Motion:** gaze drift + blink cycle look natural; **measure and report FPS** on the actual board.
4. **Reactivity:** pupil dilation tracks a manually-driven audio value (demo mode).
5. **Personas:** all 5 selectable at runtime; palettes visibly distinct; iris rebuild is quick.
6. **Phases:** listening/deliberating behaviors + guilty red-strobe/glitch and innocent green-bloom overrides.
7. **Inputs:** consumes live phase/audio from the Spark over the chosen transport; falls back to demo mode gracefully.

**Bar to clear:** the animated eye holds a smooth frame rate (target **≥ ~20 FPS**; higher is better) with drift + blink + dilation all active, and never blocks or stutters waiting on input. If the target board can't hit that with the layered approach, document why and where the time goes.

---

## 8. Libraries / dependencies

From the Adafruit CircuitPython Bundle (match the CP version): `adafruit_matrixportal`, `adafruit_display_shapes` (if using `Rect`/`Circle` for lids/pupil), `adafruit_ticks` (timing). Built-in: `displayio`, `rgbmatrix`, `framebufferio`, `bitmaptools`, `ulab` (if available in the build), and `wifi`/`socketpool` on the S3. Confirm `ulab` and `bitmaptools.arrayblit` availability early if you plan to use path 4b.

---

## 9. Open questions to confirm before building

1. **Which board** — Matrix Portal M4 or S3? Drives transport (S3 = WiFi/UDP; M4 = serial/UART) and the realistic FPS ceiling.
2. **Transport** — UDP, USB serial, or UART to the Spark? Pick one for v1.
3. **Panel scan/pitch** — standard 1/16-scan 64×32? Any `bit_depth` preference already known to look good on this panel?
4. **Persona at boot** — default judge, and does the Spark or the Portal own persona selection?
5. **Verdict timing** — does the Spark emit a distinct verdict message, and how far ahead of the squirt should the strobe start?

---

## 10. Getting started

Attach `wet-court-led-lab.html` and open the `eye:(t,a)` function and `PERSONAS` array — they are the exact spec. Begin at milestone 1 with the display bring-up in §2, stub `inputs.py` with the demo source first so the eye can be built in isolation, then layer in reactivity and the real transport. Prefer the layered `displayio` architecture (§4a) from the start; only drop to per-pixel (§4b) if fidelity demands it. Report FPS at each milestone.
