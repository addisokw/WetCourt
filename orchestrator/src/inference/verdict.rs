use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::fallbacks;
use crate::state_machine::Event;

pub async fn mock(cfg: Arc<Config>, _charge: String, _plea: String, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.deliberate_latency_ms)).await;
    let v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
    let _ = event_tx.send(Event::VerdictReady(v)).await;
}
