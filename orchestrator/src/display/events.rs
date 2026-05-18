use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisplayEvent {
    Reset,
    Idle,
    ShowCharge { text: String },
    /// Per-utterance emotion vector for downstream face animation (ACE
    /// Audio2Face-3D). Emitted by the LLM stage before `TtsAudio`. Keys are
    /// the 10 A2F-3D emotion names lowercased (amazement, anger, cheekiness,
    /// disgust, fear, grief, joy, outofbreath, pain, sadness); values are 0..1.
    /// Browser frontend ignores it; UE renderer applies it as
    /// FAudio2FaceEmotion overrides.
    TtsEmotion {
        emotions: BTreeMap<String, f32>,
        overall_strength: f32,
        override_strength: f32,
    },
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
