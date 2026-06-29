# Turret + vision — roadmap & state

Working notes for the squirt-gun turret and its vision targeting. Captures what's
done and the detailed design for what's next, so work can resume without
re-deriving it.

## Where things are (2026-06-29)

**Phase A — hardware (done, on `main`).**
- `firmware/turret/` (NanoC6 + 8-Servos, `AIM`) and `firmware/squirt/` (NanoC6 +
  relay, `FIRE`) — split into two boards because the NanoC6 has no spare GPIO
  beyond the servo I2C bus. Servos confirmed working on the rig.
- Orchestrator multi-device registry: `HELLO <role>` handshake, per-role routing,
  per-connection ack matching (`orchestrator/src/hardware/tcp.rs`). Roles:
  `ai_judge`, `gavel`, `turret`, `squirt`. Calibration in
  `orchestrator/calibration/*.toml` (turret pan/tilt → servo µs, `invert=true`).

**Phase B — vision (m1–m4a done, on `main`).**
- `vision/` Python (uv project): webcam → MediaPipe **Tasks** PoseLandmarker →
  target points; annotated **MJPEG feed** + `/state` JSON. (Legacy `solutions`
  API is gone in mediapipe 0.10.x — uses Tasks + an auto-downloaded `.task`.)
- Orchestrator **reverse-proxies** vision at `/vision/*` (same-origin, tunnel-ok).
- **Closed-loop targeting (vision-owned servo):** each frame
  `err = target_px − boresight_px`, integrate aim, stream to `/vision/aim`,
  relayed to the turret **only while armed**. Console **Vision** tab: target
  select, click-to-set boresight, arm/disarm, **Recenter**, **live gain tuning**.
  Gain/sign tuned live on the rig. Per-step clamp (8°/frame) softens overshoot.
- **m4a eye-safety (compute + show only, NOTHING FIRES):** conservative
  eye-exclusion zone from pose eye points; `fire_ok` = chest→locked,
  head→locked + operator-confirmed + eyes-clear; forehead never offered. Overlay
  shows the zone + `FIRE OK`/`NO FIRE`. Endpoints `/vision/confirm_head`,
  fields `fire_ok`/`eye_zone`/`eye_clear`/`head_confirm` in `/state`.

## Next: m4b — fire on a guilty verdict, gated by `fire_ok`

Wire the squirt to actually fire, safely. The FSM already fires on guilty
(`sentence_commands()` in `orchestrator/src/state_machine/transitions.rs` emits
`HardwareCommand::Fire` → registry routes to the `squirt` role). m4b gates that.

Design:
1. **Vision reports `fire_ok` to the orchestrator continuously.** Easiest: add
   `fire_ok` (and `locked`) to the existing aim POST body (`/vision/aim`,
   ~15 Hz). The orchestrator stores it in a shared `Arc<AtomicBool>` +
   last-update `Instant` (treat stale > ~300 ms as not-ok). Vision only POSTs aim
   when a target is set + a person is detected, so "no recent update" ⇒ not safe.
2. **Gate the trial `Fire`.** Chokepoint: the `Command::Hardware → HardwareCommand`
   adapter in `orchestrator/src/main.rs` (or `role_for`/router in `tcp.rs`).
   Rule: **if `targeting_armed`**, only forward `Fire` to the squirt when the
   stored `fire_ok` is fresh + true; otherwise **suppress the wire send but
   synthesize a `HardwareAck`** so `ExecutingSentence` still advances (mirrors the
   existing absent-role handling — never stall the trial). **If not armed**, fire
   as today (legacy; operator owns aim). Log/display the suppression reason.
3. **AppState** gains `vision_fire_ok: Arc<AtomicBool>` (+ a timestamp, e.g.
   `Arc<AtomicU64>` ms). Plumb the gate state into whichever task routes `Fire`.
4. **Console:** the Vision panel already shows `fire_ok`; add a small indicator
   on the operator tab that a shot was **held for safety** when it happens.
5. **Verify:** mock a guilty verdict with targeting armed + `fire_ok` toggled —
   confirm the squirt only fires when `fire_ok`, and the FSM advances either way
   (no 60 s stall). Socket test like the m3 relay test (fake squirt board).

Risks/notes: keep the gate fail-safe (stale/unknown ⇒ no fire when armed). The
turret only does `AIM`; the squirt only does `FIRE` — no role conflict between
vision aiming and the FSM firing.

## After m4b — Phase C (vision as a trial asset)

- **Firing still for the printed report:** on `FIRE`, the orchestrator calls a
  vision `GRAB` to capture the annotated still (defendant + crosshair) for the
  trial record. Feeds the **thermal-printer** keepsake (separate subsystem; note:
  there is a private thermal-printer worktree/branch — see project memory — that
  must NOT be pushed to this public repo).
- **Audience "terminator-cam":** reuse the read-only multi-client `/ws/view` +
  standalone-page pattern (`/face`, `/case` already exist) → a `/turret-cam` page
  with a heavier HUD overlay. Public, read-only, no controls.

## Future / deferred

- **FaceMesh eye landmarks** for a tighter eye-exclusion zone (m4a uses coarse
  pose eyes, padded generously — conservative but blunt). Adds a second model;
  watch CPU on the booth PC.
- Reduced pump pressure for head shots (if the hardware exposes it).
- Per-verdict squirt intensity (currently fixed `[squirt] duration_ms`).

## How to run / verify
- Vision (booth PC): `cd vision && uv run vision.py` → `http://localhost:8091`.
- Orchestrator: `cargo run -- --config config.dev.toml` (or the homelab compose).
- After frontend changes: `cd orchestrator/frontend && npm run build`, restart
  the orchestrator (debug serves `frontend/dist` from disk; release embeds it).
- Tests: `cargo test -p booth` (39). The turret registry was verified end-to-end
  against a fake-device socket; loop dynamics + servo/relay are hardware-only.
