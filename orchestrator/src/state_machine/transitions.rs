use std::time::{Duration, Instant};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::fallbacks;
use crate::hardware::protocol::{HardwareCommand, LightState, PanelPattern};

use super::commands::{Command, TargetingCue};
use super::events::Event;
use super::states::{CrossExam, State, Verdict, NO_DEFENSE};

/// Sentinel for an empty/unintelligible cross-examination answer.
const NO_ANSWER: &str = "[no answer given]";

pub fn step(state: State, event: Event, cfg: &Config, cross_enabled: bool) -> (State, Vec<Command>) {
    use Event::*;
    use State::*;

    if matches!(event, OperatorEmergencyStop) && !matches!(state, Idle) {
        return (
            Idle,
            vec![
                Command::Display(DisplayEvent::Reset),
                Command::Display(DisplayEvent::Idle),
                Command::Hardware(HardwareCommand::Lights(LightState::SplashIdle)),
                Command::Hardware(HardwareCommand::Panel(PanelPattern::Idle)),
            ],
        );
    }

    match (state, event) {
        (Idle, OperatorStart) => (
            GeneratingCharge { started_at: Instant::now() },
            vec![
                Command::GenerateCharge,
                Command::Display(DisplayEvent::Reset),
                Command::Display(DisplayEvent::PhaseDeadline {
                    phase: "generating_charge".into(),
                    deadline_ms: cfg.inference.charge_timeout_secs * 1000,
                }),
                Command::Hardware(HardwareCommand::Panel(PanelPattern::Thinking)),
            ],
        ),
        // Enter the maintenance/test plane — only from Idle. The atomic mirror
        // in `Runtime::handle` opens the direct-command REST gate on entry.
        (Idle, EnterMaintenance) => (
            Maintenance,
            vec![Command::Display(DisplayEvent::Maintenance { active: true })],
        ),
        (Idle, _) => (Idle, vec![]),

        // Maintenance blocks every trial event (no OperatorStart arm); only
        // ExitMaintenance (or the e-stop guard above) leaves it.
        (Maintenance, ExitMaintenance) => (
            Idle,
            vec![
                Command::Display(DisplayEvent::Maintenance { active: false }),
                Command::Display(DisplayEvent::Idle),
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
        (GeneratingCharge { .. }, ChargeFailed(_)) => {
            begin_displaying_charge(fallbacks::charges::random(), cfg)
        }
        (s @ GeneratingCharge { .. }, _) => (s, vec![]),

        (DisplayingCharge { charge, until }, Tick) if Instant::now() >= until => {
            let deadline = Instant::now() + Duration::from_secs(cfg.trial.plea_window_secs);
            (
                AwaitingPlea { charge, deadline },
                vec![
                    Command::Display(DisplayEvent::StartPleaRecording {
                        deadline_ms: cfg.trial.plea_window_secs * 1000,
                    }),
                    Command::Display(DisplayEvent::PhaseDeadline {
                        phase: "awaiting_plea".into(),
                        deadline_ms: cfg.trial.plea_window_secs * 1000,
                    }),
                    Command::Hardware(HardwareCommand::Lights(LightState::SplashArming)),
                ],
            )
        }
        (s @ DisplayingCharge { .. }, _) => (s, vec![]),

        (AwaitingPlea { charge, .. }, PleaAudioReceived(audio)) => begin_transcribing(charge, audio, cfg),
        // The accused pressed the button: reset the plea-window deadline so
        // they get the full talking time from the moment they start speaking.
        (AwaitingPlea { charge, .. }, PleaRecordingStarted) => {
            let deadline = Instant::now() + Duration::from_secs(cfg.trial.plea_window_secs);
            (
                AwaitingPlea { charge, deadline },
                vec![
                    Command::Display(DisplayEvent::PleaRecording { active: true }),
                    Command::Display(DisplayEvent::PhaseDeadline {
                        phase: "awaiting_plea".into(),
                        deadline_ms: cfg.trial.plea_window_secs * 1000,
                    }),
                ],
            )
        }
        (AwaitingPlea { charge, deadline }, Tick) if Instant::now() >= deadline => {
            begin_flushing_plea(charge, cfg)
        }
        (AwaitingPlea { charge, .. }, PleaTimeout) => begin_flushing_plea(charge, cfg),
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
                begin_deliberating(charge, NO_DEFENSE.into(), None, cfg)
            } else {
                (Transcribing { charge, started_at, audio }, vec![])
            }
        }
        (Transcribing { charge, .. }, TranscriptFailed(_)) => {
            begin_deliberating(charge, NO_DEFENSE.into(), None, cfg)
        }
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
        (CrossGeneratingQuestion { charge, plea, .. }, CrossQuestionFailed(_)) => {
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
        (CrossAwaitingAnswer { charge, plea, question, .. }, PleaRecordingStarted) => {
            let window = cfg.cross_examination.answer_window_secs;
            let deadline = Instant::now() + Duration::from_secs(window);
            (
                CrossAwaitingAnswer { charge, plea, question, deadline },
                vec![
                    Command::Display(DisplayEvent::PleaRecording { active: true }),
                    Command::Display(DisplayEvent::PhaseDeadline {
                        phase: "cross_answer".into(),
                        deadline_ms: window * 1000,
                    }),
                ],
            )
        }
        (CrossAwaitingAnswer { charge, plea, question, deadline }, Tick) if Instant::now() >= deadline => {
            begin_cross_flushing(charge, plea, question)
        }
        (CrossAwaitingAnswer { charge, plea, question, .. }, PleaTimeout) => {
            begin_cross_flushing(charge, plea, question)
        }
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
                begin_deliberating(charge, plea, Some(CrossExam { question, answer: NO_ANSWER.into() }), cfg)
            } else {
                (CrossTranscribing { charge, plea, question, started_at, audio }, vec![])
            }
        }
        (CrossTranscribing { charge, plea, question, .. }, TranscriptFailed(_)) => {
            begin_deliberating(charge, plea, Some(CrossExam { question, answer: NO_ANSWER.into() }), cfg)
        }
        (s @ CrossTranscribing { .. }, _) => (s, vec![]),

        (Deliberating { .. }, VerdictReady(v)) => begin_pronouncing(v),
        (Deliberating { started_at, charge, plea }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.inference.verdict_total_timeout_secs) {
                begin_pronouncing(fallbacks::verdicts::random(cfg.trial.guilty_bias))
            } else {
                (Deliberating { started_at, charge, plea }, vec![])
            }
        }
        (Deliberating { .. }, VerdictFailed(_)) => {
            begin_pronouncing(fallbacks::verdicts::random(cfg.trial.guilty_bias))
        }
        (s @ Deliberating { .. }, _) => (s, vec![]),

        (PronouncingVerdict { verdict, .. }, TtsFinished) => {
            let mut cmds = sentence_commands(&verdict, cfg);
            // Watchdog: if no HardwareAck/Error arrives (e.g. MCU disconnected
            // with the TCP driver), the Tick handler below escapes to Idle.
            // Sized for the full sentence sequence — lights + chained squirt
            // bursts at max intensity + ack round-trips with some headroom.
            const SENTENCE_WATCHDOG_SECS: u64 = 60;
            let deadline = Instant::now() + Duration::from_secs(SENTENCE_WATCHDOG_SECS);
            cmds.push(Command::Display(DisplayEvent::PhaseDeadline {
                phase: "executing_sentence".into(),
                deadline_ms: SENTENCE_WATCHDOG_SECS * 1000,
            }));
            (ExecutingSentence { verdict, deadline, hardware_done: false }, cmds)
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
                    Command::Hardware(HardwareCommand::Lights(LightState::SplashIdle)),
                    Command::Hardware(HardwareCommand::Panel(PanelPattern::Idle)),
                ],
            )
        }
        (ExecutingSentence { deadline, .. }, Tick) if Instant::now() >= deadline => {
            (Idle, vec![Command::Display(DisplayEvent::Idle)])
        }
        (s @ ExecutingSentence { .. }, _) => (s, vec![]),

        (Error { until, .. }, Tick) if Instant::now() >= until => {
            (Idle, vec![Command::Display(DisplayEvent::Idle)])
        }
        (s @ Error { .. }, _) => (s, vec![]),
    }
}

fn begin_displaying_charge(text: String, cfg: &Config) -> (State, Vec<Command>) {
    let until = Instant::now() + Duration::from_secs(cfg.trial.charge_display_secs);
    (
        State::DisplayingCharge { charge: text.clone(), until },
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

fn begin_transcribing(charge: String, audio: Vec<u8>, cfg: &Config) -> (State, Vec<Command>) {
    (
        State::Transcribing { charge, audio: audio.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Display(DisplayEvent::Transcribing),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "transcribing".into(),
                deadline_ms: cfg.inference.stt_timeout_secs * 1000,
            }),
            Command::Transcribe(audio),
        ],
    )
}

const PLEA_FLUSH_GRACE_MS: u64 = 2000;

fn begin_flushing_plea(charge: String, _cfg: &Config) -> (State, Vec<Command>) {
    let hard_deadline = Instant::now() + Duration::from_millis(PLEA_FLUSH_GRACE_MS);
    (
        State::FlushingPlea { charge, hard_deadline },
        vec![Command::Display(DisplayEvent::StopPleaRecording)],
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
            deadline_ms: cfg.inference.verdict_total_timeout_secs * 1000,
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
            Command::Hardware(HardwareCommand::Panel(PanelPattern::Thinking)),
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
        State::CrossAwaitingAnswer { charge, plea, question, deadline },
        vec![
            Command::Display(DisplayEvent::StartPleaRecording { deadline_ms: window * 1000 }),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "cross_answer".into(),
                deadline_ms: window * 1000,
            }),
            Command::Hardware(HardwareCommand::Lights(LightState::SplashArming)),
        ],
    )
}

fn begin_cross_flushing(charge: String, plea: String, question: String) -> (State, Vec<Command>) {
    let hard_deadline = Instant::now() + Duration::from_millis(PLEA_FLUSH_GRACE_MS);
    (
        State::CrossFlushingAnswer { charge, plea, question, hard_deadline },
        vec![Command::Display(DisplayEvent::StopPleaRecording)],
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
            Command::Display(DisplayEvent::Transcribing),
            Command::Display(DisplayEvent::PhaseDeadline {
                phase: "transcribing".into(),
                deadline_ms: cfg.inference.stt_timeout_secs * 1000,
            }),
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
    }
    cmds.push(Command::Hardware(HardwareCommand::Gavel));
    (State::PronouncingVerdict { verdict: v, audio_done: false }, cmds)
}

fn sentence_commands(v: &Verdict, cfg: &Config) -> Vec<Command> {
    let mut cmds = vec![Command::Display(DisplayEvent::ExecuteSentence { guilty: v.guilty })];
    if v.guilty {
        cmds.push(Command::Hardware(HardwareCommand::Lights(LightState::Guilty)));
        // Freeze the aim before firing (when trial-targeting): the turret holds
        // where vision locked it and the fire gate goes transparent, so the shot
        // lands on the defendant. Ordered before Fire — dispatched sequentially.
        if cfg.vision.trial_targeting {
            cmds.push(Command::Targeting(TargetingCue::Freeze));
        }
        cmds.push(Command::Hardware(HardwareCommand::Fire(cfg.squirt.duration_ms)));
        cmds.push(Command::Display(DisplayEvent::PlayCue { name: "organ_guilty".into() }));
    } else {
        cmds.push(Command::Hardware(HardwareCommand::Lights(LightState::NotGuilty)));
        cmds.push(Command::Display(DisplayEvent::PlayCue { name: "choir_acquittal".into() }));
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
                tts_model: "x".into(), tts_voice: "x".into(),
                charge_timeout_secs: 10, verdict_first_token_timeout_secs: 15,
                verdict_total_timeout_secs: 30, stt_timeout_secs: 5, tts_timeout_secs: 10,
                enable_thinking: false, api_key: None,
            },
            hardware: HardwareConfig { driver: "mock".into(), serial_port: "x".into(), baud: 0, ack_timeout_ms: 1000, bind_addr: "0.0.0.0:0".into() },
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
        assert!(freeze < fire, "freeze must precede fire so the gate is transparent");
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
