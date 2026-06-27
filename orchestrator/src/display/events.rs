use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisplayEvent {
    Reset,
    Idle,
    ShowCharge { text: String },
    /// "next binary frame is audio in this format" — emitted before each chunk
    /// so frontend knows how to decode. May appear multiple times per utterance
    /// in the pipelined LLM→TTS path.
    TtsAudio { format: String },
    /// "no more audio chunks for this utterance" — frontend schedules a
    /// `tts_finished` ClientEvent for after the queued audio drains.
    TtsEnd,
    StartPleaRecording { deadline_ms: u64 },
    StopPleaRecording,
    /// Broadcast when the operator's microphone actually starts/stops capturing
    /// — distinct from the plea *window* opening. Drives the case-view prompt
    /// ("press to begin" → "press to end") on read-only monitors.
    PleaRecording { active: bool },
    /// Operator-facing countdown helper. Emitted whenever the state machine
    /// enters a state with a deadline; `deadline_ms` is the duration from
    /// emission until the watchdog/timeout fires.
    PhaseDeadline { phase: String, deadline_ms: u64 },
    /// "The court finds the defendant…" preamble is done; pause begins. The
    /// operator console plays an ambient pad and viewers dim until TheaterEnd.
    TheaterStart,
    TheaterEnd,
    Transcribing,
    TranscriptReady { text: String },
    /// The judge's cross-examination follow-up question; shown to viewers while
    /// it's spoken, before the answer-recording window opens.
    CrossQuestion { text: String },
    DeliberationToken { text: String },
    DeliberationComplete,
    Verdict { guilty: bool, remarks: String },
    ExecuteSentence { guilty: bool },
    PlayCue { name: String },
    /// A device announced itself (`HELLO <role>`) and was accepted; surfaced to
    /// the maintenance console so its tab enables.
    DeviceConnected { role: String, addr: String },
    /// A device's connection dropped.
    DeviceDisconnected { role: String },
    /// Maintenance mode entered/left — confirms the FSM transition to the
    /// console (whose direct-control tabs gate on it).
    Maintenance { active: bool },
    #[allow(dead_code)]
    Error { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Ready,
    PleaRecordingStarted,
    PleaAudioChunk,
    PleaAudioComplete,
    TtsFinished,
    CueFinished { name: String },
}
