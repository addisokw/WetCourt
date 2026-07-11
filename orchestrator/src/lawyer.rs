//! Trial-side bridge to the lawyer phone (`counsel`).
//!
//! The Runtime rings the phone as a cross-examination answer window opens
//! (when the integration toggle is on); counsel pushes call lifecycle events
//! back at `POST /lawyer/event`, which pause/resume the window's clock. This
//! bridge only *originates* the ring — everything else flows through the
//! display server's HTTP handlers and the FSM's LawyerCall* events.

use std::time::Duration;

use tracing::{info, warn};

pub struct LawyerBridge {
    http: reqwest::Client,
    base_url: String,
}

impl LawyerBridge {
    pub fn new(base_url: String) -> Self {
        Self { http: reqwest::Client::new(), base_url }
    }

    /// Ring the defendant's phone, detached — counsel blocks up to ~25s
    /// waiting for pickup, and a down/busy/unregistered phone must never
    /// stall the FSM loop. The outcome is logged, not acted on: if they
    /// answer, counsel's `call_started` push pauses the clock.
    pub fn ring(&self, reason: String) {
        let url = format!("{}/call", self.base_url.trim_end_matches('/'));
        let http = self.http.clone();
        tokio::spawn(async move {
            match http
                .post(&url)
                .timeout(Duration::from_secs(35))
                .json(&serde_json::json!({ "reason": reason }))
                .send()
                .await
            {
                Ok(r) => info!(status = %r.status(), "lawyer ring-out resolved"),
                Err(e) => warn!("lawyer ring-out failed: {e:#}"),
            }
        });
    }
}
