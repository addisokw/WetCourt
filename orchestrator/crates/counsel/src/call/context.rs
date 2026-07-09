//! Live trial context: one GET to the orchestrator's read-only snapshot at
//! call start so the lawyer gives specifically bad advice about *this*
//! crime. Unreachable orchestrator = no case file; the persona plays that
//! as having misplaced the paperwork.

use std::time::Duration;

use serde::Deserialize;

use crate::config::TrialContextConfig;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TrialSnapshot {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub charge: Option<String>,
    #[serde(default)]
    pub plea: Option<String>,
    #[serde(default)]
    pub verdict: Option<VerdictLite>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerdictLite {
    pub guilty: bool,
    #[serde(default)]
    pub remarks: Option<String>,
}

pub async fn fetch(cfg: &TrialContextConfig) -> Option<TrialSnapshot> {
    if !cfg.enabled {
        return None;
    }
    let url = format!(
        "{}/trial/state",
        cfg.orchestrator_base_url.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    match client
        .get(&url)
        .timeout(Duration::from_millis(cfg.timeout_ms))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<TrialSnapshot>().await {
            Ok(s) => {
                tracing::info!(phase = %s.phase, charge = ?s.charge, "trial context fetched");
                Some(s)
            }
            Err(e) => {
                tracing::warn!("trial context parse failed: {e:#}");
                None
            }
        },
        Ok(resp) => {
            tracing::warn!("trial context HTTP {}", resp.status());
            None
        }
        Err(e) => {
            tracing::warn!("trial context unreachable: {e:#}");
            None
        }
    }
}

/// Render the snapshot as a system-prompt block.
pub fn case_file_block(snapshot: &Option<TrialSnapshot>) -> String {
    let Some(s) = snapshot else {
        return "\nCASE FILE: unavailable — the courthouse fax is down. You have \
                misplaced this client's paperwork; improvise, and don't let on \
                more than usual.\n"
            .to_string();
    };
    let mut out = String::from("\nCASE FILE (live from the courtroom):\n");
    out.push_str(&format!("- Trial phase: {}\n", if s.phase.is_empty() { "unknown" } else { &s.phase }));
    match &s.charge {
        Some(c) => out.push_str(&format!("- The charge against your client: {c}\n")),
        None => out.push_str("- No charge on file yet — court is idle or between trials.\n"),
    }
    if let Some(p) = &s.plea {
        out.push_str(&format!("- What they said in their plea: {p}\n"));
    }
    if let Some(v) = &s.verdict {
        out.push_str(&format!(
            "- Verdict already rendered: {}{}\n",
            if v.guilty { "GUILTY" } else { "NOT GUILTY" },
            v.remarks
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
    }
    out.push_str(
        "Work the specifics of this case file into your terrible advice.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_snapshot_reads_as_lost_paperwork() {
        let block = case_file_block(&None);
        assert!(block.contains("misplaced"));
    }

    #[test]
    fn full_snapshot_renders_all_lines() {
        let s = TrialSnapshot {
            phase: "deliberating".into(),
            charge: Some("Aggravated soup deployment".into()),
            plea: Some("It was minestrone".into()),
            verdict: Some(VerdictLite { guilty: true, remarks: Some("the beans".into()) }),
        };
        let block = case_file_block(&Some(s));
        assert!(block.contains("Aggravated soup deployment"));
        assert!(block.contains("minestrone"));
        assert!(block.contains("GUILTY"));
        assert!(block.contains("the beans"));
    }
}
