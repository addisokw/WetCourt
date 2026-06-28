//! Standalone web editor for the Wet Court crimes list.
//!
//! Serves the SolidJS UI (same stack and styling as the operator console) plus
//! a small CRUD API over a `crimes-core::CrimeStore`. It is the "crimes panel,
//! expanded and focused" — add / edit / delete / browse — with none of the
//! live-trial controls (draw filter, charge queue) that only make sense with a
//! running booth.
//!
//! The crimes JSON file stays the single source of truth: collaborators edit
//! here, the file is written in place (atomic temp+rename, via the store), and
//! the changes are committed to git out-of-band (a host-side commit/push).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use clap::Parser;
use crimes_core::{Crime, CrimeStore};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod assets;

#[derive(Parser, Debug)]
#[command(name = "crimes-editor", about = "Standalone editor for the Wet Court crimes list")]
struct Cli {
    /// Path to the crimes JSON file (the booth's source of truth).
    #[arg(long, default_value = "crimes/wet_court_crimes.json")]
    crimes: PathBuf,
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: SocketAddr,
}

#[derive(Clone)]
struct AppState {
    store: Arc<RwLock<CrimeStore>>,
}

#[derive(Serialize)]
struct Snapshot {
    crimes: Vec<Crime>,
    categories: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    // The no-repeat window is irrelevant here — the editor never draws — but
    // `load_from_file` requires one. 1 is fine.
    let store = CrimeStore::load_from_file(&cli.crimes, 1)
        .with_context(|| format!("loading crimes from {}", cli.crimes.display()))?;
    let count = store.list().len();
    if warn_if_unwritable(&cli.crimes) {
        warn!("crimes file is read-only — edits will fail to save");
    }

    let state = AppState {
        store: Arc::new(RwLock::new(store)),
    };
    info!(path = %cli.crimes.display(), crimes = count, "crimes-editor loaded");

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/crimes", get(list).post(add))
        .route("/api/crimes/{id}", put(update).delete(remove))
        .fallback(assets::serve)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("binding {}", cli.listen))?;
    info!(addr = %cli.listen, "listening");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}

async fn snapshot(state: &AppState) -> Snapshot {
    let store = state.store.read().await;
    Snapshot {
        crimes: store.list().to_vec(),
        categories: store.categories(),
    }
}

async fn list(State(s): State<AppState>) -> impl IntoResponse {
    Json(snapshot(&s).await)
}

#[derive(Deserialize)]
struct AddReq {
    category: String,
    charge: String,
    #[serde(default)]
    subject: Option<String>,
}

async fn add(State(s): State<AppState>, Json(body): Json<AddReq>) -> impl IntoResponse {
    let added = {
        let mut store = s.store.write().await;
        store.add(body.category, body.charge, body.subject).map(|c| c.clone())
    };
    match added {
        Ok(c) => {
            info!(id = c.id, "crime added");
            (StatusCode::CREATED, Json(c)).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn update(
    State(s): State<AppState>,
    Path(id): Path<u32>,
    Json(body): Json<Crime>,
) -> impl IntoResponse {
    let updated = {
        let mut store = s.store.write().await;
        if store.get(id).is_none() {
            return (StatusCode::NOT_FOUND, format!("unknown crime id {id}")).into_response();
        }
        store.update(id, body).map(|c| c.clone())
    };
    match updated {
        Ok(c) => (StatusCode::OK, Json(c)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn remove(State(s): State<AppState>, Path(id): Path<u32>) -> impl IntoResponse {
    let removed = {
        let mut store = s.store.write().await;
        store.remove(id)
    };
    match removed {
        Ok(()) => {
            info!(id, "crime removed");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// Returns true if the crimes file exists but its permissions are read-only.
/// A heads-up at startup beats a confusing 400 on the first save.
fn warn_if_unwritable(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.permissions().readonly(),
        Err(_) => false,
    }
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();
}
