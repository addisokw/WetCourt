use std::time::{Duration, Instant};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::fallbacks;
use crate::hardware::protocol::{HardwareCommand, LightState, PanelPattern};

use super::commands::Command;
use super::events::Event;
use super::states::{State, Verdict};

pub fn step(state: State, event: Event, cfg: &Config) -> (State, Vec<Command>) {
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
                Command::Hardware(HardwareCommand::Panel(PanelPattern::Thinking)),
            ],
        ),
        (Idle, _) => (Idle, vec![]),

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
                    Command::Hardware(HardwareCommand::Lights(LightState::SplashArming)),
                ],
            )
        }
        (s @ DisplayingCharge { .. }, _) => (s, vec![]),

        (AwaitingPlea { charge, .. }, PleaAudioReceived(audio)) => begin_transcribing(charge, audio, cfg),
        (AwaitingPlea { charge, deadline }, Tick) if Instant::now() >= deadline => {
            begin_transcribing(charge, Vec::new(), cfg)
        }
        (AwaitingPlea { charge, .. }, PleaTimeout) => begin_transcribing(charge, Vec::new(), cfg),
        (s @ AwaitingPlea { .. }, _) => (s, vec![]),

        (Transcribing { charge, .. }, TranscriptReady(text)) => begin_deliberating(charge, text, cfg),
        (Transcribing { charge, started_at, audio }, Tick) => {
            if started_at.elapsed() > Duration::from_secs(cfg.inference.stt_timeout_secs) {
                begin_deliberating(charge, "[no defense offered]".into(), cfg)
            } else {
                (Transcribing { charge, started_at, audio }, vec![])
            }
        }
        (Transcribing { charge, .. }, TranscriptFailed(_)) => {
            begin_deliberating(charge, "[no defense offered]".into(), cfg)
        }
        (s @ Transcribing { .. }, _) => (s, vec![]),

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
            let cmds = sentence_commands(&verdict, cfg);
            (ExecutingSentence { verdict, hardware_done: false }, cmds)
        }
        (s @ PronouncingVerdict { .. }, _) => (s, vec![]),

        (ExecutingSentence { .. }, HardwareAck(_)) | (ExecutingSentence { .. }, HardwareError(_)) => {
            let until = Instant::now() + Duration::from_secs(cfg.trial.cooldown_secs);
            (
                Cooldown { until },
                vec![
                    Command::Display(DisplayEvent::Cooldown),
                    Command::Hardware(HardwareCommand::Lights(LightState::SplashIdle)),
                    Command::Hardware(HardwareCommand::Panel(PanelPattern::Idle)),
                ],
            )
        }
        (s @ ExecutingSentence { .. }, _) => (s, vec![]),

        (Cooldown { until }, Tick) if Instant::now() >= until => {
            (Idle, vec![Command::Display(DisplayEvent::Idle)])
        }
        (s @ Cooldown { .. }, _) => (s, vec![]),

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
            Command::Speak(text),
        ],
    )
}

fn begin_transcribing(charge: String, audio: Vec<f32>, _cfg: &Config) -> (State, Vec<Command>) {
    (
        State::Transcribing { charge, audio: audio.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::StopPleaRecording),
            Command::Display(DisplayEvent::Transcribing),
            Command::Transcribe(audio),
        ],
    )
}

fn begin_deliberating(charge: String, plea: String, _cfg: &Config) -> (State, Vec<Command>) {
    (
        State::Deliberating { charge: charge.clone(), plea: plea.clone(), started_at: Instant::now() },
        vec![
            Command::Display(DisplayEvent::TranscriptReady { text: plea.clone() }),
            Command::Deliberate { charge, plea },
        ],
    )
}

fn begin_pronouncing(v: Verdict) -> (State, Vec<Command>) {
    (
        State::PronouncingVerdict { verdict: v.clone(), audio_done: false },
        vec![
            Command::Display(DisplayEvent::Verdict {
                guilty: v.guilty,
                intensity: v.intensity,
                remarks: v.remarks.clone(),
            }),
            Command::Speak(v.deliberation.clone()),
            Command::Hardware(HardwareCommand::Gavel),
        ],
    )
}

fn sentence_commands(v: &Verdict, cfg: &Config) -> Vec<Command> {
    let mut cmds = vec![Command::Display(DisplayEvent::ExecuteSentence { guilty: v.guilty })];
    if v.guilty {
        cmds.push(Command::Hardware(HardwareCommand::Lights(LightState::Guilty)));
        cmds.push(Command::Hardware(HardwareCommand::Fire(
            cfg.squirt_intensity.duration_ms(v.intensity),
        )));
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
            hardware: HardwareConfig { driver: "mock".into(), serial_port: "x".into(), baud: 0, ack_timeout_ms: 1000 },
            mock_hw: MockHwConfig { ack_latency_ms: 1, fail_rate: 0.0, simulate_estop_after_secs: 0 },
            mock_inference: MockInferenceConfig::default(),
            squirt_intensity: SquirtIntensity { level_1: 60, level_2: 100, level_3: 150, level_4: 200, level_5: 280 },
            trial: TrialConfig { plea_window_secs: 1, charge_display_secs: 1, cooldown_secs: 1, guilty_bias: 1.0 },
            display: DisplayConfig { listen_addr: "127.0.0.1:0".into() },
            logging: LoggingConfig { level: "info".into(), log_file: "x".into(), transcripts_jsonl: "x".into() },
        }
    }

    #[test]
    fn idle_to_generating_charge_on_start() {
        let cfg = test_cfg();
        let (s, cmds) = step(State::Idle, Event::OperatorStart, &cfg);
        assert!(matches!(s, State::GeneratingCharge { .. }));
        assert!(matches!(cmds[0], Command::GenerateCharge));
    }

    #[test]
    fn estop_anywhere_returns_to_idle() {
        let cfg = test_cfg();
        let s = State::Deliberating { charge: "c".into(), plea: "p".into(), started_at: Instant::now() };
        let (s2, _) = step(s, Event::OperatorEmergencyStop, &cfg);
        assert!(matches!(s2, State::Idle));
    }

    #[test]
    fn charge_ready_transitions_to_displaying() {
        let cfg = test_cfg();
        let (s, _) = step(State::GeneratingCharge { started_at: Instant::now() },
                          Event::ChargeReady("you stand accused".into()), &cfg);
        assert!(matches!(s, State::DisplayingCharge { .. }));
    }

    #[test]
    fn charge_text_threads_through_to_deliberating() {
        let cfg = test_cfg();
        let charge = "you ate the last donut".to_string();
        let (s, _) = step(
            State::Transcribing { charge: charge.clone(), audio: vec![], started_at: Instant::now() },
            Event::TranscriptReady("i did not".into()), &cfg);
        if let State::Deliberating { charge: c, plea, .. } = s {
            assert_eq!(c, charge);
            assert_eq!(plea, "i did not");
        } else { panic!("not deliberating"); }
    }

    #[test]
    fn cooldown_tick_returns_to_idle() {
        let cfg = test_cfg();
        let (s, _) = step(State::Cooldown { until: Instant::now() }, Event::Tick, &cfg);
        assert!(matches!(s, State::Idle));
    }
}
