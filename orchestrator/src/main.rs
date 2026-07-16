use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, AtomicUsize}, Arc};

use anyhow::Result;
use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod calibration;
mod capture;
mod config;
mod crimes;
mod display;
mod fallbacks;
mod hardware;
mod inference;
mod personas;
// Thermal-printer keepsake transcript: report renderer, casebook trial log, and
// the printer service. Driven from the state machine at each completed verdict.
mod printer;
mod lawyer;
mod state_machine;
mod targeting;

use display::{AppState, DisplayMessage};
use state_machine::Runtime;
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(name = "booth", about = "Wet Court of Appeals orchestrator")]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv();
    let cli = Cli::parse();
    let cfg = Arc::new(config::load(&cli.config)?);

    init_tracing(&cfg.logging.level);
    tracing::info!(path = %cli.config.display(), "config loaded");
    tracing::info!(
        driver = %cfg.hardware.driver,
        listen = %cfg.display.listen_addr,
        "booting"
    );

    let (event_tx, event_rx) = mpsc::channel::<state_machine::Event>(64);
    let (inference_tx, inference_rx) = mpsc::channel::<state_machine::Command>(32);
    let (hardware_tx, hardware_rx) = mpsc::channel::<state_machine::Command>(32);
    let (display_tx, display_rx) = mpsc::channel::<state_machine::Command>(64);

    // Persona registry: directory sits next to the binary's working dir;
    // resolve relative to the config file's parent so a non-default config
    // can co-locate its personas dir without surprises.
    let personas_dir = cli
        .config
        .parent()
        .map(|p| p.join("personas"))
        .unwrap_or_else(|| PathBuf::from("personas"));
    let registry = personas::PersonaRegistry::load_from_dir(&personas_dir, &cfg.default_persona_id)
        .map_err(|e| anyhow::anyhow!("loading personas from {}: {e:#}", personas_dir.display()))?;
    let personas = Arc::new(tokio::sync::RwLock::new(registry));
    tracing::info!(dir = %personas_dir.display(), active = %cfg.default_persona_id, "personas loaded");

    // Crime list: same convention as personas — resolved relative to the
    // config file so the curated file travels with the deployment.
    let crimes_path = cli
        .config
        .parent()
        .map(|p| p.join(&cfg.crimes.file))
        .unwrap_or_else(|| PathBuf::from(&cfg.crimes.file));
    let store = crimes::CrimeStore::load_from_file(&crimes_path, cfg.crimes.no_repeat_window)
        .map_err(|e| anyhow::anyhow!("loading crimes from {}: {e:#}", crimes_path.display()))?;
    tracing::info!(
        path = %crimes_path.display(),
        count = store.list().len(),
        source = %cfg.crimes.source,
        "crimes loaded"
    );
    let crimes = Arc::new(tokio::sync::RwLock::new(store));

    // Per-device calibration: same convention as personas/crimes — a
    // `calibration/` dir next to the config file, one `<role>.toml` per device.
    let calibration_dir = cli
        .config
        .parent()
        .map(|p| p.join("calibration"))
        .unwrap_or_else(|| PathBuf::from("calibration"));
    let calibration_registry = calibration::CalibrationRegistry::load_from_dir(&calibration_dir)
        .map_err(|e| anyhow::anyhow!("loading calibration from {}: {e:#}", calibration_dir.display()))?;
    tracing::info!(
        dir = %calibration_dir.display(),
        count = calibration_registry.list().len(),
        "calibration loaded"
    );
    let calibration = Arc::new(tokio::sync::RwLock::new(calibration_registry));

    // Casebook: the append-only trial log (`[logging] transcripts_jsonl`),
    // resolved relative to the config file like the other deployment resources.
    // It also seeds the case counter. The printer service renders + emits the
    // keepsake receipt per completed verdict, per `[printer] mode`.
    let casebook_path = cli
        .config
        .parent()
        .map(|p| p.join(&cfg.logging.transcripts_jsonl))
        .unwrap_or_else(|| PathBuf::from(&cfg.logging.transcripts_jsonl));
    let casebook = Arc::new(printer::Casebook::open(&casebook_path));
    tracing::info!(
        path = %casebook_path.display(),
        next_case = casebook.next_case_no(),
        "casebook ready"
    );
    // The state machine needs its own persona handle; `personas` is moved into
    // AppState below.
    let personas_for_sm = personas.clone();

    // Display broadcast (ws fan-out) — defined before the hardware driver so the
    // driver can publish device-presence events (DeviceConnected/Disconnected),
    // and before the printer service so it can raise operator Error banners.
    let display_bcast = broadcast::channel::<DisplayMessage>(256).0;

    let print_tx = printer::service::spawn(cfg.printer.clone(), display_bcast.clone());

    // Custom-print templates: same convention as personas/crimes — a JSON file
    // next to the config so it travels with the deployment.
    let templates_path = cli
        .config
        .parent()
        .map(|p| p.join("print_templates.json"))
        .unwrap_or_else(|| PathBuf::from("print_templates.json"));
    let print_templates = printer::templates::TemplateStore::load_from_file(&templates_path)
        .map_err(|e| anyhow::anyhow!("loading print templates from {}: {e:#}", templates_path.display()))?;
    tracing::info!(
        path = %templates_path.display(),
        count = print_templates.list().len(),
        "print templates ready"
    );
    let print_templates = Arc::new(tokio::sync::RwLock::new(print_templates));

    // Maintenance direct-control sink + shared device-presence snapshot. The
    // hardware driver consumes `maint_cmd_rx` and keeps `devices` in sync as
    // devices connect/disconnect (the mock driver seeds all roles present).
    // (Created before the inference task: the verdict service uses it for the
    // LED-face reveal.)
    let (maint_cmd_tx, maint_cmd_rx) =
        mpsc::channel::<hardware::maintenance::MaintenanceCommand>(64);
    let devices = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    // Inference: real LiteLLM client (charge + verdict) for Phase 2; STT/TTS
    // still mocked. Set [inference] mode = "mock" for offline dev.
    {
        let cfg = cfg.clone();
        let personas = personas.clone();
        let crimes = crimes.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        let maint_cmd_tx = maint_cmd_tx.clone();
        tokio::spawn(async move {
            inference::run(cfg, personas, crimes, inference_rx, event_tx, display_tx, maint_cmd_tx)
                .await;
        });
    }

    // Judge-face persona sync: whenever the face (re)connects, push the active
    // persona's eye theme so the panel stops free-running its demo rotation and
    // matches the presiding judge. (Mock driver seeds DeviceConnected for every
    // role at startup, so this also covers boot.) Live persona *switches* are
    // pushed by the /operator/persona select/update handlers.
    {
        let personas = personas.clone();
        let tx = maint_cmd_tx.clone();
        let mut presence = display_bcast.subscribe();
        tokio::spawn(async move {
            loop {
                match presence.recv().await {
                    Ok(DisplayMessage::Json(display::events::DisplayEvent::DeviceConnected {
                        role,
                        ..
                    })) if role == "judge_face" => {
                        let slug = personas.read().await.active().face_persona.clone();
                        let _ = tx
                            .send(hardware::maintenance::MaintenanceCommand {
                                target: hardware::maintenance::Role::JudgeFace,
                                cmd: hardware::HardwareCommand::Persona(slug),
                                reply: None,
                            })
                            .await;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Vision targeting arm switch + lock fire gate, shared between the HTTP
    // layer (AppState) and the hardware adapter below. `targeting_armed` gates
    // whether vision drives the turret; `vision_gate` holds the latest
    // `fire_ok` so a trial FIRE is suppressed without a fresh lock (no lock,
    // no fire — when trial targeting is enabled).
    let targeting_armed = Arc::new(AtomicBool::new(false));
    let vision_gate = Arc::new(hardware::gate::VisionFireGate::new(
        hardware::gate::FIRE_OK_STALE_MS,
    ));
    // Targeting-panel auto-fire, off by default; the dwell comes from the
    // saved vision tuning (vision.toml) so it survives restarts — 2 s when
    // nothing is saved. The enabled flag is deliberately never persisted.
    let saved_dwell_ms = {
        let reg = calibration.read().await;
        reg.get("vision")
            .and_then(|c| c.vision.as_ref())
            .and_then(|v| v.autofire_dwell_ms)
            .unwrap_or(2000)
    };
    let auto_fire = Arc::new(display::autofire::AutoFire::new(saved_dwell_ms));

    // Hardware driver (mock or tcp registry). Owns both command sources — the
    // trial state machine (via the Command::Hardware adapter) and the
    // maintenance console — plus the device snapshot and presence broadcast.
    {
        let driver = hardware::build(&cfg.hardware, &cfg.mock_hw);
        let event_tx = event_tx.clone();
        let devices = devices.clone();
        let presence = display_bcast.clone();
        let (hw_cmd_tx, hw_cmd_rx) = mpsc::channel::<hardware::HardwareCommand>(32);
        // Adapter: unwrap Command::Hardware -> HardwareCommand for the driver.
        // Three policy edges live here, all keeping policy out of the FSM:
        //  - the vision fire gate (no lock, no fire) on the trial FIRE,
        //  - the squirt calibration edge — the trial FIRE duration is the
        //    console-tuned `fire_ms` from squirt.toml (the [squirt] duration_ms
        //    config is only the fallback when no calibration exists), and
        //  - the gavel calibration edge — the FSM emits a bare `Gavel` and we
        //    resolve its geometry from `gavel.toml` into a `GavelStrike` so real
        //    verdict strikes honour the console-tuned values (mirroring how the
        //    maintenance handler resolves AIM degrees→raw at its own edge).
        let gate = vision_gate.clone();
        let gate_event_tx = event_tx.clone();
        let gate_bcast = display_bcast.clone();
        let calibration_for_adapter = calibration.clone();
        let cfg_for_adapter = cfg.clone();
        tokio::spawn(async move {
            use hardware::HardwareCommand;
            let mut hardware_rx = hardware_rx;
            while let Some(cmd) = hardware_rx.recv().await {
                let state_machine::Command::Hardware(hc) = cmd else { continue };
                // No lock, no fire: with trial targeting on, the guilty FIRE only
                // reaches the squirt on a fresh `fire_ok` — the trial's Freeze
                // disarms the aim relay just before this, so arm state is
                // irrelevant; freshness is the whole check. Otherwise suppress
                // the wire send but synthesize an ack so `ExecutingSentence`
                // still advances (mirrors the absent-role handling — never stall
                // the trial), and tell the console the shot was held.
                // (With trial_targeting off the operator owns aim + shot: ungated.)
                if matches!(hc, HardwareCommand::Fire(_))
                    && cfg_for_adapter.vision.trial_targeting
                    && !gate.fresh_fire_ok()
                {
                    tracing::warn!("vision gate: holding trial FIRE (no fresh lock)");
                    let _ = gate_bcast.send(DisplayMessage::Json(
                        display::events::DisplayEvent::FireHeld {
                            reason: "no fresh target lock".into(),
                        },
                    ));
                    let _ = gate_event_tx
                        .send(state_machine::Event::HardwareAck(
                            "OK FIRE held_for_safety".into(),
                        ))
                        .await;
                    continue;
                }
                // Calibration edges: trial FIRE duration from squirt.toml; bare
                // `Gavel` into a `GavelStrike` from gavel.toml (firmware default
                // if uncalibrated).
                let hc = match hc {
                    HardwareCommand::Fire(fallback_ms) => {
                        let reg = calibration_for_adapter.read().await;
                        let ms = reg
                            .get("squirt")
                            .and_then(|c| c.fire_ms)
                            .unwrap_or(fallback_ms);
                        HardwareCommand::Fire(ms)
                    }
                    HardwareCommand::Gavel => {
                        let reg = calibration_for_adapter.read().await;
                        match reg.get("gavel").and_then(|c| c.gavel.as_ref()) {
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
                    other => other,
                };
                if hw_cmd_tx.send(hc).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            driver
                .run(hw_cmd_rx, maint_cmd_rx, event_tx, devices, presence)
                .await
        });
    }

    // Forward state-machine display commands onto the broadcast channel.
    {
        let bcast = display_bcast.clone();
        tokio::spawn(async move { display::forwarder(display_rx, bcast).await });
    }

    // Operator-toggleable cross-examination, shared between the HTTP endpoint
    // (writes) and the state machine (reads). Seeded from config.
    let cross_enabled = Arc::new(AtomicBool::new(cfg.cross_examination.enabled));

    // State mirrors for the maintenance REST gates, written by the state
    // machine: `maintenance` opens the direct-command path; `is_idle` gates
    // maintenance entry. Initial state is Idle.
    let maintenance = Arc::new(AtomicBool::new(false));
    let is_idle = Arc::new(AtomicBool::new(true));

    // Swear-in lamp resync: the button boots dark and only animates on
    // command, so whenever it (re)connects push the lamp state the FSM would
    // have cued — the attract blink in Idle, dark otherwise (maintenance or
    // mid-trial; a window's pulse cue will re-arrive on the next state edge).
    // Mirrors the judge-face PERSONA replay above; covers boot via the mock
    // driver's seeded DeviceConnected too.
    {
        let is_idle = is_idle.clone();
        let maintenance = maintenance.clone();
        let tx = maint_cmd_tx.clone();
        let mut presence = display_bcast.subscribe();
        tokio::spawn(async move {
            loop {
                match presence.recv().await {
                    Ok(DisplayMessage::Json(display::events::DisplayEvent::DeviceConnected {
                        role,
                        ..
                    })) if role == "swear_in" => {
                        let mode = if is_idle.load(std::sync::atomic::Ordering::Relaxed)
                            && !maintenance.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            hardware::protocol::LedMode::Blink
                        } else {
                            hardware::protocol::LedMode::Off
                        };
                        let _ = tx
                            .send(hardware::maintenance::MaintenanceCommand {
                                target: hardware::maintenance::Role::SwearIn,
                                cmd: hardware::HardwareCommand::Led(mode),
                                reply: None,
                            })
                            .await;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // HTTP client for the vision reverse-proxy. No global timeout — the MJPEG
    // feed is an infinite stream; a per-request timeout guards the /state calls.
    let vision_http = reqwest::Client::builder()
        .build()
        .expect("building vision http client");

    // Trial turret choreography — shares the same arm flag / vision / calibration
    // / command sink as the operator `/vision/*` endpoints.
    let targeting = Arc::new(targeting::TargetingController::new(
        targeting_armed.clone(),
        vision_http.clone(),
        cfg.vision.base_url.clone(),
        calibration.clone(),
        maint_cmd_tx.clone(),
        vision_gate.clone(),
    ));
    // Re-seed the vision process with the saved tuning (gains/tolerance/
    // boresight/target) whenever it (re)appears — vision holds those only in
    // memory, so without this every restart reverted the console's tuning.
    targeting.spawn_tuning_seeder();

    // Guilty "moment of justice" burst capture (feeds the keepsake receipt).
    let capture = Arc::new(capture::CaptureController::new(
        vision_http.clone(),
        cfg.vision.base_url.clone(),
        cfg.capture.clone(),
    ));

    let trial_snapshot = Arc::new(std::sync::RwLock::new(
        state_machine::states::TrialSnapshot::default(),
    ));

    // Lawyer-phone trial integration: operator toggle (seeded from config),
    // live-call flag (written by counsel's /lawyer/event pushes), and the
    // ring-out bridge the Runtime uses on cross-answer entry.
    let lawyer_enabled = Arc::new(AtomicBool::new(cfg.lawyer.trial_integration));
    let lawyer_call_active = Arc::new(AtomicBool::new(false));
    let lawyer_bridge = Arc::new(lawyer::LawyerBridge::new(cfg.lawyer.base_url.clone()));
    let app_state = AppState {
        event_tx: event_tx.clone(),
        display_bcast: display_bcast.clone(),
        ws_generation: Arc::new(AtomicUsize::new(0)),
        audio_generation: Arc::new(AtomicUsize::new(0)),
        mic_generation: Arc::new(AtomicUsize::new(0)),
        mic_present: Arc::new(AtomicBool::new(false)),
        plea_buffer: Arc::new(Mutex::new(Vec::new())),
        personas,
        crimes,
        inference_cfg: cfg.inference.clone(),
        cross_enabled: cross_enabled.clone(),
        maint_cmd_tx,
        maintenance: maintenance.clone(),
        is_idle: is_idle.clone(),
        calibration,
        devices,
        vision_base_url: cfg.vision.base_url.clone(),
        vision_http,
        targeting_armed,
        vision_gate,
        targeting: targeting.clone(),
        auto_fire,
        trial_snapshot: trial_snapshot.clone(),
        lawyer_enabled: lawyer_enabled.clone(),
        lawyer_call_active: lawyer_call_active.clone(),
        lawyer_base_url: cfg.lawyer.base_url.clone(),
        print_job_tx: print_tx.clone(),
        print_templates,
        printer_cfg: cfg.printer.clone(),
    };
    let app = display::router(app_state);
    let listener = tokio::net::TcpListener::bind(&cfg.display.listen_addr).await?;
    tracing::info!("display server listening on {}", cfg.display.listen_addr);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("axum exited: {e}");
        }
    });

    // State machine runs in this task; never returns until ctrl-c.
    let runtime = Runtime::new(
        cfg.clone(),
        cross_enabled,
        maintenance,
        is_idle,
        trial_snapshot,
        event_rx,
        inference_tx,
        hardware_tx,
        display_tx,
        personas_for_sm,
        casebook,
        print_tx,
        Some(targeting),
        Some(capture),
        lawyer_enabled,
        lawyer_call_active,
        Some(lawyer_bridge),
    );
    let sm = tokio::spawn(async move { runtime.run().await });

    tokio::select! {
        _ = sm => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received, shutting down");
        }
    }
    Ok(())
}

/// Load `.env` from the current directory or any ancestor, falling back to
/// the stack's `dgx-ai-stack/.env` so the same file serves the ai-stack
/// tooling and the orchestrator dev loop. Then alias the stack's standard
/// `LITELLM_MASTER_KEY` into the figment-prefixed `BOOTH__INFERENCE__API_KEY`
/// so the dev loop is a single `cargo run`.
fn load_dotenv() {
    if dotenvy::dotenv().is_err() {
        // No .env in CWD or ancestors — try the stack's copy, from either
        // orchestrator/ or the repo root. Silently ignored if absent.
        for path in ["../dgx-ai-stack/.env", "dgx-ai-stack/.env"] {
            if dotenvy::from_path(path).is_ok() {
                break;
            }
        }
    }
    if std::env::var("BOOTH__INFERENCE__API_KEY").is_err() {
        if let Ok(key) = std::env::var("LITELLM_MASTER_KEY") {
            std::env::set_var("BOOTH__INFERENCE__API_KEY", key);
        }
    }
}

fn init_tracing(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true).with_level(true))
        .init();
}
