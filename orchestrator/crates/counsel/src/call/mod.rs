//! Call bookkeeping and the per-call media loop. counsel is a single-line
//! service: one active call, everything else gets 486 Busy Here.

pub mod agent;
pub mod context;
pub mod ivr;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use rsipstack::dialog::DialogId;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::http::Shared;
use crate::rtp::{g711, RtpEvent, RtpSession};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CallKind {
    Inbound,
    Outbound,
}

pub struct ActiveCall {
    pub id: DialogId,
    pub kind: CallKind,
    pub token: CancellationToken,
    pub started: Instant,
    pub remote: String,
}

#[derive(Default)]
pub struct CallManager {
    slot: Mutex<Option<ActiveCall>>,
    last_summary: Mutex<Option<Value>>,
    /// Orchestrator base URL for call-lifecycle pushes (`POST /lawyer/event`),
    /// so the trial clock can pause while the defendant consults counsel.
    /// `None` = no notifications (e.g. standalone testing).
    notify_base: Mutex<Option<String>>,
}

impl CallManager {
    pub fn set_notify_base(&self, url: String) {
        *self.notify_base.lock().unwrap() = Some(url);
    }

    /// Fire-and-forget lifecycle push to the orchestrator. Best-effort: a down
    /// or older orchestrator just ignores it — the phone works regardless.
    fn notify(&self, event: &'static str) {
        let Some(base) = self.notify_base.lock().unwrap().clone() else { return };
        tokio::spawn(async move {
            let url = format!("{}/lawyer/event", base.trim_end_matches('/'));
            let _ = reqwest::Client::new()
                .post(url)
                .timeout(Duration::from_secs(2))
                .json(&json!({ "event": event }))
                .send()
                .await;
        });
    }

    pub fn busy(&self) -> bool {
        self.slot.lock().unwrap().is_some()
    }

    /// Claim the line. Returns false (and changes nothing) if busy.
    pub fn begin(&self, call: ActiveCall) -> bool {
        let mut slot = self.slot.lock().unwrap();
        if slot.is_some() {
            return false;
        }
        *slot = Some(call);
        drop(slot);
        self.notify("call_started");
        true
    }

    /// Release the line if `id` holds it; cancels the call token and
    /// records a summary for /status.
    pub fn end(&self, id: &DialogId) {
        let mut slot = self.slot.lock().unwrap();
        if let Some(active) = slot.as_ref() {
            if &active.id == id {
                let active = slot.take().unwrap();
                active.token.cancel();
                let summary = json!({
                    "kind": match active.kind {
                        CallKind::Inbound => "inbound",
                        CallKind::Outbound => "outbound",
                    },
                    "remote": active.remote,
                    "duration_secs": active.started.elapsed().as_secs(),
                });
                tracing::info!(summary = %summary, "call ended");
                *self.last_summary.lock().unwrap() = Some(summary);
                drop(slot);
                self.notify("call_ended");
            }
        }
    }

    pub fn status(&self) -> Value {
        let slot = self.slot.lock().unwrap();
        json!({
            "active": slot.as_ref().map(|c| json!({
                "kind": match c.kind {
                    CallKind::Inbound => "inbound",
                    CallKind::Outbound => "outbound",
                },
                "remote": c.remote,
                "elapsed_secs": c.started.elapsed().as_secs(),
            })),
            "last": self.last_summary.lock().unwrap().clone(),
        })
    }
}

/// Drive an answered call until hangup: the lawyer agent, or a raw media
/// echo when `[audio] echo_test` is set (bring-up diagnostic).
pub async fn session_loop(
    shared: &Shared,
    session: RtpSession,
    token: CancellationToken,
) -> Result<()> {
    if shared.cfg.audio.echo_test {
        return echo_loop(shared, session, token).await;
    }
    agent::run(shared, session, token, None).await
}

async fn echo_loop(
    shared: &Shared,
    mut session: RtpSession,
    token: CancellationToken,
) -> Result<()> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(shared.cfg.audio.max_call_secs);
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!("max_call_secs reached, hanging up");
                break;
            }
            ev = session.events.recv() => match ev {
                Some(RtpEvent::Audio(samples)) => {
                    session.mixer.queue_speech(&g711::encode(&samples));
                }
                Some(RtpEvent::Dtmf(digit)) => {
                    tracing::info!(%digit, "DTMF received");
                }
                None => break,
            }
        }
    }
    Ok(())
}
