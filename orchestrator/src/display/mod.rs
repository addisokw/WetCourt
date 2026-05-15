use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State as AxumState,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, info, warn};

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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/operator/start", post(operator_start))
        .route("/operator/estop", post(operator_estop))
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
