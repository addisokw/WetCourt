use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::config::Config;
use crate::personas::PersonaRegistry;
use crate::state_machine::Event;

use super::client::LlmClient;
use super::tts::strip_markers;

/// Cross-examination question generation. Given the charge and the defendant's
/// first plea, the active judge composes ONE pointed follow-up. We force a
/// JSON object via structured output so the persona's verdict-output contract
/// can't leak a verdict here, and so we never have to parse free text.
#[derive(Debug, Deserialize)]
struct CrossOut {
    question: String,
}

const TASK: &str = "Do NOT deliver a verdict yet. You are cross-examining the defendant. \
Pick the weakest or vaguest part of what they just said and ask ONE short follow-up \
that gives them a real chance to clarify or strengthen it — an opening they can \
seize, NOT a trap engineered to make them fail. Stay completely in character (you \
may be skeptical or theatrical), but a sharp defendant should be able to turn this \
question to their advantage. One or two sentences, answerable aloud in about ten \
seconds, and it must end with a question mark. Output only the question.";

pub async fn mock(
    cfg: Arc<Config>,
    _charge: String,
    _plea: String,
    event_tx: mpsc::Sender<Event>,
) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.deliberate_latency_ms)).await;
    let _ = event_tx
        .send(Event::CrossQuestionReady(
            "And you expect this court to simply take your word for that?".into(),
        ))
        .await;
}

pub async fn real(
    cfg: Arc<Config>,
    personas: Arc<RwLock<PersonaRegistry>>,
    charge: String,
    plea: String,
    event_tx: mpsc::Sender<Event>,
) {
    // The persona's base prompt carries character + voice; the user message
    // overrides the task from "render a verdict" to "ask one question".
    let system_prompt = {
        let reg = personas.read().await;
        reg.active().system_prompt.clone()
    };

    let client = LlmClient::new(&cfg.inference);
    let user_msg = format!("CHARGE: {charge}\n\nThe defendant pleaded:\n\"{plea}\"\n\n{TASK}");
    let schema = json!({
        "type": "object",
        "properties": {
            "question": { "type": "string", "minLength": 5, "maxLength": 240 }
        },
        "required": ["question"]
    });
    let timeout = Duration::from_secs(cfg.cross_examination.question_timeout_secs);

    match client
        .chat_structured::<CrossOut>(&system_prompt, &user_msg, schema, timeout)
        .await
    {
        Ok(out) => {
            let question = strip_markers(&out.question).trim().to_string();
            if question.is_empty() {
                warn!("cross-exam question empty after stripping; skipping cross-exam");
                tracing::warn!("cross question came back empty; skipping cross-exam");
                let _ = event_tx.send(Event::CrossQuestionFailed).await;
            } else {
                info!(question = %question, "cross-exam question ready");
                let _ = event_tx.send(Event::CrossQuestionReady(question)).await;
            }
        }
        Err(e) => {
            warn!("cross-exam question generation failed: {e:#}; skipping cross-exam");
            let _ = event_tx.send(Event::CrossQuestionFailed).await;
        }
    }
}
