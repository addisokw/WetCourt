use std::time::{Duration, Instant};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::fallbacks;
use crate::hardware::protocol::{FacePhase, HardwareCommand, LedMode};

use super::commands::{Command, TargetingCue};
use super::events::Event;
use super::states::{CrossExam, State, Verdict, NO_DEFENSE};

/// Sentinel for an empty/unintelligible cross-examination answer.
const NO_ANSWER: &str = "[no answer given]";

/// Extra headroom the FSM gives the verdict service beyond its own stream
/// budget (`verdict_total_timeout_secs`) before speaking a fallback. The two
/// used to share the same value: when the FSM Tick won that race it spoke a
/// fallback verdict while `verdict::real` was still streaming its own 3-segment
/// TTS — two overlapping judge voices. The service's internal timeout always
/// resolves first now; this Tick escape only fires if the service task died.
const VERDICT_FALLBACK_GRACE_SECS: u64 = 10;

/// Watchdog for `ExecutingSentence`: if no HardwareAck/Error arrives (e.g. MCU
/// disconnected with the TCP driver), the Tick handler escapes to Idle. Sized
/// for the full sentence sequence — lights + squirt burst + ack round-trips
/// with headroom.
const SENTENCE_WATCHDOG_SECS: u64 = 60;

/// Escape hatch for `PronouncingVerdict`, sized to the spoken verdict: the
/// deliberation body at a conservative TTS pace (~12 chars/sec) plus the
/// preamble, theater beat, and margin. Normally `TtsFinished` (browser ack or
/// the TTS self-ack timer) arrives long before this; it exists so a lost event
/// can never wedge the trial.
fn pronounce_watchdog(deliberation: &str) -> Duration {
    Duration::from_secs(30 + deliberation.len() as u64 / 12)
}

pub fn step(state: State, event: Event, cfg: &Config, cross_enabled: bool) -> (State, Vec<Command>) {
    use Event::*;
    use State::*;

    if matches!(event, OperatorEmergencyStop) && !matches!(state, Idle) {
        return (
            Idle,
            vec![
                Command::Display(DisplayEvent::Reset),
                Command::Display(DisplayEvent::Idle),
                Command::Hardware(HardwareCommand::Face(FacePhase::Idle)),
                Command::Hardware(HardwareCommand::Led(LedMode::Blink)),
            ],
        );
    }

    match (state, event) {
        // The defendant's button is the start trigger too — identical to the
        // operator's start.
        (Idle, OperatorStart) | (Idle, DefendantButton) => (
            GeneratingCharge { started_at: Instant::now() },
            vec![
                Command::GenerateCharge,
                Command::Display(DisplayEvent::Reset),
                Command::Display(DisplayEvent::PhaseDeadline {
                    phase: "generating_charge".into(),
                    deadline_ms: cfg.inference.charge_timeout_secs * 1000,
                }),
                Command::Hardware(HardwareCommand::Face(FacePhase::Deliberating)),
                // Trial underway: the lamp goes dark until a press means
                // something again (the plea window's pulse).
                Command::Hardware(HardwareCommand::Led(LedMode::Off)),
            ],
        ),
        // Enter the maintenance/test plane — only from Idle. The atomic mirror
        // in `Runtime::handle` opens the direct-command REST gate on entry.
        // The lamp goes dark: trials are blocked, so its attract blink would
        // be a lie (the console tab can drive it directly from here).
        (Idle, EnterMaintenance) => (
            Maintenance,
            vec![
                Command::Display(DisplayEvent::Maintenance { active: true }),
                Command::Hardware(HardwareCommand::Led(LedMode::Off)),
            ],
        ),
        (Idle, _) => (Idle, vec![]),

        // Maintenance blocks every trial event (no OperatorStart arm); only
        // ExitMaintenance (or the e-stop guard above) leaves it.
        (Maintenance, ExitMaintenance) => (
            Idle,
            vec![
                Command::Display(DisplayEvent::Maintenance { active: false }),
                Command::Display(DisplayEvent::Idle),
                Command::Hardware(HardwareCommand::Led(LedMode::Blink)),
            ],
        ),
        (s @ Maintenance, _) => (s, vec![]),

        (GeneratingCharge { .. }, ChargeReady(text)) => begin_displaying_charge(text, cfg),
        (GeneratingCharge { started_at }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.inference.charge_timeout_secs) {
                begin_displaying_charge(fallbacks::charges::random(), cfg)
            } else {
                (GeneratingCharge { started_at }, vec![])
            }
        }
        (s @ GeneratingCharge { .. }, _) => (s, vec![]),

        // The plea window opens once the minimum display time has passed AND
        // the charge TTS has drained — a long charge is never still being read
        // over the defendant's talking time. The watchdog escapes a lost ack.
        (DisplayingCharge { charge, until, .. }, TtsFinished) if Instant::now() >= until => {
            begin_awaiting_plea(charge, cfg)
        }
        (DisplayingCharge { charge, until, watchdog_at, .. }, TtsFinished) => (
            DisplayingCharge { charge, until, tts_done: true, watchdog_at },
            vec![],
        ),
        (DisplayingCharge { charge, until, tts_done, watchdog_at }, Tick)
            if (tts_done && Instant::now() >= until) || Instant::now() >= watchdog_at =>
        {
            begin_awaiting_plea(charge, cfg)
        }
        (s @ DisplayingCharge { .. }, _) => (s, vec![]),

        (AwaitingPlea { charge, .. }, PleaAudioReceived(audio)) => begin_transcribing(charge, audio, cfg),
        // The accused pressed the button: reset the plea-window deadline so
        // they get the full talking time from the moment they start speaking.
        // (While paused on the lawyer phone the reset applies to the frozen
        // remaining time instead — the clock stays stopped.)
        (AwaitingPlea { charge, paused_remaining, .. }, PleaRecordingStarted) => {
            let window = Duration::from_secs(cfg.trial.plea_window_secs);
            let deadline = Instant::now() + window;
            let paused_remaining = paused_remaining.map(|_| window);
            let mut cmds = vec![Command::Display(DisplayEvent::PleaRecording { active: true })];
            cmds.push(clock_event("awaiting_plea", window, paused_remaining.is_some()));
            (AwaitingPlea { charge, deadline, paused_remaining }, cmds)
        }
        // Lawyer phone: picking up pauses the window; hanging up resumes it
        // with the frozen remaining time.
        (AwaitingPlea { charge, deadline, paused_remaining: None }, LawyerCallStarted) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            (
                AwaitingPlea { charge, deadline, paused_remaining: Some(remaining) },
                vec![Command::Display(DisplayEvent::ClockPaused {
                    remaining_ms: remaining.as_millis() as u64,
                })],
            )
        }
        (AwaitingPlea { charge, paused_remaining: Some(rem), .. }, LawyerCallEnded) => (
            AwaitingPlea { charge, deadline: Instant::now() + rem, paused_remaining: None },
            vec![Command::Display(DisplayEvent::PhaseDeadline {
                phase: "awaiting_plea".into(),
                deadline_ms: rem.as_millis() as u64,
            })],
        ),
        (AwaitingPlea { charge, deadline, paused_remaining: None }, Tick)
            if Instant::now() >= deadline =>
        {
            begin_flushing_plea(charge, cfg)
        }
        // "I'm done talking" — the defendant's button closes the plea window
        // early, down the same path as the deadline expiring. Ignored while
        // the clock is paused on the lawyer phone (they're consulting, and
        // the frozen countdown must not be skipped out from under them).
        (AwaitingPlea { charge, paused_remaining: None, .. }, DefendantButton) => {
            begin_flushing_plea(charge, cfg)
        }
        (s @ AwaitingPlea { .. }, _) => (s, vec![]),

        // Frontend is racing to ship its recorded blob after the deadline; take
        // it if it arrives, otherwise fall through to empty-audio transcription
        // at the hard deadline.
        (FlushingPlea { charge, .. }, PleaAudioReceived(audio)) => begin_transcribing(charge, audio, cfg),
        (FlushingPlea { charge, hard_deadline }, Tick) if Instant::now() >= hard_deadline => {
            begin_transcribing(charge, Vec::new(), cfg)
        }
        (s @ FlushingPlea { .. }, _) => (s, vec![]),

        (Transcribing { charge, .. }, TranscriptReady(text)) => after_plea(charge, text, cross_enabled, cfg),
        (Transcribing { charge, started_at, audio }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.inference.stt_timeout_secs) {
                with_plea_fallback(
                    begin_deliberating(charge, NO_DEFENSE.into(), None, cfg),
                    "plea transcription timed out",
                )
            } else {
                (Transcribing { charge, started_at, audio }, vec![])
            }
        }
        (Transcribing { charge, .. }, TranscriptFailed(err)) => with_plea_fallback(
            begin_deliberating(charge, NO_DEFENSE.into(), None, cfg),
            &format!("plea transcription failed: {err}"),
        ),
        (s @ Transcribing { .. }, _) => (s, vec![]),

        // ---- Cross-examination ----
        // The judge composes one pointed follow-up. Any timeout/failure here
        // falls straight through to the verdict so cross-exam can't wedge a trial.
        (CrossGeneratingQuestion { charge, plea, .. }, CrossQuestionReady(q)) => {
            begin_cross_speaking(charge, plea, q)
        }
        (CrossGeneratingQuestion { charge, plea, started_at }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.cross_examination.question_timeout_secs) {
                begin_deliberating(charge, plea, None, cfg)
            } else {
                (CrossGeneratingQuestion { charge, plea, started_at }, vec![])
            }
        }
        (CrossGeneratingQuestion { charge, plea, .. }, CrossQuestionFailed) => {
            begin_deliberating(charge, plea, None, cfg)
        }
        (s @ CrossGeneratingQuestion { .. }, _) => (s, vec![]),

        // Question is displayed + spoken; open the answer window once its TTS
        // drains (TtsFinished), with a watchdog if that ack never arrives.
        (CrossSpeaking { charge, plea, question, .. }, TtsFinished) => {
            begin_cross_answer(charge, plea, question, cfg)
        }
        (CrossSpeaking { charge, plea, question, started_at }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.cross_examination.question_timeout_secs) {
                begin_cross_answer(charge, plea, question, cfg)
            } else {
                (CrossSpeaking { charge, plea, question, started_at }, vec![])
            }
        }
        (s @ CrossSpeaking { .. }, _) => (s, vec![]),

        // Recording the answer — reuses the plea-recording machinery verbatim.
        (CrossAwaitingAnswer { charge, plea, question, .. }, PleaAudioReceived(audio)) => {
            begin_cross_transcribing(charge, plea, question, audio, cfg)
        }
        (CrossAwaitingAnswer { charge, plea, question, paused_remaining, .. }, PleaRecordingStarted) => {
            let window = Duration::from_secs(cfg.cross_examination.answer_window_secs);
            let deadline = Instant::now() + window;
            let paused_remaining = paused_remaining.map(|_| window);
            let mut cmds = vec![Command::Display(DisplayEvent::PleaRecording { active: true })];
            cmds.push(clock_event("cross_answer", window, paused_remaining.is_some()));
            (CrossAwaitingAnswer { charge, plea, question, deadline, paused_remaining }, cmds)
        }
        (
            CrossAwaitingAnswer { charge, plea, question, deadline, paused_remaining: None },
            LawyerCallStarted,
        ) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            (
                CrossAwaitingAnswer {
                    charge,
                    plea,
                    question,
                    deadline,
                    paused_remaining: Some(remaining),
                },
                vec![Command::Display(DisplayEvent::ClockPaused {
                    remaining_ms: remaining.as_millis() as u64,
                })],
            )
        }
        (
            CrossAwaitingAnswer { charge, plea, question, paused_remaining: Some(rem), .. },
            LawyerCallEnded,
        ) => (
            CrossAwaitingAnswer {
                charge,
                plea,
                question,
                deadline: Instant::now() + rem,
                paused_remaining: None,
            },
            vec![Command::Display(DisplayEvent::PhaseDeadline {
                phase: "cross_answer".into(),
                deadline_ms: rem.as_millis() as u64,
            })],
        ),
        (
            CrossAwaitingAnswer { charge, plea, question, deadline, paused_remaining: None },
            Tick,
        ) if Instant::now() >= deadline => {
            begin_cross_flushing(charge, plea, question)
        }
        // Same "done talking" early close as the plea window.
        (
            CrossAwaitingAnswer { charge, plea, question, paused_remaining: None, .. },
            DefendantButton,
        ) => begin_cross_flushing(charge, plea, question),
        (s @ CrossAwaitingAnswer { .. }, _) => (s, vec![]),

        (CrossFlushingAnswer { charge, plea, question, .. }, PleaAudioReceived(audio)) => {
            begin_cross_transcribing(charge, plea, question, audio, cfg)
        }
        (CrossFlushingAnswer { charge, plea, question, hard_deadline }, Tick) if Instant::now() >= hard_deadline => {
            begin_cross_transcribing(charge, plea, question, Vec::new(), cfg)
        }
        (s @ CrossFlushingAnswer { .. }, _) => (s, vec![]),

        (CrossTranscribing { charge, plea, question, .. }, TranscriptReady(answer)) => {
            begin_deliberating(charge, plea, Some(CrossExam { question, answer }), cfg)
        }
        (CrossTranscribing { charge, plea, question, started_at, audio }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.inference.stt_timeout_secs) {
                with_plea_fallback(
                    begin_deliberating(charge, plea, Some(CrossExam { question, answer: NO_ANSWER.into() }), cfg),
                    "answer transcription timed out",
                )
            } else {
                (CrossTranscribing { charge, plea, question, started_at, audio }, vec![])
            }
        }
        (CrossTranscribing { charge, plea, question, .. }, TranscriptFailed(err)) => with_plea_fallback(
            begin_deliberating(charge, plea, Some(CrossExam { question, answer: NO_ANSWER.into() }), cfg),
            &format!("answer transcription failed: {err}"),
        ),
        (s @ CrossTranscribing { .. }, _) => (s, vec![]),

        (Deliberating { .. }, VerdictReady(v)) => begin_pronouncing(v),
        (Deliberating { started_at, charge, plea }, Tick) => {
            let escape = cfg.inference.verdict_total_timeout_secs + VERDICT_FALLBACK_GRACE_SECS;
            if started_at.elapsed() > Duration::from_secs(escape) {
                begin_pronouncing(fallbacks::verdicts::random(cfg.trial.guilty_bias))
            } else {
                (Deliberating { started_at, charge, plea }, vec![])
            }
        }
        (s @ Deliberating { .. }, _) => (s, vec![]),

        // Reveal beat from the pre_announced verdict service: the gavel lands
        // with the verdict word, not back when deliberation playback started.
        (s @ PronouncingVerdict { .. }, VerdictRevealed) => {
            (s, vec![Command::Hardware(HardwareCommand::Gavel)])
        }
        (PronouncingVerdict { verdict, .. }, TtsFinished) => begin_executing_sentence(verdict, cfg),
        // Watchdog: TtsFinished normally arrives from the browser or the TTS
        // self-ack timer, but if that task dies the trial must not wedge here —
        // proceed to the sentence as if the audio had finished.
        (PronouncingVerdict { verdict, watchdog_at }, Tick) if Instant::now() >= watchdog_at => {
            begin_executing_sentence(verdict, cfg)
        }
        (s @ PronouncingVerdict { .. }, _) => (s, vec![]),

        // First hardware ack — the sentence-execution hardware has finished
        // firing. Shorten the deadline from the 60s watchdog to a cooldown
        // hold, reset lights/panel, and flag hardware_done so subsequent acks
        // (from the cleanup commands themselves) don't restart the cycle.
        (ExecutingSentence { verdict, hardware_done: false, .. }, HardwareAck(_)) |
        (ExecutingSentence { verdict, hardware_done: false, .. }, HardwareError(_)) => {
            let deadline = Instant::now() + Duration::from_secs(cfg.trial.cooldown_secs);
            (
                ExecutingSentence { verdict, deadline, hardware_done: true },
                vec![
                    Command::Display(DisplayEvent::PhaseDeadline {
                        phase: "executing_sentence".into(),
                        deadline_ms: cfg.trial.cooldown_secs * 1000,
                    }),
                    // The face keeps its verdict phase (guilty strobe / innocent
                    // bloom) through the cooldown; it resets on the Idle edge.
                ],
            )
        }
        (ExecutingSentence { deadline, .. }, Tick) if Instant::now() >= deadline => {
            (
                Idle,
                vec![
                    Command::Display(DisplayEvent::Idle),
                    Command::Hardware(HardwareCommand::Face(FacePhase::Idle)),
                    // Back to the attract blink: pressing starts the next trial.
                    Command::Hardware(HardwareCommand::Led(LedMode::Blink)),
                ],
            )
        }
        (s @ ExecutingSentence { .. }, _) => (s, vec![]),

    }
}

fn begin_displaying_charge(text: String, cfg: &Config) -> (State, Vec<Command>) {
    let until = Instant::now() + Duration::from_secs(cfg.trial.charge_display_secs);
    // TTS-length estimate (same pace as the pronounce watchdog) + margin: the
    // escape hatch if the charge TtsFinished never arrives.
    let watchdog_at = Instant::now()
        + Duration::from_secs(cfg.trial.charge_display_secs + 10 + text.len() as u64 / 12);
    (
        State::DisplayingCharge { charge: text.clone(), until, tts_done: false, watchdog_at },
        vec![
            Command::Display(DisplayEvent::ShowCharge { text: text.clone() }),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "displaying_charge".into(),
                deadline_ms: cfg.trial.charge_display_secs * 1000,
            }),
            Command::Speak(text),
        ],
    )
}

/// The `DisplayingCharge → AwaitingPlea` edge: open the plea window.
fn begin_awaiting_plea(charge: String, cfg: &Config) -> (State, Vec<Command>) {
    let deadline = Instant::now() + Duration::from_secs(cfg.trial.plea_window_secs);
    (
        State::AwaitingPlea { charge, deadline, paused_remaining: None },
        vec![
            Command::Display(DisplayEvent::StartPleaRecording {
                deadline_ms: cfg.trial.plea_window_secs * 1000,
                cross: false,
            }),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "awaiting_plea".into(),
                deadline_ms: cfg.trial.plea_window_secs * 1000,
            }),
            Command::Hardware(HardwareCommand::Face(FacePhase::Listening)),
            // Breathing lamp: a press now means "I'm done talking".
            Command::Hardware(HardwareCommand::Led(LedMode::Pulse)),
        ],
    )
}

fn begin_transcribing(charge: String, audio: Vec<u8>, cfg: &Config) -> (State, Vec<Command>) {
    (
        State::Transcribing { charge, audio: audio.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Hardware(HardwareCommand::Led(LedMode::Off)),
            Command::Display(DisplayEvent::Transcribing),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "transcribing".into(),
                deadline_ms: cfg.inference.stt_timeout_secs * 1000,
            }),
            Command::Hardware(HardwareCommand::Face(FacePhase::Deliberating)),
            Command::Transcribe(audio),
        ],
    )
}

const PLEA_FLUSH_GRACE_MS: u64 = 2000;

fn begin_flushing_plea(charge: String, _cfg: &Config) -> (State, Vec<Command>) {
    let hard_deadline = Instant::now() + Duration::from_millis(PLEA_FLUSH_GRACE_MS);
    (
        State::FlushingPlea { charge, hard_deadline },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Hardware(HardwareCommand::Led(LedMode::Off)),
        ],
    )
}

fn begin_deliberating(
    charge: String,
    plea: String,
    cross: Option<CrossExam>,
    cfg: &Config,
) -> (State, Vec<Command>) {
    let state = State::Deliberating {
        charge: charge.clone(),
        plea: plea.clone(),
        started_at: Instant::now(),
    };
    let mut cmds = vec![
        Command::Display(DisplayEvent::TranscriptReady { text: plea.clone() }),
        Command::Display(DisplayEvent::PhaseDeadline {
            phase: "deliberating".into(),
            deadline_ms: (cfg.inference.verdict_total_timeout_secs + VERDICT_FALLBACK_GRACE_SECS)
                * 1000,
        }),
    ];
    // Begin the turret's pre-verdict lock-on: arm as the judge deliberates so the
    // gun visibly acquires the defendant before the reveal.
    if cfg.vision.trial_targeting {
        cmds.push(Command::Targeting(TargetingCue::Acquire));
    }
    cmds.push(Command::Deliberate { charge, plea, cross });
    (state, cmds)
}

/// The countdown display for a (possibly paused) recording window: a normal
/// PhaseDeadline while the clock runs, or a frozen ClockPaused while the
/// defendant is consulting counsel.
fn clock_event(phase: &str, remaining: Duration, paused: bool) -> Command {
    if paused {
        Command::Display(DisplayEvent::ClockPaused { remaining_ms: remaining.as_millis() as u64 })
    } else {
        Command::Display(DisplayEvent::PhaseDeadline {
            phase: phase.into(),
            deadline_ms: remaining.as_millis() as u64,
        })
    }
}

/// Tag a fallback transition with the operator-facing banner explaining that
/// the defendant is about to be judged on "[no defense offered]" through no
/// fault of their own (STT failure/timeout) — so the operator can e-stop and
/// retry instead of silently railroading them.
fn with_plea_fallback((s, mut cmds): (State, Vec<Command>), reason: &str) -> (State, Vec<Command>) {
    cmds.push(Command::Display(DisplayEvent::PleaFallback { reason: reason.into() }));
    (s, cmds)
}

/// First plea is in. Branch into cross-examination when it's enabled and the
/// defendant actually said something; otherwise go straight to the verdict.
fn after_plea(charge: String, plea: String, cross_enabled: bool, cfg: &Config) -> (State, Vec<Command>) {
    if cross_enabled && plea.trim() != NO_DEFENSE {
        begin_cross(charge, plea, cfg)
    } else {
        begin_deliberating(charge, plea, None, cfg)
    }
}

fn begin_cross(charge: String, plea: String, cfg: &Config) -> (State, Vec<Command>) {
    (
        State::CrossGeneratingQuestion { charge: charge.clone(), plea: plea.clone(), started_at: Instant::now() },
        vec![
            Command::CrossExamine { charge, plea },
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "cross_examining".into(),
                deadline_ms: cfg.cross_examination.question_timeout_secs * 1000,
            }),
            Command::Hardware(HardwareCommand::Face(FacePhase::Deliberating)),
        ],
    )
}

fn begin_cross_speaking(charge: String, plea: String, question: String) -> (State, Vec<Command>) {
    (
        State::CrossSpeaking { charge, plea, question: question.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::CrossQuestion { text: question.clone() }),
            Command::Speak(question),
        ],
    )
}

fn begin_cross_answer(charge: String, plea: String, question: String, cfg: &Config) -> (State, Vec<Command>) {
    let window = cfg.cross_examination.answer_window_secs;
    let deadline = Instant::now() + Duration::from_secs(window);
    (
        State::CrossAwaitingAnswer { charge, plea, question, deadline, paused_remaining: None },
        vec![
            Command::Display(DisplayEvent::StartPleaRecording {
                deadline_ms: window * 1000,
                cross: true,
            }),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "cross_answer".into(),
                deadline_ms: window * 1000,
            }),
            Command::Hardware(HardwareCommand::Face(FacePhase::Listening)),
            // Same "done talking" press cue as the plea window.
            Command::Hardware(HardwareCommand::Led(LedMode::Pulse)),
        ],
    )
}

fn begin_cross_flushing(charge: String, plea: String, question: String) -> (State, Vec<Command>) {
    let hard_deadline = Instant::now() + Duration::from_millis(PLEA_FLUSH_GRACE_MS);
    (
        State::CrossFlushingAnswer { charge, plea, question, hard_deadline },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Hardware(HardwareCommand::Led(LedMode::Off)),
        ],
    )
}

fn begin_cross_transcribing(
    charge: String,
    plea: String,
    question: String,
    audio: Vec<u8>,
    cfg: &Config,
) -> (State, Vec<Command>) {
    (
        State::CrossTranscribing { charge, plea, question, audio: audio.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Hardware(HardwareCommand::Led(LedMode::Off)),
            Command::Display(DisplayEvent::Transcribing),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "transcribing".into(),
                deadline_ms: cfg.inference.stt_timeout_secs * 1000,
            }),
            Command::Hardware(HardwareCommand::Face(FacePhase::Deliberating)),
            Command::Transcribe(audio),
        ],
    )
}

fn begin_pronouncing(v: Verdict) -> (State, Vec<Command>) {
    let mut cmds = vec![];
    // Fallback paths (timeout / stream error) don't have an inference task in
    // flight, so the state machine announces and speaks immediately. The
    // inference happy path sets `pre_announced=true`; it handles its own
    // theatre + announcement + TTS via direct `display_tx` sends, and the
    // state machine just holds the state.
    if !v.pre_announced {
        cmds.push(Command::Display(DisplayEvent::Verdict {
            guilty: v.guilty,
            remarks: v.remarks.clone(),
            key_factor: v.key_factor.clone(),
        }));
        cmds.push(Command::Speak(v.deliberation.clone()));
        // Fallback path reveals immediately, so the eye flips and the gavel
        // strikes with it. On the pre_announced path the verdict service sends
        // FACE and GAVEL itself at its reveal moment (after the theater beat) —
        // firing them here would land a whole deliberation early.
        cmds.push(Command::Hardware(HardwareCommand::Face(FacePhase::verdict(v.guilty))));
        cmds.push(Command::Hardware(HardwareCommand::Gavel));
    }
    let watchdog_at = Instant::now() + pronounce_watchdog(&v.deliberation);
    (State::PronouncingVerdict { verdict: v, watchdog_at }, cmds)
}

/// The `PronouncingVerdict → ExecutingSentence` edge, shared by the normal
/// `TtsFinished` path and the pronounce watchdog.
fn begin_executing_sentence(verdict: Verdict, cfg: &Config) -> (State, Vec<Command>) {
    let mut cmds = sentence_commands(&verdict, cfg);
    let deadline = Instant::now() + Duration::from_secs(SENTENCE_WATCHDOG_SECS);
    cmds.push(Command::Display(DisplayEvent::PhaseDeadline {
        phase: "executing_sentence".into(),
        deadline_ms: SENTENCE_WATCHDOG_SECS * 1000,
    }));
    (State::ExecutingSentence { verdict, deadline, hardware_done: false }, cmds)
}

fn sentence_commands(v: &Verdict, cfg: &Config) -> Vec<Command> {
    let mut cmds = vec![Command::Display(DisplayEvent::ExecuteSentence { guilty: v.guilty })];
    if v.guilty {
        // Freeze the aim before firing (when trial-targeting): the turret holds
        // where vision locked it, so the shot lands on the defendant. Ordered
        // before Fire — dispatched sequentially. The hardware adapter still
        // holds the shot unless vision has a fresh lock (no lock, no fire), and
        // rewrites the duration to the calibrated squirt fire_ms.
        if cfg.vision.trial_targeting {
            cmds.push(Command::Targeting(TargetingCue::Freeze));
        }
        cmds.push(Command::Hardware(HardwareCommand::Fire(cfg.squirt.duration_ms)));
    } else {
        cmds.push(Command::Hardware(HardwareCommand::Ping)); // synthetic ack source
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn test_cfg() -> Config {
        Config {
            inference: InferenceConfig {
                mode: "mock".into(),
                base_url: "x".into(), chat_model: "x".into(), stt_model: "x".into(),
                tts_model: "x".into(),
                charge_timeout_secs: 10, verdict_first_token_timeout_secs: 15,
                verdict_total_timeout_secs: 30, stt_timeout_secs: 5, tts_timeout_secs: 10,
                enable_thinking: false, api_key: None,
            },
            hardware: HardwareConfig { driver: "mock".into(), ack_timeout_ms: 1000, bind_addr: "0.0.0.0:0".into(), beacon_port: 0 },
            mock_hw: MockHwConfig { ack_latency_ms: 1, fail_rate: 0.0, simulate_estop_after_secs: 0 },
            mock_inference: MockInferenceConfig::default(),
            squirt: SquirtConfig { duration_ms: 150 },
            trial: TrialConfig { plea_window_secs: 1, charge_display_secs: 1, cooldown_secs: 1, guilty_bias: 1.0 },
            cross_examination: CrossExamConfig { enabled: true, answer_window_secs: 1, question_timeout_secs: 1 },
            display: DisplayConfig { listen_addr: "127.0.0.1:0".into() },
            logging: LoggingConfig { level: "info".into(), log_file: "x".into(), transcripts_jsonl: "x".into() },
            default_persona_id: "wettington".into(),
            crimes: CrimesConfig::default(),
            printer: PrinterConfig::default(),
            vision: VisionConfig::default(),
            capture: CaptureConfig::default(),
            lawyer: LawyerConfig::default(),
        }
    }

    fn guilty_verdict(guilty: bool) -> Verdict {
        Verdict {
            guilty,
            deliberation: "d".into(),
            remarks: "r".into(),
            key_factor: None,
            pre_announced: false,
        }
    }

    #[test]
    fn deliberating_arms_targeting_when_enabled() {
        let mut cfg = test_cfg();
        cfg.vision.trial_targeting = true;
        let (_s, cmds) = begin_deliberating("c".into(), "p".into(), None, &cfg);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Targeting(TargetingCue::Acquire))));

        cfg.vision.trial_targeting = false;
        let (_s, cmds) = begin_deliberating("c".into(), "p".into(), None, &cfg);
        assert!(!cmds.iter().any(|c| matches!(c, Command::Targeting(_))));
    }

    #[test]
    fn guilty_sentence_freezes_immediately_before_fire() {
        let cfg = test_cfg(); // trial_targeting defaults on
        let cmds = sentence_commands(&guilty_verdict(true), &cfg);
        let freeze = cmds
            .iter()
            .position(|c| matches!(c, Command::Targeting(TargetingCue::Freeze)))
            .expect("freeze present");
        let fire = cmds
            .iter()
            .position(|c| matches!(c, Command::Hardware(HardwareCommand::Fire(_))))
            .expect("fire present");
        assert!(freeze < fire, "freeze must precede fire so the gun holds its lock for the shot");
    }

    #[test]
    fn guilty_sentence_no_targeting_when_disabled() {
        let mut cfg = test_cfg();
        cfg.vision.trial_targeting = false;
        let cmds = sentence_commands(&guilty_verdict(true), &cfg);
        assert!(!cmds.iter().any(|c| matches!(c, Command::Targeting(_))));
        // Still fires — old behaviour, ungated.
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Fire(_)))));
    }

    #[test]
    fn not_guilty_sentence_never_freezes_or_fires() {
        let cfg = test_cfg();
        let cmds = sentence_commands(&guilty_verdict(false), &cfg);
        assert!(!cmds.iter().any(|c| matches!(c, Command::Targeting(_))));
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Fire(_)))));
    }

    #[test]
    fn idle_to_generating_charge_on_start() {
        let cfg = test_cfg();
        let (s, cmds) = step(State::Idle, Event::OperatorStart, &cfg, false);
        assert!(matches!(s, State::GeneratingCharge { .. }));
        assert!(matches!(cmds[0], Command::GenerateCharge));
    }

    #[test]
    fn defendant_button_starts_trial_from_idle_and_darkens_the_lamp() {
        let cfg = test_cfg();
        let (s, cmds) = step(State::Idle, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::GeneratingCharge { .. }));
        assert!(matches!(cmds[0], Command::GenerateCharge));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Off)))));
    }

    #[test]
    fn defendant_button_closes_the_plea_window_early() {
        let cfg = test_cfg();
        let s = State::AwaitingPlea {
            charge: "c".into(),
            deadline: Instant::now() + Duration::from_secs(60), // nowhere near expiry
            paused_remaining: None,
        };
        let (s, cmds) = step(s, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::FlushingPlea { .. }));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Display(DisplayEvent::StopPleaRecording))));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Off)))));
    }

    #[test]
    fn defendant_button_ignored_while_paused_on_the_lawyer_phone() {
        let cfg = test_cfg();
        let s = State::AwaitingPlea {
            charge: "c".into(),
            deadline: Instant::now() + Duration::from_secs(60),
            paused_remaining: Some(Duration::from_secs(30)),
        };
        let (s, cmds) = step(s, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::AwaitingPlea { paused_remaining: Some(_), .. }));
        assert!(cmds.is_empty());
    }

    #[test]
    fn defendant_button_closes_the_cross_answer_window_early() {
        let cfg = test_cfg();
        let s = State::CrossAwaitingAnswer {
            charge: "c".into(),
            plea: "p".into(),
            question: "q?".into(),
            deadline: Instant::now() + Duration::from_secs(60),
            paused_remaining: None,
        };
        let (s, cmds) = step(s, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::CrossFlushingAnswer { .. }));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Display(DisplayEvent::StopPleaRecording))));
    }

    #[test]
    fn defendant_button_ignored_outside_its_states() {
        let cfg = test_cfg();
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() };
        let (s, cmds) = step(s, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::Deliberating { .. }));
        assert!(cmds.is_empty());

        let (s, cmds) = step(State::Maintenance, Event::DefendantButton, &cfg, false);
        assert!(matches!(s, State::Maintenance));
        assert!(cmds.is_empty());
    }

    #[test]
    fn lamp_cues_track_the_idle_and_window_edges() {
        let cfg = test_cfg();
        // Plea window opening cues the "done talking" pulse.
        let (_s, cmds) = begin_awaiting_plea("c".into(), &cfg);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Pulse)))));
        let (_s, cmds) = begin_cross_answer("c".into(), "p".into(), "q?".into(), &cfg);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Pulse)))));
        // E-stop and maintenance exit land in Idle with the attract blink.
        let mid = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() };
        let (_s, cmds) = step(mid, Event::OperatorEmergencyStop, &cfg, false);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Blink)))));
        let (_s, cmds) = step(State::Maintenance, Event::ExitMaintenance, &cfg, false);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Blink)))));
        // Entering maintenance darkens the lamp (trials are blocked).
        let (_s, cmds) = step(State::Idle, Event::EnterMaintenance, &cfg, false);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Led(LedMode::Off)))));
    }

    #[test]
    fn estop_anywhere_returns_to_idle() {
        let cfg = test_cfg();
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() };
        let (s2, _) = step(s, Event::OperatorEmergencyStop, &cfg, false);
        assert!(matches!(s2, State::Idle));
    }

    #[test]
    fn charge_ready_transitions_to_displaying() {
        let cfg = test_cfg();
        let (s, _) = step(State::GeneratingCharge { started_at: Instant::now() },
                          Event::ChargeReady("you stand accused".into()), &cfg, false);
        assert!(matches!(s, State::DisplayingCharge { .. }));
    }

    #[test]
    fn plea_window_waits_for_charge_tts() {
        let cfg = test_cfg();
        let far = Instant::now() + Duration::from_secs(60);
        // Min display passed but TTS still speaking → hold.
        let s = State::DisplayingCharge {
            charge: "c".into(),
            until: Instant::now(),
            tts_done: false,
            watchdog_at: far,
        };
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::DisplayingCharge { .. }));
        // TTS drains → plea window opens (cross=false on the event).
        let (s, cmds) = step(s, Event::TtsFinished, &cfg, false);
        assert!(matches!(s, State::AwaitingPlea { .. }));
        assert!(cmds.iter().any(|c| matches!(
            c,
            Command::Display(DisplayEvent::StartPleaRecording { cross: false, .. })
        )));
    }

    #[test]
    fn charge_tts_before_min_display_waits_for_the_min() {
        let cfg = test_cfg();
        let far = Instant::now() + Duration::from_secs(60);
        let s = State::DisplayingCharge {
            charge: "c".into(),
            until: Instant::now() + Duration::from_secs(30),
            tts_done: false,
            watchdog_at: far,
        };
        // TTS drained early → latch tts_done, keep displaying until `until`.
        let (s, _) = step(s, Event::TtsFinished, &cfg, false);
        match &s {
            State::DisplayingCharge { tts_done, .. } => assert!(tts_done),
            other => panic!("expected DisplayingCharge, got {}", other.name()),
        }
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::DisplayingCharge { .. }));
    }

    #[test]
    fn charge_watchdog_escapes_a_lost_tts_ack() {
        let cfg = test_cfg();
        let s = State::DisplayingCharge {
            charge: "c".into(),
            until: Instant::now(),
            tts_done: false,
            watchdog_at: Instant::now(),
        };
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::AwaitingPlea { .. }));
    }

    #[test]
    fn stt_failure_raises_the_plea_fallback_banner() {
        let cfg = test_cfg();
        let (s, cmds) = step(
            State::Transcribing { charge: "c".into(), audio: vec![1], started_at: Instant::now() },
            Event::TranscriptFailed("kokoro exploded".into()),
            &cfg,
            false,
        );
        assert!(matches!(s, State::Deliberating { .. }));
        assert!(cmds.iter().any(|c| matches!(
            c,
            Command::Display(DisplayEvent::PleaFallback { .. })
        )));
    }

    #[test]
    fn cross_answer_window_is_flagged_cross() {
        let cfg = test_cfg();
        let (_s, cmds) = begin_cross_answer("c".into(), "p".into(), "q?".into(), &cfg);
        assert!(cmds.iter().any(|c| matches!(
            c,
            Command::Display(DisplayEvent::StartPleaRecording { cross: true, .. })
        )));
    }

    #[test]
    fn charge_text_threads_through_to_deliberating() {
        let cfg = test_cfg();
        let charge = "you ate the last donut".to_string();
        // cross-exam off → straight to deliberation.
        let (s, _) = step(
            State::Transcribing { charge: charge.clone(), audio: vec![], started_at: Instant::now() },
            Event::TranscriptReady("i did not".into()), &cfg, false);
        if let State::Deliberating { charge: c, plea, .. } = s {
            assert_eq!(c, charge);
            assert_eq!(plea, "i did not");
        } else { panic!("not deliberating"); }
    }

    #[test]
    fn fallback_verdict_flips_the_face_immediately_but_preannounced_does_not() {
        let cfg = test_cfg();
        // Fallback path (pre_announced=false): reveal is immediate, face flips.
        let (_s, cmds) = step(
            State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() },
            Event::VerdictReady(guilty_verdict(true)),
            &cfg,
            false,
        );
        assert!(cmds.iter().any(|c| matches!(
            c,
            Command::Hardware(HardwareCommand::Face(FacePhase::VerdictGuilty))
        )));
        // Pre-announced path: the verdict service owns the reveal moment — the
        // FSM must NOT flip the face at VerdictReady (it would spoil the verdict).
        let mut v = guilty_verdict(true);
        v.pre_announced = true;
        let (_s, cmds) = step(
            State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() },
            Event::VerdictReady(v),
            &cfg,
            false,
        );
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Face(_)))));
    }

    #[test]
    fn lawyer_call_pauses_and_resumes_the_plea_clock() {
        let cfg = test_cfg();
        let s = State::AwaitingPlea {
            charge: "c".into(),
            deadline: Instant::now() + Duration::from_secs(10),
            paused_remaining: None,
        };
        let (s, cmds) = step(s, Event::LawyerCallStarted, &cfg, false);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Display(DisplayEvent::ClockPaused { .. }))));
        assert!(matches!(s, State::AwaitingPlea { paused_remaining: Some(_), .. }));

        // While paused, a Tick far past the original deadline must NOT flush.
        let s = State::AwaitingPlea {
            charge: "c".into(),
            deadline: Instant::now() - Duration::from_secs(5),
            paused_remaining: Some(Duration::from_secs(7)),
        };
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::AwaitingPlea { paused_remaining: Some(_), .. }));

        // Hanging up restores the frozen remaining time.
        let (s, cmds) = step(s, Event::LawyerCallEnded, &cfg, false);
        match &s {
            State::AwaitingPlea { deadline, paused_remaining, .. } => {
                assert!(paused_remaining.is_none());
                let rem = deadline.saturating_duration_since(Instant::now());
                assert!(rem > Duration::from_secs(6) && rem <= Duration::from_secs(7));
            }
            other => panic!("expected AwaitingPlea, got {}", other.name()),
        }
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Display(DisplayEvent::PhaseDeadline { .. }))));
    }

    #[test]
    fn lawyer_call_pauses_the_cross_answer_clock() {
        let cfg = test_cfg();
        let s = State::CrossAwaitingAnswer {
            charge: "c".into(),
            plea: "p".into(),
            question: "q?".into(),
            deadline: Instant::now() - Duration::from_secs(1), // already expired
            paused_remaining: Some(Duration::from_secs(4)),
        };
        // Paused: expired deadline doesn't flush the answer window.
        let (s, _) = step(s, Event::Tick, &cfg, true);
        assert!(matches!(s, State::CrossAwaitingAnswer { .. }));
        let (s, _) = step(s, Event::LawyerCallEnded, &cfg, true);
        assert!(matches!(s, State::CrossAwaitingAnswer { paused_remaining: None, .. }));
    }

    #[test]
    fn lawyer_events_ignored_outside_recording_windows() {
        let cfg = test_cfg();
        let s = State::Deliberating {
            charge: "c".into(),
            plea: "p".into(),
            started_at: Instant::now(),
        };
        let (s, cmds) = step(s, Event::LawyerCallStarted, &cfg, false);
        assert!(matches!(s, State::Deliberating { .. }));
        assert!(cmds.is_empty());
    }

    #[test]
    fn pronouncing_verdict_watchdog_escapes_to_sentence() {
        let cfg = test_cfg();
        let (s, cmds) = step(
            State::PronouncingVerdict {
                verdict: guilty_verdict(false),
                watchdog_at: Instant::now(),
            },
            Event::Tick,
            &cfg,
            false,
        );
        assert!(matches!(s, State::ExecutingSentence { .. }));
        // The escape runs the full sentence edge, not a bare state swap.
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Display(DisplayEvent::ExecuteSentence { .. }))));
    }

    #[test]
    fn pre_announced_pronouncing_strikes_gavel_at_reveal_only() {
        let cfg = test_cfg();
        let mut v = guilty_verdict(true);
        v.pre_announced = true;
        // Entering PronouncingVerdict on the pre_announced path fires nothing —
        // no gavel, no eye flip, no verdict broadcast (the service owns those).
        let (s, cmds) = begin_pronouncing(v);
        assert!(cmds.is_empty());
        // The gavel lands at the service's reveal beat.
        let (s, cmds) = step(s, Event::VerdictRevealed, &cfg, false);
        assert!(matches!(s, State::PronouncingVerdict { .. }));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Gavel))));
    }

    #[test]
    fn fallback_pronouncing_strikes_gavel_immediately() {
        let v = guilty_verdict(true); // pre_announced = false
        let (_, cmds) = begin_pronouncing(v);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Hardware(HardwareCommand::Gavel))));
    }

    #[test]
    fn pronouncing_verdict_holds_until_watchdog() {
        let cfg = test_cfg();
        let (s, _) = step(
            State::PronouncingVerdict {
                verdict: guilty_verdict(true),
                watchdog_at: Instant::now() + Duration::from_secs(60),
            },
            Event::Tick,
            &cfg,
            false,
        );
        assert!(matches!(s, State::PronouncingVerdict { .. }));
    }

    #[test]
    fn deliberating_fallback_waits_out_the_stream_budget() {
        let cfg = test_cfg(); // verdict_total_timeout_secs = 30
        let total = cfg.inference.verdict_total_timeout_secs;
        // Just past the verdict service's own stream budget: the FSM must NOT
        // speak a fallback yet — the service is still resolving (possibly mid-TTS).
        let started_at = Instant::now() - Duration::from_secs(total + 1);
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at };
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::Deliberating { .. }));
        // Past the grace too → fallback verdict.
        let started_at = Instant::now() - Duration::from_secs(total + VERDICT_FALLBACK_GRACE_SECS + 1);
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at };
        let (s, _) = step(s, Event::Tick, &cfg, false);
        assert!(matches!(s, State::PronouncingVerdict { .. }));
    }

    #[test]
    fn executing_sentence_tick_past_deadline_returns_to_idle() {
        let cfg = test_cfg();
        let v = fallbacks::verdicts::random(1.0);
        let (s, _) = step(
            State::ExecutingSentence { verdict: v, deadline: Instant::now(), hardware_done: true },
            Event::Tick,
            &cfg,
            false,
        );
        assert!(matches!(s, State::Idle));
    }

    #[test]
    fn cross_enabled_routes_first_plea_to_question() {
        let cfg = test_cfg();
        let (s, cmds) = step(
            State::Transcribing { charge: "c".into(), audio: vec![], started_at: Instant::now() },
            Event::TranscriptReady("a real defense".into()), &cfg, true);
        assert!(matches!(s, State::CrossGeneratingQuestion { .. }));
        assert!(cmds.iter().any(|c| matches!(c, Command::CrossExamine { .. })));
    }

    #[test]
    fn cross_skipped_when_no_defense() {
        let cfg = test_cfg();
        let (s, _) = step(
            State::Transcribing { charge: "c".into(), audio: vec![], started_at: Instant::now() },
            Event::TranscriptReady(NO_DEFENSE.into()), &cfg, true);
        assert!(matches!(s, State::Deliberating { .. }));
    }

    #[test]
    fn cross_question_threads_answer_into_deliberation() {
        let cfg = test_cfg();
        // question ready → speaking
        let (s, _) = step(
            State::CrossGeneratingQuestion { charge: "c".into(), plea: "p".into(), started_at: Instant::now() },
            Event::CrossQuestionReady("really?".into()), &cfg, true);
        assert!(matches!(s, State::CrossSpeaking { .. }));
        // tts done → open answer window
        let (s, _) = step(s, Event::TtsFinished, &cfg, true);
        assert!(matches!(s, State::CrossAwaitingAnswer { .. }));
        // answer audio → transcribing
        let (s, _) = step(s, Event::PleaAudioReceived(vec![1, 2, 3]), &cfg, true);
        assert!(matches!(s, State::CrossTranscribing { .. }));
        // answer transcript → deliberate carrying the full exchange
        let (s, cmds) = step(s, Event::TranscriptReady("yes".into()), &cfg, true);
        assert!(matches!(s, State::Deliberating { .. }));
        let cross = cmds.iter().find_map(|c| match c {
            Command::Deliberate { cross, .. } => Some(cross.clone()),
            _ => None,
        }).expect("deliberate command");
        let cross = cross.expect("cross present");
        assert_eq!(cross.question, "really?");
        assert_eq!(cross.answer, "yes");
    }

    #[test]
    fn idle_enters_maintenance() {
        let cfg = test_cfg();
        let (s, cmds) = step(State::Idle, Event::EnterMaintenance, &cfg, false);
        assert!(matches!(s, State::Maintenance));
        assert!(cmds.iter().any(|c| matches!(
            c,
            Command::Display(DisplayEvent::Maintenance { active: true })
        )));
    }

    #[test]
    fn maintenance_blocks_operator_start() {
        let cfg = test_cfg();
        let (s, cmds) = step(State::Maintenance, Event::OperatorStart, &cfg, false);
        assert!(matches!(s, State::Maintenance));
        assert!(cmds.is_empty());
    }

    #[test]
    fn maintenance_exits_to_idle() {
        let cfg = test_cfg();
        let (s, _) = step(State::Maintenance, Event::ExitMaintenance, &cfg, false);
        assert!(matches!(s, State::Idle));
    }

    #[test]
    fn non_idle_ignores_enter_maintenance() {
        let cfg = test_cfg();
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() };
        let (s2, _) = step(s, Event::EnterMaintenance, &cfg, false);
        assert!(matches!(s2, State::Deliberating { .. }));
    }

    #[test]
    fn estop_exits_maintenance() {
        let cfg = test_cfg();
        let (s, _) = step(State::Maintenance, Event::OperatorEmergencyStop, &cfg, false);
        assert!(matches!(s, State::Idle));
    }

    #[test]
    fn cross_question_timeout_falls_through_to_verdict() {
        let cfg = test_cfg();
        let started = Instant::now() - Duration::from_secs(cfg.cross_examination.question_timeout_secs + 1);
        let (s, _) = step(
            State::CrossGeneratingQuestion { charge: "c".into(), plea: "p".into(), started_at: started },
            Event::Tick, &cfg, true);
        assert!(matches!(s, State::Deliberating { .. }));
    }
}
