use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::config::{HardwareConfig, MockHwConfig};
use crate::state_machine::Event;

pub mod mock;
pub mod protocol;
pub mod tcp;

pub use protocol::HardwareCommand;

#[async_trait]
pub trait HardwareDriver: Send {
    async fn run(
        self: Box<Self>,
        cmd_rx: mpsc::Receiver<HardwareCommand>,
        event_tx: mpsc::Sender<Event>,
    );
}

pub fn build(cfg: &HardwareConfig, mock_cfg: &MockHwConfig) -> Box<dyn HardwareDriver> {
    match cfg.driver.as_str() {
        "mock" => Box::new(mock::MockDriver::new(mock_cfg.clone())),
        "tcp" => Box::new(tcp::TcpDriver::new(cfg.bind_addr.clone(), cfg.ack_timeout_ms)),
        "serial" => unimplemented!("serial driver lands in Phase 4"),
        other => panic!("unknown hardware.driver: {other}"),
    }
}
