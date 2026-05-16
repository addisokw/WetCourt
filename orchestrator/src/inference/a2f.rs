//! Audio2Face-3D streaming client.
//!
//! Feasibility spike (feat/a2f-3d-spike). Opens a WebSocket to the
//! `audio2face` container, forwards PCM s16le @ 24 kHz as binary frames, and
//! deserializes JSON blendshape frames back out.
//!
//! Back-pressure policy:
//! - `audio_in` is bounded; senders use `try_send` and drop chunks on full.
//!   That guarantees TTS never stalls on a slow A2F service; the cost is a
//!   gap in blendshape coverage for the dropped slice.
//! - `frames_out` is bounded; the reader task drops the oldest frame on full.
//!   Same rationale: a slow renderer must never back up the inference loop.
//!
//! On any connection failure the session shuts both channels cleanly and the
//! caller is expected to continue without A2F (verdict::real does).
use std::time::Duration;
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::config::A2fConfig;

const AUDIO_CHAN_CAP: usize = 64;   // ~3 s of TTS chunks at typical Kokoro pacing
const FRAMES_CHAN_CAP: usize = 64;  // ~2 s of 30 Hz blendshape frames

#[derive(Debug, Clone, Deserialize)]
pub struct BlendshapeFrame {
    #[allow(dead_code)]
    pub frame_idx: u64,
    pub audio_offset_ms: f32,
    pub weights: Vec<f32>,
}

/// Bidirectional channel-pair handle. Drop both sides to close the session.
pub struct A2fSession {
    audio_in: mpsc::Sender<Bytes>,
    frames_out: Option<mpsc::Receiver<BlendshapeFrame>>,
}

impl A2fSession {
    /// Open the WS and spawn the I/O task. Returns Err if the connection
    /// can't be established within `cfg.timeout_ms`.
    pub async fn open(cfg: &A2fConfig) -> Result<Self> {
        let url = cfg.base_url.clone();
        let connect = tokio_tungstenite::connect_async(&url);
        let (ws, _resp) = tokio::time::timeout(Duration::from_millis(cfg.timeout_ms), connect)
            .await
            .map_err(|_| anyhow!("a2f connect timeout after {} ms", cfg.timeout_ms))?
            .with_context(|| format!("a2f ws connect to {url}"))?;
        info!(url = %url, "a2f session opened");

        let (audio_tx, audio_rx) = mpsc::channel::<Bytes>(AUDIO_CHAN_CAP);
        let (frames_tx, frames_rx) = mpsc::channel::<BlendshapeFrame>(FRAMES_CHAN_CAP);

        tokio::spawn(run_session(ws, audio_rx, frames_tx));

        Ok(Self { audio_in: audio_tx, frames_out: Some(frames_rx) })
    }

    /// Push one chunk of PCM s16le @ 24 kHz toward the A2F service.
    /// Returns false (and logs at debug) if the channel is full — caller is
    /// expected to continue regardless. Never blocks.
    pub fn push_audio(&self, chunk: Bytes) -> bool {
        match self.audio_in.try_send(chunk) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!("a2f audio_in full; dropping chunk");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Take the frames receiver. Can only be called once.
    pub fn take_frames_rx(&mut self) -> Option<mpsc::Receiver<BlendshapeFrame>> {
        self.frames_out.take()
    }
}

async fn run_session<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    mut audio_rx: mpsc::Receiver<Bytes>,
    frames_tx: mpsc::Sender<BlendshapeFrame>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    let mut audio_bytes_sent: usize = 0;
    let mut frames_received: u64 = 0;

    loop {
        tokio::select! {
            // PCM upstream.
            chunk = audio_rx.recv() => match chunk {
                Some(b) => {
                    audio_bytes_sent += b.len();
                    if let Err(e) = sink.send(Message::Binary(b.into())).await {
                        warn!("a2f ws send failed: {e}");
                        break;
                    }
                }
                None => {
                    // TTS done — close write side cleanly.
                    let _ = sink.close().await;
                    debug!("a2f audio_in drained; closing ws");
                    // Don't break yet — keep reading any tail frames.
                }
            },
            // Blendshape downstream.
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<BlendshapeFrame>(&t) {
                        Ok(frame) => {
                            frames_received += 1;
                            // Drop-on-full: prefer continuing inference over back-pressure.
                            if let Err(mpsc::error::TrySendError::Full(_)) = frames_tx.try_send(frame) {
                                debug!("a2f frames_out full; dropping frame");
                            }
                        }
                        Err(e) => warn!("a2f bad frame json: {e} — payload: {t}"),
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => { warn!("a2f ws read error: {e}"); break; }
            }
        }
    }

    info!(audio_bytes_sent, frames_received, "a2f session closed");
}
