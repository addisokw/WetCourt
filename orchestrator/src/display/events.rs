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
    Transcribing,
    TranscriptReady { text: String },
    DeliberationToken { text: String },
    DeliberationComplete,
    Verdict { guilty: bool, intensity: u8, remarks: String },
    ExecuteSentence { guilty: bool },
    PlayCue { name: String },
    Cooldown,
    #[allow(dead_code)]
    Error { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Ready,
    PleaAudioChunk,
    PleaAudioComplete,
    TtsFinished,
    CueFinished { name: String },
}
