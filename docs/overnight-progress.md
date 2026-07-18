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
