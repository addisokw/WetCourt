use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::state_machine::{Command, Event};

use super::client::LlmClient;

/// Mock: emits a `tts_audio`/`tts_end` pair with no actual bytes and self-acks
/// so the state machine cycles without a browser.
pub async fn mock(
    cfg: Arc<Config>,
    _text: String,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    let _ = display_tx
        .send(Command::Display(DisplayEvent::TtsAudio { format: "pcm_s16le_24000".into() }))
        .await;
    let _ = display_tx.send(Command::Display(DisplayEvent::TtsEnd)).await;
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.tts_latency_ms)).await;
    let _ = event_tx.send(Event::TtsFinished).await;
}

/// Real: stream PCM bytes from Kokoro, forward each chunk as a binary frame
/// preceded by a `tts_audio` JSON header, then a final `tts_end`. The frontend
/// emits `tts_finished` when its audio queue drains; if no client is connected
/// we self-ack so the state machine still moves on.
pub async fn real(
    cfg: Arc<Config>,
    text: String,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    let text = strip_markers(&text);
    if text.is_empty() {
        let _ = event_tx.send(Event::TtsFinished).await;
        return;
    }
    let client = LlmClient::new(&cfg.inference);
    let connect_to = Duration::from_secs(cfg.inference.tts_timeout_secs);
    match client.synth_pcm_stream(&text, connect_to).await {
        Ok(stream) => {
            futures_util::pin_mut!(stream);
            let _ = display_tx
                .send(Command::Display(DisplayEvent::TtsAudio { format: "pcm_s16le_24000".into() }))
                .await;
            let mut total = 0usize;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(b) => {
                        total += b.len();
                        let _ = display_tx.send(Command::DisplayBinary(b)).await;
                    }
                    Err(e) => {
                        warn!("tts stream error after {total}B: {e:#}");
                        break;
                    }
                }
            }
            let _ = display_tx.send(Command::Display(DisplayEvent::TtsEnd)).await;
            info!(bytes = total, "tts stream complete");
            // If the browser is connected it will emit tts_finished after audio
            // drains. If not, fire it ourselves after a small grace so the
            // state machine doesn't wedge.
            schedule_self_ack(event_tx, total).await;
        }
        Err(e) => {
            warn!("tts open failed: {e:#}; skipping audio");
            let _ = event_tx.send(Event::TtsFinished).await;
        }
    }
}

/// Pump a single sentence's PCM straight to the frontend without sending the
/// `TtsAudio` header (caller does so once per session) or the closing
/// `TtsEnd`. Returns total bytes pushed.
pub async fn synth_into_display(
    client: &LlmClient,
    text: &str,
    display_tx: &mpsc::Sender<Command>,
    connect_to: Duration,
) -> anyhow::Result<usize> {
    let stream = client.synth_pcm_stream(text, connect_to).await?;
    futures_util::pin_mut!(stream);
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let b: Bytes = chunk?;
        total += b.len();
        let _ = display_tx.send(Command::DisplayBinary(b)).await;
    }
    Ok(total)
}

/// Strip "VERDICT: …" / "INTENSITY: …" / "EMOTION: …" lines from a body of
/// text so they're never spoken aloud.
pub fn strip_markers(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("VERDICT:")
                && !t.starts_with("INTENSITY:")
                && !t.starts_with("EMOTION:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Estimate playback duration from PCM bytes and emit `TtsFinished` after that
/// long. This is a fallback for headless / disconnected runs; the live browser
/// path beats us to it via the `tts_finished` ClientEvent.
async fn schedule_self_ack(event_tx: mpsc::Sender<Event>, pcm_bytes: usize) {
    // 24kHz, mono, s16le → 48000 bytes per second.
    let secs = (pcm_bytes as f64 / 48_000.0).max(0.1);
    tokio::time::sleep(Duration::from_secs_f64(secs + 0.5)).await;
    let _ = event_tx.send(Event::TtsFinished).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markers_drops_emotion_intensity_and_verdict() {
        let raw = "EMOTION: anger=0.8\nINTENSITY: 4\n\nYour plea is feeble.\n\nVERDICT: GUILTY";
        assert_eq!(strip_markers(raw), "Your plea is feeble.");
    }
}
