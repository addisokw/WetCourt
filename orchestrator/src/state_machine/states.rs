use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub guilty: bool,
    pub deliberation: String,
    pub remarks: String,
    /// True when the verdict service has already streamed TTS audio to the
    /// frontend during deliberation (pipelined LLM→TTS path). The state
    /// machine then skips the redundant `Speak` command in PronouncingVerdict.
    #[serde(default)]
    pub pre_announced: bool,
}

#[derive(Debug, Clone)]
pub enum State {
    Idle,
    GeneratingCharge { started_at: Instant },
    DisplayingCharge { charge: String, until: Instant },
    AwaitingPlea { charge: String, deadline: Instant },
    /// Plea window elapsed; we've told the frontend to stop recording but are
    /// waiting briefly for its in-flight audio upload to arrive before
    /// committing to transcription with whatever bytes (if any) we have.
    FlushingPlea { charge: String, hard_deadline: Instant },
    Transcribing { charge: String, audio: Vec<u8>, started_at: Instant },
    Deliberating { charge: String, plea: String, started_at: Instant },
    PronouncingVerdict { verdict: Verdict, audio_done: bool },
    ExecutingSentence { verdict: Verdict, deadline: Instant, hardware_done: bool },
    Error { message: String, until: Instant },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::GeneratingCharge { .. } => "generating_charge",
            State::DisplayingCharge { .. } => "displaying_charge",
            State::AwaitingPlea { .. } => "awaiting_plea",
            State::FlushingPlea { .. } => "awaiting_plea",
            State::Transcribing { .. } => "transcribing",
            State::Deliberating { .. } => "deliberating",
            State::PronouncingVerdict { .. } => "pronouncing_verdict",
            State::ExecutingSentence { .. } => "executing_sentence",
            State::Error { .. } => "error",
        }
    }
}
