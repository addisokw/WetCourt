use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, AtomicUsize}, Arc};

use anyhow::Result;
use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod calibration;
mod config;
mod crimes;
mod display;
mod fallbacks;
mod hardware;
mod inference;
mod personas;
mod state_machine;

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

    // Inference: real LiteLLM client (charge + verdict) for Phase 2; STT/TTS
    // still mocked. Set [inference] mode = "mock" for offline dev.
    {
        let cfg = cfg.clone();
        let personas = personas.clone();
        let crimes = crimes.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        tokio::spawn(async move {
            inference::run(cfg, personas, crimes, inference_rx, event_tx, display_tx).await;
        });
    }

    // Display broadcast (ws fan-out) — defined before the hardware driver so the
    // driver can publish device-presence events (DeviceConnected/Disconnected).
    let display_bcast = broadcast::channel::<DisplayMessage>(256).0;

    // Maintenance direct-control sink + shared device-presence snapshot. The
    // hardware driver consumes `maint_cmd_rx` and keeps `devices` in sync as
    // devices connect/disconnect (the mock driver seeds all roles present).
    let (maint_cmd_tx, maint_cmd_rx) =
        mpsc::channel::<hardware::maintenance::MaintenanceCommand>(64);
    let devices = Arc::new(tokio::sync::RwLock::new(Vec::new()));

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
        tokio::spawn(async move {
            let mut hardware_rx = hardware_rx;
            while let Some(cmd) = hardware_rx.recv().await {
                if let state_machine::Command::Hardware(hc) = cmd {
                    if hw_cmd_tx.send(hc).await.is_err() {
                        break;
                    }
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

    // HTTP client for the vision reverse-proxy. No global timeout — the MJPEG
    // feed is an infinite stream; a per-request timeout guards the /state calls.
    let vision_http = reqwest::Client::builder()
        .build()
        .expect("building vision http client");

    let app_state = AppState {
        event_tx: event_tx.clone(),
        display_bcast: display_bcast.clone(),
        ws_clients: Arc::new(AtomicUsize::new(0)),
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
    let runtime = Runtime::new(cfg.clone(), cross_enabled, maintenance, is_idle, event_rx, inference_tx, hardware_tx, display_tx);
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
