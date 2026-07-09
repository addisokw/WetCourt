use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::audio::cover::CoverAssets;
use crate::call::CallManager;
use crate::config::Config;
use crate::inference::Backend;
use crate::persona::LawyerPersona;
use crate::sip::registrar::RegistrationStore;

/// State shared between the SIP layer and the control plane.
pub struct AppShared {
    pub cfg: Config,
    pub registrar: RegistrationStore,
    pub calls: CallManager,
    pub backend: Backend,
    pub persona: LawyerPersona,
    pub cover: CoverAssets,
    pub ring_tx: tokio::sync::mpsc::Sender<crate::sip::RingRequest>,
    /// Where finished calls land as WAV+JSON pairs; `None` = recording off.
    pub recording_dir: Option<std::path::PathBuf>,
}

pub type Shared = Arc<AppShared>;

pub fn router(shared: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/call", post(call))
        .with_state(shared)
}

pub async fn serve(shared: Shared) -> Result<()> {
    let bind = shared.cfg.control.bind.clone();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding control plane on {bind}"))?;
    tracing::info!("control plane listening on {bind}");
    axum::serve(listener, router(shared)).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn status(State(shared): State<Shared>) -> impl IntoResponse {
    let registrations: Vec<_> = shared
        .registrar
        .snapshot()
        .into_iter()
        .map(|r| {
            json!({
                "user": r.username,
                "destination": r.destination.to_string(),
                "expires": r.expires,
                "age_secs": r.at.elapsed().as_secs(),
            })
        })
        .collect();
    Json(json!({
        "service": "counsel",
        "registered": !registrations.is_empty(),
        "registrations": registrations,
        "call": shared.calls.status(),
    }))
}

#[derive(serde::Deserialize, Default)]
struct CallBody {
    reason: Option<String>,
}

/// Ring the booth phone. Blocks until answered / no-answer / refused so the
/// operator console gets a truthful outcome (~25 s worst case).
async fn call(
    State(shared): State<Shared>,
    body: Option<Json<CallBody>>,
) -> impl IntoResponse {
    use crate::sip::outbound::RingOutcome;

    let reason = body.and_then(|Json(b)| b.reason);
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if shared
        .ring_tx
        .send(crate::sip::RingRequest { reason, reply: reply_tx })
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "sip endpoint not running" })),
        );
    }
    match reply_rx.await {
        Ok(Ok(outcome)) => {
            let status = match outcome {
                RingOutcome::Answered => StatusCode::OK,
                RingOutcome::NoAnswer => StatusCode::REQUEST_TIMEOUT,
                RingOutcome::Rejected => StatusCode::BAD_GATEWAY,
                RingOutcome::Busy => StatusCode::CONFLICT,
                RingOutcome::NotRegistered => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status, Json(json!({ "outcome": outcome })))
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("{e:#}") })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "ring-out task dropped" })),
        ),
    }
}
