# Overnight batch — progress log

Branch: `feat/overnight-batch` off `main` (`864d3bc`). NOT deployed.
Implements `docs/overnight-plan.md`. Order: 1→2→3→4→7→6→5.

Legend: ✅ done · 🚧 partial · ⛔ blocked · 🔧 HARDWARE PASS NEEDED

## SUMMARY — all 7 features implemented, tests green, nothing deployed

| # | Feature | Commit | Config flag (default) | Needs HW pass |
|---|---------|--------|-----------------------|---------------|
| F1 | Cross countdown reset on call-end | `834ce5b` | — | no |
| F2 | Idle "press button" big text | `9d7b9b6` | — | eyeball only |
| F3 | "Lawyer calling" overlay | `21b811c` | — | live-call check |
| F4 | Receipt coupons | `24336e7` | `printer.coupon_frequency="off"` | no |
| F7 | Neck droop on call | `e5adbc6` | `lawyer.neck_droop_on_call=false` | **yes (motion)** |
| F6 | Idle attract mode | `83f5c05` | `attract.enabled=false` | **yes (motion+audio)** |
| F5 | Lawyer audio on speaker | `5a35ee1` | `lawyer.speaker_playback=false` + counsel `audio.speaker_playback=false` | **yes (live call)** |

Final state: `cargo build` 0 errors, `cargo test` 147 passed, `npm run build` green, all 4 edited TOMLs parse. All motion/audio features ship OFF. **Deploy is a human step** (`deploy_spark.sh --build`, Rust changed) followed by the hardware passes noted per-feature below.

## FOLLOW-UP — live operator-console toggles (F4/F5/F6)

Added after the batch, same branch. Config seeds the startup value; the console now flips them live (no restart), following the existing cross-exam toggle pattern (`AtomicBool`/`AtomicU8` + `/operator/*` GET+POST + a panel control).
- **F6 attract** — checkbox in the Judge's Mind tab (`/operator/attract`). Runtime reads an `Arc<AtomicBool>` instead of `cfg.attract.enabled`.
- **F4 coupons** — dropdown (off/rare/sometimes/always) in the Print tab (`/operator/coupons`). Print service reads an `Arc<AtomicU8>` fresh per receipt; `coupon_level`/`coupon_level_str` map string↔level.
- **F5 speaker audio** — checkbox in the Lawyer tab (`/operator/lawyer_speaker`), gates the orchestrator side live. Counsel's tee flag stays config (leave it on; the orchestrator gate is the live switch). 
- F7 neck droop deliberately left config-only (motion safety — enable via config after its hardware pass).
- `cargo build` 0 errors, `cargo test` 147 passed, `npm run build` green.

## POST-BATCH — acquittal keepsake photo ("THE VINDICATED")

Completes the capture-all-verdicts feature. Acquittals were being captured to disk (verified live on the Spark: case 64 had 8 valid JPEGs) and printed, but the receipt's photo block was still `if rec.guilty`, so the acquittal's captured still never printed. Now `render()` prints the keepsake photo for EVERY verdict — guilty under "-- MOMENT OF JUSTICE --", acquitted under "-- THE VINDICATED --" (generalized `moment_of_justice` → `keepsake_photo(header)`). Test: `both_verdicts_render_the_keepsake_photo` (both headers + photo block present, neither cross-contaminates). `cargo test` 148.

## POST-BATCH — press-to-record plea/answer button

Fix for a real-world confusion: the mic auto-opened when the plea window opened, so people who pressed the button to "start" actually *ended* their plea before speaking. Now the **first press starts recording, the second press ends it** — matching the on-screen "Press the button to begin… / Press again to end" prompts (which were already written for this).
- Added a `recording: bool` to `AwaitingPlea` and `CrossAwaitingAnswer`; the window opens with `recording:false` (no capture), first `DefendantButton` emits `StartPleaRecording{record:true}` + flips the prompt, second press flushes. `StartPleaRecording` gained a `record` flag; the frontend only starts the mic when `record:true`.
- Applies to **both** the plea window and the cross-examination answer window (same mic mechanic).
- Tests: `first_press_starts_recording_second_press_closes_the_plea_window` and the cross equivalent. `cargo test` 147, `npm run build` green. No config gate (behavior change; revert = git).

Detail per feature:

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

## F5 — Lawyer audio over the primary speaker + phone effect (gated OFF) ✅ code / 🔧 hardware
Full pipeline implemented, gated off at both ends (nothing streams unless BOTH flags on):
- **Counsel** (`crates/counsel`): `queue_pcm24` now taps the exact phone-colored 8 kHz signal the handset hears (µ-law round-trip: `g711::decode(g711::encode(down))`). `synth_to_queue` accumulates the whole lawyer line and, if `[audio] speaker_playback`, fire-and-forget POSTs it (s16le 8 kHz) to the orchestrator `/lawyer/audio`. One burst per line → the booth trails the handset by ~a line length (documented tradeoff; a chunked/streaming version is the future refinement).
- **Orchestrator**: new `POST /lawyer/audio` (`display/mod.rs`) — gated by `[lawyer] speaker_playback`; fans the PCM to the speaker kiosk as a `DisplayEvent::LawyerAudio{format}` header + a `DisplayMessage::Binary`. New event variant in `events.rs`; `AppState.lawyer_speaker_playback` from main.rs.
- **Frontend**: `ws.ts` handles `lawyer_audio` → `setPhoneRoute(true)` + start a session; `tts_audio` now sets `setPhoneRoute(false)`. `audio.ts` routes phone frames through a lazy ~300–3400 Hz band-pass (instead of the robot worklet) and builds the buffer at 8 kHz (Web Audio resamples → correct pitch; the low rate is itself part of the timbre). Judge TTS path untouched.
- Config: `[lawyer] speaker_playback=false` (orchestrator, both TOMLs) + `[audio] speaker_playback=false` (counsel, both TOMLs). `cargo build`+`cargo test` (147) + `npm run build` all green.
- No automated test: it's real-time cross-crate audio with no unit surface (the µ-law transform lives in the already-present `g711`). 
- 🔧 HARDWARE PASS NEEDED (whole feature — cannot be exercised without a live call). MANUAL TEST: set both `speaker_playback = true`, restart orchestrator + counsel, place a call during cross-exam, and confirm: (1) the lawyer's voice comes over the booth speaker with a tinny phone timbre, (2) it trails the handset by ~a line, (3) the judge's own TTS afterward sounds normal (robot voice, not phone-filtered), (4) no audio-token contention glitches. If the ~line-length lag is too much, switch counsel to chunked POSTs.
