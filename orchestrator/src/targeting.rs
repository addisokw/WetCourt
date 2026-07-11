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
use tracing::debug;

use crate::calibration::CalibrationRegistry;
use crate::hardware::maintenance::{MaintenanceCommand, Role};
use crate::hardware::protocol::HardwareCommand;
use crate::state_machine::TargetingCue;

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
                // Reset the vision aim integrator to center (via /target head, which
                // also re-selects the head target) so the gun visibly sweeps from
                // idle onto the defendant, then arm so the orchestrator relays that
                // aim to the turret.
                self.targeting_armed.store(true, Ordering::Relaxed);
                self.spawn_vision_post("target", serde_json::json!({ "part": "head" }));
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

    /// Best-effort POST to the vision process, detached so a slow/down vision
    /// never blocks the caller (the FSM loop).
    fn spawn_vision_post(&self, path: &str, body: serde_json::Value) {
        let url = format!("{}/{path}", self.vision_base_url.trim_end_matches('/'));
        let http = self.vision_http.clone();
        tokio::spawn(async move {
            let _ = http.post(url).timeout(Duration::from_secs(2)).json(&body).send().await;
        });
    }

    /// Command the turret back to its calibrated center, detached.
    fn spawn_recenter(&self) {
        let calibration = self.calibration.clone();
        let tx = self.maint_cmd_tx.clone();
        tokio::spawn(async move {
            let raw = {
                let reg = calibration.read().await;
                reg.get(Role::Turret.as_str()).and_then(|c| c.aim_to_raw(0.0, 0.0).ok())
            };
            if let Some((pan, tilt)) = raw {
                let _ = tx
                    .send(MaintenanceCommand {
                        target: Role::Turret,
                        cmd: HardwareCommand::Aim { pan, tilt },
                        reply: None,
                    })
                    .await;
            }
        });
    }
}
