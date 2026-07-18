//! Trial turret-aiming choreography.
//!
//! Executes the [`TargetingCue`]s the trial state machine emits, sharing the same
//! `targeting_armed` flag, vision process, calibration, and hardware command sink
//! the operator's `/vision/*` endpoints use. The FSM decides *when* (arm during
//! deliberation for suspense, freeze-then-fire on guilty, idle between trials);
//! this performs the side effects.
//!
//! The **disarms** are set synchronously so they are ordered against the
//! commands around them (notably: `Freeze` disarms before the guilty `Fire` is
//! dispatched, so the gun holds its lock instead of chasing new aim). The
//! `Acquire` **arm** is deliberately not: it lands only after vision confirms
//! the aim-integrator reset. While disarmed the integrator winds up — its aim
//! is not relayed, the gun doesn't move, so the boresight error never
//! converges and the commanded aim saturates at the limits — and arming
//! before the reset relayed that stale aim to the turret as a full-speed
//! sling. Armed therefore implies the integrator was reset. The fire gate is
//! independent of the arm flag — the shot still requires a fresh lock (see
//! `hardware::gate`). The slower bits — vision POSTs, the arm that follows
//! one, and the turret recenter — are spawned fire-and-forget so a downed
//! vision process can never stall the state-machine loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::calibration::{CalibrationRegistry, VisionCal};
use crate::hardware::gate::VisionFireGate;
use crate::hardware::maintenance::{MaintenanceCommand, Role};
use crate::hardware::protocol::HardwareCommand;
use crate::state_machine::TargetingCue;

/// How often the tuning seeder probes the vision process's `/health`.
const SEED_PROBE_SECS: u64 = 3;

/// Recenter / fallback glide: peak slew rate (°/s) and stream cadence. The
/// eased profile peaks at ~1.5× the average, so a 90° sweep takes ~4.5 s —
/// a calm return, not a snap.
const GLIDE_RATE_DEG_S: f32 = 30.0;
const GLIDE_TICK_MS: u64 = 33;
/// Glides bounded to something sane even for a wild tracked position.
const GLIDE_MAX_SECS: f32 = 6.0;

/// Smoothstep ease-in/out (3t² − 2t³): zero slope at both ends, so the
/// mechanics accelerate and settle gently instead of jerking.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::smoothstep;

    #[test]
    fn smoothstep_eases_in_and_out() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert_eq!(smoothstep(0.5), 0.5);
        assert_eq!(smoothstep(-1.0), 0.0); // clamped
        assert_eq!(smoothstep(2.0), 1.0);
        // Ease-in/out: the first tenth covers far less ground than a linear
        // ramp; the middle is faster than linear.
        assert!(smoothstep(0.1) < 0.05);
        assert!(smoothstep(0.6) - smoothstep(0.4) > 0.2);
        // Monotonic across the whole span.
        let mut prev = 0.0;
        for i in 1..=100 {
            let v = smoothstep(i as f32 / 100.0);
            assert!(v >= prev);
            prev = v;
        }
    }
}

pub struct TargetingController {
    targeting_armed: Arc<AtomicBool>,
    vision_http: reqwest::Client,
    vision_base_url: String,
    calibration: Arc<RwLock<CalibrationRegistry>>,
    maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
    vision_gate: Arc<VisionFireGate>,
    /// Last *logical* aim (degrees) sent to each pan/tilt role, recorded at
    /// every send site (vision stream, console AIM, glides). Glides start from
    /// here — the host has no position feedback from the servos.
    last_aim: Mutex<HashMap<Role, (f32, f32)>>,
    /// Bumped whenever a new motion intent supersedes the current one; an
    /// in-flight glide checks it each tick and stops silently when stale.
    glide_gen: Arc<AtomicU64>,
}

impl TargetingController {
    pub fn new(
        targeting_armed: Arc<AtomicBool>,
        vision_http: reqwest::Client,
        vision_base_url: String,
        calibration: Arc<RwLock<CalibrationRegistry>>,
        maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
        vision_gate: Arc<VisionFireGate>,
    ) -> Self {
        Self {
            targeting_armed,
            vision_http,
            vision_base_url,
            calibration,
            maint_cmd_tx,
            vision_gate,
            last_aim: Mutex::new(HashMap::new()),
            glide_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Record the logical aim just sent to a role (degrees). Called from every
    /// aim source — the vision relay, the console's direct AIM, and the glide
    /// itself — so a later glide knows where the hardware currently points.
    pub fn note_aim(&self, role: Role, pan: f32, tilt: f32) {
        self.last_aim.lock().unwrap().insert(role, (pan, tilt));
    }

    /// Invalidate any in-flight glide (a new motion source is taking over).
    fn cancel_glide(&self) -> u64 {
        self.glide_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// A live aim source (vision relay, console AIM) is taking over: stop any
    /// in-flight glide so the two never fight over the servos.
    pub fn take_over(&self) {
        self.cancel_glide();
    }

    pub async fn execute(self: &Arc<Self>, cue: TargetingCue) {
        match cue {
            TargetingCue::Acquire => {
                // Reset the vision aim integrator to center (via /target, which
                // also re-selects the target part), then arm so the orchestrator
                // relays that aim to the turret and the gun visibly sweeps from
                // idle onto the defendant. The part comes from the saved vision
                // tuning (vision.toml, console "Save tuning"); "head" if none
                // saved. STRICTLY reset-then-arm: the integrator winds up while
                // disarmed (see module doc), and arming first relayed the stale
                // saturated aim — the turret slung off at full servo speed
                // before the reset landed. Detached so the POST can't stall the
                // FSM loop; the generation check drops the arm if a later cue
                // superseded this one while the POST was in flight.
                let my_gen = self.cancel_glide(); // vision owns the aim now
                let (part, fallback) = {
                    let reg = self.calibration.read().await;
                    let v = reg.get("vision").and_then(|c| c.vision.as_ref());
                    (
                        v.map(|v| v.target_part.clone()).unwrap_or_else(|| "head".into()),
                        v.and_then(|v| v.fallback_aim),
                    )
                };
                let this = self.clone();
                tokio::spawn(async move {
                    let reset_ok =
                        this.vision_post("target", serde_json::json!({ "part": part })).await;
                    if this.glide_gen.load(Ordering::Relaxed) != my_gen {
                        return; // superseded — the newer cue owns the arm state
                    }
                    if reset_ok {
                        this.targeting_armed.store(true, Ordering::Relaxed);
                        debug!("targeting: acquire (aim reset to center, armed)");
                    } else if let Some([pan, tilt]) = fallback {
                        // Vision-failure fallback: park the gun (and the judge's
                        // gaze) on the calibrated above-the-mic spot during the
                        // deliberation, so the guilty shot has somewhere real to
                        // land. Stays disarmed — a vision process recovering
                        // mid-deliberation would stream its wound-up aim before
                        // the tuning seeder gets to reset it.
                        warn!(pan, tilt, "targeting: vision down at acquire — gliding to fallback aim, staying disarmed");
                        this.spawn_glide(&[Role::Turret, Role::JudgeNeck], pan, tilt);
                    } else {
                        warn!("targeting: vision down at acquire, no fallback aim — staying disarmed");
                    }
                });
            }
            TargetingCue::Freeze => {
                // Disarm in place: vision stops driving the turret, so the gun holds
                // the aim it locked and the guilty shot lands there. Synchronous —
                // ordered before Fire. The fire gate still requires a fresh lock
                // (vision keeps posting fire_ok while tracking), so if there was no
                // lock the shot is held rather than fired blind.
                self.cancel_glide();
                self.targeting_armed.store(false, Ordering::Relaxed);
                // Vision-failure fallback: with no fresh lock at the freeze (vision
                // dead or the target lost), aim the calibrated above-the-mic spot
                // and open the gate for this one shot — a configured fallback means
                // the operator prefers a fixed soak over a held FIRE. In the
                // vision-down case the gun has been parked there since Acquire; a
                // mid-trial loss may still be slewing as the stream fires.
                if !self.vision_gate.fresh_fire_ok() {
                    let fallback = {
                        let reg = self.calibration.read().await;
                        reg.get("vision").and_then(|c| c.vision.as_ref()).and_then(|v| v.fallback_aim)
                    };
                    if let Some([pan, tilt]) = fallback {
                        warn!(pan, tilt, "targeting: no fresh lock at freeze — firing on fallback aim");
                        self.send_aim_deg(Role::Turret, pan, tilt).await;
                        let (np, nt) = self.follow_aim(Role::JudgeNeck, pan, tilt).await;
                        self.send_aim_deg(Role::JudgeNeck, np, nt).await;
                        self.vision_gate.record(true);
                    }
                }
                debug!("targeting: freeze (disarmed in place)");
            }
            TargetingCue::Idle => {
                // Disarm and calmly return the turret (and the judge's gaze) to
                // center, resetting vision for the next trial. The eased glide —
                // not a snap — also covers leaving maintenance mode, which enters
                // Idle the same way.
                self.targeting_armed.store(false, Ordering::Relaxed);
                self.spawn_vision_post("center", serde_json::json!({}));
                self.spawn_glide(&[Role::Turret, Role::JudgeNeck], 0.0, 0.0);
                debug!("targeting: idle (disarmed, gliding to center)");
            }
        }
    }

    /// Glide the given pan/tilt roles to a logical aim (degrees) with an eased
    /// (smoothstep) profile at ~[`GLIDE_RATE_DEG_S`] peak — calm on the
    /// mechanics and on the audience, vs. the servo-speed snap of a single AIM.
    /// The target is in the shared (turret) frame; follower roles map it
    /// through their `[follow]` transform first, same as the live vision relay.
    /// Roles with no recorded aim (fresh boot) jump directly — there is nothing
    /// to interpolate from. Superseded glides (a new cue, another glide) stop
    /// at the next tick. Detached; never blocks the caller.
    pub fn spawn_glide(self: &Arc<Self>, roles: &[Role], pan: f32, tilt: f32) {
        let my_gen = self.cancel_glide();
        let this = self.clone();
        let roles = roles.to_vec();
        tokio::spawn(async move {
            // Per-role target (through the follow transform) and start; the
            // farthest axis of any role sets one shared duration so paired
            // roles (turret + neck) arrive together.
            let targets: Vec<(Role, (f32, f32))> = {
                let mut out = Vec::with_capacity(roles.len());
                for r in &roles {
                    out.push((*r, this.follow_aim(*r, pan, tilt).await));
                }
                out
            };
            let legs: Vec<(Role, (f32, f32), (f32, f32))> = {
                let last = this.last_aim.lock().unwrap();
                targets
                    .iter()
                    .map(|(r, tgt)| (*r, last.get(r).copied().unwrap_or(*tgt), *tgt))
                    .collect()
            };
            let dist = legs
                .iter()
                .map(|(_, (p0, t0), (p1, t1))| (p0 - p1).abs().max((t0 - t1).abs()))
                .fold(0.0_f32, f32::max);
            // Eased peak speed ≈ 1.5 × average → duration = 1.5 × dist / rate.
            let secs = (1.5 * dist / GLIDE_RATE_DEG_S).min(GLIDE_MAX_SECS);
            let steps = ((secs * 1000.0 / GLIDE_TICK_MS as f32).ceil() as u32).max(1);
            for i in 1..=steps {
                if this.glide_gen.load(Ordering::Relaxed) != my_gen {
                    return; // superseded — whoever took over owns the aim now
                }
                let s = smoothstep(i as f32 / steps as f32);
                for (role, (p0, t0), (p1, t1)) in &legs {
                    this.send_aim_deg(*role, p0 + (p1 - p0) * s, t0 + (t1 - t0) * s).await;
                }
                tokio::time::sleep(Duration::from_millis(GLIDE_TICK_MS)).await;
            }
        });
    }

    /// Map a shared (turret-frame) aim into a role's own logical degrees via
    /// its `[follow]` calibration; identity for roles without one.
    async fn follow_aim(&self, role: Role, pan: f32, tilt: f32) -> (f32, f32) {
        let reg = self.calibration.read().await;
        reg.get(role.as_str()).map(|c| c.follow_aim(pan, tilt)).unwrap_or((pan, tilt))
    }

    /// Send one logical aim (degrees) to a pan/tilt role via its calibration,
    /// mirroring the eye's catchlight when the neck moves, and record it in the
    /// aim tracker. Roles without calibration drop silently (same as the other
    /// aim paths).
    async fn send_aim_deg(&self, role: Role, pan: f32, tilt: f32) {
        let raw = {
            let reg = self.calibration.read().await;
            reg.get(role.as_str()).and_then(|c| c.aim_to_raw(pan, tilt).ok())
        };
        let Some((rp, rt)) = raw else { return };
        self.note_aim(role, pan, tilt);
        let _ = self
            .maint_cmd_tx
            .send(MaintenanceCommand {
                target: role,
                cmd: HardwareCommand::Aim { pan: rp, tilt: rt },
                reply: None,
            })
            .await;
        if role == Role::JudgeNeck {
            let _ = self
                .maint_cmd_tx
                .send(MaintenanceCommand {
                    target: Role::JudgeFace,
                    cmd: HardwareCommand::FaceAim { pan, tilt },
                    reply: None,
                })
                .await;
        }
    }

    /// F6 attract mode: a small, gentle neck move in degrees (kept within the
    /// working range by the caller). Public wrapper over the internal aim path;
    /// mirrors to the face like any other neck aim.
    pub async fn nudge_neck(&self, pan_deg: f32, tilt_deg: f32) {
        self.send_aim_deg(Role::JudgeNeck, pan_deg, tilt_deg).await;
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

    /// POST to the vision process, reporting whether it landed (2xx). For
    /// callers that must order follow-up work after the POST — notably the
    /// Acquire arm, which may only flip once the aim-integrator reset is
    /// confirmed.
    async fn vision_post(&self, path: &str, body: serde_json::Value) -> bool {
        let url = format!("{}/{path}", self.vision_base_url.trim_end_matches('/'));
        matches!(
            self.vision_http.post(url).timeout(Duration::from_secs(2)).json(&body).send().await,
            Ok(resp) if resp.status().is_success()
        )
    }

    /// Best-effort POST to the vision process, detached so a slow/down vision
    /// never blocks the caller (the FSM loop).
    fn spawn_vision_post(self: &Arc<Self>, path: &str, body: serde_json::Value) {
        let this = self.clone();
        let path = path.to_string();
        tokio::spawn(async move {
            let _ = this.vision_post(&path, body).await;
        });
    }

}
