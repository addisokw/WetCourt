//! Targeting-panel auto-fire: fire the squirt once the turret has held a lock on
//! its target for a dwell time.
//!
//! Vision streams `fire_ok` (= locked on target) to `/vision/aim` at ~15 Hz while
//! it is actively tracking. This tracks how long that lock has held continuously
//! and, when auto-fire is enabled and targeting is armed, trips once per dwell so
//! the aim handler can fire the squirt. Timing lives here (server-side, at frame
//! rate) rather than in the browser so a laggy or backgrounded console can never
//! delay or double-trigger a real shot.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A shot can't fire again until at least this long after the last one — a floor
/// against a jittery lock re-triggering in a burst even with a tiny dwell.
const COOLDOWN: Duration = Duration::from_millis(750);
/// If aim updates stop for longer than this, the continuous lock is considered
/// broken and the dwell restarts on the next frame. Vision posts at ~15 Hz while
/// tracking and stops when the target is lost, so a gap means "lock lost".
const GAP_RESET: Duration = Duration::from_millis(300);
/// Upper bound on the operator-set dwell (ms), so a fat-fingered value can't wedge
/// auto-fire into never firing.
const MAX_DWELL_MS: u64 = 60_000;

pub struct AutoFire {
    enabled: AtomicBool,
    dwell_ms: AtomicU64,
    timing: Mutex<Timing>,
}

#[derive(Default)]
struct Timing {
    /// When the current continuous lock began (reset on loss / gap / after firing).
    locked_since: Option<Instant>,
    /// Last aim frame seen — used to detect a stream gap (= lock lost).
    last_frame: Option<Instant>,
    /// Last time we fired — enforces COOLDOWN.
    last_fire: Option<Instant>,
    /// Whether this continuous lock has already fired (one shot per lock).
    fired_this_lock: bool,
}

impl AutoFire {
    pub fn new(dwell_ms: u64) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            dwell_ms: AtomicU64::new(dwell_ms.min(MAX_DWELL_MS)),
            timing: Mutex::new(Timing::default()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn dwell_ms(&self) -> u64 {
        self.dwell_ms.load(Ordering::Relaxed)
    }

    /// Apply an operator update (either field optional).
    pub fn set(&self, enabled: Option<bool>, dwell_ms: Option<u64>) {
        if let Some(e) = enabled {
            self.enabled.store(e, Ordering::Relaxed);
        }
        if let Some(d) = dwell_ms {
            self.dwell_ms.store(d.min(MAX_DWELL_MS), Ordering::Relaxed);
        }
    }

    /// Feed one vision aim frame. Returns `true` exactly once each time the lock
    /// has held continuously for the dwell (while enabled + armed) — the caller
    /// fires the squirt on `true`.
    pub fn on_frame(&self, armed: bool, fire_ok: bool) -> bool {
        self.on_frame_at(armed, fire_ok, Instant::now())
    }

    fn on_frame_at(&self, armed: bool, fire_ok: bool, now: Instant) -> bool {
        let dwell = Duration::from_millis(self.dwell_ms());
        let mut t = self.timing.lock().unwrap();

        // A gap in the aim stream means tracking was interrupted → restart dwell.
        if let Some(prev) = t.last_frame {
            if now.saturating_duration_since(prev) > GAP_RESET {
                t.locked_since = None;
                t.fired_this_lock = false;
            }
        }
        t.last_frame = Some(now);

        if !self.enabled() || !armed || !fire_ok {
            t.locked_since = None;
            t.fired_this_lock = false;
            return false;
        }

        let since = *t.locked_since.get_or_insert(now);
        if t.fired_this_lock {
            return false;
        }
        if now.saturating_duration_since(since) < dwell {
            return false;
        }
        if let Some(lf) = t.last_fire {
            if now.saturating_duration_since(lf) < COOLDOWN {
                return false;
            }
        }
        t.fired_this_lock = true;
        t.last_fire = Some(now);
        true
    }

    /// Status for the console: `(enabled, dwell_ms, locked_ms)` where `locked_ms`
    /// is how long the current lock has held (0 when not locked, the stream has
    /// gone stale, or this lock has already fired).
    pub fn status(&self) -> (bool, u64, u64) {
        let now = Instant::now();
        let t = self.timing.lock().unwrap();
        let stale = t
            .last_frame
            .map_or(true, |p| now.saturating_duration_since(p) > GAP_RESET);
        let locked_ms = match t.locked_since {
            Some(since) if !t.fired_this_lock && !stale => {
                now.saturating_duration_since(since).as_millis() as u64
            }
            _ => 0,
        };
        (self.enabled(), self.dwell_ms(), locked_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed_enabled(dwell_ms: u64) -> AutoFire {
        let af = AutoFire::new(dwell_ms);
        af.set(Some(true), None);
        af
    }

    /// Feed locked frames at a realistic ~15 Hz cadence starting at `start`, and
    /// return the offset (ms) of the first frame that fired, if any.
    fn feed_until_fire(af: &AutoFire, start: Instant, count: u64) -> Option<u64> {
        const STEP_MS: u64 = 50; // < GAP_RESET, like vision's ~66 ms posts
        for i in 0..count {
            let off = i * STEP_MS;
            if af.on_frame_at(true, true, start + Duration::from_millis(off)) {
                return Some(off);
            }
        }
        None
    }

    #[test]
    fn fires_after_dwell_once_per_lock() {
        let af = armed_enabled(1000);
        let t0 = Instant::now();
        // Fires at the first frame at/after the 1 s dwell.
        assert_eq!(feed_until_fire(&af, t0, 40), Some(1000));
        // Holding the lock afterward does not fire again (one shot per lock).
        for i in 21..40 {
            assert!(!af.on_frame_at(true, true, t0 + Duration::from_millis(i * 50)));
        }
    }

    #[test]
    fn losing_lock_then_reacquiring_re_arms() {
        let af = armed_enabled(1000);
        let t0 = Instant::now();
        assert_eq!(feed_until_fire(&af, t0, 40), Some(1000)); // first shot
        // Lose the lock, then re-acquire and hold the dwell again → fires anew.
        let t1 = t0 + Duration::from_millis(1050);
        assert!(!af.on_frame_at(true, false, t1)); // lock lost → reset
        assert_eq!(feed_until_fire(&af, t1 + Duration::from_millis(50), 40), Some(1000));
    }

    #[test]
    fn a_stream_gap_restarts_the_dwell() {
        let af = armed_enabled(500);
        let t0 = Instant::now();
        assert!(!af.on_frame_at(true, true, t0)); // lock starts
        assert!(!af.on_frame_at(true, true, t0 + Duration::from_millis(100)));
        // >GAP_RESET with no frames: the lock is considered lost. From here a full
        // fresh dwell is required again.
        let after_gap = t0 + Duration::from_millis(1000);
        assert_eq!(feed_until_fire(&af, after_gap, 40), Some(500));
    }

    #[test]
    fn disarmed_or_disabled_never_fires() {
        let af = armed_enabled(0); // zero dwell would fire instantly if allowed
        let t0 = Instant::now();
        assert!(!af.on_frame_at(false, true, t0)); // disarmed
        af.set(Some(false), None);
        assert!(!af.on_frame_at(true, true, t0 + Duration::from_millis(10))); // disabled
    }

    #[test]
    fn not_locked_never_fires() {
        let af = armed_enabled(0);
        let t0 = Instant::now();
        assert!(!af.on_frame_at(true, false, t0));
        assert!(!af.on_frame_at(true, false, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn dwell_is_clamped() {
        let af = AutoFire::new(u64::MAX);
        assert_eq!(af.dwell_ms(), MAX_DWELL_MS);
        af.set(None, Some(999_999_999));
        assert_eq!(af.dwell_ms(), MAX_DWELL_MS);
    }
}
