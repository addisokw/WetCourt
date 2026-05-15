use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::state_machine::{Command, Event};

/// Phase 1 mock TTS:
///  - Sends a `tts_audio` JSON header so the protocol round-trip is exercised.
///  - Self-emits TtsFinished after a configurable delay so the state machine
///    advances even with no browser connected (doc §5.1: trials complete with
///    or without the frontend).
pub async fn mock(
    cfg: Arc<Config>,
    text: String,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    let _ = text;
    let _ = display_tx
        .send(Command::Display(DisplayEvent::TtsAudio { format: "wav".into() }))
        .await;
    // (Real binary frame emission lives in the display layer; mock skips audio bytes.)
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.tts_latency_ms)).await;
    let _ = event_tx.send(Event::TtsFinished).await;
}
