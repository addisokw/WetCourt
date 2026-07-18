# Overnight Feature Implementation Plan

Work order for an autonomous run. Implement every feature below. All file:line
references were captured from the current tree (2026-07-18) — verify they still
resolve before editing; treat them as starting points, not gospel.

---

## GLOBAL GUARDRAILS (read first)

1. **Branch.** Create `feat/overnight-batch` off `main`. Do NOT work on `main`.
2. **One commit per feature**, in the order below. Each commit message: what +
   why + whether it needs a hardware pass. Use the repo's commit trailers
   (Co-Authored-By + Claude-Session lines, as in recent history).
3. **Green before every commit:** from `orchestrator/`, `cargo build` and
   `cargo test` must pass; for frontend changes, `cd orchestrator/frontend &&
   npm run build` must pass. Never commit a red tree.
4. **DO NOT DEPLOY.** No `deploy_spark.sh`, no ssh to the Spark, no restart of
   any booth service. Leave everything committed and undeployed for review.
5. **Hardware items ship OFF.** Features 5, 6, 7 touch audio/motion that cannot
   be verified without the booth. Each MUST be gated behind a config flag that
   defaults to OFF/false, so nothing changes booth behavior until a human
   enables it during a babysat session.
6. **Progress log.** Maintain `docs/overnight-progress.md`: one section per
   feature — status (done/partial/blocked), what was implemented, tests added,
   and an explicit "HARDWARE PASS NEEDED: <what to check>" line for 3,5,6,7.
   Append; never overwrite.
7. If a feature is genuinely blocked, commit what compiles, write the blocker in
   the progress log, and move on — do not stall the whole batch.

### Verify commands
```
cd orchestrator && cargo build && cargo test
cd orchestrator/frontend && npm run build
```

### Implementation order
Do the safe Tier-1 items first (1→2→3→4), then Tier-2/3 (7→6→5). Rationale:
front-load the fully-verifiable work; leave the riskiest (audio) last so a
blocker there doesn't strand the easy wins.

---

## FEATURE 1 — Cross-exam countdown resets after the lawyer call ends

**Objective.** When a lawyer call ends mid cross-examination, restart the answer
window at the FULL duration instead of resuming the frozen remainder.

**Current behavior.** `transitions.rs:261-276`, arm
`(CrossAwaitingAnswer{paused_remaining:Some(rem)}, LawyerCallEnded)` sets
`deadline = Instant::now() + rem` and emits `PhaseDeadline{deadline_ms: rem}`.

**Change.** Reset to the full window:
- `deadline = Instant::now() + Duration::from_secs(cfg.cross_examination.answer_window_secs)`
- emit `PhaseDeadline{phase:"cross_answer", deadline_ms: answer_window_secs*1000}`

**Scope.** Cross window ONLY. Leave the initial-plea twin
(`transitions.rs:143-157`) resuming-with-remainder as it is today.

**Acceptance.**
- New unit test in `transitions.rs` tests: enter `CrossAwaitingAnswer`, fire
  `LawyerCallStarted` (pauses), fire `LawyerCallEnded`, assert the new deadline
  is ~`answer_window_secs` out (not the smaller remainder) and
  `paused_remaining` is `None`.
- `cargo test` green.

**Risk.** None. Pure logic, fully testable.

---

## FEATURE 2 — "Press the button to start trial" big idle text

**Objective.** Replace the small idle line on the case monitor with large,
attention-grabbing copy prompting the defendant to start.

**Hook.** `orchestrator/frontend/src/CaseView.tsx:27-29`, the `StateInstruction`
idle `<Match when={currentState() === 'idle' || currentState() === 'connected'}>`.
Currently: `<p class="instruction">Step up. The court will hear your case.</p>`.

**Change.**
- Use the existing large pulsing style: `class="instruction big"` (CSS
  `app.css:410-419`, `instruction-pulse` keyframes).
- Copy: **"PRESS THE BUTTON TO STAND TRIAL"** (edit freely — see Draft Content).

**Note.** `CaseContent` is reused embedded in the operator console
(`App.tsx:191`, `.case-embed`); the big text will appear there too. Acceptable.

**Acceptance.** `npm run build` green. (Visual confirmation is a kiosk eyeball —
note it in the progress log, low risk.)

**Risk.** None (software). No config gate needed.

---

## FEATURE 3 — "Your lawyer is calling — pick up the phone" overlay during cross

**Objective.** When the judge rings counsel during cross-examination, show a
full-screen overlay on the case monitor telling the defendant to answer the
phone. Clear it on pickup or when the cross window closes.

**Backend.**
- Add `DisplayEvent::LawyerCalling { on: bool }` to `display/events.rs` (near
  `ClockPaused` :66 / `PhaseDeadline` :62). It auto-forwards to the monitor via
  the existing `Command::Display` path (`mod.rs:355-383` → `main.rs:319`).
- Emit `on:true` at the ring site `state_machine/mod.rs:216-225`, right where
  `bridge.ring(...)` is called (dispatch
  `Command::Display(DisplayEvent::LawyerCalling{on:true})`).
- Emit `on:false` when the call connects — the `LawyerCallStarted` handling
  (`transitions.rs:243-260`) — AND when cross ends without a call (window
  flush/early-close / leaving `CrossAwaitingAnswer`). Ensure it always clears so
  the overlay can't get stuck on.

**Frontend.**
- `ws.ts`: handle `lawyer_calling` → a new signal `setLawyerCalling(ev.on)`.
  Also clear it on `reset`/`idle` (ws.ts:183-208) as a safety net.
- `CaseView.tsx`: add a full-screen overlay in `CaseContent` (sibling of the
  blocks), gated on `lawyerCalling()`. Style like the theater overlay rules
  (`app.css:288-302`).
- Copy: **"YOUR LAWYER IS CALLING"** / **"Pick up the phone."**

**Config gate.** Not required (it only shows when a call is actually rung, which
already requires the lawyer integration to be enabled). No default-behavior
change when lawyer is off.

**Acceptance.** `cargo build` + `npm run build` green. Add a Rust test that the
ring path emits `LawyerCalling{on:true}` and that `LawyerCallStarted` emits
`{on:false}`. HARDWARE PASS NEEDED: confirm the overlay actually appears/clears
on a live call.

**Risk.** Low. Software, but the on/off lifecycle needs care — verify no stuck
overlay.

---

## FEATURE 4 — Random "bad lawyer" coupons on receipts (runtime-configurable)

**Objective.** Occasionally print a coupon advertising the counsel personas
("Dewey, Soakem & Howe") at the bottom of the trial receipt. Frequency must be
switchable at runtime via config (no rebuild).

**Backend.**
- Add a `coupon()` helper in `printer/report.rs` alongside the disabled
  `footer()` (~:241-253); call it from `render()` at :97 (where `footer` is
  commented out), BEFORE the final `feed(2).cut()` at :99. Mirror existing
  section helpers (`Align::Center`, `bold`, `Font::B` flavor lines, `asciify()`
  on ALL text, an inverse bar like `verdict()` :180-184 for the coupon header).
- Keep `render()` DETERMINISTIC: pass `coupon: Option<CouponCopy>` via
  `ReportOpts` (:27-42). The random roll + copy pick happens in the LIVE caller
  (the printer service render path that builds `ReportOpts` for a `PrintJob`),
  NOT inside `render()`, so the snapshot tests stay stable.
- Coupon copy: hardcode a small table in the orchestrator (the printer cannot
  depend on the counsel crate). Use the Draft Content below.

**Config (runtime-switchable).**
- Add `[printer] coupon_frequency = "off"` with `#[serde(default)]`. Values:
  `"off"` | `"rare"` (~1 in 6) | `"sometimes"` (~1 in 3) | `"always"`.
  Reloadable via `deploy_spark.sh --restart` (config is bind-mounted; no
  rebuild) — that satisfies "switch during runtime."
- STRETCH (only if cheap): expose the same setting as a dropdown in the operator
  console Print tab so it's switchable without a restart. If it balloons scope,
  skip it and note in the progress log.
- Default `"off"` so behavior is unchanged until enabled.

**Randomness.** Use `rand` (add to `orchestrator/Cargo.toml` if absent). One roll
per printed trial; if two keepsake copies are printed, the same coupon appears on
both (roll once per trial, not per copy).

**Acceptance.** `cargo test` green (existing `renders_both_outcomes` and
snapshot tests must still pass — that's why the roll lives outside `render()`).
Add a test: `render()` with `coupon: Some(..)` includes the coupon text; with
`None` it does not and output is unchanged from today.

**Risk.** Low. Watch the deterministic-test constraint.

---

## FEATURE 7 — Judge neck "power down" droop while on the lawyer call

**Objective.** When a lawyer call is active, droop the judge's neck to full
"powered down" tilt; restore when the call ends. Config-gated, default OFF.

**Hook.** `/lawyer/event` handler `display/mod.rs:632-644` (`AppState` already
exposes `maint_cmd_tx` at :81):
- On `call_started` (gate on the new config flag): send a **RAW** targeted
  command — `MaintenanceCommand{ target: Role::JudgeNeck,
  cmd: HardwareCommand::Aim{ pan: <center raw ~1583>, tilt: 2167 }, reply: None }`.
  MUST be raw: the degrees→raw path clamps tilt at 1967 (`calibration/mod.rs:64`),
  so it can't reach the firmware's `TILT_DROOP=2167`. Pan MUST be centered — the
  firmware droop-zone pan-lock (`firmware/judge-neck/main.py:157-166`) freezes
  pan while `tilt > TILT_SAFE(1967)`.
- On `call_ended`: restore — send `Aim{ pan: center, tilt: 1500 }` (BOOT_TILT
  home).

**Config.** `[lawyer] neck_droop_on_call = false` (`#[serde(default)]`).

**Constants to source (don't hardcode blindly).** Center pan raw from
`orchestrator/calibration/judge_neck.toml` (`pan.center` ≈ 1583). Droop tilt
2167 and home 1500 from firmware `main.py:61,67`.

**Acceptance.** `cargo build` + `cargo test` green. Unit-test the handler emits
the droop command on start and the home command on end WHEN the flag is on, and
NOTHING when off. HARDWARE PASS NEEDED: this is real motion near the range that
previously snapped a tilt mount — verify slew is gentle, pan stays centered, and
the mount tolerates sustained full droop. Do not enable in config.

**Risk.** HIGH (motion). Code is small; the danger is physical. Ship OFF.

---

## FEATURE 6 — Judge idle/attract mode

**Objective.** During idle, periodically entice passers-by: a random spoken
Wettington-style line + a small neck movement + a face cue. Config-gated,
default OFF. Must never run during a trial and must stop instantly when one
starts.

**Approach (impure Runtime timer — required, because the neck can't be driven by
a plain FSM `Command`; `HardwareCommand::Aim` routes to the turret, not the
neck).**
- In `state_machine/mod.rs`, add to `Runtime` a `next_attract: Option<Instant>`
  and (for neck moves) a `maint_cmd_tx` clone (wire it through `Runtime::new` +
  `main.rs`; the inference path already holds one, so the channel exists).
- In the run loop's `Tick` handling (`mod.rs:135-145`), when `is_idle` is true
  and attract is enabled: after being idle for `idle_secs_before`, every
  `interval_secs` fire ONE attract beat:
  - `Command::Speak(<random line>)` (streams via the active persona's TTS voice
    to the browser audio kiosk; silent if none connected — fine).
  - a small neck pan sweep via `maint_cmd_tx` targeted `Aim` — SMALL amplitude,
    within the safe pan range, tilt at working center (NOT droop), respecting
    the firmware slew. Keep pan within a few degrees of center.
  - optionally re-assert `Face(FacePhase::Idle)` or a livelier face cue.
- Reset `next_attract = None` the instant the machine leaves Idle (trial start),
  so a beat never overlaps a trial. Guard: if not idle, do nothing.

**Constraint (good news).** Firmware 0.5 is SILENT while parked (no idle I2C
poll → no twitch). Attract moves are position *writes*, which are safe. Do NOT
add any periodic position read-back.

**Config.** New `[attract]` section: `enabled = false`, `idle_secs_before = 20`,
`interval_secs = 45`. All `#[serde(default)]`.

**Content.** Draw lines at random from the pool in Draft Content (pompous
Wettington). Vary selection by an index that changes each beat (don't use
`Math.random`/`rand` if it complicates tests — a rotating counter is fine).

**Acceptance.** `cargo build` + `cargo test` green. Unit-test: when enabled and
idle past the threshold, a Tick produces a `Speak` command; when disabled, or
when not idle, it produces none; leaving idle clears `next_attract`. HARDWARE
PASS NEEDED: confirm neck sweep amplitude/slew is gentle and the TTS cadence
isn't annoying. Ship OFF.

**Risk.** MEDIUM (motion + audio-to-empty-room). Ship OFF.

---

## FEATURE 5 — Lawyer audio over the primary speaker with phone effect

**Objective.** Play the lawyer's spoken call audio over the booth's primary
speaker (in addition to the phone), with telephone-quality coloration.
Config-gated, default OFF. This is the highest-risk item — cross-crate,
real-time, unverifiable without a live call.

**Current gap.** The orchestrator never receives call audio; it lives entirely
in the counsel crate over SIP. The lawyer's speech is synthesized in
`counsel/src/call/agent.rs:synth_to_queue` (raw 24 kHz s16le `chunk`) then
decimated 24k→8k + µ-law-encoded in `queue_pcm24` (:296-321) for RTP to the
phone.

**Chosen strategy — tee the PCM in counsel, inject into the orchestrator's
existing player.**
1. **Counsel side (gated by a counsel config flag, default off).** In
   `synth_to_queue`, tee each raw 24k chunk to a new outbound sink that POSTs/
   streams to a NEW orchestrator endpoint. Counsel already knows the orchestrator
   base URL (`call/mod.rs:40 notify_base`). For an authentic phone timbre, run
   the tee'd chunk through the EXISTING `Decimator` + `g711` round-trip
   (24k→8k→µ-law→decode→back up) before sending — reuses code, gives a real
   telephone band for free. (Alternative: send clean 24k and bandpass on the
   client — see below.)
2. **Orchestrator side (gated by `[lawyer] speaker_playback = false`).** New
   endpoint on the display server that injects the received PCM into
   `display_bcast` as `DisplayMessage::Binary`, preceded by a NEW header event
   (e.g. `DisplayEvent::LawyerAudio { format }`) — distinct from `TtsAudio` so
   the client routes it to a PHONE chain, NOT the robot voice worklet.
3. **Frontend.** In `ws.ts` handle `lawyer_audio` like `tts_audio` but flag the
   next binary as lawyer audio; in `audio.ts`, route lawyer frames to a
   `BiquadFilterNode` bandpass (~300–3400 Hz) instead of `getRobotInput(ctx)`
   (:107). If counsel already µ-law-colored the audio (strategy 1 above), the
   client bandpass can be light or skipped.

**Concurrency.** Calls happen during the plea/cross windows while the judge is
silent, so lawyer audio and judge TTS don't overlap in practice. Still, ensure
the lawyer stream uses the same single audio-generation token path and does not
corrupt a subsequent judge `TtsAudio` session (send a clean `TtsEnd`-equivalent
when the lawyer line finishes).

**Config.** `[lawyer] speaker_playback = false` (orchestrator) + a counsel-side
tee flag default off. Nothing streams unless BOTH are on.

**Acceptance.** `cargo build` + `cargo test` (both crates) + `npm run build`
green. Because it can't be exercised without a live call, add: (a) unit tests for
the µ-law/bandpass transform if extracted as a pure fn; (b) a documented MANUAL
test procedure in the progress log (place a call, confirm lawyer audio on the
booth speaker with phone timbre, confirm judge TTS afterward is unaffected).
HARDWARE PASS NEEDED: the whole feature. Ship OFF.

**Risk.** HIGH. If time runs short, commit the counsel tee + orchestrator
endpoint + frontend routing as far as it compiles, and write the remaining wiring
+ manual-test steps in the progress log. Do not let this block features 1–4/6/7.

---

## DRAFT CONTENT (edit freely — these are first drafts)

### Feature 2 — idle text
> **PRESS THE BUTTON TO STAND TRIAL**

### Feature 3 — lawyer-calling overlay
> **YOUR LAWYER IS CALLING**
> Pick up the phone.

### Feature 6 — attract lines (pompous Wettington; spoken in the active judge's TTS voice)
- "The court grows BORED. Someone — approach, and be judged."
- "An empty docket is an insult to justice. Step forward."
- "I did not power on this morning to watch you loiter. A defendant. Now."
- "You there — yes, you, pretending not to see me. The bench is waiting."
- "Justice is thirsty and the docket is dry. Rectify this."
- "I have all the time in the world and none of the patience. Approach."
- "Somewhere in this crowd stands a criminal. Have the decency to confess in person."
- "The Wet Court of Appeals is now accepting the guilty, the petty, and the merely unlucky."
- "Step up, state your crime, and let the water decide."
- "A courtroom without a defendant is merely a very expensive fountain. Fix that."

### Feature 4 — coupon copy (Dewey, Soakem & Howe; absurd deadpan — the counsel voice)
- Header (inverse bar): **DEWEY, SOAKEM & HOWE — ATTORNEYS AT LAW(ish)**
- Random tagline (pick one per coupon):
  - "My law degree is from a cereal box. The box was very prestigious."
  - "We've never won a case. We've also never given up. (We have given up.)"
  - "Ask about our defense strategy: cite Puddle v. Splash, 1974. It means nothing."
  - "Were you framed by the weather? We can't help, but we'll listen."
  - "First consultation free. Second consultation also free. We can't work out billing."
  - "When all else fails, cry. We will cry with you."
- Footer line: **CALL DEWEY. ADMIT NOTHING.**

---

## SUMMARY TABLE

| # | Feature | Tier | Config flag (default) | Hardware pass? |
|---|---------|------|-----------------------|----------------|
| 1 | Cross countdown reset on call-end | Safe | — | no |
| 2 | Idle "press button" big text | Safe | — | no (kiosk eyeball) |
| 3 | "Lawyer calling" overlay | Safe | — | verify on live call |
| 4 | Receipt coupons | Safe | `printer.coupon_frequency="off"` | no |
| 7 | Neck droop on call | Motion | `lawyer.neck_droop_on_call=false` | YES |
| 6 | Idle attract mode | Motion+audio | `attract.enabled=false` | YES |
| 5 | Lawyer audio on speaker | Audio | `lawyer.speaker_playback=false` (+counsel tee) | YES (whole feature) |
