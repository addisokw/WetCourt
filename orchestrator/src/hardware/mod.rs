use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::config::{HardwareConfig, MockHwConfig};
use crate::display::DisplayMessage;
use crate::state_machine::Event;

pub mod gate;
pub mod maintenance;
pub mod mock;
pub mod protocol;
pub mod tcp;

pub use protocol::HardwareCommand;

use maintenance::{DeviceInfo, MaintenanceCommand};

#[async_trait]
pub trait HardwareDriver: Send {
    /// Drive the hardware fleet. Owns both command sources — the trial state
    /// machine (`cmd_rx`, untargeted) and the maintenance console (`maint_rx`,
    /// targeted + reply) — and keeps the shared device `snapshot` + `presence`
    /// broadcasts in sync as devices connect/disconnect.
    async fn run(
        self: Box<Self>,
        cmd_rx: mpsc::Receiver<HardwareCommand>,
        maint_rx: mpsc::Receiver<MaintenanceCommand>,
        event_tx: mpsc::Sender<Event>,
        devices: Arc<RwLock<Vec<DeviceInfo>>>,
        presence: broadcast::Sender<DisplayMessage>,
    );
}

pub fn build(cfg: &HardwareConfig, mock_cfg: &MockHwConfig) -> Box<dyn HardwareDriver> {
    match cfg.driver.as_str() {
        "mock" => Box::new(mock::MockDriver::new(mock_cfg.clone())),
        "tcp" => Box::new(tcp::TcpRegistry::new(cfg.bind_addr.clone(), cfg.ack_timeout_ms)),
        other => panic!("unknown hardware.driver: {other} (expected \"mock\" or \"tcp\")"),
    }
}
