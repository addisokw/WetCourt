use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod config;
mod display;
mod fallbacks;
mod hardware;
mod inference;
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

    // Boot-time LLM reachability probe. The state machine is happy to run
    // forever on canned fallbacks if every inference call 401s or DNS-fails;
    // we lost a debugging session to exactly that. Fail loudly here instead.
    if cfg.inference.mode == "real" {
        let client = inference::client::LlmClient::new(&cfg.inference);
        if let Err(e) = client.health_check().await {
            tracing::error!(
                base_url = %cfg.inference.base_url,
                "LLM endpoint health check FAILED: {e:#}"
            );
            tracing::error!(
                "orchestrator refusing to start — fix the LLM connection or set inference.mode = \"mock\""
            );
            return Err(e);
        }
        tracing::info!(base_url = %cfg.inference.base_url, "LLM endpoint reachable");
    }

    let (event_tx, event_rx) = mpsc::channel::<state_machine::Event>(64);
    let (inference_tx, inference_rx) = mpsc::channel::<state_machine::Command>(32);
    let (hardware_tx, hardware_rx) = mpsc::channel::<state_machine::Command>(32);
    let (display_tx, display_rx) = mpsc::channel::<state_machine::Command>(64);

    // Inference: real LiteLLM client (charge + verdict) for Phase 2; STT/TTS
    // still mocked. Set [inference] mode = "mock" for offline dev.
    {
        let cfg = cfg.clone();
        let event_tx = event_tx.clone();
        let display_tx = display_tx.clone();
        tokio::spawn(async move {
            inference::run(cfg, inference_rx, event_tx, display_tx).await;
        });
    }

    // Hardware driver (mock or serial; Phase 1 = mock only).
    {
        let driver = hardware::build(&cfg.hardware, &cfg.mock_hw);
        let event_tx = event_tx.clone();
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
        tokio::spawn(async move { driver.run(hw_cmd_rx, event_tx).await });
    }

    // Display server: broadcast to ws clients, plus axum task.
    let display_bcast = broadcast::channel::<DisplayMessage>(256).0;
    {
        let bcast = display_bcast.clone();
        tokio::spawn(async move { display::forwarder(display_rx, bcast).await });
    }

    let app_state = AppState {
        event_tx: event_tx.clone(),
        display_bcast: display_bcast.clone(),
        plea_buffer: Arc::new(Mutex::new(Vec::new())),
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
    let runtime = Runtime::new(cfg.clone(), event_rx, inference_tx, hardware_tx, display_tx);
    let sm = tokio::spawn(async move { runtime.run().await });

    tokio::select! {
        _ = sm => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received, shutting down");
        }
    }
    Ok(())
}

/// Load `.env` from the current directory or any ancestor, then alias the
/// stack's standard `LITELLM_MASTER_KEY` into the figment-prefixed
/// `BOOTH__INFERENCE__API_KEY` so the dev loop is a single `cargo run`.
fn load_dotenv() {
    let _ = dotenvy::dotenv(); // walks parents for .env; silently ignored if absent
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
