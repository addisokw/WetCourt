use serde::{Deserialize, Serialize};
use std::time::Instant;

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
    DisplayingCharge { charge: String, until: Instant },
    AwaitingPlea { charge: String, deadline: Instant },
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
    /// Recording the defendant's answer (reuses the plea-recording machinery).
    CrossAwaitingAnswer { charge: String, plea: String, question: String, deadline: Instant },
    /// Answer window elapsed; brief grace for the in-flight audio upload.
    CrossFlushingAnswer { charge: String, plea: String, question: String, hard_deadline: Instant },
    /// Transcribing the answer before deliberation.
    CrossTranscribing { charge: String, plea: String, question: String, audio: Vec<u8>, started_at: Instant },
    Deliberating { charge: String, plea: String, started_at: Instant },
    PronouncingVerdict { verdict: Verdict, audio_done: bool },
    ExecutingSentence { verdict: Verdict, deadline: Instant, hardware_done: bool },
    Error { message: String, until: Instant },
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
            State::Error { .. } => "error",
        }
    }
}

/// Read-only mirror of the trial for `GET /trial/state` — the lawyer-phone
/// service polls it at call start so the AI lawyer knows the live charge.
/// Derived from whatever the `State` variant happens to carry; fields the
/// current phase doesn't know are simply absent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrialSnapshot {
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plea: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<VerdictSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerdictSnapshot {
    pub guilty: bool,
    pub remarks: String,
}

impl From<&State> for TrialSnapshot {
    fn from(state: &State) -> Self {
        let phase = state.name();
        let (charge, plea, verdict) = match state {
            State::DisplayingCharge { charge, .. } => (Some(charge.clone()), None, None),
            State::AwaitingPlea { charge, .. } | State::FlushingPlea { charge, .. } => {
                (Some(charge.clone()), None, None)
            }
            State::Transcribing { charge, .. } => (Some(charge.clone()), None, None),
            State::CrossGeneratingQuestion { charge, plea, .. }
            | State::CrossSpeaking { charge, plea, .. }
            | State::CrossAwaitingAnswer { charge, plea, .. }
            | State::CrossFlushingAnswer { charge, plea, .. }
            | State::CrossTranscribing { charge, plea, .. }
            | State::Deliberating { charge, plea, .. } => {
                (Some(charge.clone()), Some(plea.clone()), None)
            }
            State::PronouncingVerdict { verdict, .. }
            | State::ExecutingSentence { verdict, .. } => (
                None,
                None,
                Some(VerdictSnapshot {
                    guilty: verdict.guilty,
                    remarks: verdict.remarks.clone(),
                }),
            ),
            _ => (None, None, None),
        };
        Self { phase, charge, plea, verdict }
    }
}
