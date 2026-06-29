# vision

The turret's camera process: a webcam (bolted to the gun, so it moves with the
aim) → person/body-part detection → an annotated video feed and a `/state` JSON
of target points. It's a **non-MCU host process**, separate from the firmware,
talking to the orchestrator over the network (see
[`../docs/hardware-architecture.md`](../docs/hardware-architecture.md)).

**Location-flexible** (a deployment knob, like the orchestrator): runs on the
booth PC for dev and migrates to the DGX Spark (a `dgx-ai-stack/` container) for
production — same code, the webcam and config follow it.

## Status — Phase B, milestone 1 (sensing + feed)

Done here: capture + MediaPipe pose, target points (chest, shoulders, head, rough
eyes), annotated MJPEG feed, `/state` JSON. Still to come: orchestrator
integration + operator panel, closed-loop AIM targeting, and the eye-exclusion
safety zone (precise FaceMesh eyes) before any auto-fire.

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
| `GET /state` | latest detection JSON (target pixels, frame size, eyes) |
| `GET /health` | `ok` |

`/state` shape:

```json
{ "ts": 1750000000.0, "frame": {"w":640,"h":480}, "person": true,
  "targets": {"chest":[320,300], "shoulders":[[280,240],[360,240]], "head":[320,160]},
  "eyes": [[305,150],[335,150]] }
```

## Config (flags or `BOOTH_VISION_*` env)

| Flag | Env | Default |
|---|---|---|
| `--camera` | `BOOTH_VISION_CAMERA` | `0` |
| `--host` | `BOOTH_VISION_HOST` | `0.0.0.0` |
| `--port` | `BOOTH_VISION_PORT` | `8091` |
| `--width` / `--height` | `BOOTH_VISION_WIDTH/HEIGHT` | `640` / `480` |
| `--quality` | `BOOTH_VISION_QUALITY` | `80` |

## Safety note

This milestone only **senses** — it never moves the turret or fires. The chest
point is the default target; the head point is drawn amber and the eyes red, but
auto-aim and the eye-exclusion no-fire rule come in later milestones, where head
shots require operator confirmation and the forehead is never auto-targeted.
