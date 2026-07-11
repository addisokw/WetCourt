# Integration Plan — making the trial loop fluid end-to-end

Findings from a full-project integration audit (2026-07-11): the FSM spine is
solid — every inference call has a timeout + fallback, finalization
(casebook/receipt/capture) is correctly wired — but several theatrical
components are built on both ends and never cued in the middle, and the whole
show's audio/mic path hangs off the single operator browser tab.

Phases are sequenced by show-impact and risk; each is independently shippable.
Sizes: S = under an hour, M = a session, L = multi-session.

Suggested order: **0 → 1 → 4 → 2 → 3 → 5** (Phase 4 is all small wins and can
jump ahead of the structural Phase 2 if a show is imminent; Phase 3 is
independent of 2 and can swap with it). Phases 0+1 together are roughly a day
of work.

---

## Phase 0 — Safety and stall fixes (do first; all small-to-medium, all orchestrator)

The bugs that can ruin a live trial regardless of anything else.

1. **`PronouncingVerdict` watchdog** (S) — the only state with no Tick escape
   (`transitions.rs:232-246`); a lost `TtsFinished` wedges the trial until
   e-stop. Add a `started_at` to the variant and a generous Tick escape
   (~45s, or estimated from deliberation length).
2. **De-race the verdict fallback** (S) — the FSM `Deliberating` timeout and
   the verdict streamer's total budget are both `verdict_total_timeout_secs`
   (30s). If the FSM Tick wins, it speaks a fallback verdict while
   `verdict::real` is still streaming its 3-segment TTS → two overlapping
   judge voices. Give the FSM +5–10s headroom (or a separate knob).
3. **Gate auto-fire behind maintenance mode** (M) — auto-fire is an
   exploration/tuning tool (Vision tab), not a show feature, and today it can
   fire the squirt pre-verdict during the Deliberating lock-on, bypassing the
   FSM and the fire gate (`display/mod.rs:534-550`). The gate must be
   server-side (`POST /vision/autofire` is unauthenticated to the browser):
   - `POST /vision/autofire` with `enabled: true` returns 409 unless the
     maintenance flag is set (atomic mirror already exists for
     `/maintenance/command`).
   - **Exiting maintenance force-disables auto-fire** — the load-bearing
     part; it can never stay latched into a live show. Trials only start
     from Idle, so no separate trial interlock is needed.
   - Frontend: auto-fire row disabled with a "requires maintenance mode"
     hint when live (same 🔒 affordance as the hardware tabs).
   - Open question: whether "Fire now" and manual arm on the same panel get
     the same treatment (arm/disarm live is arguably a legitimate operator
     action).
4. **No lock, no fire** (M) — today `Freeze` disarms before `Fire`, which
   makes the fire gate transparent: the guilty shot fires wherever the gun
   happens to point even if vision never locked (offline, no person, lost
   track). Change: the trial's guilty `Fire` only passes if there was a
   fresh lock (`fire_ok`) at freeze time; otherwise hold the shot via the
   existing `FireHeld` path (banner + synthesized ack keeps the trial
   advancing — that machinery already exists, it just never applies to the
   trial shot today). No firing into the void or bystanders.
   Note: with `[vision] trial_targeting = false` (manual aiming mode) the
   fire stays ungated as today — decide if that mode should also require an
   operator confirm.
5. **One fire duration** (S) — the trial fires `squirt.duration_ms` (150ms)
   while console/auto-fire use calibration `fire_ms` (30ms) — the show fires
   5× what you calibrate. Pick one source of truth (suggestion: calibration
   `fire_ms`, since it's the one tuned from the console); make
   `sentence_commands` read it; deprecate the other.

**Verify:** existing FSM unit tests + a full mock-driver trial run watching
the event log; a mock trial with auto-fire latched on pre-gate must show no
Squirt command during deliberation.

---

## Phase 1 — The physical judge performs (biggest theatrical payoff)

The firmware side already exists (`firmware/judge-face/inputs.py:227-263`
implements FACE/PERSONA); this is mostly orchestrator plumbing.

6. **Wire `FACE` phases end-to-end** (M) — add `HardwareCommand::Face(FacePhase)`
   with `idle | listening | deliberating | verdict_guilty | verdict_innocent`,
   route to `Role::JudgeFace` in `role_for`, emit from the FSM:
   - `listening` on plea / cross-answer windows
   - `deliberating` on Deliberating
   - the verdict variant in `begin_pronouncing`
   - `idle` on return to Idle / e-stop
   Keep `PANEL` as legacy fallback. This is the moment the LED judge actually
   reacts to the verdict.
7. **Pupil dilation on a state-driven pattern** (M) — the dilation effect is
   visually strong, so it should run regularly rather than hang off a live
   audio envelope. Once #6 lands, the pattern can live entirely in firmware,
   keyed off the current FACE phase — no new protocol needed. Pattern TBD;
   candidates to try on the bench:
   - slow "breathing" oscillation while idle
   - quicker, irregular dilation while `deliberating` (thinking)
   - a sharp full dilation at the verdict reveal, then settle
   - brief dilation "reactions" at random intervals while `listening`
8. **`PERSONA` sync** (S) — send `PERSONA <id>` to the face at startup and on
   `persona/select`, so the matrix stops free-running its own demo rotation.
9. **Neck choreography** (M, stretch) — minimum viable: mirror the vision aim
   to the neck during Deliberating (the judge visibly *looks at* the
   defendant, and the existing neck→face catchlight parallax mirror comes
   alive for free), recenter on Idle. Fancier moves are post-show polish.

Cut from this phase: **`PlayCue` is dropped** — the organ/choir cue path was
never wired to a player and we're not keeping it. Deleting the emissions and
`CueFinished` plumbing is folded into the Phase 5 dead-code sweep.

**Verify:** bench test with the Matrix Portal + one mock trial per verdict
outcome.

---

## Phase 2 — Presence infrastructure (WS resync + booth audio)

10. **State snapshot on WS connect** (M) — both `/ws` and `/ws/view` push only
    `Idle` on connect; any mid-trial (re)connect shows "Step up." until the
    next transition, and reconnecting during maintenance silently re-locks
    the hardware tabs. Add a `DisplayEvent::Snapshot` (state name, charge,
    plea, cross question, verdict, active phase deadline, maintenance flag)
    sent on connect — extend the existing `TrialSnapshot` (already maintained
    for the lawyer phone) rather than invent a second mirror. Frontend
    applies it on open.
11. **Decouple booth audio from the operator laptop** (L) — TTS PCM goes only
    to the single operator `/ws` client and the plea mic is the operator
    tab's MediaRecorder; laptop closed = silent show and no plea possible.
    Make `/case` (booth kiosk, has speakers) audio-capable:
    `/ws/view?audio=1` forwards binary PCM to exactly one designated audio
    client. `tts_finished` stays server-self-acked (already true) so playback
    clients are never load-bearing. Decide which machine is the booth's voice;
    decide whether the plea mic moves to the kiosk (`/case` requesting mic
    permission) or stays operator-side.

**Verify:** kill the operator tab mid-trial; show stays audible and the
defendant can still plead.

---

## Phase 3 — Lawyer phone joins the trial (L)

The counsel stack is fully built and hardware-verified; today its only trial
coupling is pulling `GET /trial/state` at call start. Wire it into the trial
proper:

12. **Off-hook pauses the clock** — once charges have been presented, picking
    the phone up during a plea or cross-answer window pauses the countdown
    until it goes back on the hook. Mechanics:
    - Counsel pushes call lifecycle events to the orchestrator (e.g.
      `POST /lawyer/event` with `call_started` / `call_ended` — counsel
      already knows the orchestrator base URL from the trial-context fetch).
      Alternative: orchestrator polls `/lawyer/status`, but push is cleaner.
    - New FSM events `LawyerCallStarted` / `LawyerCallEnded`; in
      `AwaitingPlea` / `CrossAwaitingAnswer`, capture remaining window time
      on call start and restore the deadline on call end.
    - Displays need a paused-countdown affordance (re-emit `PhaseDeadline`
      on resume; a `paused` flag on the case view so the countdown visibly
      freezes rather than lying).
13. **Ring on cross-examination** — when the cross-answer window opens, the
    orchestrator triggers counsel ring-out (`POST /call` with a
    reason seeded from the judge's question), with the same stop-clock
    behavior while the call is up. Decide the exact beat: ring as the answer
    window opens (defendant can consult before/while answering).
14. **Operator enable/disable** — a toggle like `cross_enabled`
    (`GET/POST /operator/lawyer_integration`, checkbox on the Lawyer or
    Judge Mind panel) that turns off both the off-hook pause and the
    cross-exam ring for busy days, to move people through trials faster.
15. **Keep the force-ring button** — the existing operator `/lawyer/call`
    ring-out stays as-is, independent of the toggle.

**Verify:** softphone registered as the ATA; run a trial, pick up during the
plea window, confirm the countdown freezes and resumes; enable cross-exam and
confirm the ring fires when the answer window opens; toggle off and confirm
neither behavior triggers.

---

## Phase 4 — Dead-air and flow polish (frontend + small FSM tweaks)

16. **Gate `DisplayingCharge` on `TtsFinished`** (S) — currently advances on a
    fixed 5s ignoring TTS (`transitions.rs:76`), so a long charge is still
    being read when the plea window opens. Advance on TTS-done with the fixed
    duration as watchdog.
17. **Cross-answer gets its own prompt** (S) — the answer window reuses plain
    `StartPleaRecording` (`transitions.rs:386`), so the screen says "begin
    your defense" when the judge just asked a follow-up. Flavor the event
    (or add a boolean) → "Answer the judge's question."
18. **Server-side marker stripping** (S) — deliberation tokens stream raw; the
    client strips `VERDICT:`/`KEY_FACTOR:` per complete line, so a mid-marker
    token split can flash "VERDICT: GUIL" on the big screen. Line-buffer in
    `verdict.rs` before broadcast; delete the client-side `stripMarkers`
    hazard.
19. **Surface silent plea failures** (S) — empty/failed STT silently becomes
    "[no defense offered]" (guilty-leaning) with only a `warn!` log. Add an
    operator banner (like the existing FireHeld one) so the operator can
    e-stop instead of railroading the defendant.

(Dropped: a `generating_charge` dead-air state — charges now come from the
pre-generated list/queue, so this phase is near-instant in practice.)

---

## Phase 5 — Housekeeping (batch opportunistically)

20. **`tts_speed`** (S) — validated, set in 5 of 6 personas, never sent to
    Kokoro (`client.rs:72-77`). Add `speed` to the request body; five
    personas' tuning starts working.
21. **Printer preflight** (S) — the transport has rich `Status::is_ready`
    support but the service writes blindly; paper-out prints into the void.
    Check status before writing + operator-visible warning.
22. **Crimes reload** (S) — crimes-editor edits require an orchestrator
    restart. Add a console "reload crimes" button or file-watch.
23. **Dead code sweep** (S) — the whole `PlayCue`/`CueFinished` cue path
    (cut in Phase 1), `DisplayEvent::Error`, never-emitted
    `ChargeFailed`/`TranscriptFailed`/`VerdictFailed`, `AimMsg.locked`,
    `PanelPattern::Verdict` (superseded by FACE), stale `whisper-1`/Parakeet
    naming, `pcmResidue` not reset on e-stop.
24. **Voice catalogue** (M) — hardcoded 28-voice const can drift from the
    Kokoro deployment (operator picks a voice that 500s at synth time).
    Fetch from Kokoro at startup, or validate at persona-save.
25. **Decide on LIGHTS** (decision + S) — the FSM emits four light states
    every trial; no device owns the role and the router silently drops them.
    Either spec a splash-lights device or delete the emissions until hardware
    exists.

(All booth hardware — gavel, turret, squirt, judge-neck, and the HT801 — is
already verified; the firmware README "not yet verified" notes are stale and
can be cleaned up in the sweep.)

---

## Decisions to make before starting

- **Which machine is the booth's voice and mic** (Phase 2 item 11) — shapes
  the WS changes.
- **Pupil dilation pattern** (Phase 1 item 7) — pick from the candidates on
  the bench; firmware-only once FACE phases land.
- **Do "Fire now" / manual arm also get maintenance-gated** (Phase 0 item 3)?
- **Cross-exam ring timing** (Phase 3 item 13) — ring as the answer window
  opens, or as the question is being asked?
