use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub guilty: bool,
    pub intensity: u8,
    pub deliberation: String,
    pub remarks: String,
}

#[derive(Debug, Clone)]
pub enum State {
    Idle,
    GeneratingCharge { started_at: Instant },
    DisplayingCharge { charge: String, until: Instant },
    AwaitingPlea { charge: String, deadline: Instant },
    Transcribing { charge: String, audio: Vec<f32>, started_at: Instant },
    Deliberating { charge: String, plea: String, started_at: Instant },
    PronouncingVerdict { verdict: Verdict, audio_done: bool },
    ExecutingSentence { verdict: Verdict, hardware_done: bool },
    Cooldown { until: Instant },
    Error { message: String, until: Instant },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::GeneratingCharge { .. } => "generating_charge",
            State::DisplayingCharge { .. } => "displaying_charge",
            State::AwaitingPlea { .. } => "awaiting_plea",
            State::Transcribing { .. } => "transcribing",
            State::Deliberating { .. } => "deliberating",
            State::PronouncingVerdict { .. } => "pronouncing_verdict",
            State::ExecutingSentence { .. } => "executing_sentence",
            State::Cooldown { .. } => "cooldown",
            State::Error { .. } => "error",
        }
    }
}
