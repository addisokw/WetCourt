//! Direct-control plane for the maintenance/test console.
//!
//! These commands bypass the trial state machine entirely: the operator picks a
//! target device role and an action, and the device registry routes it to the
//! connection that owns that role. This is gated behind the FSM's `Maintenance`
//! state (entered only from `Idle`), so it can never collide with a live trial.
//!
//! The multi-device registry that consumes `MaintenanceCommand` is built
//! separately (see `docs/hardware-architecture.md`). This module defines the
//! wire-neutral contract the registry must honour; until it lands, a stub task
//! in `main.rs` drains the channel against the mock driver.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::protocol::HardwareCommand;

/// Device roles in the v2 multi-device protocol.
///
/// Two spellings coexist on purpose:
/// - `as_str()` is the `snake_case` form used by the JSON API and the per-role
///   calibration filenames (`ai_judge`, `gavel`, `turret`).
/// - `from_wire()` parses the `HELLO <role>` token, accepting the protocol
///   spec's canonical hyphenated spelling (`ai-judge`) and tolerating the
///   underscore form so firmware authors can't trip on the separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    AiJudge,
    Gavel,
    Turret,
}

impl Role {
    /// `snake_case` name — JSON API + calibration filenames.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::AiJudge => "ai_judge",
            Role::Gavel => "gavel",
            Role::Turret => "turret",
        }
    }

    /// Parse the `HELLO <role>` wire token. Accepts the protocol spec's
    /// hyphenated `ai-judge` and tolerates `ai_judge`; returns `None` for an
    /// unknown role (caller replies `BYE unknown_role`).
    pub fn from_wire(s: &str) -> Option<Role> {
        match s {
            "turret" => Some(Role::Turret),
            "gavel" => Some(Role::Gavel),
            "ai-judge" | "ai_judge" => Some(Role::AiJudge),
            _ => None,
        }
    }
}

/// The outcome of a direct command, surfaced back to the operator console so
/// each button/stick action shows an inline OK/ERR/timeout chip.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HwAckResult {
    /// Device replied `OK <verb>`; `line` is the raw reply.
    Ok { line: String },
    /// Device replied `ERR <verb> <reason>` (or routing/validation failed).
    Err { reason: String },
    /// No ack arrived within the per-command timeout.
    Timeout,
    /// No device currently owns the target role. Produced by the device
    /// registry (built separately); the mock stub never hits this path.
    #[allow(dead_code)]
    NoDevice,
}

/// A direct hardware command from the maintenance console. The registry routes
/// it by `target`. `reply` returns the device ack to the REST handler; `None`
/// is fire-and-forget — used for the high-rate AIM stream where waiting on each
/// ack would add head-of-line latency.
pub struct MaintenanceCommand {
    pub target: Role,
    pub cmd: HardwareCommand,
    pub reply: Option<oneshot::Sender<HwAckResult>>,
}

/// One connected device, as surfaced to the console's presence view. The
/// registry owns the authoritative table; `AppState` holds a shared snapshot
/// readable by `GET /maintenance/devices`.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub role: String,
    pub addr: String,
}
