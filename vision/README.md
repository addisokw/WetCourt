# vision

The turret's camera process: a webcam (bolted to the gun, so it moves with the
aim) → person/body-part detection → an annotated video feed and a `/state` JSON
of target points. It's a **non-MCU host process**, separate from the firmware,
talking to the orchestrator over the network (see
[`../docs/hardware-architecture.md`](../docs/hardware-architecture.md)).

**Location-flexible** (a deployment knob, like the orchestrator): runs on the
booth PC for dev and migrates to the DGX Spark (a `dgx-ai-stack/` container) for
production — same code, the webcam and config follow it.

## Status — Phase B, milestones 1–4a

Done: capture + MediaPipe pose, target points, annotated MJPEG feed, `/state`
JSON (m1); orchestrator reverse-proxy + operator panel (m2); **vision-owned
closed-loop targeting** — a proportional servo that streams aim to the
orchestrator, which relays it to the turret only while armed (m3); the
**eye-exclusion safety layer** — a no-fire zone around the eyes, a `fire_ok`
flag, and head-shot operator confirmation (m4a, *computed + shown only — nothing
fires yet*). Still to come: firing on a guilty verdict gated by `fire_ok` (m4b).

## Eye-safety (m4a)

A conservative **eye-exclusion zone** is built from the pose eye points (bounding
box padded by `--eye-pad`, more upward toward the brow). The **impact point** is
the boresight; `fire_ok` requires:

- **chest:** locked (the torso is inherently clear of the eyes);
- **head:** locked **and** operator-confirmed (`/confirm_head`) **and** eyes
  detected **and** the impact (+`--impact-radius` px) clear of the eye zone;
- **forehead** is never offered as a target.

The feed overlays the eye zone (red if the impact would hit it) and a
`FIRE OK` / `NO FIRE` flag. This is conservative by design — a coarse, generous
zone errs toward *not* firing near the face. (FaceMesh would give tighter eye
landmarks; a future precision upgrade.)

## Targeting (m3)

The camera is bolted to the gun, so the gun always points at a fixed **boresight
pixel**. The loop nudges the commanded turret aim so the chosen body part moves
onto that pixel:

```
each frame, if a target part is set and a person is detected:
  err = target_pixel - boresight_pixel
  aim += gain * err        (per axis; integrates to zero error)
  POST aim -> orchestrator  (relayed to the turret only while ARMED)
```

Operator workflow (in the console **Vision** tab): pick a target part, click the
feed to set the boresight, watch the overlay track (gun still still), then
**Arm** to let it drive the turret. **Tune the gains and their sign** on the
real rig so the target converges without oscillating — the sign depends on
servo direction + camera orientation. Disarm stops the gun instantly.

**Tuning persists host-side, like the servo calibrations.** This process holds
gains/tolerance/boresight/target only in memory (seeded from CLI flags), so on
its own it forgets them on every restart. The deliberate flow: tune live in
the console (edits apply immediately), then **Save tuning** — the orchestrator
stores them in `orchestrator/calibration/vision.toml` and re-pushes them to
this process every time it (re)appears on `/health` (orchestrator launch or a
vision restart). The CLI `--gain-*`/`--tolerance` flags are only the pre-seed
defaults; the saved tuning overrides them within seconds of startup. The trial
FSM also acquires the *saved* `target_part` (chest|head) at deliberation, and
the auto-fire dwell rides along in the same file.

## Run (dev, booth PC)

Uses [uv](https://docs.astral.sh/uv/). `uv run` creates the venv, installs the
deps from `pyproject.toml`, and runs — one step:

```sh
cd vision
uv run vision.py            # serves on http://0.0.0.0:8091
# open http://localhost:8091
```

Pass flags through, e.g. `uv run vision.py --camera 1`. (`uv sync` just
creates/updates `.venv` without running.) If no camera is found it serves a
"no camera" test pattern, so the server still comes up.

On first run it downloads the MediaPipe pose model (~3 MB) to `models/`
(gitignored), cached thereafter. Point `--model` at a pre-downloaded `.task` for
an offline booth.

## Endpoints

| Route | What |
|---|---|
| `GET /` | HTML page embedding the live feed |
| `GET /feed` | annotated MJPEG stream |
| `GET /state` | latest detection JSON (target pixels, boresight, aim, locked, eyes) |
| `POST /target` | `{"part":"none"\|"chest"\|"head"}` — choose what to track (clears any person selection: fresh acquisition = fresh subject) |
| `POST /aimpoint` | `{"x","y"}` — click-to-aim: one-shot open-loop nudge putting the clicked pixel on the boresight (gain-converted, ±20°/click, stops the body servo; the held aim streams ~0.8s with `fire_ok:false`) |
| `POST /select` | `{"x","y"}` — click-to-track: select the person track at/nearest the pixel and servo onto *them*; `{"clear":true}` deselects. A selected-but-lost track HOLDS the gun (never migrates to a bystander) |
| `POST /boresight` | `{"x":int,"y":int}` — set the boresight pixel |
| `POST /gains` | `{gain_pan?,gain_tilt?,tolerance?}` — live-tune the servo (in-memory; Save in the console to persist) |
| `POST /center` | stop tracking, reset aim integrator, clear selection (recovery) |
| `GET /health` | `ok` |

The operator drives `/target`, `/aimpoint`, `/select`, and `/boresight` through
the orchestrator (`/vision/*`) so the console stays same-origin; aim is
streamed to the orchestrator's `/vision/aim` and gated by `/vision/arm`.

Detection runs multi-person (`--max-poses`, default 4) through a greedy
nearest-centroid tracker with stable integer ids (positional identity —
someone who leaves and returns gets a new id; the operator re-clicks). With no
selection, the servo targets the track nearest the boresight.

`/state` shape:

```json
{ "ts": 1750000000.0, "frame": {"w":640,"h":480}, "person": true,
  "targets": {"chest":[320,300], "shoulders":[[280,240],[360,240]], "head":[320,160]},
  "eyes": [[305,150],[335,150]],
  "tracks": [{"id":1, "center":[320,300], "box":[250,120,390,470]}],
  "selected": 1, "selected_visible": true }
```

## Config (flags or `BOOTH_VISION_*` env)

| Flag | Env | Default |
|---|---|---|
| `--camera` | `BOOTH_VISION_CAMERA` | `0` |
| `--host` | `BOOTH_VISION_HOST` | `0.0.0.0` |
| `--port` | `BOOTH_VISION_PORT` | `8091` |
| `--width` / `--height` | `BOOTH_VISION_WIDTH/HEIGHT` | `640` / `480` |
| `--quality` | `BOOTH_VISION_QUALITY` | `80` |
| `--orchestrator` | `BOOTH_VISION_ORCH` | `http://localhost:8080` |
| `--max-poses` | `BOOTH_VISION_MAX_POSES` | `4` (people detected per frame) |
| `--gain-pan` / `--gain-tilt` | `BOOTH_VISION_GAIN_PAN/TILT` | `0.025` (tune live; sign matters) |
| `--tolerance` | `BOOTH_VISION_TOL` | `12` (px for LOCKED) |
| `--eye-pad` | `BOOTH_VISION_EYE_PAD` | `0.8` (eye-zone size; bigger = safer) |
| `--impact-radius` | `BOOTH_VISION_IMPACT_R` | `25` (px impact uncertainty) |

## Safety note

This milestone only **senses** — it never moves the turret or fires. The chest
point is the default target; the head point is drawn amber and the eyes red, but
auto-aim and the eye-exclusion no-fire rule come in later milestones, where head
shots require operator confirmation and the forehead is never auto-targeted.
