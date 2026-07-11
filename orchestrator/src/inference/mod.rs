use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::warn;

use crate::config::Config;
use crate::crimes::CrimeStore;
use crate::hardware::maintenance::MaintenanceCommand;
use crate::personas::PersonaRegistry;
use crate::state_machine::{Command, Event};

pub mod charge;
pub mod client;
pub mod cross;
pub mod stt;
pub mod tts;
pub mod verdict;

pub async fn run(
    cfg: Arc<Config>,
    personas: Arc<RwLock<PersonaRegistry>>,
    crimes: Arc<RwLock<CrimeStore>>,
    mut cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
    // For the verdict service's LED-face reveal (FACE verdict:* at the same
    // beat as the Verdict display event) — bypasses the FSM like its other
    // reveal choreography.
    maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
) {
    let mode = cfg.inference.mode.as_str();
    if mode != "real" && mode != "mock" {
        warn!(mode, "unknown inference.mode; falling back to 'mock'");
    }
    let real = mode == "real";

    while let Some(cmd) = cmd_rx.recv().await {
        let cfg = cfg.clone();
        let personas = personas.clone();
        let crimes = crimes.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        match cmd {
            Command::GenerateCharge => {
                tokio::spawn(async move {
                    charge::next(cfg, crimes, real, event_tx).await;
                });
            }
            Command::Transcribe(audio) => {
                tokio::spawn(async move {
                    if real { stt::real(cfg, audio, event_tx).await }
                    else    { stt::mock(cfg, audio, event_tx).await }
                });
            }
            Command::CrossExamine { charge: c, plea } => {
                tokio::spawn(async move {
                    if real { cross::real(cfg, personas, c, plea, event_tx).await }
                    else    { cross::mock(cfg, c, plea, event_tx).await }
                });
            }
            Command::Deliberate { charge: c, plea, cross } => {
                let maint_cmd_tx = maint_cmd_tx.clone();
                tokio::spawn(async move {
                    if real { verdict::real(cfg, personas, c, plea, cross, event_tx, display_tx, maint_cmd_tx).await }
                    else    { verdict::mock(cfg, c, plea, cross, event_tx).await }
                });
            }
            Command::Speak(text) => {
                tokio::spawn(async move {
                    if real { tts::real(cfg, personas, text, event_tx, display_tx).await }
                    else    { tts::mock(cfg, text, event_tx, display_tx).await }
                });
            }
            _ => {}
        }
    }
}
