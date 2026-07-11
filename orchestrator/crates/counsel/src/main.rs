mod audio;
mod call;
mod config;
mod http;
mod inference;
mod persona;
mod recorder;
mod rtp;
mod sip;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// The court-appointed lawyer: SIP endpoint for the booth phone, voice-agent
/// loop over the shared LiteLLM stack.
#[derive(Parser, Debug)]
struct Args {
    /// Path to the TOML config. Sections are overridable via COUNSEL__ env
    /// vars (e.g. COUNSEL__INFERENCE__BASE_URL).
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    load_dotenv();
    let cfg = config::load(&args.config)?;
    init_tracing(&cfg.logging.level);
    tracing::info!(config = %args.config.display(), "counsel starting");

    // Persona and assets resolve relative to the config file's directory,
    // same convention as the orchestrator's personas/ dir.
    let base_dir = args.config.parent().unwrap_or(std::path::Path::new("."));
    let persona = persona::LawyerPersona::load(&base_dir.join(&cfg.persona.file))?;
    tracing::info!(id = %persona.id, persona = %persona.display_name, voice = %persona.tts_voice, "lawyer on retainer");
    let cover = audio::cover::CoverAssets::load(&base_dir.join(&cfg.persona.assets_dir));
    let backend = inference::Backend::from_config(&cfg.inference);
    let recording_dir = cfg
        .recording
        .enabled
        .then(|| base_dir.join(&cfg.recording.dir));
    if let Some(d) = &recording_dir {
        tracing::info!(dir = %d.display(), "call recording enabled");
    }

    let (ring_tx, ring_rx) = tokio::sync::mpsc::channel(4);
    let shared = Arc::new(http::AppShared {
        cfg: cfg.clone(),
        registrar: Default::default(),
        calls: {
            // Call-lifecycle pushes go to the same orchestrator the case file
            // comes from, so the trial clock can pause during consultations.
            let calls = call::CallManager::default();
            calls.set_notify_base(cfg.trial_context.orchestrator_base_url.clone());
            calls
        },
        backend,
        persona,
        cover,
        ring_tx,
        recording_dir,
    });

    let token = tokio_util::sync::CancellationToken::new();
    let http_task = tokio::spawn(http::serve(shared.clone()));
    let sip_task = tokio::spawn(sip::run(shared, ring_rx, token.clone()));

    tokio::select! {
        r = http_task => {
            tracing::error!("control plane exited: {r:?}");
        }
        r = sip_task => {
            tracing::error!("SIP endpoint exited: {r:?}");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received, shutting down");
        }
    }
    token.cancel();
    Ok(())
}

/// Same convention as the orchestrator: load `.env` (or the ai-stack's copy)
/// and alias the stack's LITELLM_MASTER_KEY into the figment-prefixed
/// COUNSEL__INFERENCE__API_KEY so dev is a single `cargo run`.
fn load_dotenv() {
    if dotenvy::dotenv().is_err() {
        for path in [
            "../dgx-ai-stack/.env",
            "dgx-ai-stack/.env",
            "../../../dgx-ai-stack/.env",
        ] {
            if dotenvy::from_path(path).is_ok() {
                break;
            }
        }
    }
    if std::env::var("COUNSEL__INFERENCE__API_KEY").is_err() {
        if let Ok(key) = std::env::var("LITELLM_MASTER_KEY") {
            std::env::set_var("COUNSEL__INFERENCE__API_KEY", key);
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
