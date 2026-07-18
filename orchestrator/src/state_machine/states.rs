use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Sentinel transcript used when the defendant said nothing intelligible. Shared
/// by the STT path (which emits it) and the state machine (which skips
/// cross-examination on it). Single source of truth for the literal.
pub const NO_DEFENSE: &str = "[no defense offered]";

/// One judge follow-up and the defendant's reply, threaded into deliberation
/// when cross-examination ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossExam {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub guilty: bool,
    pub deliberation: String,
    pub remarks: String,
    /// The 2–4 word factor the judge named as deciding the case ("sincere
    /// apology", "bragged about it"). Parsed from the `KEY_FACTOR:` marker line;
    /// surfaced on the case screen so the crowd learns the rules by watching.
    /// `None` on fallback verdicts or when the model omitted the marker.
    #[serde(default)]
    pub key_factor: Option<String>,
    /// True when the verdict service has already streamed TTS audio to the
    /// frontend during deliberation (pipelined LLM→TTS path). The state
    /// machine then skips the redundant `Speak` command in PronouncingVerdict.
    #[serde(default)]
    pub pre_announced: bool,
}

#[derive(Debug, Clone)]
pub enum State {
    Idle,
    /// Operator maintenance/test mode. Entered only from `Idle`; blocks trials
    /// while active so the console can drive hardware directly. Exited back to
    /// `Idle` via `ExitMaintenance` (or an e-stop).
    Maintenance,
    GeneratingCharge { started_at: Instant },
    /// The charge is on screen and being read aloud. Advances once BOTH the
    /// minimum display time (`until`) has passed and the charge TTS has drained
    /// (`tts_done`), so the plea window can never open over the judge's voice;
    /// `watchdog_at` escapes if the TtsFinished ack is lost.
    DisplayingCharge { charge: String, until: Instant, tts_done: bool, watchdog_at: Instant },
    /// Plea window. `paused_remaining` is set while the defendant is on the
    /// lawyer phone: the countdown freezes with that much time left and the
    /// deadline is restored when the call ends. `recording` is false until the
    /// defendant presses the button to START speaking — so the first press
    /// starts the plea and the second press ends it (people press the button to
    /// begin, and used to accidentally end an auto-started plea before speaking).
    AwaitingPlea {
        charge: String,
        deadline: Instant,
        paused_remaining: Option<Duration>,
        recording: bool,
    },
    /// Plea window elapsed; we've told the frontend to stop recording but are
    /// waiting briefly for its in-flight audio upload to arrive before
    /// committing to transcription with whatever bytes (if any) we have.
    FlushingPlea { charge: String, hard_deadline: Instant },
    Transcribing { charge: String, audio: Vec<u8>, started_at: Instant },
    /// Cross-examination: the judge is composing one pointed follow-up question
    /// from the charge + first plea.
    CrossGeneratingQuestion { charge: String, plea: String, started_at: Instant },
    /// The question is being displayed and spoken; we open the answer window
    /// once its TTS drains (or a watchdog fires).
    CrossSpeaking { charge: String, plea: String, question: String, started_at: Instant },
    /// Recording the defendant's answer (reuses the plea-recording machinery,
    /// including the lawyer-phone clock pause).
    CrossAwaitingAnswer {
        charge: String,
        plea: String,
        question: String,
        deadline: Instant,
        paused_remaining: Option<Duration>,
        /// False until the defendant presses to START answering (first press
        /// starts, second press ends) — same press-to-record flow as the plea.
        recording: bool,
    },
    /// Answer window elapsed; brief grace for the in-flight audio upload.
    CrossFlushingAnswer { charge: String, plea: String, question: String, hard_deadline: Instant },
    /// Transcribing the answer before deliberation.
    CrossTranscribing { charge: String, plea: String, question: String, audio: Vec<u8>, started_at: Instant },
    Deliberating { charge: String, plea: String, started_at: Instant },
    /// Speaking the verdict. Advances on `TtsFinished` (from the browser or the
    /// TTS self-ack timer); `watchdog_at` is the escape hatch if that event is
    /// lost — the only thing standing between a dead TTS task and a wedged trial.
    PronouncingVerdict { verdict: Verdict, watchdog_at: Instant },
    ExecutingSentence { verdict: Verdict, deadline: Instant, hardware_done: bool },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Maintenance => "maintenance",
            State::GeneratingCharge { .. } => "generating_charge",
            State::DisplayingCharge { .. } => "displaying_charge",
            State::AwaitingPlea { .. } => "awaiting_plea",
            State::FlushingPlea { .. } => "awaiting_plea",
            State::Transcribing { .. } => "transcribing",
            State::CrossGeneratingQuestion { .. } => "cross_examining",
            State::CrossSpeaking { .. } => "cross_examining",
            State::CrossAwaitingAnswer { .. } => "cross_answer",
            State::CrossFlushingAnswer { .. } => "cross_answer",
            State::CrossTranscribing { .. } => "transcribing",
            State::Deliberating { .. } => "deliberating",
            State::PronouncingVerdict { .. } => "pronouncing_verdict",
            State::ExecutingSentence { .. } => "executing_sentence",
        }
    }
}

/// Read-only mirror of the trial, refreshed on every FSM transition. Serves
/// two consumers: `GET /trial/state` (the lawyer-phone service polls it at
/// call start so the AI lawyer knows the live charge) and the WebSocket
/// connect-time `Snapshot` event (so a console or audience monitor that
/// (re)connects mid-trial resyncs instead of showing stale idle). Derived from
/// whatever the `State` variant happens to carry; fields the current phase
/// doesn't know are simply absent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrialSnapshot {
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plea: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<VerdictSnapshot>,
    /// Absolute deadline of the current window (plea/answer/sentence hold),
    /// when the phase has one. Not serialized — the WS layer converts it to a
    /// remaining-ms countdown at connect time.
    #[serde(skip)]
    pub deadline: Option<Instant>,
    /// Set while the window's clock is paused for a lawyer consultation: the
    /// frozen remaining time. Not serialized (WS snapshot only).
    #[serde(skip)]
    pub paused_remaining: Option<Duration>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerdictSnapshot {
    pub guilty: bool,
    pub remarks: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_factor: Option<String>,
}

impl From<&State> for TrialSnapshot {
    fn from(state: &State) -> Self {
        let phase = state.name();
        let mut snap = TrialSnapshot { phase, ..Default::default() };
        match state {
            State::DisplayingCharge { charge, until, .. } => {
                snap.charge = Some(charge.clone());
                snap.deadline = Some(*until);
            }
            State::AwaitingPlea { charge, deadline, paused_remaining, .. } => {
                snap.charge = Some(charge.clone());
                snap.deadline = Some(*deadline);
                snap.paused_remaining = *paused_remaining;
            }
            State::FlushingPlea { charge, .. } | State::Transcribing { charge, .. } => {
                snap.charge = Some(charge.clone());
            }
            State::CrossGeneratingQuestion { charge, plea, .. } => {
                snap.charge = Some(charge.clone());
                snap.plea = Some(plea.clone());
            }
            State::CrossSpeaking { charge, plea, question, .. } => {
                snap.charge = Some(charge.clone());
                snap.plea = Some(plea.clone());
                snap.cross_question = Some(question.clone());
            }
            State::CrossAwaitingAnswer { charge, plea, question, deadline, paused_remaining, .. } => {
                snap.charge = Some(charge.clone());
                snap.plea = Some(plea.clone());
                snap.cross_question = Some(question.clone());
                snap.deadline = Some(*deadline);
                snap.paused_remaining = *paused_remaining;
            }
            State::CrossFlushingAnswer { charge, plea, question, .. }
            | State::CrossTranscribing { charge, plea, question, .. } => {
                snap.charge = Some(charge.clone());
                snap.plea = Some(plea.clone());
                snap.cross_question = Some(question.clone());
            }
            State::Deliberating { charge, plea, .. } => {
                snap.charge = Some(charge.clone());
                snap.plea = Some(plea.clone());
            }
            State::PronouncingVerdict { verdict, .. } => {
                snap.verdict = Some(VerdictSnapshot::from(verdict));
            }
            State::ExecutingSentence { verdict, deadline, .. } => {
                snap.verdict = Some(VerdictSnapshot::from(verdict));
                snap.deadline = Some(*deadline);
            }
            _ => {}
        }
        snap
    }
}

impl From<&Verdict> for VerdictSnapshot {
    fn from(v: &Verdict) -> Self {
        Self { guilty: v.guilty, remarks: v.remarks.clone(), key_factor: v.key_factor.clone() }
    }
}
