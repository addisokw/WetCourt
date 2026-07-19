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
    // Secret operator macro modes; the verdict service consults the active set
    // (e.g. #42 forces acquittal) since it owns the pre-announced reveal.
    operator_modes: Arc<crate::operator_modes::OperatorModes>,
) {
    let mode = cfg.inference.mode.as_str();
    if mode != "real" && mode != "mock" {
        warn!(mode, "unknown inference.mode; falling back to 'mock'");
    }
    let real = mode == "real";

    // Handles of in-flight tasks, so an e-stop's CancelSpeech can abort them
    // all — most importantly a TTS/verdict stream mid-delivery, which would
    // otherwise keep pumping PCM at the clients from a detached task.
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    while let Some(cmd) = cmd_rx.recv().await {
        tasks.retain(|t| !t.is_finished());
        let cfg = cfg.clone();
        let personas = personas.clone();
        let crimes = crimes.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        match cmd {
            Command::GenerateCharge => {
                tasks.push(tokio::spawn(async move {
                    charge::next(cfg, crimes, real, event_tx).await;
                }));
            }
            Command::Transcribe(audio) => {
                tasks.push(tokio::spawn(async move {
                    if real { stt::real(cfg, audio, event_tx).await }
                    else    { stt::mock(cfg, audio, event_tx).await }
                }));
            }
            Command::CrossExamine { charge: c, plea } => {
                tasks.push(tokio::spawn(async move {
                    if real { cross::real(cfg, personas, c, plea, event_tx).await }
                    else    { cross::mock(cfg, c, plea, event_tx).await }
                }));
            }
            Command::Deliberate { charge: c, plea, cross, anchors } => {
                let maint_cmd_tx = maint_cmd_tx.clone();
                let modes = operator_modes.clone();
                tasks.push(tokio::spawn(async move {
                    if real { verdict::real(cfg, personas, c, plea, cross, anchors, event_tx, display_tx, maint_cmd_tx, modes).await }
                    else    { verdict::mock(cfg, c, plea, cross, event_tx, modes).await }
                }));
            }
            Command::Speak(text) => {
                tasks.push(tokio::spawn(async move {
                    if real { tts::real(cfg, personas, text, event_tx, display_tx).await }
                    else    { tts::mock(cfg, text, event_tx, display_tx).await }
                }));
            }
            Command::CancelSpeech => {
                for t in tasks.drain(..) {
                    t.abort();
                }
            }
            _ => {}
        }
    }
}
