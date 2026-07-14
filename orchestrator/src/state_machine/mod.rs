use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tokio::time::MissedTickBehavior;
use tracing::info;

use crate::config::Config;
use crate::personas::PersonaRegistry;
use crate::printer::service::PrintJob;
use crate::printer::{Casebook, TrialRecord};

pub mod commands;
pub mod events;
pub mod states;
pub mod transitions;

pub use commands::{Command, TargetingCue};
pub use events::Event;
pub use states::State;

pub struct Runtime {
    state: State,
    cfg: Arc<Config>,
    /// Operator-toggleable cross-examination switch, shared with the display
    /// server's `/operator/cross_exam` endpoint. Read once per event.
    cross_enabled: Arc<AtomicBool>,
    /// Lock-free mirrors of the current state for the maintenance REST gates:
    /// `maintenance` is true while in `State::Maintenance` (opens the direct-
    /// command path); `is_idle` is true in `State::Idle` (gates entry).
    maintenance: Arc<AtomicBool>,
    is_idle: Arc<AtomicBool>,
    /// Read-only mirror of the trial for `GET /trial/state` (lawyer phone).
    /// Written on every transition beside the atomic mirrors above.
    trial_snapshot: Arc<std::sync::RwLock<states::TrialSnapshot>>,
    event_rx: mpsc::Receiver<Event>,
    inference_tx: mpsc::Sender<Command>,
    hardware_tx: mpsc::Sender<Command>,
    display_tx: mpsc::Sender<Command>,
    /// Read once per trial to snapshot the presiding judge's name for the
    /// keepsake (mid-trial persona changes don't apply, by design).
    personas: Arc<RwLock<PersonaRegistry>>,
    /// The append-only trial log; also the source of the case counter.
    casebook: Arc<Casebook>,
    /// Next case number, seeded from the casebook at startup and bumped once per
    /// recorded verdict (so aborted trials don't consume numbers).
    next_case_no: AtomicU64,
    /// Finalized records go here for the printer service to render + emit.
    print_tx: mpsc::Sender<PrintJob>,
    /// Accumulates the in-flight trial's pieces; `None` between trials.
    draft: Option<TrialDraft>,
    /// Drives the turret aiming sequence during trials (arm on deliberation,
    /// freeze-then-fire on guilty, idle between trials). `None` disables it (and
    /// in tests, which don't wire up vision/hardware).
    targeting: Option<Arc<crate::targeting::TargetingController>>,
    /// Captures the guilty "moment of justice" burst for the keepsake. `None`
    /// disables it (and in tests).
    capture: Option<Arc<crate::capture::CaptureController>>,
    /// Operator toggle for the lawyer-phone trial integration; shared with the
    /// display server's `/operator/lawyer_integration` + `/lawyer/event`.
    lawyer_enabled: Arc<AtomicBool>,
    /// Live lawyer-call flag (written by `/lawyer/event`). Read on window-entry
    /// edges so a window that opens mid-call starts paused.
    lawyer_call_active: Arc<AtomicBool>,
    /// Rings the phone as a cross-answer window opens. `None` disables (tests).
    lawyer: Option<Arc<crate::lawyer::LawyerBridge>>,
}

/// Mutable scratchpad that gathers one trial's pieces as they flow past the
/// Runtime. The pure state machine drops charge/plea/cross once the verdict is
/// reached, so the impure shell collects them here and finalizes into a
/// [`TrialRecord`] when sentence execution begins.
struct TrialDraft {
    judge_name: String,
    ts: String,
    charge: String,
    plea: String,
    cross: Option<states::CrossExam>,
    verdict: Option<states::Verdict>,
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Arc<Config>,
        cross_enabled: Arc<AtomicBool>,
        maintenance: Arc<AtomicBool>,
        is_idle: Arc<AtomicBool>,
        trial_snapshot: Arc<std::sync::RwLock<states::TrialSnapshot>>,
        event_rx: mpsc::Receiver<Event>,
        inference_tx: mpsc::Sender<Command>,
        hardware_tx: mpsc::Sender<Command>,
        display_tx: mpsc::Sender<Command>,
        personas: Arc<RwLock<PersonaRegistry>>,
        casebook: Arc<Casebook>,
        print_tx: mpsc::Sender<PrintJob>,
        targeting: Option<Arc<crate::targeting::TargetingController>>,
        capture: Option<Arc<crate::capture::CaptureController>>,
        lawyer_enabled: Arc<AtomicBool>,
        lawyer_call_active: Arc<AtomicBool>,
        lawyer: Option<Arc<crate::lawyer::LawyerBridge>>,
    ) -> Self {
        let next_case_no = AtomicU64::new(casebook.next_case_no());
        Self {
            state: State::Idle,
            cfg,
            cross_enabled,
            maintenance,
            is_idle,
            trial_snapshot,
            event_rx,
            inference_tx,
            hardware_tx,
            display_tx,
            personas,
            casebook,
            next_case_no,
            print_tx,
            draft: None,
            targeting,
            capture,
            lawyer_enabled,
            lawyer_call_active,
            lawyer,
        }
    }

    pub async fn run(mut self) {
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        info!("state machine running, initial state: {}", self.state.name());
        loop {
            tokio::select! {
                Some(ev) = self.event_rx.recv() => self.handle(ev).await,
                _ = ticker.tick() => self.handle(Event::Tick).await,
            }
        }
    }

    async fn handle(&mut self, ev: Event) {
        let prev_name = self.state.name();
        let interesting = !matches!(ev, Event::Tick);
        let prev = std::mem::replace(&mut self.state, State::Idle);
        let cross_enabled = self.cross_enabled.load(Ordering::Relaxed);
        let (next, cmds) = transitions::step(prev, ev, &self.cfg, cross_enabled);
        if next.name() != prev_name {
            info!(from = prev_name, to = next.name(), "state_transition");
        } else if interesting && !cmds.is_empty() {
            tracing::debug!(state = next.name(), "event handled, no transition");
        }

        // Keepsake/casebook: open a fresh draft on trial start, harvest the
        // trial's pieces as they pass, and finalize the moment sentence
        // execution begins (the verdict is final by then). Keyed on the actual
        // Idle→GeneratingCharge edge (not the start *event*) so a stray
        // OperatorStart/DefendantButton in maintenance or mid-trial can never
        // clobber the live draft.
        let is_start = next.name() == "generating_charge" && prev_name != "generating_charge";
        if is_start {
            self.begin_draft().await;
        }
        self.harvest(&next, &cmds);
        let entering_sentence =
            prev_name != "executing_sentence" && next.name() == "executing_sentence";

        self.state = next;
        // Refresh the REST-gate mirrors to match the new state.
        self.maintenance
            .store(matches!(self.state, State::Maintenance), Ordering::Relaxed);
        self.is_idle
            .store(matches!(self.state, State::Idle), Ordering::Relaxed);
        if let Ok(mut snap) = self.trial_snapshot.write() {
            *snap = states::TrialSnapshot::from(&self.state);
        }

        if entering_sentence {
            self.finalize_trial();
        }

        // Trial-targeting: a trial always begins and ends with the gun idle. On
        // start we disarm any manual arm and center; on any return to Idle
        // (normal finish, abort, or e-stop) we disarm and center for the next
        // defendant. Acquire/Freeze are emitted as commands by the transitions;
        // these two entry edges cover every path to a static idle gun.
        let entering_idle = prev_name != "idle" && self.state.name() == "idle";
        if self.cfg.vision.trial_targeting {
            if let Some(t) = &self.targeting {
                if is_start || entering_idle {
                    t.execute(TargetingCue::Idle).await;
                }
            }
        }

        for cmd in cmds {
            self.dispatch(cmd).await;
        }

        // Lawyer-phone trial integration (when enabled): ring the phone as a
        // cross-answer window opens, and pause any freshly opened window if
        // the defendant is *already* on the phone (they picked up during the
        // charge or the judge's question — the call_started event fired before
        // the window existed, so we synthesize the pause on entry).
        if self.lawyer_enabled.load(Ordering::Relaxed) {
            let entered_plea =
                matches!(self.state, State::AwaitingPlea { .. }) && prev_name != "awaiting_plea";
            let entered_cross = matches!(self.state, State::CrossAwaitingAnswer { .. })
                && prev_name != "cross_answer";
            let call_active = self.lawyer_call_active.load(Ordering::Relaxed);
            if entered_cross && !call_active {
                if let (Some(bridge), State::CrossAwaitingAnswer { question, .. }) =
                    (&self.lawyer, &self.state)
                {
                    bridge.ring(format!(
                        "the judge just asked your client: '{question}' — they need \
                         your finest advice before they answer"
                    ));
                }
            }
            if (entered_plea || entered_cross) && call_active {
                let prev = std::mem::replace(&mut self.state, State::Idle);
                let (next, cmds) =
                    transitions::step(prev, Event::LawyerCallStarted, &self.cfg, cross_enabled);
                self.state = next;
                if let Ok(mut snap) = self.trial_snapshot.write() {
                    *snap = states::TrialSnapshot::from(&self.state);
                }
                for cmd in cmds {
                    self.dispatch(cmd).await;
                }
            }
        }
    }

    /// Begin a fresh trial draft, snapshotting the presiding judge and the
    /// wall-clock open time.
    async fn begin_draft(&mut self) {
        let judge_name = self.personas.read().await.active().display_name.clone();
        self.draft = Some(TrialDraft {
            judge_name,
            ts: chrono::Local::now().to_rfc3339(),
            charge: String::new(),
            plea: String::new(),
            cross: None,
            verdict: None,
        });
    }

    /// Copy the trial's pieces out of the new state and the dispatched commands
    /// into the draft. No-op when no trial is in flight.
    fn harvest(&mut self, s: &State, cmds: &[Command]) {
        let Some(draft) = self.draft.as_mut() else { return };
        use State::*;
        match s {
            DisplayingCharge { charge, .. }
            | AwaitingPlea { charge, .. }
            | FlushingPlea { charge, .. }
            | Transcribing { charge, .. } => draft.charge = charge.clone(),
            CrossGeneratingQuestion { charge, plea, .. }
            | CrossSpeaking { charge, plea, .. }
            | CrossAwaitingAnswer { charge, plea, .. }
            | CrossFlushingAnswer { charge, plea, .. }
            | CrossTranscribing { charge, plea, .. }
            | Deliberating { charge, plea, .. } => {
                draft.charge = charge.clone();
                draft.plea = plea.clone();
            }
            PronouncingVerdict { verdict, .. } | ExecutingSentence { verdict, .. } => {
                if draft.verdict.is_none() {
                    draft.verdict = Some(verdict.clone());
                }
            }
            _ => {}
        }
        // The full cross-examination exchange only exists in the Deliberate
        // command (the state keeps the question but not the answer).
        for c in cmds {
            if let Command::Deliberate { cross: Some(cx), .. } = c {
                draft.cross = Some(cx.clone());
            }
        }
    }

    /// Assemble the finalized [`TrialRecord`], append it to the casebook, and
    /// queue it for printing. Assigns the case number here so only completed
    /// verdicts consume one.
    fn finalize_trial(&mut self) {
        let Some(draft) = self.draft.take() else { return };
        let Some(verdict) = draft.verdict else {
            tracing::warn!("reached sentence with no captured verdict; not recording");
            return;
        };
        let case_no = self.next_case_no.fetch_add(1, Ordering::SeqCst);
        let mut record = TrialRecord {
            case_no,
            ts: draft.ts,
            charge: draft.charge,
            plea: draft.plea,
            cross: draft.cross,
            judge_name: draft.judge_name,
            guilty: verdict.guilty,
            deliberation: verdict.deliberation,
            remarks: verdict.remarks,
            key_factor: verdict.key_factor,
            capture_dir: None,
            still_jpeg: None,
        };

        // Guilty verdicts get a "moment of justice" burst: record the capture dir
        // in the casebook now, then hand the record to the capture task, which
        // grabs the frames, attaches the receipt still, and queues the print.
        // (Not-guilty / capture-off prints immediately.)
        let cap = match (record.guilty && self.cfg.capture.enabled).then_some(()) {
            Some(()) => self.capture.clone(),
            None => None,
        };
        if let Some(c) = &cap {
            record.capture_dir = Some(c.case_dir(&record.case_label()).display().to_string());
        }

        match self.casebook.record(&record) {
            Ok(()) => info!(case_no = record.case_no, guilty = record.guilty, "trial recorded"),
            Err(e) => tracing::error!("casebook append failed: {e:#}"),
        }

        if let Some(c) = &cap {
            c.spawn(record, self.print_tx.clone());
        } else if let Err(e) = self.print_tx.try_send(PrintJob::Trial(record)) {
            // Non-blocking: a backed-up printer drops the receipt rather than
            // stalling the trial loop. The casebook line is already durable.
            tracing::warn!("keepsake not queued for print: {e}");
        }
    }

    async fn dispatch(&self, cmd: Command) {
        match cmd {
            Command::GenerateCharge | Command::Transcribe(_) | Command::CrossExamine { .. } | Command::Deliberate { .. } | Command::Speak(_) => {
                if self.inference_tx.send(cmd).await.is_err() {
                    tracing::error!("inference channel closed");
                }
            }
            Command::Hardware(_) => {
                if self.hardware_tx.send(cmd).await.is_err() {
                    tracing::error!("hardware channel closed");
                }
            }
            Command::Targeting(cue) => {
                if let Some(t) = &self.targeting {
                    t.execute(cue).await;
                }
            }
            Command::Display(_) | Command::DisplayBinary(_) => {
                if self.display_tx.send(cmd).await.is_err() {
                    tracing::warn!("display channel closed (no client?)");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use crate::config::*;
    use crate::state_machine::states::Verdict;

    fn mk_cfg() -> Config {
        Config {
            inference: InferenceConfig {
                mode: "mock".into(), base_url: "x".into(), chat_model: "x".into(),
                stt_model: "x".into(), tts_model: "x".into(),
                charge_timeout_secs: 10, verdict_first_token_timeout_secs: 15,
                verdict_total_timeout_secs: 30, stt_timeout_secs: 5, tts_timeout_secs: 10,
                enable_thinking: false, api_key: None,
            },
            hardware: HardwareConfig { driver: "mock".into(), ack_timeout_ms: 1000, bind_addr: "0.0.0.0:0".into(), beacon_port: 0 },
            mock_hw: MockHwConfig { ack_latency_ms: 1, fail_rate: 0.0, simulate_estop_after_secs: 0 },
            mock_inference: MockInferenceConfig::default(),
            squirt: SquirtConfig { duration_ms: 150 },
            trial: TrialConfig { plea_window_secs: 1, charge_display_secs: 0, cooldown_secs: 1, guilty_bias: 1.0 },
            cross_examination: CrossExamConfig { enabled: false, answer_window_secs: 1, question_timeout_secs: 1 },
            display: DisplayConfig { listen_addr: "127.0.0.1:0".into() },
            logging: LoggingConfig { level: "info".into(), log_file: "x".into(), transcripts_jsonl: "x".into() },
            default_persona_id: "judge".into(),
            crimes: CrimesConfig::default(),
            printer: PrinterConfig::default(),
            vision: VisionConfig::default(),
            capture: CaptureConfig::default(),
            lawyer: LawyerConfig::default(),
        }
    }

    fn write_persona(dir: &std::path::Path) {
        std::fs::write(
            dir.join("judge.toml"),
            "id = \"judge\"\ndisplay_name = \"Judge Testwater\"\nsystem_prompt = \"be a judge\"\nguilty_bias = 0.5\ntts_voice = \"bm_george\"\n",
        )
        .unwrap();
        // The registry now requires the shared judge core alongside personas.
        std::fs::write(dir.join("core.md"), "TEST CORE\n\n=== YOUR PERSONA ===\n").unwrap();
    }

    /// Drive a full trial through the Runtime with explicit events (bypassing
    /// the inference/hardware tasks) and assert it finalizes into both the
    /// casebook and the print queue, with all fields harvested correctly.
    #[tokio::test]
    async fn trial_finalizes_into_casebook_and_print_queue() {
        let tag = std::process::id();
        let pdir = std::env::temp_dir().join(format!("wc_sm_personas_{tag}"));
        std::fs::create_dir_all(&pdir).unwrap();
        write_persona(&pdir);
        let book = std::env::temp_dir().join(format!("wc_sm_casebook_{tag}.jsonl"));
        let _ = std::fs::remove_file(&book);

        let personas = Arc::new(RwLock::new(
            PersonaRegistry::load_from_dir(&pdir, "judge").unwrap(),
        ));
        let casebook = Arc::new(Casebook::open(&book));

        let (_event_tx, event_rx) = mpsc::channel::<Event>(16);
        let (inf_tx, _inf_rx) = mpsc::channel::<Command>(16);
        let (hw_tx, _hw_rx) = mpsc::channel::<Command>(16);
        let (disp_tx, _disp_rx) = mpsc::channel::<Command>(64);
        let (print_tx, mut print_rx) = mpsc::channel::<PrintJob>(8);

        let mut rt = Runtime::new(
            Arc::new(mk_cfg()),
            Arc::new(AtomicBool::new(false)), // cross-exam off → simplest path
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(std::sync::RwLock::new(states::TrialSnapshot::default())),
            event_rx,
            inf_tx,
            hw_tx,
            disp_tx,
            personas,
            casebook.clone(),
            print_tx,
            None, // no targeting controller in the unit test
            None, // no capture controller in the unit test
            Arc::new(AtomicBool::new(false)), // lawyer integration off
            Arc::new(AtomicBool::new(false)),
            None, // no lawyer bridge in the unit test
        );

        rt.handle(Event::OperatorStart).await;
        rt.handle(Event::ChargeReady("the CHARGE".into())).await;
        // charge_display_secs = 0, so the charge TTS draining is what opens
        // the plea window.
        rt.handle(Event::TtsFinished).await; // → AwaitingPlea
        rt.handle(Event::PleaAudioReceived(vec![1, 2, 3])).await; // → Transcribing
        rt.handle(Event::TranscriptReady("the PLEA".into())).await; // → Deliberating
        rt.handle(Event::VerdictReady(Verdict {
            guilty: true,
            deliberation: "the DELIB".into(),
            remarks: "the REMARKS".into(),
            key_factor: Some("the FACTOR".into()),
            pre_announced: false,
        }))
        .await; // → PronouncingVerdict
        rt.handle(Event::TtsFinished).await; // → ExecutingSentence → finalize

        // Queued for printing, with every field harvested from the right place.
        let PrintJob::Trial(rec) = print_rx.try_recv().expect("a record was queued for print") else {
            panic!("trial finalization queued a non-trial job");
        };
        assert_eq!(rec.case_no, 1);
        assert_eq!(rec.charge, "the CHARGE");
        assert_eq!(rec.plea, "the PLEA");
        assert_eq!(rec.judge_name, "Judge Testwater");
        assert!(rec.guilty);
        assert_eq!(rec.deliberation, "the DELIB");
        assert_eq!(rec.remarks, "the REMARKS");
        assert_eq!(rec.key_factor.as_deref(), Some("the FACTOR"));

        // Appended to the casebook, and the counter advanced.
        let txt = std::fs::read_to_string(&book).unwrap();
        assert_eq!(txt.lines().count(), 1);
        assert_eq!(casebook.next_case_no(), 2);

        let _ = std::fs::remove_file(&book);
        let _ = std::fs::remove_dir_all(&pdir);
    }
}
