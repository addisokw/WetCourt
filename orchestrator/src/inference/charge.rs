use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;
use crate::fallbacks;
use crate::state_machine::Event;

use super::client::LlmClient;

const SYSTEM_PROMPT: &str = "You are an absurdist court issuing comedic charges against visitors at an interactive art exhibit. Generate ONE brief charge (1-2 sentences max) that is absurd but harmless. Acceptable themes: technical pedantry, internet culture, everyday foibles, anachronistic crimes against good taste, violations of unspoken social norms.

Forbidden: real people, politics, religion, anything sexual, anything that targets protected groups, anything genuinely accusatory. Keep it silly.

Examples:
- \"You stand accused of pronouncing 'gif' with a hard G.\"
- \"You are charged with reply-all on a company-wide email.\"
- \"You stand accused of leaving a single dish in the sink for over 72 hours.\"

Output JSON only, no preamble.";

#[derive(Debug, Deserialize)]
struct ChargeOut {
    charge: String,
}

pub async fn mock(cfg: Arc<Config>, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.charge_latency_ms)).await;
    let charge = fallbacks::charges::random();
    let _ = event_tx.send(Event::ChargeReady(charge)).await;
}

pub async fn real(cfg: Arc<Config>, event_tx: mpsc::Sender<Event>) {
    let client = LlmClient::new(&cfg.inference);
    let schema = json!({
        "type": "object",
        "properties": {
            "charge": { "type": "string", "minLength": 10, "maxLength": 200 }
        },
        "required": ["charge"]
    });
    let timeout = Duration::from_secs(cfg.inference.charge_timeout_secs);
    match client
        .chat_structured::<ChargeOut>(SYSTEM_PROMPT, "Generate the next charge.", schema, timeout)
        .await
    {
        Ok(out) => {
            info!(charge = %out.charge, "charge generated");
            let _ = event_tx.send(Event::ChargeReady(out.charge)).await;
        }
        Err(e) => {
            warn!("charge generation failed: {e:#}; using fallback");
            let _ = event_tx.send(Event::ChargeReady(fallbacks::charges::random())).await;
        }
    }
}
