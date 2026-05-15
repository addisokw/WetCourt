use std::time::Duration;

use async_trait::async_trait;
use rand::Rng;
use tokio::sync::mpsc;
use tracing::info;

use crate::config::MockHwConfig;
use crate::state_machine::Event;

use super::{HardwareCommand, HardwareDriver};

pub struct MockDriver {
    cfg: MockHwConfig,
}

impl MockDriver {
    pub fn new(cfg: MockHwConfig) -> Self { Self { cfg } }
}

#[async_trait]
impl HardwareDriver for MockDriver {
    async fn run(
        self: Box<Self>,
        mut cmd_rx: mpsc::Receiver<HardwareCommand>,
        event_tx: mpsc::Sender<Event>,
    ) {
        if self.cfg.simulate_estop_after_secs > 0 {
            let tx = event_tx.clone();
            let secs = self.cfg.simulate_estop_after_secs;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                tracing::warn!("mock_hw: simulated ESTOP firing");
                let _ = tx.send(Event::OperatorEmergencyStop).await;
            });
        }

        while let Some(cmd) = cmd_rx.recv().await {
            let line = cmd.to_line();
            info!(target: "mock_hw", "{}", line);
            tokio::time::sleep(Duration::from_millis(self.cfg.ack_latency_ms)).await;
            let fail = self.cfg.fail_rate > 0.0
                && rand::thread_rng().gen::<f64>() < self.cfg.fail_rate;
            let ev = if fail {
                Event::HardwareError(format!("mock fail: {line}"))
            } else {
                Event::HardwareAck(line)
            };
            if event_tx.send(ev).await.is_err() {
                break;
            }
        }
    }
}
