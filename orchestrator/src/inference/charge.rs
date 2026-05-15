use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::fallbacks;
use crate::state_machine::Event;

pub async fn mock(cfg: Arc<Config>, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.charge_latency_ms)).await;
    let charge = fallbacks::charges::random();
    let _ = event_tx.send(Event::ChargeReady(charge)).await;
}
