use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::state_machine::Event;

pub async fn mock(cfg: Arc<Config>, _audio: Vec<f32>, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.transcribe_latency_ms)).await;
    let _ = event_tx
        .send(Event::TranscriptReady(
            "I plead temporary insanity, your honor.".into(),
        ))
        .await;
}
