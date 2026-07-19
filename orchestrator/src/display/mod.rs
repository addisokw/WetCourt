use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Path, Query, State as AxumState,
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
use crate::hardware::protocol::{HardwareCommand, LedMode, PanelPattern};
use crate::inference::client::LlmClient;
use crate::personas::{verdict_parse, Persona, PersonaRegistry};
use crate::state_machine::{Command, Event};

pub mod assets;
pub mod autofire;
pub mod events;
pub mod print;

use autofire::AutoFire;
use events::{ClientEvent, DisplayEvent};

/// Application close code sent to an operator `/ws` session that a newer client
/// has superseded. The frontend treats this code as "go dormant" (don't
/// auto-reconnect), so two consoles can't supersede each other indefinitely.
/// Application-reserved range is 4000–4999.
const WS_SUPERSEDED: u16 = 4000;

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
    /// Monotonic `/ws` connection generation. Each new operator client bumps it
    /// and supersedes the previous one (last-connection-wins), so a reconnect is
    /// never rejected by a stale session that hasn't cleaned up yet.
    pub ws_generation: Arc<AtomicUsize>,
    /// Monotonic `/ws/view?audio=1` generation — the newest audio viewer is the
    /// booth's speakers; older ones silently stop receiving PCM.
    pub audio_generation: Arc<AtomicUsize>,
    /// Monotonic `/ws/view?mic=1` generation — the newest mic viewer is the
    /// booth's microphone; uplink from superseded ones is silently dropped.
    pub mic_generation: Arc<AtomicUsize>,
    /// True while a live `?mic=1` viewer is connected. Mirrored into the
    /// snapshot and broadcast as `MicOwner` so the operator console knows to
    /// keep its own microphone shut (and to take over if the kiosk dies).
    pub mic_present: Arc<AtomicBool>,
    /// Buffer for binary plea audio uploaded by the frontend across multiple
    /// frames. Cleared when `plea_audio_complete` is received.
    pub plea_buffer: Arc<Mutex<Vec<u8>>>,
    pub personas: Arc<RwLock<PersonaRegistry>>,
    pub crimes: Arc<RwLock<CrimeStore>>,
    pub inference_cfg: InferenceConfig,
    /// F7: when true, droop the judge's neck to full "powered down" tilt while a
    /// lawyer call is active. Off by default (motion feature, needs a hardware pass).
    pub lawyer_neck_droop_on_call: bool,
    /// F5: when true, fan lawyer call-audio POSTs to the primary-speaker kiosk.
    /// Operator-toggleable (`/operator/lawyer_speaker`); seeded from config.
    pub lawyer_speaker_playback: Arc<AtomicBool>,
    /// F6: live operator toggle for idle attract mode (`/operator/attract`).
    pub attract_enabled: Arc<AtomicBool>,
    /// F4: live coupon frequency (0=off,1=rare,2=sometimes,3=always), shared with
    /// the print service. Operator-toggleable via `/operator/coupons`.
    pub coupon_frequency: Arc<std::sync::atomic::AtomicU8>,
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
    /// Hardware safety gate for vision targeting: vision streams aim to
    /// `/vision/aim`, but the orchestrator only relays it to the turret while
    /// this is set. Disarmed = the gun never moves from vision, even though
    /// vision keeps tracking. Operator-toggled via `/vision/arm`.
    pub targeting_armed: Arc<AtomicBool>,
    /// Targeting-panel auto-fire: fires the squirt once vision holds a lock for a
    /// dwell time (while armed + enabled). Fed by every `/vision/aim` frame.
    pub auto_fire: Arc<AutoFire>,
    /// Eye-safety fire gate (m4b). Vision reports `fire_ok` on each aim POST;
    /// this stores the latest verdict so the trial `FIRE` path can require a
    /// fresh `fire_ok` while targeting is armed. Shared with the hardware
    /// adapter task in `main.rs`.
    pub vision_gate: Arc<crate::hardware::gate::VisionFireGate>,
    /// Trial targeting controller — shared here for its aim tracker (every
    /// logical AIM send is recorded so glides know where the gun points) and
    /// its eased glide-to-center (the console Recenter button).
    pub targeting: Arc<crate::targeting::TargetingController>,
    /// Read-only trial mirror for `GET /trial/state` (the lawyer phone reads
    /// it at call pickup). Written by `Runtime` on every transition.
    pub trial_snapshot: Arc<std::sync::RwLock<crate::state_machine::states::TrialSnapshot>>,
    /// Operator toggle for the lawyer-phone trial integration (off-hook clock
    /// pause + cross-exam ring-out). The force-ring button is independent.
    pub lawyer_enabled: Arc<AtomicBool>,
    /// Whether a lawyer call is live right now (set by counsel's lifecycle
    /// pushes at /lawyer/event). The Runtime reads it to pause a freshly
    /// opened window when the defendant is already on the phone.
    pub lawyer_call_active: Arc<AtomicBool>,
    /// Base URL of the lawyer-phone service (`counsel`); `/lawyer/*` proxies
    /// there, same pattern as vision (reuses `vision_http`).
    pub lawyer_base_url: String,
    /// Job sink shared with the trial path — custom prints queue behind
    /// keepsakes on the one physical printer.
    pub print_job_tx: mpsc::Sender<crate::printer::service::PrintJob>,
    /// Named custom-print templates (`print_templates.json` next to the config).
    pub print_templates: Arc<RwLock<crate::printer::templates::TemplateStore>>,
    /// Printer tunables the print handlers need directly (dot width for
    /// previews; the service holds its own copy for rendering).
    pub printer_cfg: crate::config::PrinterConfig,
    /// Secret operator macro modes (armed from the booth phone keypad or
    /// `/operator/modes/*`). Shared with the Runtime (latch/reset lifecycle)
    /// and the inference task (mode effects like the #42 verdict override).
    pub operator_modes: Arc<crate::operator_modes::OperatorModes>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/view", get(view_ws_handler))
        .route("/operator/start", post(operator_start))
        .route("/operator/defendant_press", post(operator_defendant_press))
        .route("/operator/estop", post(operator_estop))
        .route("/operator/personas", get(list_personas))
        .route("/operator/voices", get(list_voices))
        .route("/operator/persona", get(get_active_persona).post(create_persona))
        .route("/operator/persona/{id}", put(update_persona))
        .route("/operator/persona/{id}/select", post(select_persona))
        .route("/operator/persona/{id}/save", post(save_persona))
        .route("/operator/persona/{id}/test", post(test_persona))
        .route("/operator/crimes", get(list_crimes).post(add_crime))
        .route("/operator/crimes/reload", post(reload_crimes))
        .route("/operator/crimes/categories", post(set_disabled_categories))
        .route("/operator/crimes/queue", post(queue_charge))
        .route("/operator/crimes/queue/{index}", delete(unqueue_charge))
        .route("/operator/crimes/{id}", put(update_crime).delete(delete_crime))
        .route("/operator/cross_exam", get(get_cross_exam).post(set_cross_exam))
        .route("/operator/attract", get(get_attract).post(set_attract))
        .route("/operator/coupons", get(get_coupons).post(set_coupons))
        .route("/operator/lawyer_speaker", get(get_lawyer_speaker).post(set_lawyer_speaker))
        // ---- Secret operator macro modes (phone keypad / console) ----
        .route("/operator/modes", get(get_operator_modes))
        .route("/operator/modes/arm", post(arm_operator_mode))
        .route("/operator/modes/clear", post(clear_operator_modes))
        // ---- Audio check (console Audio tab): end-to-end source verification ----
        .route("/operator/audio/tts_test", post(audio_tts_test))
        .route("/operator/audio/stt_test", post(audio_stt_test))
        // ---- Custom prints (own sub-router: raised body limit for images) ----
        .merge(print::router())
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
        .route("/vision/snapshot", get(vision_snapshot))
        .route("/vision/state", get(vision_state))
        // ---- Vision targeting ----
        .route("/vision/aim", post(vision_aim))
        .route("/vision/arm", get(get_targeting_arm).post(set_targeting_arm))
        .route("/vision/autofire", get(get_auto_fire).post(set_auto_fire))
        .route("/vision/target", post(vision_target))
        .route("/vision/aimpoint", post(vision_aimpoint))
        .route("/vision/select", post(vision_select))
        .route("/vision/boresight", post(vision_boresight))
        .route("/vision/gains", post(vision_gains))
        .route("/vision/center", post(vision_center))
        // ---- Lawyer phone (read-only trial snapshot + counsel proxy) ----
        .route("/trial/state", get(trial_state))
        .route("/lawyer/status", get(lawyer_status))
        .route("/lawyer/call", post(lawyer_call))
        .route("/lawyer/event", post(lawyer_event))
        .route("/lawyer/audio", post(lawyer_audio))
        .route("/operator/lawyer_integration", get(get_lawyer_integration).post(set_lawyer_integration))
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

/// Console "simulate press": inject exactly the event a wire `BUTTON` from the
/// swear-in board produces, so the start / done-talking paths are testable
/// without the physical button.
async fn operator_defendant_press(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    info!("operator: simulated defendant press");
    if s.event_tx.send(Event::DefendantButton).await.is_err() {
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

// ---- Audio check ----

#[derive(Deserialize)]
struct TtsTestReq {
    #[serde(default)]
    text: Option<String>,
}

/// Speaker check: synthesize a test phrase with the active persona's voice and
/// stream it down the normal TTS WS path (header + binary PCM + end), so it
/// exercises Kokoro, the socket, the robot-voice graph, and the speakers —
/// exactly what a trial exercises. Errors surface synchronously so the console
/// can say *which* leg is broken. Idle/maintenance only: never talks over a
/// trial. The stray `tts_finished` ack lands in Idle and is ignored.
async fn audio_tts_test(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<TtsTestReq>,
) -> impl IntoResponse {
    if !s.is_idle.load(Ordering::Relaxed) && !s.maintenance.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "audio check only runs while idle").into_response();
    }
    let text = body
        .text
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Testing, testing. The Wet Court is now in session.".into());
    let (voice, speed) = {
        let reg = s.personas.read().await;
        let p = reg.active();
        (p.tts_voice.clone(), p.tts_speed)
    };
    let client = LlmClient::new(&s.inference_cfg);
    let connect_to = Duration::from_secs(s.inference_cfg.tts_timeout_secs);
    let stream = match client.synth_pcm_stream(&text, &voice, speed, connect_to).await {
        Ok(st) => st,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("tts synth failed: {e:#}")).into_response()
        }
    };
    info!("operator: audio check — tts test");
    let bcast = s.display_bcast.clone();
    tokio::spawn(async move {
        futures_util::pin_mut!(stream);
        let _ = bcast.send(DisplayMessage::Json(DisplayEvent::TtsAudio {
            format: "pcm_s16le_24000".into(),
        }));
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => {
                    let _ = bcast.send(DisplayMessage::Binary(b));
                }
                Err(e) => {
                    warn!("audio check: tts stream error: {e:#}");
                    break;
                }
            }
        }
        let _ = bcast.send(DisplayMessage::Json(DisplayEvent::TtsEnd));
    });
    (StatusCode::OK, "").into_response()
}

#[derive(Serialize)]
struct SttTestResp {
    transcript: String,
}

/// Mic check, far end: the console records a short clip and posts the raw
/// blob here; we run it through the real STT route and hand the transcript
/// back. Proves mic → browser capture → upload → Parakeet, end to end.
async fn audio_stt_test(AxumState(s): AxumState<AppState>, body: Bytes) -> impl IntoResponse {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "no audio uploaded").into_response();
    }
    info!(bytes = body.len(), "operator: audio check — stt test");
    let client = LlmClient::new(&s.inference_cfg);
    let to = Duration::from_secs(s.inference_cfg.stt_timeout_secs);
    match client.transcribe(body, "audio-check.webm", to).await {
        Ok(text) => (StatusCode::OK, Json(SttTestResp { transcript: text })).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("stt failed: {e:#}")).into_response(),
    }
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

// F6: attract-mode live toggle.
async fn get_attract(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let enabled = s.attract_enabled.load(Ordering::Relaxed);
    (StatusCode::OK, Json(CrossExamState { enabled }))
}
async fn set_attract(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<CrossExamState>,
) -> impl IntoResponse {
    s.attract_enabled.store(body.enabled, Ordering::Relaxed);
    info!(enabled = body.enabled, "operator: attract mode toggled");
    (StatusCode::OK, Json(CrossExamState { enabled: body.enabled }))
}

// F5: lawyer-audio-over-speaker live toggle (orchestrator-side gate).
async fn get_lawyer_speaker(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let enabled = s.lawyer_speaker_playback.load(Ordering::Relaxed);
    (StatusCode::OK, Json(CrossExamState { enabled }))
}
async fn set_lawyer_speaker(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<CrossExamState>,
) -> impl IntoResponse {
    s.lawyer_speaker_playback.store(body.enabled, Ordering::Relaxed);
    info!(enabled = body.enabled, "operator: lawyer speaker playback toggled");
    (StatusCode::OK, Json(CrossExamState { enabled: body.enabled }))
}

#[derive(Serialize, Deserialize)]
struct CouponFreqState {
    frequency: String,
}

// F4: coupon-frequency live dropdown (off | rare | sometimes | always).
async fn get_coupons(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let frequency =
        crate::printer::service::coupon_level_str(s.coupon_frequency.load(Ordering::Relaxed)).into();
    (StatusCode::OK, Json(CouponFreqState { frequency }))
}
async fn set_coupons(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<CouponFreqState>,
) -> impl IntoResponse {
    let level = crate::printer::service::coupon_level(&body.frequency);
    s.coupon_frequency.store(level, Ordering::Relaxed);
    let frequency = crate::printer::service::coupon_level_str(level).to_string();
    info!(frequency = %frequency, "operator: coupon frequency set");
    (StatusCode::OK, Json(CouponFreqState { frequency }))
}

// ---- Secret operator macro modes ----

#[derive(Deserialize)]
struct ArmModeReq {
    code: u16,
}

fn modes_state_json(s: &AppState) -> serde_json::Value {
    let (armed, active) = s.operator_modes.snapshot();
    serde_json::json!({ "armed": armed, "active": active })
}

/// Broadcast the current armed/active sets to every display client (the case
/// monitor renders them as the discreet bare-number indicator).
fn broadcast_modes(s: &AppState) {
    let (armed, active) = s.operator_modes.snapshot();
    let _ = s
        .display_bcast
        .send(DisplayMessage::Json(DisplayEvent::OperatorModes { armed, active }));
}

async fn get_operator_modes(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let (armed, active) = s.operator_modes.snapshot();
    let registry: Vec<_> = crate::operator_modes::REGISTRY
        .iter()
        .map(|m| serde_json::json!({ "code": m.code, "slug": m.slug, "description": m.description }))
        .collect();
    Json(serde_json::json!({ "armed": armed, "active": active, "registry": registry }))
}

/// Arm a mode for the next trial. Idle-only: modes latch on the trial-start
/// edge, so arming mid-trial would be dead until the following case anyway —
/// rejecting keeps the phone console's feedback truthful.
async fn arm_operator_mode(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<ArmModeReq>,
) -> Response {
    // Reserved disarm action (`#0#` on the phone): clear armed modes and ack
    // with 200 so the handset plays the accept tone. Allowed any time —
    // clearing the armed set is always safe (it never touches a latched
    // active mode), so this is not idle-gated like arming.
    if body.code == crate::operator_modes::CODE_DISARM {
        let changed = s.operator_modes.clear_armed();
        info!(changed, "operator modes: disarmed via #0");
        if changed {
            broadcast_modes(&s);
        }
        return (StatusCode::OK, Json(modes_state_json(&s))).into_response();
    }
    if !s.is_idle.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "modes can only be armed while the court is idle")
            .into_response();
    }
    match s.operator_modes.arm(body.code) {
        Ok(()) => {
            let slug = crate::operator_modes::lookup(body.code).map(|m| m.slug).unwrap_or("?");
            info!(code = body.code, slug, "operator mode armed");
            broadcast_modes(&s);
            (StatusCode::OK, Json(modes_state_json(&s))).into_response()
        }
        Err(()) => {
            warn!(code = body.code, "unknown operator mode code");
            (StatusCode::NOT_FOUND, "unknown mode code").into_response()
        }
    }
}

/// Clear the armed set (operator changed their mind). Never touches the
/// active set — un-forcing a mode latched into a running trial is a footgun.
async fn clear_operator_modes(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    if s.operator_modes.clear_armed() {
        info!("operator modes: armed set cleared");
        broadcast_modes(&s);
    }
    (StatusCode::OK, Json(modes_state_json(&s)))
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
    // Auto-fire is a maintenance-only tuning tool — never let it stay latched
    // into a live show. (Belt: the aim handler also refuses to act on it
    // outside maintenance, which covers the e-stop exit path too.)
    if s.auto_fire.enabled() {
        s.auto_fire.set(Some(false), None);
        info!("auto-fire disabled on maintenance exit");
    }
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
    /// Strike using the target role's *saved* `gavel.toml` geometry.
    Gavel,
    /// Strike using geometry from the request body — the console's "test strike"
    /// of the current (possibly unsaved) form values.
    GavelStrike {
        rest: i32,
        raise: i32,
        strike: i32,
        raise_dwell_ms: u32,
        strike_dwell_ms: u32,
        settle_dwell_ms: u32,
        strikes: u32,
    },
    /// Jog the gavel servo to a raw pulse-width (µs) for live position preview.
    GavelJog { us: i32 },
    Aim { pan: f32, tilt: f32 },
    Panel { pattern: String },
    /// Drive the swear-in button's lamp (`off`/`on`/`blink`/`pulse`).
    Led { mode: String },
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
    // A judge-neck AIM also fans out to the judge-face in degrees: the eye's
    // catchlight counter-moves with the neck pose (specular parallax).
    let mut face_mirror: Option<MaintenanceCommand> = None;
    let cmd = match req.spec {
        CmdSpec::Fire { ms } => HardwareCommand::Fire(ms),
        CmdSpec::Gavel => {
            // Plain "Strike" uses the saved geometry; bare GAVEL (firmware
            // default) if the role has no [gavel] calibration yet.
            let reg = s.calibration.read().await;
            match reg.get(req.target.as_str()).and_then(|c| c.gavel.as_ref()) {
                Some(g) => HardwareCommand::GavelStrike {
                    rest: g.rest,
                    raise: g.raise,
                    strike: g.strike,
                    raise_dwell_ms: g.raise_dwell_ms,
                    strike_dwell_ms: g.strike_dwell_ms,
                    settle_dwell_ms: g.settle_dwell_ms,
                    strikes: g.strikes,
                },
                None => HardwareCommand::Gavel,
            }
        }
        CmdSpec::GavelStrike {
            rest,
            raise,
            strike,
            raise_dwell_ms,
            strike_dwell_ms,
            settle_dwell_ms,
            strikes,
        } => HardwareCommand::GavelStrike {
            rest,
            raise,
            strike,
            raise_dwell_ms,
            strike_dwell_ms,
            settle_dwell_ms,
            strikes,
        },
        CmdSpec::GavelJog { us } => HardwareCommand::GavelJog(us),
        CmdSpec::Led { mode } => match LedMode::from_str(&mode) {
            Some(m) => HardwareCommand::Led(m),
            None => {
                return (StatusCode::BAD_REQUEST, format!("unknown led mode '{mode}'"))
                    .into_response()
            }
        },
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
            if req.target == Role::JudgeNeck {
                face_mirror = Some(MaintenanceCommand {
                    target: Role::JudgeFace,
                    cmd: HardwareCommand::FaceAim { pan, tilt },
                    reply: None, // fire-and-forget; the face may be absent
                });
            }
            match cal.aim_to_raw(pan, tilt) {
                Ok((p, t)) => {
                    // The operator owns the aim now: stop any in-flight glide
                    // and record where this role points for the next one.
                    s.targeting.take_over();
                    s.targeting.note_aim(req.target, pan, tilt);
                    HardwareCommand::Aim { pan: p, tilt: t }
                }
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            }
        }
    };

    if let Some(m) = face_mirror {
        let _ = s.maint_cmd_tx.send(m).await;
    }

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
/// Single-frame JPEG proxy. Unlike `/vision/feed` (an endless
/// `multipart/x-mixed-replace` stream that Safari's <img> refuses to render
/// through the proxy's keep-alive connection), this returns one finite
/// `image/jpeg` the console polls — universally renderable in every browser.
async fn vision_snapshot(AxumState(s): AxumState<AppState>) -> Response {
    let url = format!("{}/snapshot", s.vision_base_url.trim_end_matches('/'));
    match s
        .vision_http
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let ct = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/jpeg")
                .to_string();
            match resp.bytes().await {
                Ok(body) => (
                    status,
                    [
                        (axum::http::header::CONTENT_TYPE, ct),
                        (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
                    ],
                    body,
                )
                    .into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("vision read error: {e}")).into_response(),
            }
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "vision offline").into_response(),
    }
}

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

/// Read-only trial snapshot for the lawyer-phone service: current phase,
/// charge, plea, verdict — whatever the FSM state carries right now.
async fn trial_state(AxumState(s): AxumState<AppState>) -> Response {
    match s.trial_snapshot.read() {
        Ok(snap) => axum::Json(snap.clone()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "snapshot poisoned").into_response(),
    }
}

/// Proxy counsel's `/status` (registered? in a call?) for the console panel.
/// Call-lifecycle push from counsel: the defendant picked up / hung up the
/// lawyer phone. Tracks the live-call flag always; forwards the pause event to
/// the FSM only while the trial integration is enabled (the resume event is
/// always forwarded so a mid-call toggle-off can't leave a clock frozen).
#[derive(Deserialize)]
struct LawyerEventReq {
    event: String,
}

/// Firmware judge-neck `TILT_DROOP` (raw µs). Above the calibrated tilt max
/// (1967), so it's sent RAW — `aim_to_raw` would clamp it back to the working
/// range. See [[judge-neck-motion-safety]].
const NECK_DROOP_TILT_RAW: i32 = 2167;

/// Pure decision for F7: the raw neck `Aim` to send, or `None` when disabled.
/// `droop` = call active → full droop tilt; otherwise restore to `home_tilt`.
/// Extracted so it's testable without building an `AppState`.
fn neck_droop_command(
    enabled: bool,
    droop: bool,
    pan_center: i32,
    home_tilt: i32,
) -> Option<HardwareCommand> {
    if !enabled {
        return None;
    }
    let tilt = if droop { NECK_DROOP_TILT_RAW } else { home_tilt };
    Some(HardwareCommand::Aim { pan: pan_center, tilt })
}

/// F7: droop the neck to full power-down tilt while on a lawyer call, or restore
/// to home when it ends. Pan is held at center (raw, from the 0°/0° mapping) to
/// satisfy the firmware droop-zone pan-lock. No-op unless enabled, or if the
/// neck has no calibration loaded.
async fn drive_neck_droop(s: &AppState, droop: bool) {
    if !s.lawyer_neck_droop_on_call {
        return;
    }
    let (pan_center, home_tilt) = {
        let reg = s.calibration.read().await;
        match reg
            .get(Role::JudgeNeck.as_str())
            .and_then(|c| c.aim_to_raw(0.0, 0.0).ok())
        {
            Some(pair) => pair,
            None => {
                warn!("F7 neck droop: no judge_neck calibration; skipping");
                return;
            }
        }
    };
    if let Some(cmd) = neck_droop_command(s.lawyer_neck_droop_on_call, droop, pan_center, home_tilt)
    {
        info!(droop, "F7: sending neck droop/restore");
        let _ = s
            .maint_cmd_tx
            .send(MaintenanceCommand { target: Role::JudgeNeck, cmd, reply: None })
            .await;
    }
}

/// F5: counsel POSTs a burst of the lawyer's spoken audio (phone-band 8 kHz
/// s16le PCM) here; we fan it out to the primary-speaker kiosk with a
/// `LawyerAudio` header so the client applies a telephone filter instead of the
/// judge's robot voice. No-op (204) unless `[lawyer] speaker_playback` is on.
async fn lawyer_audio(
    AxumState(s): AxumState<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !s.lawyer_speaker_playback.load(Ordering::Relaxed) || body.is_empty() {
        return StatusCode::NO_CONTENT;
    }
    let _ = s
        .display_bcast
        .send(DisplayMessage::Json(DisplayEvent::LawyerAudio {
            format: "pcm_s16le_8000".into(),
        }));
    let _ = s.display_bcast.send(DisplayMessage::Binary(body));
    StatusCode::NO_CONTENT
}

async fn lawyer_event(
    AxumState(s): AxumState<AppState>,
    Json(req): Json<LawyerEventReq>,
) -> impl IntoResponse {
    match req.event.as_str() {
        "call_started" => {
            s.lawyer_call_active.store(true, Ordering::Relaxed);
            info!("lawyer call started");
            if s.lawyer_enabled.load(Ordering::Relaxed) {
                let _ = s.event_tx.send(Event::LawyerCallStarted).await;
            }
            drive_neck_droop(&s, true).await;
        }
        "call_ended" => {
            s.lawyer_call_active.store(false, Ordering::Relaxed);
            info!("lawyer call ended");
            let _ = s.event_tx.send(Event::LawyerCallEnded).await;
            drive_neck_droop(&s, false).await;
        }
        other => {
            warn!(event = other, "unknown lawyer event");
            return StatusCode::BAD_REQUEST;
        }
    }
    StatusCode::NO_CONTENT
}

#[derive(Serialize, Deserialize)]
struct LawyerIntegrationState {
    enabled: bool,
}

async fn get_lawyer_integration(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    Json(LawyerIntegrationState { enabled: s.lawyer_enabled.load(Ordering::Relaxed) })
}

async fn set_lawyer_integration(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<LawyerIntegrationState>,
) -> impl IntoResponse {
    s.lawyer_enabled.store(body.enabled, Ordering::Relaxed);
    info!(enabled = body.enabled, "lawyer trial integration toggled");
    (StatusCode::OK, Json(LawyerIntegrationState { enabled: body.enabled }))
}

async fn lawyer_status(AxumState(s): AxumState<AppState>) -> Response {
    let url = format!("{}/status", s.lawyer_base_url.trim_end_matches('/'));
    lawyer_forward(s.vision_http.get(&url).timeout(Duration::from_secs(2))).await
}

/// Proxy a ring-out to counsel. Long timeout — counsel lets the phone ring
/// up to ~25 s before reporting no-answer, and the console wants the truth.
async fn lawyer_call(
    AxumState(s): AxumState<AppState>,
    body: Option<axum::Json<serde_json::Value>>,
) -> Response {
    let url = format!("{}/call", s.lawyer_base_url.trim_end_matches('/'));
    let payload = body.map(|axum::Json(v)| v).unwrap_or(serde_json::json!({}));
    lawyer_forward(
        s.vision_http
            .post(&url)
            .json(&payload)
            .timeout(Duration::from_secs(40)),
    )
    .await
}

async fn lawyer_forward(req: reqwest::RequestBuilder) -> Response {
    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            match resp.bytes().await {
                Ok(body) => (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
                    .into_response(),
                Err(e) => {
                    (StatusCode::BAD_GATEWAY, format!("lawyer read error: {e}")).into_response()
                }
            }
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "lawyer offline").into_response(),
    }
}

/// Aim command streamed from the vision process (degrees). Relayed to the turret
/// only when targeting is armed; otherwise accepted-and-ignored so vision can
/// track visibly without the gun moving.
#[derive(Deserialize)]
struct AimMsg {
    pan: f32,
    tilt: f32,
    /// Eye-safety verdict for this frame (m4b). Optional so an older vision
    /// build that omits it reads as not-ok — fail-safe.
    #[serde(default)]
    fire_ok: bool,
}

// (vision also posts a `locked` flag; serde ignores unknown fields — `fire_ok`
// subsumes it.)

async fn vision_aim(AxumState(s): AxumState<AppState>, Json(aim): Json<AimMsg>) -> StatusCode {
    // Record the safety verdict on every frame, even while disarmed, so the
    // trial FIRE gate has a fresh value the instant the operator arms.
    s.vision_gate.record(aim.fire_ok);

    let armed = s.targeting_armed.load(Ordering::Relaxed);

    // Auto-fire: once the lock has held for the operator-set dwell (armed +
    // enabled), fire the squirt for its console-configured duration. The dwell
    // timing lives in `AutoFire` (frame-rate, server-side); we just act on the
    // one-shot trip here. Maintenance-only: outside maintenance mode the frame
    // is fed as disarmed so the dwell resets and auto-fire can never trip
    // during a trial (the FSM arms targeting for the pre-verdict lock-on, and
    // that arm must not be able to squirt anyone before a verdict).
    let maint = s.maintenance.load(Ordering::Relaxed);
    if s.auto_fire.on_frame(armed && maint, aim.fire_ok) {
        let fire_ms = {
            let reg = s.calibration.read().await;
            reg.get(Role::Squirt.as_str())
                .and_then(|c| c.fire_ms)
                .unwrap_or(150)
        };
        info!(fire_ms, "auto-fire: lock held for dwell, firing squirt");
        let _ = s
            .maint_cmd_tx
            .send(MaintenanceCommand {
                target: Role::Squirt,
                cmd: HardwareCommand::Fire(fire_ms),
                reply: None,
            })
            .await;
    }

    if !armed {
        return StatusCode::NO_CONTENT; // disarmed: don't move the gun
    }
    // Vision owns the aim while armed: stop any in-flight recenter glide and
    // keep the aim tracker current for the next one.
    s.targeting.take_over();
    // Apply the turret calibration (degrees → raw µs, clamped to limits).
    let raw = {
        let reg = s.calibration.read().await;
        reg.get(Role::Turret.as_str())
            .and_then(|c| c.aim_to_raw(aim.pan, aim.tilt).ok())
    };
    if let Some((pan, tilt)) = raw {
        s.targeting.note_aim(Role::Turret, aim.pan, aim.tilt);
        let _ = s
            .maint_cmd_tx
            .send(MaintenanceCommand {
                target: Role::Turret,
                cmd: HardwareCommand::Aim { pan, tilt },
                reply: None, // fire-and-forget high-rate stream
            })
            .await;
    }
    // Judge-neck mirror: while vision is driving the turret, the judge's head
    // tracks the same target — it visibly *looks at* the defendant during the
    // pre-verdict lock-on — and the eye's catchlight counter-moves via the
    // FaceAim fan-out (same pairing as the maintenance AIM handler). The
    // turret-frame aim goes through the neck's `[follow]` transform (scale /
    // mirror, console-tunable) first, so the head can track subtly and face
    // the right way; the neck calibration then clamps to its own softer
    // limits. Absent devices just drop.
    let neck = {
        let reg = s.calibration.read().await;
        reg.get(Role::JudgeNeck.as_str()).and_then(|c| {
            let (p, t) = c.follow_aim(aim.pan, aim.tilt);
            c.aim_to_raw(p, t).ok().map(|raw| (raw, (p, t)))
        })
    };
    if let Some(((pan, tilt), (neck_pan, neck_tilt))) = neck {
        s.targeting.note_aim(Role::JudgeNeck, neck_pan, neck_tilt);
        let _ = s
            .maint_cmd_tx
            .send(MaintenanceCommand {
                target: Role::JudgeNeck,
                cmd: HardwareCommand::Aim { pan, tilt },
                reply: None,
            })
            .await;
        let _ = s
            .maint_cmd_tx
            .send(MaintenanceCommand {
                target: Role::JudgeFace,
                cmd: HardwareCommand::FaceAim { pan: neck_pan, tilt: neck_tilt },
                reply: None,
            })
            .await;
    }
    StatusCode::NO_CONTENT
}

#[derive(Serialize, Deserialize)]
struct ArmState {
    armed: bool,
}

async fn get_targeting_arm(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    Json(ArmState { armed: s.targeting_armed.load(Ordering::Relaxed) })
}

async fn set_targeting_arm(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<ArmState>,
) -> Response {
    if body.armed {
        // Fresh acquisition BEFORE the flag flips. While disarmed, vision's aim
        // integrator winds up: its aim isn't relayed, the gun doesn't move, so
        // the boresight error never converges and the commanded aim saturates
        // at the limits. Arming without a reset slung the gun to that stale aim
        // on the first relayed frame. Same /target reset the trial Acquire cue
        // does (re-selects the saved target part, clears any person selection);
        // refusing to arm when it fails keeps the invariant that armed ⇒
        // integrator reset — the console poll snaps the button back.
        let part = {
            let reg = s.calibration.read().await;
            reg.get("vision")
                .and_then(|c| c.vision.as_ref())
                .map(|v| v.target_part.clone())
                .unwrap_or_else(|| "head".into())
        };
        let reset = s
            .vision_http
            .post(format!("{}/target", s.vision_base_url.trim_end_matches('/')))
            .timeout(Duration::from_secs(2))
            .json(&serde_json::json!({ "part": part }))
            .send()
            .await;
        if !matches!(reset, Ok(ref resp) if resp.status().is_success()) {
            warn!("vision targeting arm refused: aim reset unreachable (vision offline?)");
            return (StatusCode::BAD_GATEWAY, "vision offline — not armed").into_response();
        }
    }
    s.targeting_armed.store(body.armed, Ordering::Relaxed);
    info!(armed = body.armed, "vision targeting arm");
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Serialize)]
struct AutoFireStatus {
    enabled: bool,
    dwell_ms: u64,
    /// How long the current lock has held (ms); 0 when not locked.
    locked_ms: u64,
    /// Mirror of targeting arm — auto-fire only acts while armed.
    armed: bool,
}

#[derive(Deserialize)]
struct AutoFirePatch {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    dwell_ms: Option<u64>,
}

async fn get_auto_fire(AxumState(s): AxumState<AppState>) -> impl IntoResponse {
    let (enabled, dwell_ms, locked_ms) = s.auto_fire.status();
    Json(AutoFireStatus {
        enabled,
        dwell_ms,
        locked_ms,
        armed: s.targeting_armed.load(Ordering::Relaxed),
    })
}

async fn set_auto_fire(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<AutoFirePatch>,
) -> Response {
    // Enabling is gated on maintenance mode (a tuning tool, not a show
    // feature); disabling and dwell edits are always allowed.
    if body.enabled == Some(true) && !s.maintenance.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "auto-fire requires maintenance mode").into_response();
    }
    s.auto_fire.set(body.enabled, body.dwell_ms);
    info!(
        enabled = s.auto_fire.enabled(),
        dwell_ms = s.auto_fire.dwell_ms(),
        "vision auto-fire updated"
    );
    StatusCode::NO_CONTENT.into_response()
}

/// Forward the operator's targeting-control POSTs (target part, boresight pixel)
/// to the vision process, keeping the console same-origin.
async fn vision_forward_post(s: &AppState, sub: &str, body: Bytes) -> Response {
    let url = format!("{}/{}", s.vision_base_url.trim_end_matches('/'), sub);
    match s
        .vision_http
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(2))
        .body(body)
        .send()
        .await
    {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            (status, resp.bytes().await.unwrap_or_default()).into_response()
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "vision offline").into_response(),
    }
}

async fn vision_target(AxumState(s): AxumState<AppState>, body: Bytes) -> Response {
    vision_forward_post(&s, "target", body).await
}

/// Click-to-aim: one-shot open-loop nudge toward a clicked feed pixel.
async fn vision_aimpoint(AxumState(s): AxumState<AppState>, body: Bytes) -> Response {
    vision_forward_post(&s, "aimpoint", body).await
}

/// Click-to-track: select (or clear) the person track under a clicked pixel.
async fn vision_select(AxumState(s): AxumState<AppState>, body: Bytes) -> Response {
    vision_forward_post(&s, "select", body).await
}

async fn vision_boresight(AxumState(s): AxumState<AppState>, body: Bytes) -> Response {
    vision_forward_post(&s, "boresight", body).await
}

async fn vision_gains(AxumState(s): AxumState<AppState>, body: Bytes) -> Response {
    vision_forward_post(&s, "gains", body).await
}

/// One-click recovery from an overshoot: disarm targeting, reset vision's aim
/// integrator, and glide the turret (and the judge's gaze) back to center —
/// works even while disarmed, so the operator doesn't have to enter
/// maintenance to re-center.
async fn vision_center(AxumState(s): AxumState<AppState>) -> Response {
    s.targeting_armed.store(false, Ordering::Relaxed);
    // Reset the vision-side integrator + stop tracking (best-effort).
    let _ = s
        .vision_http
        .post(format!("{}/center", s.vision_base_url.trim_end_matches('/')))
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    // Eased return to center — same calm glide as the trial-idle recenter.
    s.targeting.spawn_glide(&[Role::Turret, Role::JudgeNeck], 0.0, 0.0);
    StatusCode::NO_CONTENT.into_response()
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
    // Last connection wins: bump the generation; the previous session notices it
    // is no longer current and exits. This makes reconnects (and the operator
    // re-opening the page) reliable instead of getting a "single client only"
    // rejection from a not-yet-cleaned-up stale socket.
    let my_gen = state.ws_generation.fetch_add(1, Ordering::SeqCst) + 1;
    ws.on_upgrade(move |socket| ws_session(socket, state, my_gen))
}

/// Read-only WebSocket for presentational monitors (the `/case` view).
/// Subscribes to the display broadcast and ignores anything the client sends.
/// Doesn't participate in the single-client budget that `/ws` enforces, so
/// multiple read-only viewers can connect simultaneously.
///
/// `?audio=1` opts a viewer in as the booth's speakers: it also receives the
/// binary PCM frames (TTS audio). Exactly one audio viewer is live at a time —
/// the newest one wins, older ones silently stop getting PCM (they keep the
/// JSON stream; no close/reconnect churn on a kiosk).
async fn view_ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<ViewWsParams>,
    AxumState(state): AxumState<AppState>,
) -> impl IntoResponse {
    let flag = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|a| matches!(a, "1" | "true" | "yes"))
    };
    let audio_gen = flag(&q.audio)
        .then(|| state.audio_generation.fetch_add(1, Ordering::SeqCst) + 1);
    let mic_gen = flag(&q.mic)
        .then(|| state.mic_generation.fetch_add(1, Ordering::SeqCst) + 1);
    ws.on_upgrade(move |socket| view_ws_session(socket, state, audio_gen, mic_gen))
}

#[derive(Deserialize)]
struct ViewWsParams {
    #[serde(default)]
    audio: Option<String>,
    #[serde(default)]
    mic: Option<String>,
}

async fn view_ws_session(
    mut socket: WebSocket,
    state: AppState,
    audio_gen: Option<usize>,
    mic_gen: Option<usize>,
) {
    info!(
        audio = audio_gen.is_some(),
        mic = mic_gen.is_some(),
        "view ws client connected"
    );
    // A mic viewer announces itself so the operator console shuts its own mic.
    if mic_gen.is_some() {
        state.mic_present.store(true, Ordering::SeqCst);
        let _ = state
            .display_bcast
            .send(DisplayMessage::Json(DisplayEvent::MicOwner { present: true }));
    }
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&snapshot_event(&state)).unwrap().into(),
        ))
        .await;
    // Seed the persona's robot voice params — needed by an ?audio=1 viewer to
    // colour playback; harmless for the rest.
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
                    // PCM goes only to the *current* audio viewer.
                    let live = audio_gen
                        .is_some_and(|g| state.audio_generation.load(Ordering::SeqCst) == g);
                    if live && socket.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("view ws lagged {n} display messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => {
                // Uplink is accepted only from the *current* mic viewer: the
                // plea lifecycle events and the recorded audio frames. Everyone
                // else stays read-only (superseded mic viewers included).
                let live_mic = mic_gen
                    .is_some_and(|g| state.mic_generation.load(Ordering::SeqCst) == g);
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(t))) if live_mic => {
                        handle_client_text(&t, &state).await;
                    }
                    Some(Ok(Message::Binary(b))) if live_mic => {
                        state.plea_buffer.lock().await.extend_from_slice(&b);
                    }
                    Some(Ok(_)) => {} // ignore — read-only
                    Some(Err(e)) => { warn!("view ws error: {e}"); break; }
                }
            }
        }
    }
    // If the live mic viewer is going away, hand the mic back to the operator
    // console (superseded viewers don't clear the newer owner's claim).
    if mic_gen.is_some_and(|g| state.mic_generation.load(Ordering::SeqCst) == g) {
        state.mic_present.store(false, Ordering::SeqCst);
        let _ = state
            .display_bcast
            .send(DisplayMessage::Json(DisplayEvent::MicOwner { present: false }));
    }
    info!("view ws client disconnected");
}

/// Connect-time resync event: the live trial view-state from the FSM's
/// snapshot mirror, so any (re)connecting client renders the current phase
/// instead of stale idle. Verdict fields are withheld while the phase is
/// `pronouncing_verdict` — on the pipelined path that state begins a whole
/// deliberation-TTS before the reveal, and a mid-trial reconnect must never
/// flash GUILTY early. (By `executing_sentence` the reveal has happened.)
fn snapshot_event(s: &AppState) -> DisplayEvent {
    let snap = s
        .trial_snapshot
        .read()
        .map(|g| g.clone())
        .unwrap_or_default();
    let revealed = snap.phase == "executing_sentence";
    let (operator_armed, operator_active) = s.operator_modes.snapshot();
    DisplayEvent::Snapshot {
        phase: snap.phase.to_string(),
        charge: snap.charge,
        plea: snap.plea,
        cross_question: snap.cross_question,
        verdict_guilty: snap.verdict.as_ref().filter(|_| revealed).map(|v| v.guilty),
        verdict_remarks: snap
            .verdict
            .as_ref()
            .filter(|_| revealed)
            .map(|v| v.remarks.clone()),
        verdict_key_factor: snap
            .verdict
            .as_ref()
            .filter(|_| revealed)
            .and_then(|v| v.key_factor.clone()),
        deadline_ms: snap.paused_remaining.map(|d| d.as_millis() as u64).or_else(|| {
            snap.deadline
                .map(|d| d.saturating_duration_since(std::time::Instant::now()).as_millis() as u64)
        }),
        clock_paused: snap.paused_remaining.is_some(),
        maintenance: s.maintenance.load(Ordering::Relaxed),
        mic_owner: s.mic_present.load(Ordering::SeqCst),
        operator_armed,
        operator_active,
    }
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
        gain: p.robot.gain,
    }
}

async fn ws_session(mut socket: WebSocket, state: AppState, my_gen: usize) {
    info!("ws client connected");
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&snapshot_event(&state)).unwrap().into(),
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
    // Periodically check whether a newer client has superseded us.
    let mut supersede_check = tokio::time::interval(Duration::from_millis(400));

    loop {
        tokio::select! {
            _ = supersede_check.tick() => {
                if state.ws_generation.load(Ordering::SeqCst) != my_gen {
                    info!("ws superseded by a newer client");
                    // Tell the displaced client *why* it's closing so it stays
                    // dormant instead of auto-reconnecting — otherwise two open
                    // operator consoles supersede each other forever (each
                    // reconnect bumps the generation and evicts the other).
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: WS_SUPERSEDED,
                            reason: "superseded".into(),
                        })))
                        .await;
                    break;
                }
            }
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

    info!("ws client disconnected");
}

async fn handle_client_text(text: &str, state: &AppState) {
    match serde_json::from_str::<ClientEvent>(text) {
        Ok(ClientEvent::Ready) => debug!("client ready"),
        Ok(ClientEvent::PleaRecordingStarted) => {
            // A fresh recording is starting: drop any bytes a dead client
            // uploaded before it vanished mid-window (a stale partial blob
            // prepended to the new one would corrupt the container format).
            state.plea_buffer.lock().await.clear();
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
            let face = reg.active().face_persona.clone();
            drop(reg);
            let _ = s.display_bcast.send(DisplayMessage::Json(ev));
            send_face_persona(&s, face).await;
            (StatusCode::NO_CONTENT, String::new()).into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// Push an eye-theme switch to the LED-matrix judge face. Fire-and-forget —
/// an absent face just drops it (it re-syncs on reconnect via the
/// DeviceConnected watcher in main).
async fn send_face_persona(s: &AppState, slug: String) {
    let _ = s
        .maint_cmd_tx
        .send(MaintenanceCommand {
            target: Role::JudgeFace,
            cmd: HardwareCommand::Persona(slug),
            reply: None,
        })
        .await;
}

/// Best-effort voice validation against the live TTS backend: a tiny synth
/// probe at persona save time, so a voice that has drifted out of the Kokoro
/// deployment fails the edit with a clear 400 instead of 500ing mid-verdict.
/// An unreachable/slow backend accepts with a warning (offline editing works);
/// mock mode skips entirely.
async fn validate_voice(cfg: &InferenceConfig, voice: &str) -> Result<(), String> {
    if cfg.mode != "real" {
        return Ok(());
    }
    let client = LlmClient::new(cfg);
    match client.synth_pcm_stream("ok", voice, None, Duration::from_secs(4)).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{e:#}");
            // Only a definite backend rejection blocks the save; anything that
            // smells like "backend down/slow" must not break persona editing.
            let looks_like_rejection =
                msg.contains("400") || msg.to_ascii_lowercase().contains("voice");
            if looks_like_rejection {
                Err(format!("voice '{voice}' rejected by the TTS backend: {msg}"))
            } else {
                warn!("voice probe inconclusive (accepting '{voice}'): {msg}");
                Ok(())
            }
        }
    }
}

async fn update_persona(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Persona>,
) -> impl IntoResponse {
    if let Err(msg) = validate_voice(&s.inference_cfg, &body.tts_voice).await {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    let mut reg = s.personas.write().await;
    if !reg.get(&id).is_some() {
        return (StatusCode::NOT_FOUND, format!("unknown persona '{id}'")).into_response();
    }
    let is_active = reg.active_id() == id;
    match reg.update(&id, body.clone()) {
        Ok(()) => {
            let updated = reg.get(&id).unwrap().clone();
            drop(reg);
            // If this is the live persona, push its (possibly retuned) robot
            // params to audio clients so the change is heard immediately —
            // and its (possibly re-themed) eye to the LED face.
            if is_active {
                let _ = s.display_bcast.send(DisplayMessage::Json(robot_params_event(&updated)));
                send_face_persona(&s, updated.face_persona.clone()).await;
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
    if let Err(msg) = validate_voice(&s.inference_cfg, &body.tts_voice).await {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    key_factor: Option<String>,
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
            Some(p) => reg.verdict_prompt(p),
            None => return (StatusCode::NOT_FOUND, format!("unknown persona '{id}'")).into_response(),
        }
    };

    let client = LlmClient::new(&s.inference_cfg);
    let user_msg = format!("CHARGE: {}\n\nPLEA: {}\n\nRender your verdict.", body.charge, body.plea);
    let first_to = Duration::from_secs(s.inference_cfg.verdict_first_token_timeout_secs);
    let total_to = Duration::from_secs(s.inference_cfg.verdict_total_timeout_secs);

    let stream = match client
        .chat_stream(&system_prompt, &user_msg, s.inference_cfg.verdict_temperature, first_to, total_to)
        .await
    {
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
        Some(p) => TestResp { deliberation: p.deliberation, guilty: p.guilty, key_factor: p.key_factor },
        None => TestResp { deliberation: full.trim().to_string(), guilty: false, key_factor: None },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

// ---- Crime list operator endpoints ----

#[derive(Serialize)]
struct CrimesResp {
    crimes: Vec<Crime>,
    categories: Vec<String>,
    disabled_categories: Vec<String>,
    queue: Vec<String>,
}

async fn crimes_snapshot(s: &AppState) -> CrimesResp {
    let store = s.crimes.read().await;
    CrimesResp {
        crimes: store.list().to_vec(),
        categories: store.categories(),
        disabled_categories: store.disabled_categories().iter().cloned().collect(),
        queue: store.queue().map(str::to_string).collect(),
    }
}

/// Re-read the crimes file from disk (picks up out-of-process crimes-editor
/// edits without a booth restart). Preserves the operator queue / filter /
/// no-repeat history.
async fn reload_crimes(AxumState(s): AxumState<AppState>) -> Response {
    let mut store = s.crimes.write().await;
    match store.reload() {
        Ok(()) => {
            let n = store.list().len();
            info!(count = n, "crimes reloaded from disk");
            (StatusCode::OK, Json(serde_json::json!({ "count": n }))).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("reload failed: {e:#}")).into_response(),
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
struct DisabledCategoriesReq {
    /// Full replacement set; empty list re-enables every category.
    disabled: std::collections::BTreeSet<String>,
}

async fn set_disabled_categories(
    AxumState(s): AxumState<AppState>,
    Json(body): Json<DisabledCategoriesReq>,
) -> impl IntoResponse {
    let mut store = s.crimes.write().await;
    match store.set_disabled_categories(body.disabled) {
        Ok(()) => {
            info!(disabled = ?store.disabled_categories(), "crime categories updated");
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

#[cfg(test)]
mod f7_tests {
    use super::{neck_droop_command, NECK_DROOP_TILT_RAW};
    use crate::hardware::protocol::HardwareCommand;

    #[test]
    fn neck_droop_command_gated_and_correct() {
        // Disabled → never commands the neck.
        assert!(neck_droop_command(false, true, 1583, 1500).is_none());
        assert!(neck_droop_command(false, false, 1583, 1500).is_none());

        // Enabled + call active → full droop tilt at center pan.
        match neck_droop_command(true, true, 1583, 1500) {
            Some(HardwareCommand::Aim { pan, tilt }) => {
                assert_eq!(pan, 1583);
                assert_eq!(tilt, NECK_DROOP_TILT_RAW);
            }
            other => panic!("expected droop Aim, got {other:?}"),
        }

        // Enabled + call ended → restore to home tilt.
        match neck_droop_command(true, false, 1583, 1500) {
            Some(HardwareCommand::Aim { pan, tilt }) => {
                assert_eq!(pan, 1583);
                assert_eq!(tilt, 1500);
            }
            other => panic!("expected home Aim, got {other:?}"),
        }
    }
}
