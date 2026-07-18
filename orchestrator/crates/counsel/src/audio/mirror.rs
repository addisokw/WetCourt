//! Booth mirror: tee of the caller's earpiece feed to the orchestrator, so
//! the audience hears the lawyer through the booth speakers as well as the
//! handset.
//!
//! The RTP send task pushes each outbound 20 ms frame (speech, hold music,
//! keyboard clatter — exactly what the caller hears) as 8 kHz s16le PCM.
//! One chunked `POST /lawyer/audio` streams them for the life of the call;
//! the orchestrator rebroadcasts to its display clients. Best-effort by
//! design: a down or older orchestrator means the frames are dropped and the
//! phone works exactly as before.

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Frames buffered toward a stalled orchestrator before we drop instead of
/// building latency (the mirror must track the handset, not lag it).
const QUEUE_FRAMES: usize = 64; // ~1.3 s

pub struct BoothMirror {
    tx: mpsc::Sender<Result<Bytes, std::convert::Infallible>>,
}

impl BoothMirror {
    /// Mirror for a new call, if `[audio] booth_mirror` is on. Streams to the
    /// same orchestrator the trial-context fetch uses.
    pub fn from_shared(shared: &crate::http::Shared) -> Option<Self> {
        if !shared.cfg.audio.booth_mirror {
            return None;
        }
        Some(Self::start(&shared.cfg.trial_context.orchestrator_base_url))
    }

    /// Open the stream toward the orchestrator. The POST lives until the
    /// mirror is dropped (call teardown closes the channel, ending the body).
    pub fn start(orchestrator_base_url: &str) -> Self {
        let url = format!(
            "{}/lawyer/audio",
            orchestrator_base_url.trim_end_matches('/')
        );
        let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
        tokio::spawn(async move {
            let body = reqwest::Body::wrap_stream(ReceiverStream::new(rx));
            match reqwest::Client::new().post(&url).body(body).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => tracing::warn!(status = %r.status(), "booth mirror rejected"),
                Err(e) => tracing::warn!("booth mirror stream ended: {e:#}"),
            }
        });
        Self { tx }
    }

    /// Queue one frame of 8 kHz linear PCM. Never blocks the RTP tick: if the
    /// stream is stalled or gone the frame is dropped.
    pub fn push(&self, samples: &[i16]) {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let _ = self.tx.try_send(Ok(Bytes::from(bytes)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::post, Router};
    use futures_util::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Full round trip: frames pushed into the mirror arrive at the
    /// orchestrator endpoint as s16le bytes, and dropping the mirror ends
    /// the chunked body so the handler completes.
    #[tokio::test]
    async fn streams_frames_and_ends_on_drop() {
        let got: Arc<Mutex<Vec<u8>>> = Arc::default();
        let done = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().route(
            "/lawyer/audio",
            post({
                let (got, done) = (got.clone(), done.clone());
                move |body: Body| async move {
                    let mut stream = body.into_data_stream();
                    while let Some(Ok(chunk)) = stream.next().await {
                        got.lock().unwrap().extend_from_slice(&chunk);
                    }
                    done.notify_one();
                    "ok"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mirror = BoothMirror::start(&format!("http://{addr}"));
        let frame: Vec<i16> = (0..160).collect();
        mirror.push(&frame);
        mirror.push(&frame);
        drop(mirror);

        tokio::time::timeout(Duration::from_secs(5), done.notified())
            .await
            .expect("handler should complete once the mirror is dropped");
        let bytes = got.lock().unwrap().clone();
        assert_eq!(bytes.len(), 2 * 160 * 2, "two 160-sample frames of s16le");
        assert_eq!(&bytes[..4], &[0, 0, 1, 0], "little-endian sample order");
    }
}
