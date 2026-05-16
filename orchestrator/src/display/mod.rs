use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxumState,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::config::InferenceConfig;
use crate::inference::client::LlmClient;
use crate::personas::{verdict_parse, Persona, PersonaRegistry};
use crate::state_machine::{Command, Event};

pub mod assets;
pub mod events;

use events::{ClientEvent, DisplayEvent};

/// What the orchestrator pushes down the WebSocket. The display task fans these
/// out to whichever client is currently connected.
#[derive(Debug, Clone)]
pub enum DisplayMessage {
    Json(DisplayEvent),
    Binary(Bytes),
}

#[derive(Clone)]
pub struct AppState {
    pub event_tx: mpsc::Sender<Event>,
    pub display_bcast: broadcast::Sender<DisplayMessage>,
    pub ws_clients: Arc<AtomicUsize>,
    /// Buffer for binary plea audio uploaded by the frontend across multiple
    /// frames. Cleared when `plea_audio_complete` is received.
    pub plea_buffer: Arc<Mutex<Vec<u8>>>,
    pub personas: Arc<RwLock<PersonaRegistry>>,
    pub inference_cfg: InferenceConfig,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/operator/start", post(operator_start))
        .route("/operator/estop", post(operator_estop))
        .route("/operator/personas", get(list_personas))
        .route("/operator/persona", get(get_active_persona).post(create_persona))
        .route("/operator/persona/{id}", put(update_persona))
        .route("/operator/persona/{id}/select", post(select_persona))
        .route("/operator/persona/{id}/save", post(save_persona))
        .route("/operator/persona/{id}/test", post(test_persona))
        .route("/health", get(health))
        .fallback(assets::serve)
        .with_state(state)
}

async fn health() -> &'static str { "ok" }

async fn operator_start(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    info!("operator: start");
    if s.event_tx.send(Event::OperatorStart).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "event channel closed");
    }
    (StatusCode::NO_CONTENT, "")
}

async fn operator_estop(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    info!("operator: estop");
    if s.event_tx.send(Event::OperatorEmergencyStop).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "event channel closed");
    }
    (StatusCode::NO_CONTENT, "")
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<AppState>,
) -> impl IntoResponse {
    let prev = state.ws_clients.fetch_add(1, Ordering::SeqCst);
    if prev > 0 {
        state.ws_clients.fetch_sub(1, Ordering::SeqCst);
        warn!("rejecting ws upgrade: client already connected");
        return (StatusCode::CONFLICT, "single client only").into_response();
    }
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: AppState) {
    info!("ws client connected");
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&DisplayEvent::Idle).unwrap().into(),
        ))
        .await;
    let mut bcast_rx = state.display_bcast.subscribe();

    loop {
        tokio::select! {
            ev = bcast_rx.recv() => match ev {
                Ok(DisplayMessage::Json(de)) => {
                    let json = serde_json::to_string(&de).unwrap();
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Ok(DisplayMessage::Binary(b)) => {
                    if socket.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("ws lagged {n} display messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    handle_client_text(&t, &state).await;
                }
                Some(Ok(Message::Binary(b))) => {
                    state.plea_buffer.lock().await.extend_from_slice(&b);
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => { warn!("ws error: {e}"); break; }
            }
        }
    }

    state.ws_clients.fetch_sub(1, Ordering::SeqCst);
    info!("ws client disconnected");
}

async fn handle_client_text(text: &str, state: &AppState) {
    match serde_json::from_str::<ClientEvent>(text) {
        Ok(ClientEvent::Ready) => debug!("client ready"),
        Ok(ClientEvent::TtsFinished) => {
            let _ = state.event_tx.send(Event::TtsFinished).await;
        }
        Ok(ClientEvent::PleaAudioComplete) => {
            let mut buf = state.plea_buffer.lock().await;
            let audio = std::mem::take(&mut *buf);
            info!(bytes = audio.len(), "plea audio complete");
            let _ = state.event_tx.send(Event::PleaAudioReceived(audio)).await;
        }
        Ok(ClientEvent::PleaAudioChunk) => {} // header before binary frame; nothing to do
        Ok(ClientEvent::CueFinished { name }) => debug!(cue = %name, "cue_finished"),
        Err(e) => warn!("bad client message: {e} — payload: {text}"),
    }
}

// ---- Persona operator endpoints ----

#[derive(Serialize)]
struct PersonaSummary {
    id: String,
    display_name: String,
}

#[derive(Serialize)]
struct PersonasListResp {
    active_id: String,
    personas: Vec<PersonaSummary>,
}

async fn list_personas(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let reg = s.personas.read().await;
    let resp = PersonasListResp {
        active_id: reg.active_id().to_string(),
        personas: reg
            .list()
            .iter()
            .map(|p| PersonaSummary { id: p.id.clone(), display_name: p.display_name.clone() })
            .collect(),
    };
    (StatusCode::OK, Json(resp)).into_response()
}

async fn get_active_persona(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let reg = s.personas.read().await;
    (StatusCode::OK, Json(reg.active().clone())).into_response()
}

async fn select_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut reg = s.personas.write().await;
    match reg.set_active(&id) {
        Ok(()) => (StatusCode::NO_CONTENT, String::new()).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn update_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Persona>,
) -> impl IntoResponse {
    let mut reg = s.personas.write().await;
    if !reg.get(&id).is_some() {
        return (StatusCode::NOT_FOUND, format!("unknown persona '{id}'")).into_response();
    }
    match reg.update(&id, body.clone()) {
        Ok(()) => (StatusCode::OK, Json(reg.get(&id).unwrap().clone())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn save_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let reg = s.personas.read().await;
    if reg.get(&id).is_none() {
        return (StatusCode::NOT_FOUND, format!("unknown persona '{id}'")).into_response();
    }
    match reg.save(&id) {
        Ok(()) => (StatusCode::NO_CONTENT, String::new()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_persona(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<Persona>,
) -> impl IntoResponse {
    let mut reg = s.personas.write().await;
    if reg.get(&body.id).is_some() {
        return (StatusCode::CONFLICT, format!("id '{}' already exists", body.id)).into_response();
    }
    let id = body.id.clone();
    match reg.create(body) {
        Ok(()) => (StatusCode::CREATED, Json(reg.get(&id).unwrap().clone())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct TestReq {
    charge: String,
    plea: String,
}

#[derive(Serialize)]
struct TestResp {
    deliberation: String,
    guilty: bool,
    intensity: u8,
}

async fn test_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TestReq>,
) -> impl IntoResponse {
    let system_prompt = {
        let reg = s.personas.read().await;
        match reg.get(&id) {
            Some(p) => p.system_prompt.clone(),
            None => return (StatusCode::NOT_FOUND, format!("unknown persona '{id}'")).into_response(),
        }
    };

    let client = LlmClient::new(&s.inference_cfg);
    let user_msg = format!("CHARGE: {}\n\nPLEA: {}\n\nRender your verdict.", body.charge, body.plea);
    let first_to = Duration::from_secs(s.inference_cfg.verdict_first_token_timeout_secs);
    let total_to = Duration::from_secs(s.inference_cfg.verdict_total_timeout_secs);

    let stream = match client.chat_stream(&system_prompt, &user_msg, first_to, total_to).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("llm stream open failed: {e:#}")).into_response(),
    };
    futures_util::pin_mut!(stream);
    let mut full = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => full.push_str(&chunk),
            Err(e) => return (StatusCode::BAD_GATEWAY, format!("llm stream error: {e:#}")).into_response(),
        }
    }

    let parsed = verdict_parse::parse(&full);
    let resp = match parsed {
        Some(p) => TestResp { deliberation: p.deliberation, guilty: p.guilty, intensity: p.intensity },
        None => TestResp { deliberation: full.trim().to_string(), guilty: false, intensity: 0 },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

/// Forwards Display + DisplayBinary commands from the state machine and the
/// inference subsystem onto the broadcast channel.
pub async fn forwarder(mut display_rx: mpsc::Receiver<Command>, bcast: broadcast::Sender<DisplayMessage>) {
    while let Some(cmd) = display_rx.recv().await {
        let msg = match cmd {
            Command::Display(de) => DisplayMessage::Json(de),
            Command::DisplayBinary(b) => DisplayMessage::Binary(b),
            _ => continue,
        };
        let _ = bcast.send(msg);
    }
}
