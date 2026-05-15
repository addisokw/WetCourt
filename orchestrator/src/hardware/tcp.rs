use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;
use tracing::{info, warn};

use crate::state_machine::Event;

use super::{HardwareCommand, HardwareDriver};

/// Plain-TCP, line-protocol hardware driver. Listens on `bind_addr` and
/// accepts one MCU at a time; matches §5.2 from the architecture doc.
/// Commands the MCU dials in over WiFi, sends `BUTTON\n` on the lectern
/// button and `ESTOP\n` on the e-stop. Host writes `FIRE 150\n`, `GAVEL\n`,
/// etc. and expects `OK <cmd>\n` / `ERR <cmd> <reason>\n` per command.
pub struct TcpDriver {
    bind_addr: String,
    ack_timeout: Duration,
}

impl TcpDriver {
    pub fn new(bind_addr: String, ack_timeout_ms: u64) -> Self {
        Self {
            bind_addr,
            ack_timeout: Duration::from_millis(ack_timeout_ms),
        }
    }
}

#[async_trait]
impl HardwareDriver for TcpDriver {
    async fn run(
        self: Box<Self>,
        cmd_rx: mpsc::Receiver<HardwareCommand>,
        event_tx: mpsc::Sender<Event>,
    ) {
        let listener = match TcpListener::bind(&self.bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!("tcp_hw: bind {} failed: {e}", self.bind_addr);
                return;
            }
        };
        info!("tcp_hw: listening on {}", self.bind_addr);

        let cmd_rx = std::sync::Arc::new(Mutex::new(cmd_rx));

        loop {
            let (socket, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    warn!("tcp_hw: accept error: {e}");
                    continue;
                }
            };
            info!("tcp_hw: MCU connected from {peer}");
            let _ = socket.set_nodelay(true);

            let event_tx = event_tx.clone();
            let cmd_rx = cmd_rx.clone();
            let ack_to = self.ack_timeout;
            if let Err(e) = run_session(socket, cmd_rx, event_tx, ack_to).await {
                warn!("tcp_hw: session ended: {e:#}");
            } else {
                info!("tcp_hw: session ended cleanly");
            }
        }
    }
}

async fn run_session(
    socket: TcpStream,
    cmd_rx: std::sync::Arc<Mutex<mpsc::Receiver<HardwareCommand>>>,
    event_tx: mpsc::Sender<Event>,
    ack_timeout: Duration,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    // Drain any stale acks/errors that arrived between sessions.
    // (Each command waits inline for its own ack via the pending channel.)
    let (ack_tx, mut ack_rx) = mpsc::channel::<String>(8);

    // Reader task: parse incoming lines, route unsolicited events to the
    // state machine and route OK/ERR to the per-command waiter.
    let event_tx_reader = event_tx.clone();
    let reader_task = tokio::spawn(async move {
        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    warn!("tcp_hw: read error: {e}");
                    break;
                }
            };
            let trimmed = line[..n].trim_end_matches(['\r', '\n']).to_string();
            if trimmed.is_empty() {
                continue;
            }
            info!(target: "tcp_hw", "rx: {trimmed}");

            let first = trimmed.split_whitespace().next().unwrap_or("");
            match first {
                "OK" | "ERR" => {
                    let _ = ack_tx.send(trimmed).await;
                }
                "ESTOP" => {
                    let _ = event_tx_reader.send(Event::OperatorEmergencyStop).await;
                }
                "BUTTON" => {
                    let _ = event_tx_reader.send(Event::OperatorStart).await;
                }
                "PONG" => { /* ignore */ }
                _ => warn!("tcp_hw: unrecognized line: {trimmed}"),
            }
        }
    });

    // Writer loop: pump HardwareCommands, expect a matching OK/ERR per command.
    let send_result: anyhow::Result<()> = async {
        loop {
            let mut rx = cmd_rx.lock().await;
            let Some(cmd) = rx.recv().await else { break };
            drop(rx);

            let line = format!("{}\n", cmd.to_line());
            info!(target: "tcp_hw", "tx: {}", line.trim_end());
            write_half.write_all(line.as_bytes()).await?;
            write_half.flush().await?;

            let ev = match timeout(ack_timeout, ack_rx.recv()).await {
                Ok(Some(reply)) if reply.starts_with("OK") => Event::HardwareAck(reply),
                Ok(Some(reply)) => Event::HardwareError(reply),
                Ok(None) => {
                    return Err(anyhow::anyhow!("ack channel closed"));
                }
                Err(_) => Event::HardwareError(format!("timeout: {}", cmd.to_line())),
            };
            if event_tx.send(ev).await.is_err() {
                break;
            }
        }
        Ok(())
    }
    .await;

    reader_task.abort();
    send_result
}
