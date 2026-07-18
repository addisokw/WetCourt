# Overnight batch — progress log

Branch: `feat/overnight-batch` off `main` (`864d3bc`). NOT deployed.
Implements `docs/overnight-plan.md`. Order: 1→2→3→4→7→6→5.

Legend: ✅ done · 🚧 partial · ⛔ blocked · 🔧 HARDWARE PASS NEEDED

---

## F1 — Cross-exam countdown resets after lawyer call ends ✅
- `transitions.rs`: `(CrossAwaitingAnswer{paused_remaining:Some(_)}, LawyerCallEnded)` now resets `deadline` to the full `cross_examination.answer_window_secs` and emits `PhaseDeadline{deadline_ms = window}` instead of resuming the frozen remainder. Cross window only; initial-plea twin left resuming-with-remainder as before.
- Tests: added `cross_answer_window_resets_to_full_on_call_end` (asserts ~full window, not the 4s remainder, + the PhaseDeadline command). `cargo test`: 143 passed.
- No config gate, no hardware dependency.

## F2 — Idle "press button to stand trial" big text ✅
- `CaseView.tsx` `StateInstruction` idle Match: swapped the small `instruction` line for `instruction big` (existing pulsing style, `app.css:410`) with copy "PRESS THE BUTTON TO STAND TRIAL".
- Note: `CaseContent` is reused in the operator console (`App.tsx:191`), so the big text also appears in the embedded console preview when idle — acceptable.
- `dist/` is gitignored (built in the orchestrator Docker `--build` step), so only the source change is committed. `npm run build` green.
- 🔧 minor: kiosk eyeball to confirm sizing on the real monitor (low risk).

## F3 — "Your lawyer is calling — pick up the phone" overlay during cross ✅
- Backend: new `DisplayEvent::LawyerCalling { on }` (`events.rs`). Emitted `on:true` at the ring site in `state_machine/mod.rs` (when entering cross with lawyer enabled + no active call); `on:false` on real pickup (`Event::LawyerCallStarted`) or when the cross window closes without a call (`prev == cross_answer && now != cross_answer`). Auto-forwards to the monitor via the existing `Command::Display` path.
- Frontend: new `lawyerCalling` signal + `lawyer_calling` handler in `ws.ts` (cleared on reset/idle); full-screen overlay in `CaseView.tsx` (ringing ☎, "YOUR LAWYER IS CALLING / Pick up the phone"); CSS in `app.css`.
- Tests: added `lawyer_calling_overlay_emitted_on_ring_and_cleared_on_pickup` (drives a trial into the cross answer window with lawyer enabled, asserts on:true at ring and on:false at pickup). `cargo test`: 144 passed. `npm run build` green.
- 🔧 HARDWARE PASS NEEDED: confirm the overlay actually shows on a live ring and clears on pickup / window close (no stuck overlay).

## F4 — Runtime-configurable receipt coupons ✅
- `report.rs`: new `CouponCopy` struct + hardcoded "Dewey, Soakem & Howe" copy (headline, 6 rotating taglines, footer) + `random_coupon()`. `render()` stays deterministic — it only renders `opts.coupon: Option<CouponCopy>` (default `None`). New `coupon()` helper (inverse headline bar, wrapped tagline, bold footer) prints at the bottom, where the disabled footer was.
- `service.rs`: `roll_coupon(frequency)` does the random roll ONCE per trial (both keepsake copies match) — "off" | "rare"(1/6) | "sometimes"(1/3) | "always"; unknown → off.
- Config: `[printer] coupon_frequency` (`#[serde(default)]` = "off"), in config.toml + config.dev.toml. Runtime-switchable via `--restart` (bind-mounted config, no rebuild). NOTE: a live operator-console dropdown was NOT added (stretch goal) — switching is config+restart.
- Custom/operator prints don't get coupons (only the trial keepsake path sets `opts.coupon`) — intended.
- Tests: `coupon_present_only_when_opts_carry_one` (absent by default, present with a coupon, adds bytes). Existing snapshot tests unaffected (render stays deterministic). `cargo test`: 145 passed.

## F7 — Neck droop on lawyer call (gated OFF) ✅ code / 🔧 hardware
- `display/mod.rs`: `lawyer_event` now calls `drive_neck_droop(&s, true)` on `call_started` and `(.., false)` on `call_ended`. Sends a RAW targeted `MaintenanceCommand{JudgeNeck, Aim{pan:center, tilt:2167}}` (droop) / `{tilt:home}` (restore). Center pan + home tilt come from `aim_to_raw(0.0, 0.0)` on the judge_neck calibration (no hardcoded pan). Raw tilt 2167 bypasses the degree clamp (which tops at 1967) to reach firmware `TILT_DROOP`; pan centered to satisfy the firmware droop-zone pan-lock.
- Config: `[lawyer] neck_droop_on_call` (`#[serde(default)]` = false) in config.toml + config.dev.toml. `AppState.lawyer_neck_droop_on_call` set from it in main.rs.
- Extracted the pure decision `neck_droop_command()` and tested it (`f7_tests`): disabled→None, droop→tilt 2167 at center pan, restore→home tilt. `cargo test`: 146 passed.
- Open notes: does NOT call `targeting.note_aim` for the droop (the raw pose is outside the degree range); targeting only drives the neck from Deliberating, and calls occur in the plea/cross windows, so contention is unlikely — but verify the neck hands back cleanly to targeting if a call ends right before deliberation.
- 🔧 HARDWARE PASS NEEDED: this is real motion near the range that previously snapped the tilt mount. Verify slew is gentle, pan stays centered, the mount tolerates sustained full droop, and restore returns to level. Ships OFF (`neck_droop_on_call = false`).

## F6 — Idle attract mode (gated OFF) ✅ code / 🔧 hardware
- `state_machine/mod.rs`: `Runtime` gained `next_attract: Option<Instant>` + `attract_idx`. In `handle()`, while `State::Idle` and `attract.enabled`, arm `next_attract = now + idle_secs_before`; each Tick past it fires `fire_attract_beat()` and re-arms at `+ interval_secs`. Leaving idle clears `next_attract` (never fires during a trial).
- `fire_attract_beat()`: dispatches `Command::Speak(<rotating Wettington line>)` (10-line `ATTRACT_LINES` pool, spoken in the active persona's voice) + a small alternating neck sweep via a new `TargetingController::nudge_neck(pan_deg, tilt_deg)` (±8°, best-effort — needs the targeting controller; the neck's face-mirror gives the eyes a cue too). No neck via the plain FSM path (that routes to the turret).
- Config: `[attract] enabled=false, idle_secs_before=20, interval_secs=45` (new `AttractConfig`, `#[serde(default)]`) in both TOMLs. `mk_cfg`/transitions test configs updated.
- Tests: `attract_beats_only_when_enabled_and_idle` — enabled+idle fires exactly one on-pool line per Tick (0/0 timing), no beats during a trial, disabled never speaks. `cargo test`: 147 passed.
- Notes: TTS goes to the browser audio kiosk — silent if none connected (fine). Uses a rotating counter, not RNG, so beats are deterministic/testable.
- 🔧 HARDWARE PASS NEEDED: confirm the ±8° neck sweep amplitude/slew is gentle and the 45s TTS cadence isn't annoying; confirm attract stops instantly when a trial starts. Ships OFF (`attract.enabled = false`).
