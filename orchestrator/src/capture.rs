//! Guilty "moment of justice" capture.
//!
//! On a guilty verdict the Runtime spawns [`CaptureController::spawn`], which
//! grabs a short burst of un-annotated frames from the vision service (`/clean`)
//! around the blast, saves them under `dir/<case_label>/frame_NN.jpg`, attaches
//! one to the [`TrialRecord`] for the keepsake receipt, then queues it for print.
//! Detached from the FSM loop — a slow or downed vision never stalls a trial, and
//! the receipt still prints (with its placeholder still) if the capture fails.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::CaptureConfig;
use crate::printer::record::TrialRecord;
use crate::printer::service::PrintJob;

pub struct CaptureController {
    http: reqwest::Client,
    vision_base_url: String,
    cfg: CaptureConfig,
}

impl CaptureController {
    pub fn new(http: reqwest::Client, vision_base_url: String, cfg: CaptureConfig) -> Self {
        Self { http, vision_base_url, cfg }
    }

    /// Where a case's burst is saved, e.g. `captures/WCA-0042`.
    pub fn case_dir(&self, case_label: &str) -> PathBuf {
        PathBuf::from(&self.cfg.dir).join(case_label)
    }

    /// Grab the burst, attach the chosen receipt still to `record`, then queue it
    /// for print. Spawned per guilty trial; never blocks the caller.
    pub fn spawn(self: &Arc<Self>, mut record: TrialRecord, print_tx: mpsc::Sender<PrintJob>) {
        let this = self.clone();
        tokio::spawn(async move {
            let frames = this.grab_burst(&record).await;
            // Pick the middle frame — most likely to catch the water on target.
            if !frames.is_empty() {
                record.still_jpeg = Some(frames[frames.len() / 2].clone());
            }
            info!(
                case = %record.case_label(),
                frames = frames.len(),
                "moment-of-justice burst captured"
            );
            if let Err(e) = print_tx.send(PrintJob::Trial(record)).await {
                warn!("capture: keepsake not queued for print: {e}");
            }
        });
    }

    async fn grab_burst(&self, record: &TrialRecord) -> Vec<Vec<u8>> {
        let dir = self.case_dir(&record.case_label());
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("capture: cannot create {}: {e}", dir.display());
            return Vec::new();
        }
        // Let the FIRE go out and the water reach the defendant before grabbing.
        tokio::time::sleep(Duration::from_millis(self.cfg.fire_delay_ms)).await;

        let url = format!("{}/clean", self.vision_base_url.trim_end_matches('/'));
        let mut frames = Vec::new();
        for i in 0..self.cfg.frames {
            match self.grab_one(&url).await {
                Some(bytes) => {
                    let path = dir.join(format!("frame_{i:02}.jpg"));
                    if let Err(e) = tokio::fs::write(&path, &bytes).await {
                        warn!("capture: write {} failed: {e}", path.display());
                    }
                    frames.push(bytes);
                }
                None => warn!("capture: frame {i} grab failed"),
            }
            if i + 1 < self.cfg.frames {
                tokio::time::sleep(Duration::from_millis(self.cfg.interval_ms)).await;
            }
        }
        frames
    }

    async fn grab_one(&self, url: &str) -> Option<Vec<u8>> {
        let resp = self.http.get(url).timeout(Duration::from_secs(2)).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.bytes().await.ok().map(|b| b.to_vec())
    }
}
