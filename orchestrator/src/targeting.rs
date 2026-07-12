//! Trial turret-aiming choreography.
//!
//! Executes the [`TargetingCue`]s the trial state machine emits, sharing the same
//! `targeting_armed` flag, vision process, calibration, and hardware command sink
//! the operator's `/vision/*` endpoints use. The FSM decides *when* (arm during
//! deliberation for suspense, freeze-then-fire on guilty, idle between trials);
//! this performs the side effects.
//!
//! The arm/disarm flag is set **synchronously** so it is ordered against the
//! commands around it (notably: `Freeze` disarms before the guilty `Fire` is
//! dispatched, so the gun holds its lock instead of chasing new aim). The fire
//! gate is independent of the arm flag — the shot still requires a fresh lock
//! (see `hardware::gate`). The slower bits — the best-effort vision POST and
//! the turret recenter — are spawned fire-and-forget so a downed vision process
//! can never stall the state-machine loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::calibration::{CalibrationRegistry, VisionCal};
use crate::hardware::maintenance::{MaintenanceCommand, Role};
use crate::hardware::protocol::HardwareCommand;
use crate::state_machine::TargetingCue;

/// How often the tuning seeder probes the vision process's `/health`.
const SEED_PROBE_SECS: u64 = 3;

pub struct TargetingController {
    targeting_armed: Arc<AtomicBool>,
    vision_http: reqwest::Client,
    vision_base_url: String,
    calibration: Arc<RwLock<CalibrationRegistry>>,
    maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
}

impl TargetingController {
    pub fn new(
        targeting_armed: Arc<AtomicBool>,
        vision_http: reqwest::Client,
        vision_base_url: String,
        calibration: Arc<RwLock<CalibrationRegistry>>,
        maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
    ) -> Self {
        Self { targeting_armed, vision_http, vision_base_url, calibration, maint_cmd_tx }
    }

    pub async fn execute(&self, cue: TargetingCue) {
        match cue {
            TargetingCue::Acquire => {
                // Reset the vision aim integrator to center (via /target, which
                // also re-selects the target part) so the gun visibly sweeps from
                // idle onto the defendant, then arm so the orchestrator relays that
                // aim to the turret. The part comes from the saved vision tuning
                // (vision.toml, console "Save tuning"); "head" if none saved.
                self.targeting_armed.store(true, Ordering::Relaxed);
                let part = {
                    let reg = self.calibration.read().await;
                    reg.get("vision")
                        .and_then(|c| c.vision.as_ref())
                        .map(|v| v.target_part.clone())
                        .unwrap_or_else(|| "head".into())
                };
                self.spawn_vision_post("target", serde_json::json!({ "part": part }));
                debug!("targeting: acquire (armed, aim reset to center)");
            }
            TargetingCue::Freeze => {
                // Disarm in place: vision stops driving the turret, so the gun holds
                // the aim it locked and the guilty shot lands there. Synchronous —
                // ordered before Fire. The fire gate still requires a fresh lock
                // (vision keeps posting fire_ok while tracking), so if there was no
                // lock the shot is held rather than fired blind.
                self.targeting_armed.store(false, Ordering::Relaxed);
                debug!("targeting: freeze (disarmed in place)");
            }
            TargetingCue::Idle => {
                // Disarm and return the turret to center, resetting vision for the
                // next trial. Mirrors the operator /vision/center recovery.
                self.targeting_armed.store(false, Ordering::Relaxed);
                self.spawn_vision_post("center", serde_json::json!({}));
                self.spawn_recenter();
                debug!("targeting: idle (disarmed, turret centering)");
            }
        }
    }

    /// Push the saved vision tuning to the vision process whenever it
    /// (re)appears. The vision process holds gains/tolerance/boresight only in
    /// memory (seeded from its CLI defaults), so every vision — or orchestrator —
    /// restart used to silently revert the console's tuning. This watches
    /// `/health` and, on each offline→online transition (including the first
    /// sighting after launch), applies the tuning saved in `vision.toml`.
    ///
    /// Deliberately transition-only: while vision stays up, live unsaved tuning
    /// is never clobbered by a reconcile.
    pub fn spawn_tuning_seeder(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut online = false;
            loop {
                let up = this.vision_healthy().await;
                if up && !online {
                    let saved = {
                        let reg = this.calibration.read().await;
                        reg.get("vision").and_then(|c| c.vision.clone())
                    };
                    match saved {
                        Some(v) => this.apply_tuning(&v).await,
                        None => debug!("vision up; no saved tuning (vision.toml) to seed"),
                    }
                }
                online = up;
                tokio::time::sleep(Duration::from_secs(SEED_PROBE_SECS)).await;
            }
        });
    }

    async fn vision_healthy(&self) -> bool {
        let url = format!("{}/health", self.vision_base_url.trim_end_matches('/'));
        matches!(
            self.vision_http
                .get(url)
                .timeout(Duration::from_secs(2))
                .send()
                .await,
            Ok(resp) if resp.status().is_success()
        )
    }

    /// Apply a saved tuning to the live vision process (gains + tolerance,
    /// boresight when calibrated, resting target part). Failures warn — the
    /// next offline→online transition retries.
    pub async fn apply_tuning(&self, v: &VisionCal) {
        let base = self.vision_base_url.trim_end_matches('/').to_string();
        let posts: Vec<(&str, serde_json::Value)> = [
            Some((
                "gains",
                serde_json::json!({
                    "gain_pan": v.gain_pan,
                    "gain_tilt": v.gain_tilt,
                    "tolerance": v.tolerance,
                }),
            )),
            v.boresight
                .map(|[x, y]| ("boresight", serde_json::json!({ "x": x, "y": y }))),
            Some(("target", serde_json::json!({ "part": v.target_part }))),
        ]
        .into_iter()
        .flatten()
        .collect();
        for (path, body) in posts {
            let res = self
                .vision_http
                .post(format!("{base}/{path}"))
                .timeout(Duration::from_secs(2))
                .json(&body)
                .send()
                .await;
            match res {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => warn!("vision tuning seed: /{path} rejected ({})", resp.status()),
                Err(e) => {
                    warn!("vision tuning seed: /{path} failed: {e}");
                    return; // it just went away again; the next transition retries
                }
            }
        }
        info!(
            gain_pan = v.gain_pan,
            gain_tilt = v.gain_tilt,
            tolerance = v.tolerance,
            boresight = ?v.boresight,
            target_part = %v.target_part,
            "vision tuning seeded from saved calibration"
        );
    }

    /// Best-effort POST to the vision process, detached so a slow/down vision
    /// never blocks the caller (the FSM loop).
    fn spawn_vision_post(&self, path: &str, body: serde_json::Value) {
        let url = format!("{}/{path}", self.vision_base_url.trim_end_matches('/'));
        let http = self.vision_http.clone();
        tokio::spawn(async move {
            let _ = http.post(url).timeout(Duration::from_secs(2)).json(&body).send().await;
        });
    }

    /// Command the turret — and the judge's head, which mirrors the vision aim
    /// during trials — back to calibrated center, detached.
    fn spawn_recenter(&self) {
        let calibration = self.calibration.clone();
        let tx = self.maint_cmd_tx.clone();
        tokio::spawn(async move {
            for role in [Role::Turret, Role::JudgeNeck] {
                let raw = {
                    let reg = calibration.read().await;
                    reg.get(role.as_str()).and_then(|c| c.aim_to_raw(0.0, 0.0).ok())
                };
                if let Some((pan, tilt)) = raw {
                    let _ = tx
                        .send(MaintenanceCommand {
                            target: role,
                            cmd: HardwareCommand::Aim { pan, tilt },
                            reply: None,
                        })
                        .await;
                }
            }
            // Reset the eye's catchlight parallax with the neck.
            let _ = tx
                .send(MaintenanceCommand {
                    target: Role::JudgeFace,
                    cmd: HardwareCommand::FaceAim { pan: 0.0, tilt: 0.0 },
                    reply: None,
                })
                .await;
        });
    }
}
