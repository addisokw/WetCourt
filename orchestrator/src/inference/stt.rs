use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;
use crate::state_machine::Event;

use super::client::LlmClient;

const EMPTY_PLEA: &str = "[no defense offered]";

pub async fn mock(cfg: Arc<Config>, _audio: Vec<u8>, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.transcribe_latency_ms)).await;
    let _ = event_tx
        .send(Event::TranscriptReady(
            "I plead temporary insanity, your honor.".into(),
        ))
        .await;
}

pub async fn real(cfg: Arc<Config>, audio: Vec<u8>, event_tx: mpsc::Sender<Event>) {
    if audio.is_empty() {
        let _ = event_tx.send(Event::TranscriptReady(EMPTY_PLEA.into())).await;
        return;
    }
    let client = LlmClient::new(&cfg.inference);
    let timeout = Duration::from_secs(cfg.inference.stt_timeout_secs);
    // Frontend uploads webm/opus via MediaRecorder. Parakeet handles standard formats.
    match client.transcribe(Bytes::from(audio), "plea.webm", timeout).await {
        Ok(text) if text.is_empty() => {
            info!("transcript empty; treating as no defense");
            let _ = event_tx.send(Event::TranscriptReady(EMPTY_PLEA.into())).await;
        }
        Ok(text) => {
            info!(plea = %text, "transcript ready");
            let _ = event_tx.send(Event::TranscriptReady(text)).await;
        }
        Err(e) => {
            warn!("transcribe failed: {e:#}; treating as no defense");
            let _ = event_tx.send(Event::TranscriptReady(EMPTY_PLEA.into())).await;
        }
    }
}
