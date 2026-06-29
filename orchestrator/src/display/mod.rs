use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxumState,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::calibration::{Calibration, CalibrationRegistry};
use crate::config::InferenceConfig;
use crate::crimes::{Crime, CrimeStore};
use crate::hardware::maintenance::{DeviceInfo, HwAckResult, MaintenanceCommand, Role};
use crate::hardware::protocol::{HardwareCommand, PanelPattern};
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
    pub crimes: Arc<RwLock<CrimeStore>>,
    pub inference_cfg: InferenceConfig,
    /// Operator-toggleable cross-examination switch, shared with the state
    /// machine `Runtime`, which reads it when a plea comes in.
    pub cross_enabled: Arc<AtomicBool>,
    /// Direct-control command sink (bypasses the trial FSM). Consumed by the
    /// device registry; gated by `maintenance`.
    pub maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
    /// True while the FSM is in `State::Maintenance` (mirror; opens the
    /// direct-command path). Written by `Runtime`.
    pub maintenance: Arc<AtomicBool>,
    /// True while the FSM is in `State::Idle` (mirror; gates maintenance entry).
    pub is_idle: Arc<AtomicBool>,
    /// Per-device host-side calibration registry (degrees → raw transform).
    pub calibration: Arc<RwLock<CalibrationRegistry>>,
    /// Snapshot of currently-connected devices for `GET /maintenance/devices`.
    /// The device registry keeps this in sync; empty until it lands.
    pub devices: Arc<RwLock<Vec<DeviceInfo>>>,
    /// Base URL of the turret vision process; the orchestrator reverse-proxies
    /// its feed/state at `/vision/*` so the console stays same-origin.
    pub vision_base_url: String,
    /// HTTP client for the vision proxy. No global timeout — the MJPEG feed is
    /// an infinite stream; a per-request timeout guards the short /state calls.
    pub vision_http: reqwest::Client,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/view", get(view_ws_handler))
        .route("/operator/start", post(operator_start))
        .route("/operator/estop", post(operator_estop))
        .route("/operator/personas", get(list_personas))
        .route("/operator/voices", get(list_voices))
        .route("/operator/persona", get(get_active_persona).post(create_persona))
        .route("/operator/persona/{id}", put(update_persona))
        .route("/operator/persona/{id}/select", post(select_persona))
        .route("/operator/persona/{id}/save", post(save_persona))
        .route("/operator/persona/{id}/test", post(test_persona))
        .route("/operator/crimes", get(list_crimes).post(add_crime))
        .route("/operator/crimes/filter", post(set_crime_filter))
        .route("/operator/crimes/queue", post(queue_charge))
        .route("/operator/crimes/queue/{index}", delete(unqueue_charge))
        .route("/operator/crimes/{id}", put(update_crime).delete(delete_crime))
        .route("/operator/cross_exam", get(get_cross_exam).post(set_cross_exam))
        // ---- Maintenance / hardware test plane ----
        .route("/maintenance/enter", post(maintenance_enter))
        .route("/maintenance/exit", post(maintenance_exit))
        .route("/maintenance/command", post(maintenance_command))
        .route("/maintenance/devices", get(maintenance_devices))
        .route("/maintenance/calibration", get(list_calibrations))
        .route("/maintenance/calibration/{role}", get(get_calibration).put(update_calibration))
        .route("/maintenance/calibration/{role}/save", post(save_calibration))
        // ---- Vision proxy (reverse-proxies the vision process) ----
        .route("/vision/feed", get(vision_feed))
        .route("/vision/state", get(vision_state))
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

#[derive(Serialize, Deserialize)]
struct CrossExamState {
    enabled: bool,
}

async fn get_cross_exam(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let enabled = s.cross_enabled.load(Ordering::Relaxed);
    (StatusCode::OK, Json(CrossExamState { enabled }))
}

async fn set_cross_exam(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<CrossExamState>,
) -> impl IntoResponse {
    s.cross_enabled.store(body.enabled, Ordering::Relaxed);
    info!(enabled = body.enabled, "operator: cross-examination toggled");
    (StatusCode::OK, Json(CrossExamState { enabled: body.enabled }))
}

// ---- Maintenance / hardware test plane ----

async fn maintenance_enter(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    if !s.is_idle.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "maintenance can only be entered from idle");
    }
    info!("maintenance: enter");
    if s.event_tx.send(Event::EnterMaintenance).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "event channel closed");
    }
    (StatusCode::ACCEPTED, "")
}

async fn maintenance_exit(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    info!("maintenance: exit");
    if s.event_tx.send(Event::ExitMaintenance).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "event channel closed");
    }
    (StatusCode::ACCEPTED, "")
}

/// One direct hardware action from the console. `cmd` selects the verb; AIM
/// carries *logical degrees* (transformed to raw via calibration here). Set
/// `stream: true` for fire-and-forget (the high-rate AIM stream) to skip the
/// ack wait.
#[derive(Deserialize)]
struct CommandReq {
    target: Role,
    #[serde(flatten)]
    spec: CmdSpec,
    #[serde(default)]
    stream: bool,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum CmdSpec {
    Fire { ms: u32 },
    Gavel,
    Aim { pan: f32, tilt: f32 },
    Panel { pattern: String },
    Ping,
}

async fn maintenance_command(
    AxumState(s): AxumState<AppState>,
    Json(req): Json<CommandReq>,
) -> impl IntoResponse {
    if !s.maintenance.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "not in maintenance mode").into_response();
    }

    // Build the wire command, applying calibration for AIM (degrees → raw).
    let cmd = match req.spec {
        CmdSpec::Fire { ms } => HardwareCommand::Fire(ms),
        CmdSpec::Gavel => HardwareCommand::Gavel,
        CmdSpec::Ping => HardwareCommand::Ping,
        CmdSpec::Panel { pattern } => match pattern.as_str() {
            "idle" => HardwareCommand::Panel(PanelPattern::Idle),
            "thinking" => HardwareCommand::Panel(PanelPattern::Thinking),
            "verdict" => HardwareCommand::Panel(PanelPattern::Verdict),
            other => {
                return (StatusCode::BAD_REQUEST, format!("unknown panel pattern '{other}'"))
                    .into_response()
            }
        },
        CmdSpec::Aim { pan, tilt } => {
            let reg = s.calibration.read().await;
            let cal = match reg.get(req.target.as_str()) {
                Some(c) => c,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("no calibration for role '{}'", req.target.as_str()),
                    )
                        .into_response()
                }
            };
            match cal.aim_to_raw(pan, tilt) {
                Ok((p, t)) => HardwareCommand::Aim { pan: p, tilt: t },
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            }
        }
    };

    // Fire-and-forget (AIM stream): send without waiting for an ack.
    if req.stream {
        let mc = MaintenanceCommand { target: req.target, cmd, reply: None };
        if s.maint_cmd_tx.send(mc).await.is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "hardware channel closed").into_response();
        }
        return (StatusCode::ACCEPTED, String::new()).into_response();
    }

    // Awaited command: return the device's OK/ERR/timeout in the body.
    let (tx, rx) = oneshot::channel();
    let mc = MaintenanceCommand { target: req.target, cmd, reply: Some(tx) };
    if s.maint_cmd_tx.send(mc).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "hardware channel closed").into_response();
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(_)) => (StatusCode::OK, Json(HwAckResult::Timeout)).into_response(),
        Err(_) => (StatusCode::OK, Json(HwAckResult::Timeout)).into_response(),
    }
}

async fn maintenance_devices(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let devices = s.devices.read().await.clone();
    (StatusCode::OK, Json(devices))
}

/// Reverse-proxy the vision process's MJPEG feed. Streamed (no per-request
/// timeout — it's an infinite multipart stream); a `502` is returned if the
/// vision process is unreachable so the panel can show "offline".
async fn vision_feed(AxumState(s): AxumState<AppState>) -> Response {
    let url = format!("{}/feed", s.vision_base_url.trim_end_matches('/'));
    match s.vision_http.get(&url).send().await {
        Ok(resp) => {
            let mut builder = Response::builder().status(resp.status().as_u16());
            if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
                builder = builder.header(axum::http::header::CONTENT_TYPE, ct.as_bytes());
            }
            builder
                .body(Body::from_stream(resp.bytes_stream()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(e) => {
            debug!("vision feed proxy: {e}");
            (StatusCode::BAD_GATEWAY, "vision offline").into_response()
        }
    }
}

/// Proxy the vision process's `/state` JSON (short, with a guard timeout).
async fn vision_state(AxumState(s): AxumState<AppState>) -> Response {
    let url = format!("{}/state", s.vision_base_url.trim_end_matches('/'));
    match s
        .vision_http
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            match resp.bytes().await {
                Ok(body) => (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
                    .into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("vision read error: {e}")).into_response(),
            }
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "vision offline").into_response(),
    }
}

async fn list_calibrations(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let reg = s.calibration.read().await;
    let cals: Vec<Calibration> = reg.list().into_iter().cloned().collect();
    (StatusCode::OK, Json(cals)).into_response()
}

async fn get_calibration(
    AxumState(s): AxumState<AppState>,
    Path(role): Path<String>,
) -> impl IntoResponse {
    let reg = s.calibration.read().await;
    match reg.get(&role) {
        Some(c) => (StatusCode::OK, Json(c.clone())).into_response(),
        None => (StatusCode::NOT_FOUND, format!("unknown role '{role}'")).into_response(),
    }
}

async fn update_calibration(
    AxumState(s): AxumState<AppState>,
    Path(role): Path<String>,
    Json(body): Json<Calibration>,
) -> impl IntoResponse {
    let mut reg = s.calibration.write().await;
    if reg.get(&role).is_none() {
        return (StatusCode::NOT_FOUND, format!("unknown role '{role}'")).into_response();
    }
    match reg.update(&role, body) {
        Ok(()) => (StatusCode::OK, Json(reg.get(&role).unwrap().clone())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn save_calibration(
    AxumState(s): AxumState<AppState>,
    Path(role): Path<String>,
) -> impl IntoResponse {
    let reg = s.calibration.read().await;
    if reg.get(&role).is_none() {
        return (StatusCode::NOT_FOUND, format!("unknown role '{role}'")).into_response();
    }
    match reg.save(&role) {
        Ok(()) => (StatusCode::NO_CONTENT, String::new()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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

/// Read-only WebSocket for presentational monitors (judge face, case info).
/// Subscribes to the display broadcast but forwards only JSON events (no PCM
/// binary frames) and ignores anything the client sends. Doesn't participate
/// in the single-client budget that `/ws` enforces, so multiple read-only
/// viewers can connect simultaneously.
async fn view_ws_handler(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| view_ws_session(socket, state))
}

async fn view_ws_session(mut socket: WebSocket, state: AppState) {
    info!("view ws client connected");
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
                Ok(DisplayMessage::Binary(_)) => {} // read-only viewers don't need PCM
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("view ws lagged {n} display messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ignore — read-only
                Some(Err(e)) => { warn!("view ws error: {e}"); break; }
            }
        }
    }
    info!("view ws client disconnected");
}

/// The active persona's robot params as a display event (for connect-push and
/// broadcast-on-change), so audio clients colour playback to match the persona.
fn robot_params_event(p: &Persona) -> DisplayEvent {
    DisplayEvent::RobotParams {
        intensity: p.robot.intensity,
        glitch_rate: p.robot.glitch_rate,
        ring_hz: p.robot.ring_hz,
        saturation: p.robot.saturation,
        peak_hz: p.robot.peak_hz,
    }
}

async fn ws_session(mut socket: WebSocket, state: AppState) {
    info!("ws client connected");
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&DisplayEvent::Idle).unwrap().into(),
        ))
        .await;
    // Seed this audio client with the active persona's robot params.
    {
        let ev = robot_params_event(state.personas.read().await.active());
        let _ = socket
            .send(Message::Text(serde_json::to_string(&ev).unwrap().into()))
            .await;
    }
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
        Ok(ClientEvent::PleaRecordingStarted) => {
            // Hand to the state machine — it resets the plea-window deadline
            // and emits the PleaRecording + PhaseDeadline broadcasts.
            let _ = state.event_tx.send(Event::PleaRecordingStarted).await;
        }
        Ok(ClientEvent::TtsFinished) => {
            let _ = state.event_tx.send(Event::TtsFinished).await;
        }
        Ok(ClientEvent::PleaAudioComplete) => {
            let _ = state
                .display_bcast
                .send(DisplayMessage::Json(DisplayEvent::PleaRecording { active: false }));
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

#[derive(Serialize)]
struct VoiceEntry {
    id: &'static str,
    label: &'static str,
    group: &'static str,
}

#[derive(Serialize)]
struct VoicesResp {
    voices: &'static [VoiceEntry],
}

// Kokoro voice catalogue. Kept here because LiteLLM's OpenAI-compatible
// `/v1/audio/speech` doesn't expose a voice-listing endpoint to forward.
// Add / remove rows to match the Kokoro deployment if it diverges.
const VOICES: &[VoiceEntry] = &[
    VoiceEntry { id: "af_heart",    label: "Heart",    group: "American Female" },
    VoiceEntry { id: "af_alloy",    label: "Alloy",    group: "American Female" },
    VoiceEntry { id: "af_aoede",    label: "Aoede",    group: "American Female" },
    VoiceEntry { id: "af_bella",    label: "Bella",    group: "American Female" },
    VoiceEntry { id: "af_jessica",  label: "Jessica",  group: "American Female" },
    VoiceEntry { id: "af_kore",     label: "Kore",     group: "American Female" },
    VoiceEntry { id: "af_nicole",   label: "Nicole",   group: "American Female" },
    VoiceEntry { id: "af_nova",     label: "Nova",     group: "American Female" },
    VoiceEntry { id: "af_river",    label: "River",    group: "American Female" },
    VoiceEntry { id: "af_sarah",    label: "Sarah",    group: "American Female" },
    VoiceEntry { id: "af_sky",      label: "Sky",      group: "American Female" },
    VoiceEntry { id: "am_adam",     label: "Adam",     group: "American Male" },
    VoiceEntry { id: "am_echo",     label: "Echo",     group: "American Male" },
    VoiceEntry { id: "am_eric",     label: "Eric",     group: "American Male" },
    VoiceEntry { id: "am_fenrir",   label: "Fenrir",   group: "American Male" },
    VoiceEntry { id: "am_liam",     label: "Liam",     group: "American Male" },
    VoiceEntry { id: "am_michael",  label: "Michael",  group: "American Male" },
    VoiceEntry { id: "am_onyx",     label: "Onyx",     group: "American Male" },
    VoiceEntry { id: "am_puck",     label: "Puck",     group: "American Male" },
    VoiceEntry { id: "am_santa",    label: "Santa",    group: "American Male" },
    VoiceEntry { id: "bf_alice",    label: "Alice",    group: "British Female" },
    VoiceEntry { id: "bf_emma",     label: "Emma",     group: "British Female" },
    VoiceEntry { id: "bf_isabella", label: "Isabella", group: "British Female" },
    VoiceEntry { id: "bf_lily",     label: "Lily",     group: "British Female" },
    VoiceEntry { id: "bm_daniel",   label: "Daniel",   group: "British Male" },
    VoiceEntry { id: "bm_fable",    label: "Fable",    group: "British Male" },
    VoiceEntry { id: "bm_george",   label: "George",   group: "British Male" },
    VoiceEntry { id: "bm_lewis",    label: "Lewis",    group: "British Male" },
];

async fn list_voices() -> impl IntoResponse {
    (StatusCode::OK, Json(VoicesResp { voices: VOICES })).into_response()
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
        Ok(()) => {
            let ev = robot_params_event(reg.active());
            drop(reg);
            let _ = s.display_bcast.send(DisplayMessage::Json(ev));
            (StatusCode::NO_CONTENT, String::new()).into_response()
        }
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
    let is_active = reg.active_id() == id;
    match reg.update(&id, body.clone()) {
        Ok(()) => {
            let updated = reg.get(&id).unwrap().clone();
            // If this is the live persona, push its (possibly retuned) robot
            // params to audio clients so the change is heard immediately.
            if is_active {
                let _ = s.display_bcast.send(DisplayMessage::Json(robot_params_event(&updated)));
            }
            (StatusCode::OK, Json(updated)).into_response()
        }
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
}

async fn test_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TestReq>,
) -> impl IntoResponse {
    // Mirror the live path: inject the persona's guilty_bias into the prompt so
    // the operator's dry-run reflects the same conviction tuning.
    let system_prompt = {
        let reg = s.personas.read().await;
        match reg.get(&id) {
            Some(p) => p.system_prompt_with_bias(),
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
        Some(p) => TestResp { deliberation: p.deliberation, guilty: p.guilty },
        None => TestResp { deliberation: full.trim().to_string(), guilty: false },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

// ---- Crime list operator endpoints ----

#[derive(Serialize)]
struct CrimesResp {
    crimes: Vec<Crime>,
    categories: Vec<String>,
    category_filter: Option<String>,
    queue: Vec<String>,
}

async fn crimes_snapshot(s: &AppState) -> CrimesResp {
    let store = s.crimes.read().await;
    CrimesResp {
        crimes: store.list().to_vec(),
        categories: store.categories(),
        category_filter: store.category_filter().map(str::to_string),
        queue: store.queue().map(str::to_string).collect(),
    }
}

async fn list_crimes(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(crimes_snapshot(&s).await)).into_response()
}

#[derive(Deserialize)]
struct AddCrimeReq {
    category: String,
    charge: String,
    #[serde(default)]
    subject: Option<String>,
}

async fn add_crime(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<AddCrimeReq>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.add(body.category, body.charge, body.subject) {
        Ok(c) => {
            info!(id = c.id, "crime added");
            (StatusCode::CREATED, Json(c.clone())).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn update_crime(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<u32>,
    Json(body): Json<Crime>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    if store.get(id).is_none() {
        return (StatusCode::NOT_FOUND, format!("unknown crime id {id}")).into_response();
    }
    match store.update(id, body) {
        Ok(c) => (StatusCode::OK, Json(c.clone())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn delete_crime(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<u32>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.remove(id) {
        Ok(()) => {
            info!(id, "crime removed");
            (StatusCode::NO_CONTENT, String::new()).into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct FilterReq {
    /// None / null clears the filter (draw from all categories).
    category: Option<String>,
}

async fn set_crime_filter(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<FilterReq>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.set_category_filter(body.category) {
        Ok(()) => {
            info!(filter = ?store.category_filter(), "crime category filter set");
            (StatusCode::NO_CONTENT, String::new()).into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct QueueReq {
    charge: String,
}

async fn queue_charge(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<QueueReq>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.queue_push(body.charge) {
        Ok(()) => {
            info!(depth = store.queue().count(), "charge queued for next trial");
            (StatusCode::NO_CONTENT, String::new()).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn unqueue_charge(
    AxumState(s): AxumState<AppState>,
    Path(index): Path<usize>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.queue_remove(index) {
        Ok(()) => (StatusCode::NO_CONTENT, String::new()).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
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
