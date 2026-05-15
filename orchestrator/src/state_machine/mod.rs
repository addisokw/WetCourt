use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::info;

use crate::config::Config;

pub mod commands;
pub mod events;
pub mod states;
pub mod transitions;

pub use commands::Command;
pub use events::Event;
pub use states::State;

pub struct Runtime {
    state: State,
    cfg: Arc<Config>,
    event_rx: mpsc::Receiver<Event>,
    inference_tx: mpsc::Sender<Command>,
    hardware_tx: mpsc::Sender<Command>,
    display_tx: mpsc::Sender<Command>,
}

impl Runtime {
    pub fn new(
        cfg: Arc<Config>,
        event_rx: mpsc::Receiver<Event>,
        inference_tx: mpsc::Sender<Command>,
        hardware_tx: mpsc::Sender<Command>,
        display_tx: mpsc::Sender<Command>,
    ) -> Self {
        Self { state: State::Idle, cfg, event_rx, inference_tx, hardware_tx, display_tx }
    }

    pub async fn run(mut self) {
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        info!("state machine running, initial state: {}", self.state.name());
        loop {
            tokio::select! {
                Some(ev) = self.event_rx.recv() => self.handle(ev).await,
                _ = ticker.tick() => self.handle(Event::Tick).await,
            }
        }
    }

    async fn handle(&mut self, ev: Event) {
        let prev_name = self.state.name();
        let interesting = !matches!(ev, Event::Tick);
        let prev = std::mem::replace(&mut self.state, State::Idle);
        let (next, cmds) = transitions::step(prev, ev, &self.cfg);
        if next.name() != prev_name {
            info!(from = prev_name, to = next.name(), "state_transition");
        } else if interesting && !cmds.is_empty() {
            tracing::debug!(state = next.name(), "event handled, no transition");
        }
        self.state = next;
        for cmd in cmds {
            self.dispatch(cmd).await;
        }
    }

    async fn dispatch(&self, cmd: Command) {
        match cmd {
            Command::GenerateCharge | Command::Transcribe(_) | Command::Deliberate { .. } | Command::Speak(_) => {
                if self.inference_tx.send(cmd).await.is_err() {
                    tracing::error!("inference channel closed");
                }
            }
            Command::Hardware(_) => {
                if self.hardware_tx.send(cmd).await.is_err() {
                    tracing::error!("hardware channel closed");
                }
            }
            Command::Display(_) | Command::DisplayBinary(_) => {
                if self.display_tx.send(cmd).await.is_err() {
                    tracing::warn!("display channel closed (no client?)");
                }
            }
        }
    }
}
