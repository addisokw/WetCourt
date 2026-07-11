//! Vision fire gate for trial firing.
//!
//! Vision computes a per-frame `fire_ok` (the aim is locked on the selected
//! target — see `vision/vision.py`) and streams it to the orchestrator on the
//! aim POST. This gate stores the latest verdict and lets a trial `FIRE` reach
//! the squirt only on a *fresh* `fire_ok`: no lock, no fire. The freshness
//! check is what makes "no target / no person / vision crashed" fail safe:
//! vision only posts while it is actively tracking, so a stale timestamp means
//! we have no current evidence it is locked on a target — the shot is held
//! rather than fired into the void (or a bystander).
//!
//! Arm state is deliberately not consulted here: the trial's `Freeze` cue
//! disarms the aim relay right before `Fire` (so the gun holds its lock), and
//! the gate must keep protecting the shot through that disarm. With
//! `[vision] trial_targeting = false` (manual aiming mode) the adapter skips
//! the gate entirely — the operator owns the aim and the shot.
//!
//! (The eye-exclusion safety zone that once also fed `fire_ok` was retired when
//! the softer nozzle made the stream safe; the lock gating here remains.)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// A `fire_ok` update older than this is treated as no evidence at all → no
/// fire. Vision posts aim at ~15 Hz (~66 ms), so 300 ms tolerates a few dropped
/// frames without ever firing on a stale verdict.
pub const FIRE_OK_STALE_MS: u64 = 300;

/// Process-monotonic milliseconds. Used to timestamp `fire_ok` updates and to
/// age them at the gate — a single clock so the staleness check is consistent
/// between the writer (the aim handler) and the reader (the fire gate).
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Shared vision fire-gate state: the latest `fire_ok` from vision and when it landed.
pub struct VisionFireGate {
    fire_ok: AtomicBool,
    updated_ms: AtomicU64,
    stale_ms: u64,
}

impl VisionFireGate {
    pub fn new(stale_ms: u64) -> Self {
        Self {
            fire_ok: AtomicBool::new(false),
            // 0 reads as "ancient" until the first real update lands, so the gate
            // is closed before vision has said anything.
            updated_ms: AtomicU64::new(0),
            stale_ms,
        }
    }

    /// Record the latest safety verdict streamed from vision. Called on every
    /// aim POST regardless of arm state, so the gate stays warm through the
    /// trial's Freeze (disarm-in-place) right up to the shot.
    pub fn record(&self, fire_ok: bool) {
        // Order matters for the reader: stamp the time only after the value, so a
        // racing `fresh_fire_ok` can never see a fresh timestamp paired with a
        // stale `true`. (Both relaxed loads, but value-before-time is the safe
        // pairing for a fail-safe gate.)
        self.fire_ok.store(fire_ok, Ordering::Relaxed);
        self.updated_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Whether vision has *current* evidence of a lock on the target: a `true`
    /// `fire_ok` no older than the staleness window. Fail-safe — unknown/stale
    /// ⇒ false.
    pub fn fresh_fire_ok(&self) -> bool {
        let age = now_ms().saturating_sub(self.updated_ms.load(Ordering::Relaxed));
        decide(self.fire_ok.load(Ordering::Relaxed), age, self.stale_ms)
    }
}

/// Pure decision: fire only on a fresh, true `fire_ok`.
fn decide(fire_ok: bool, age_ms: u64, stale_ms: u64) -> bool {
    fire_ok && age_ms <= stale_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_fresh_fire_ok() {
        assert!(decide(true, 0, 300)); // fresh + ok → fire
        assert!(decide(true, 300, 300)); // exactly at the edge still counts
    }

    #[test]
    fn holds_when_not_ok() {
        assert!(!decide(false, 0, 300)); // fresh but not ok → hold
    }

    #[test]
    fn holds_when_stale() {
        assert!(!decide(true, 301, 300)); // ok but stale → hold
        assert!(!decide(false, 301, 300)); // stale and not ok → hold
    }

    #[test]
    fn gate_closed_until_first_record() {
        let gate = VisionFireGate::new(FIRE_OK_STALE_MS);
        assert!(!gate.fresh_fire_ok()); // nothing recorded yet → closed
        gate.record(true);
        assert!(gate.fresh_fire_ok()); // fresh ok → open
        gate.record(false);
        assert!(!gate.fresh_fire_ok()); // fresh not-ok → closed
    }
}
