//! Mock hardware driver for offline dev. Represents "all roles present and
//! responsive": it seeds the device snapshot with every role on startup so the
//! console's presence badges and per-role tabs light up with nothing plugged
//! in, then acks every trial command and every maintenance command (honoring
//! the configured latency/fail-rate). It absorbs what used to be a separate
//! maintenance stub in `main.rs`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::Rng;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::info;

use crate::config::MockHwConfig;
use crate::display::events::DisplayEvent;
use crate::display::DisplayMessage;
use crate::state_machine::Event;

use super::maintenance::{DeviceInfo, HwAckResult, MaintenanceCommand, Role};
use super::{HardwareCommand, HardwareDriver};

/// Every role the mock pretends is connected.
const MOCK_ROLES: [Role; 5] =
    [Role::JudgeFace, Role::JudgeNeck, Role::Gavel, Role::Turret, Role::Squirt];

pub struct MockDriver {
    cfg: MockHwConfig,
}

impl MockDriver {
    pub fn new(cfg: MockHwConfig) -> Self {
        Self { cfg }
    }

    fn fails(&self) -> bool {
        self.cfg.fail_rate > 0.0 && rand::thread_rng().gen::<f64>() < self.cfg.fail_rate
    }
}

#[async_trait]
impl HardwareDriver for MockDriver {
    async fn run(
        self: Box<Self>,
        mut cmd_rx: mpsc::Receiver<HardwareCommand>,
        mut maint_rx: mpsc::Receiver<MaintenanceCommand>,
        event_tx: mpsc::Sender<Event>,
        devices: Arc<RwLock<Vec<DeviceInfo>>>,
        presence: broadcast::Sender<DisplayMessage>,
    ) {
        // Seed "all roles present" so the console looks like a wired booth.
        *devices.write().await = MOCK_ROLES
            .iter()
            .map(|r| DeviceInfo {
                role: r.as_str().into(),
                addr: "mock".into(),
            })
            .collect();
        for r in MOCK_ROLES {
            let _ = presence.send(DisplayMessage::Json(DisplayEvent::DeviceConnected {
                role: r.as_str().into(),
                addr: "mock".into(),
            }));
        }

        if self.cfg.simulate_estop_after_secs > 0 {
            let tx = event_tx.clone();
            let secs = self.cfg.simulate_estop_after_secs;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                tracing::warn!("mock_hw: simulated ESTOP firing");
                let _ = tx.send(Event::OperatorEmergencyStop).await;
            });
        }

        let latency = Duration::from_millis(self.cfg.ack_latency_ms);
        loop {
            tokio::select! {
                // Trial commands: ack everything (FSM advances on the first ack).
                Some(cmd) = cmd_rx.recv() => {
                    let line = cmd.to_line();
                    info!(target: "mock_hw", "{line}");
                    tokio::time::sleep(latency).await;
                    let ev = if self.fails() {
                        Event::HardwareError(format!("mock fail: {line}"))
                    } else {
                        Event::HardwareAck(line)
                    };
                    if event_tx.send(ev).await.is_err() {
                        break;
                    }
                }

                // Maintenance commands: reply Ok/Err (never NoDevice — all present).
                Some(mc) = maint_rx.recv() => {
                    let MaintenanceCommand { target, cmd, reply } = mc;
                    let line = cmd.to_line();
                    info!(target: "mock_hw", role = target.as_str(), "maint: {line}");
                    tokio::time::sleep(latency).await;
                    if let Some(tx) = reply {
                        let result = if self.fails() {
                            HwAckResult::Err { reason: format!("ERR {line}") }
                        } else {
                            HwAckResult::Ok { line: format!("OK {line}") }
                        };
                        let _ = tx.send(result);
                    }
                }

                else => break,
            }
        }
    }
}
