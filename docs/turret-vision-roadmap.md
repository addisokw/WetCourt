# Turret + vision — roadmap & state

Working notes for the squirt-gun turret and its vision targeting. Captures what's
done and the detailed design for what's next, so work can resume without
re-deriving it.

## Where things are (2026-06-29, m4b done)

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

## m4b — fire on a guilty verdict, gated by `fire_ok` (done, on `main`)

The squirt fires on a guilty verdict, eye-safety-gated. As built:
1. **Vision reports `fire_ok` (+ `locked`) on the aim POST body** (`/vision/aim`,
   ~15 Hz), only while actively tracking (target set + person detected).
2. **`VisionFireGate`** (`orchestrator/src/hardware/gate.rs`) stores the latest
   `fire_ok` + a process-monotonic `now_ms()` timestamp; `fire_allowed(armed)`
   is the fail-safe decision — transparent when disarmed, else requires a fresh
   (`<= FIRE_OK_STALE_MS`, 300 ms) true `fire_ok`. Pure `decide()` helper is
   unit-tested (armed/disarmed × fresh/stale × ok/not). The aim handler calls
   `record()` on *every* frame (even disarmed) so the gate is fresh the instant
   the operator arms.
3. **Gate chokepoint = the `Command::Hardware` adapter in `main.rs`.** A trial
   `Fire` while armed + not-allowed is **suppressed on the wire but a
   `HardwareAck` is synthesized** so `ExecutingSentence` advances (never stalls —
   mirrors absent-role handling), and a `DisplayEvent::FireHeld` is broadcast.
   Disarmed ⇒ fire as today (operator owns aim).
4. **Console:** amber "Shot held for safety" banner on the operator tab
   (`App.tsx`, `fire_held` event → `fireHeldReason` signal, cleared at idle).
5. **Tests:** gate logic covered by `gate.rs` unit tests (`cargo test -p booth`,
   now 45). End-to-end fire on the real squirt board is hardware-only.

Fail-safe invariant: stale/unknown/false `fire_ok` ⇒ **no fire** when armed. The
turret only does `AIM`; the squirt only does `FIRE` — no role conflict between
vision aiming and the FSM firing.

## Next: Phase C (vision as a trial asset)

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
- Tests: `cargo test -p booth` (45). The turret registry was verified end-to-end
  against a fake-device socket; loop dynamics + servo/relay are hardware-only.
