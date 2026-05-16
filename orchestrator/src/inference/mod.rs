use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use crate::config::Config;
use crate::state_machine::{Command, Event};

pub mod a2f;
pub mod charge;
pub mod client;
pub mod stt;
pub mod tts;
pub mod verdict;

pub async fn run(
    cfg: Arc<Config>,
    mut cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    let mode = cfg.inference.mode.as_str();
    if mode != "real" && mode != "mock" {
        warn!(mode, "unknown inference.mode; falling back to 'mock'");
    }
    let real = mode == "real";

    while let Some(cmd) = cmd_rx.recv().await {
        let cfg = cfg.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        match cmd {
            Command::GenerateCharge => {
                tokio::spawn(async move {
                    if real { charge::real(cfg, event_tx).await }
                    else    { charge::mock(cfg, event_tx).await }
                });
            }
            Command::Transcribe(audio) => {
                tokio::spawn(async move {
                    if real { stt::real(cfg, audio, event_tx).await }
                    else    { stt::mock(cfg, audio, event_tx).await }
                });
            }
            Command::Deliberate { charge: c, plea } => {
                tokio::spawn(async move {
                    if real { verdict::real(cfg, c, plea, event_tx, display_tx).await }
                    else    { verdict::mock(cfg, c, plea, event_tx).await }
                });
            }
            Command::Speak(text) => {
                tokio::spawn(async move {
                    if real { tts::real(cfg, text, event_tx, display_tx).await }
                    else    { tts::mock(cfg, text, event_tx, display_tx).await }
                });
            }
            _ => {}
        }
    }
}
