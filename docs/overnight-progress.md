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
