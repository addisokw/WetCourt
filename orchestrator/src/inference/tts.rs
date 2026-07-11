use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::personas::PersonaRegistry;
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
    personas: Arc<RwLock<PersonaRegistry>>,
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
    let voice = personas.read().await.active().tts_voice.clone();
    match client.synth_pcm_stream(&text, &voice, connect_to).await {
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
    voice: &str,
    display_tx: &mpsc::Sender<Command>,
    connect_to: Duration,
) -> anyhow::Result<usize> {
    let stream = client.synth_pcm_stream(text, voice, connect_to).await?;
    futures_util::pin_mut!(stream);
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let b: Bytes = chunk?;
        total += b.len();
        let _ = display_tx.send(Command::DisplayBinary(b)).await;
    }
    Ok(total)
}

/// Strip "VERDICT: …" / "INTENSITY: …" lines from a body of text so they're
/// never spoken aloud.
pub fn strip_markers(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("VERDICT:")
                && !t.starts_with("INTENSITY:")
                && !t.starts_with("KEY_FACTOR:")
                && !t.starts_with("REASON:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

const MARKERS: [&str; 4] = ["VERDICT:", "INTENSITY:", "KEY_FACTOR:", "REASON:"];

enum LineState {
    /// The line so far is still a viable marker prefix (or just whitespace);
    /// its chars sit in `held`, unemitted.
    Undecided,
    /// The line diverged from every marker — pass chars straight through.
    Emitting,
    /// The line IS a marker line — swallow it (and its newline) entirely.
    Dropping,
}

/// Incremental [`strip_markers`] for the deliberation token stream: text passes
/// through as it arrives, but any line that could still turn into a marker line
/// is held back until it either diverges (flushed) or confirms (dropped). This
/// runs server-side, before broadcast, so a token split like "VERD|ICT: GUIL…"
/// can never flash the verdict on the big screens ahead of the reveal.
pub struct StreamMarkerFilter {
    state: LineState,
    held: String,
}

impl StreamMarkerFilter {
    pub fn new() -> Self {
        Self { state: LineState::Undecided, held: String::new() }
    }

    /// Feed a chunk; returns the text that is now safe to display. A trailing
    /// fragment still ambiguous at stream end is deliberately never flushed —
    /// in practice that tail *is* the marker block.
    pub fn push(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for ch in chunk.chars() {
            match self.state {
                LineState::Dropping => {
                    if ch == '\n' {
                        self.state = LineState::Undecided;
                    }
                }
                LineState::Emitting => {
                    out.push(ch);
                    if ch == '\n' {
                        self.state = LineState::Undecided;
                    }
                }
                LineState::Undecided => {
                    if ch == '\n' {
                        // Line ended while still ambiguous (e.g. a bare
                        // "VERDICT" with no colon) — a real line, flush it.
                        out.push_str(&self.held);
                        out.push('\n');
                        self.held.clear();
                        continue;
                    }
                    self.held.push(ch);
                    let t = self.held.trim_start();
                    if t.is_empty() {
                        continue; // leading whitespace: still ambiguous
                    }
                    if MARKERS.iter().any(|m| t.starts_with(m)) {
                        self.state = LineState::Dropping;
                        self.held.clear();
                    } else if !MARKERS.iter().any(|m| m.starts_with(t)) {
                        // Diverged from every marker — flush and stream on.
                        out.push_str(&self.held);
                        self.held.clear();
                        self.state = LineState::Emitting;
                    }
                }
            }
        }
        out
    }
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

    fn run(chunks: &[&str]) -> String {
        let mut f = StreamMarkerFilter::new();
        chunks.iter().map(|c| f.push(c)).collect()
    }

    #[test]
    fn plain_text_streams_through_incrementally() {
        let mut f = StreamMarkerFilter::new();
        // Once a line diverges from the markers, chars flow immediately.
        assert_eq!(f.push("Your plea"), "Your plea");
        assert_eq!(f.push(" is nonsense.\nMore."), " is nonsense.\nMore.");
    }

    #[test]
    fn marker_split_across_tokens_never_leaks() {
        assert_eq!(run(&["Weak.\n", "VERD", "ICT: GUIL", "TY\nafter"]), "Weak.\nafter");
    }

    #[test]
    fn all_marker_lines_dropped_with_leading_whitespace() {
        assert_eq!(
            run(&["ok\n  KEY_FACTOR: smugness\nREASON: none\nVERDICT: ACQUITTED"]),
            "ok\n"
        );
    }

    #[test]
    fn bare_marker_word_without_colon_is_kept() {
        assert_eq!(run(&["The VERDICT is mine.\nVERDICT\ndone\n"]), "The VERDICT is mine.\nVERDICT\ndone\n");
    }

    #[test]
    fn ambiguous_tail_is_withheld() {
        // "REASON" with no colon and no newline at stream end: withheld (it's
        // almost certainly the marker block starting).
        assert_eq!(run(&["fine.\nREASON"]), "fine.\n");
    }
}
