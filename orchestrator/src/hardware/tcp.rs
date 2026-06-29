//! Multi-device TCP registry (protocol v2). Accepts N device connections, each
//! identifying itself with a `HELLO <role>` handshake, and routes commands per
//! role. Two command sources feed it: the trial state machine (untargeted
//! `HardwareCommand`s, routed by a verb→role map) and the maintenance console
//! (`MaintenanceCommand`s with an explicit target + optional reply).
//!
//! Topology: an **acceptor** task (this `run`) owns the listener and spawns one
//! **connection** task per socket; a single **router** task owns the
//! `role → connection` table and does all routing + presence bookkeeping. The
//! per-connection ack matching is a pipelined FIFO queue (one `OK`/`ERR` per
//! command, in order) so the high-rate fire-and-forget AIM stream never blocks.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};
use tokio::time::{sleep_until, timeout, Instant};
use tracing::{info, warn};

use crate::display::events::DisplayEvent;
use crate::display::DisplayMessage;
use crate::state_machine::Event;

use super::maintenance::{DeviceInfo, HwAckResult, MaintenanceCommand, Role};
use super::{HardwareCommand, HardwareDriver};

/// Monotonic connection generation, used to epoch-guard deregistration so a
/// replaced (reconnected) connection's late EOF can't evict its successor.
static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

pub struct TcpRegistry {
    bind_addr: String,
    ack_timeout: Duration,
}

impl TcpRegistry {
    pub fn new(bind_addr: String, ack_timeout_ms: u64) -> Self {
        Self {
            bind_addr,
            ack_timeout: Duration::from_millis(ack_timeout_ms),
        }
    }
}

/// Where a resolved ack goes. `Trial` feeds the state machine; `Reply` answers
/// an awaited maintenance command; `None` is a fire-and-forget AIM (discarded).
enum AckSink {
    Trial,
    Reply(oneshot::Sender<HwAckResult>),
    None,
}

/// One command written to a device, awaiting its `OK`/`ERR`.
struct Pending {
    line: String,
    deadline: Instant,
    sink: AckSink,
}

/// A command handed from the router to a connection task to write.
struct Outbound {
    line: String,
    sink: AckSink,
}

/// The router's handle on one connected device.
struct ConnHandle {
    addr: String,
    gen: u64,
    tx: mpsc::Sender<Outbound>,
}

/// Connection→router control messages.
enum RouterMsg {
    Register {
        role: Role,
        addr: String,
        gen: u64,
        tx: mpsc::Sender<Outbound>,
    },
    Deregister {
        role: Role,
        gen: u64,
    },
}

/// Lines parsed off a connection's read half.
enum Inbound {
    Ack(String),
    Button,
}

#[async_trait]
impl HardwareDriver for TcpRegistry {
    async fn run(
        self: Box<Self>,
        cmd_rx: mpsc::Receiver<HardwareCommand>,
        maint_rx: mpsc::Receiver<MaintenanceCommand>,
        event_tx: mpsc::Sender<Event>,
        devices: Arc<RwLock<Vec<DeviceInfo>>>,
        presence: broadcast::Sender<DisplayMessage>,
    ) {
        let listener = match TcpListener::bind(&self.bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!("tcp_hw: bind {} failed: {e}", self.bind_addr);
                return;
            }
        };
        info!("tcp_hw: registry listening on {}", self.bind_addr);

        let (reg_tx, reg_rx) = mpsc::channel::<RouterMsg>(64);
        tokio::spawn(router(
            cmd_rx,
            maint_rx,
            reg_rx,
            event_tx.clone(),
            devices,
            presence,
        ));

        loop {
            let (socket, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    warn!("tcp_hw: accept error: {e}");
                    continue;
                }
            };
            let _ = socket.set_nodelay(true);
            let reg_tx = reg_tx.clone();
            let event_tx = event_tx.clone();
            let ack_timeout = self.ack_timeout;
            tokio::spawn(async move {
                if let Err(e) =
                    handle_connection(socket, peer.to_string(), reg_tx, event_tx, ack_timeout).await
                {
                    warn!("tcp_hw: connection {peer} ended: {e:#}");
                }
            });
        }
    }
}

/// The router: sole owner of the `role → connection` table. Routes both command
/// sources and keeps the device snapshot + presence broadcasts in sync.
async fn router(
    mut cmd_rx: mpsc::Receiver<HardwareCommand>,
    mut maint_rx: mpsc::Receiver<MaintenanceCommand>,
    mut reg_rx: mpsc::Receiver<RouterMsg>,
    event_tx: mpsc::Sender<Event>,
    devices: Arc<RwLock<Vec<DeviceInfo>>>,
    presence: broadcast::Sender<DisplayMessage>,
) {
    let mut table: HashMap<Role, ConnHandle> = HashMap::new();
    loop {
        tokio::select! {
            Some(msg) = reg_rx.recv() => match msg {
                RouterMsg::Register { role, addr, gen, tx } => {
                    table.insert(role, ConnHandle { addr: addr.clone(), gen, tx });
                    publish(&table, &devices, &presence,
                        DisplayEvent::DeviceConnected { role: role.as_str().into(), addr: addr.clone() }).await;
                    info!("registry: {} connected ({addr})", role.as_str());
                }
                RouterMsg::Deregister { role, gen } => {
                    // Epoch guard: only evict if this is still the live connection.
                    if table.get(&role).is_some_and(|h| h.gen == gen) {
                        table.remove(&role);
                        publish(&table, &devices, &presence,
                            DisplayEvent::DeviceDisconnected { role: role.as_str().into() }).await;
                        info!("registry: {} disconnected", role.as_str());
                    }
                }
            },

            // Trial command (untargeted): route by verb→role.
            Some(cmd) = cmd_rx.recv() => {
                let line = cmd.to_line();
                match role_for(&cmd) {
                    RouteTarget::Role(role) => match table.get(&role) {
                        Some(h) => { let _ = h.tx.send(Outbound { line, sink: AckSink::Trial }).await; }
                        None => {
                            // Never stall the trial: an absent owner of a gating verb
                            // still needs *some* event so ExecutingSentence advances.
                            warn!("registry: no {} for trial '{line}'; synthesizing error", role.as_str());
                            let _ = event_tx.send(Event::HardwareError(format!("no device: {line}"))).await;
                        }
                    },
                    // Ping is the not-guilty branch's only ack source; it is a local
                    // filler, never routed to a device. Ack it immediately.
                    RouteTarget::SynthAck => {
                        let _ = event_tx.send(Event::HardwareAck(format!("OK {line}"))).await;
                    }
                    // Deferred verb (Lights): no owner, and never the sole ack source on
                    // a gating path — skip silently so it can't steal FIRE-ack timing.
                    RouteTarget::Skip => info!("registry: skipping deferred '{line}'"),
                }
            },

            // Maintenance command (explicit target + optional reply).
            Some(mc) = maint_rx.recv() => {
                let MaintenanceCommand { target, cmd, reply } = mc;
                let line = cmd.to_line();
                match table.get(&target) {
                    Some(h) => {
                        let sink = match reply {
                            Some(tx) => AckSink::Reply(tx),
                            None => AckSink::None,
                        };
                        let _ = h.tx.send(Outbound { line, sink }).await;
                    }
                    None => {
                        if let Some(tx) = reply {
                            let _ = tx.send(HwAckResult::NoDevice);
                        }
                    }
                }
            },

            else => break,
        }
    }
}

/// Rebuild the shared device snapshot and emit a presence event.
async fn publish(
    table: &HashMap<Role, ConnHandle>,
    devices: &Arc<RwLock<Vec<DeviceInfo>>>,
    presence: &broadcast::Sender<DisplayMessage>,
    event: DisplayEvent,
) {
    let snap: Vec<DeviceInfo> = table
        .iter()
        .map(|(role, h)| DeviceInfo {
            role: role.as_str().into(),
            addr: h.addr.clone(),
        })
        .collect();
    *devices.write().await = snap;
    let _ = presence.send(DisplayMessage::Json(event));
}

enum RouteTarget {
    Role(Role),
    /// Ack locally without touching a device (trial `Ping` filler).
    SynthAck,
    /// Deferred/ownerless verb: drop with no event.
    Skip,
}

fn role_for(cmd: &HardwareCommand) -> RouteTarget {
    match cmd {
        // Aim and fire are separate boards: the turret pans/tilts, the squirt
        // board pulls the trigger.
        HardwareCommand::Fire(_) => RouteTarget::Role(Role::Squirt),
        HardwareCommand::Aim { .. } => RouteTarget::Role(Role::Turret),
        HardwareCommand::Gavel
        | HardwareCommand::GavelStrike { .. }
        | HardwareCommand::GavelJog(_) => RouteTarget::Role(Role::Gavel),
        HardwareCommand::Panel(_) => RouteTarget::Role(Role::AiJudge),
        HardwareCommand::Lights(_) => RouteTarget::Skip,
        HardwareCommand::Ping => RouteTarget::SynthAck,
    }
}

/// One device connection: HELLO handshake, then a select loop over outbound
/// writes / inbound acks / per-command timeouts.
async fn handle_connection(
    socket: tokio::net::TcpStream,
    addr: String,
    reg_tx: mpsc::Sender<RouterMsg>,
    event_tx: mpsc::Sender<Event>,
    ack_timeout: Duration,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);

    // --- HELLO handshake ---
    let mut first = String::new();
    let role = match timeout(ack_timeout, reader.read_line(&mut first)).await {
        Ok(Ok(n)) if n > 0 => {
            let hello = first.trim_end_matches(['\r', '\n']);
            let mut parts = hello.split_whitespace();
            if parts.next() != Some("HELLO") {
                write_half.write_all(b"BYE bad_hello\n").await.ok();
                return Ok(());
            }
            match parts.next().and_then(Role::from_wire) {
                Some(role) => {
                    let fw = parts.next().unwrap_or("?");
                    info!("registry: HELLO {} fw={fw} from {addr}", role.as_str());
                    role
                }
                None => {
                    write_half.write_all(b"BYE unknown_role\n").await.ok();
                    return Ok(());
                }
            }
        }
        _ => {
            write_half.write_all(b"BYE bad_hello\n").await.ok();
            return Ok(());
        }
    };
    write_half.write_all(b"WELCOME\n").await?;
    write_half.flush().await?;

    // --- Register with the router ---
    let gen = NEXT_GEN.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(64);
    if reg_tx
        .send(RouterMsg::Register {
            role,
            addr: addr.clone(),
            gen,
            tx: out_tx,
        })
        .await
        .is_err()
    {
        return Ok(()); // router gone; nothing to do
    }

    // --- Reader subtask: read_line is not cancel-safe, so it lives alone here
    // and forwards parsed lines to the select loop below. ---
    let (in_tx, mut in_rx) = mpsc::channel::<Inbound>(16);
    let reader_task = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break, // EOF / read error → channel closes → loop ends
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        continue;
                    }
                    let first = trimmed.split_whitespace().next().unwrap_or("");
                    let msg = match first {
                        "OK" | "ERR" => Inbound::Ack(trimmed.to_string()),
                        "BUTTON" => Inbound::Button,
                        "PONG" => continue, // tolerated, but PING is acked with OK PING
                        _ => {
                            warn!("tcp_hw: unrecognized line from {first}: {trimmed}");
                            continue;
                        }
                    };
                    if in_tx.send(msg).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // --- Session loop ---
    let mut pending: VecDeque<Pending> = VecDeque::new();
    loop {
        let front_deadline = pending.front().map(|p| p.deadline);
        tokio::select! {
            // Outbound: write the command, enqueue it for ack matching.
            out = out_rx.recv() => match out {
                Some(o) => {
                    write_half.write_all(format!("{}\n", o.line).as_bytes()).await?;
                    write_half.flush().await?;
                    pending.push_back(Pending {
                        line: o.line,
                        deadline: Instant::now() + ack_timeout,
                        sink: o.sink,
                    });
                }
                None => break, // router dropped our handle (replaced or shutdown)
            },

            // Inbound: resolve the front pending command, or handle BUTTON.
            inb = in_rx.recv() => match inb {
                Some(Inbound::Ack(line)) => match pending.pop_front() {
                    Some(p) => {
                        let outcome = if line.starts_with("OK") {
                            Outcome::Ok(line)
                        } else {
                            Outcome::Err(line)
                        };
                        resolve(p.sink, outcome, &event_tx).await;
                    }
                    None => warn!("tcp_hw: ack with empty queue from {addr}: {line}"),
                },
                Some(Inbound::Button) => {
                    let _ = event_tx.send(Event::OperatorStart).await;
                }
                None => break, // reader ended (EOF)
            },

            // Timeout: the front command outlived its deadline (acks are FIFO, so
            // expiring the front is always correct).
            _ = async {
                match front_deadline {
                    Some(d) => sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Some(p) = pending.pop_front() {
                    resolve(p.sink, Outcome::Timeout(p.line), &event_tx).await;
                }
            },
        }
    }

    reader_task.abort();
    let _ = reg_tx.send(RouterMsg::Deregister { role, gen }).await;
    Ok(())
}

enum Outcome {
    Ok(String),
    Err(String),
    Timeout(String),
}

/// Deliver a resolved command outcome to its sink.
async fn resolve(sink: AckSink, outcome: Outcome, event_tx: &mpsc::Sender<Event>) {
    match sink {
        AckSink::Trial => {
            let ev = match outcome {
                Outcome::Ok(line) => Event::HardwareAck(line),
                Outcome::Err(line) => Event::HardwareError(line),
                Outcome::Timeout(line) => Event::HardwareError(format!("timeout: {line}")),
            };
            let _ = event_tx.send(ev).await;
        }
        AckSink::Reply(tx) => {
            let result = match outcome {
                Outcome::Ok(line) => HwAckResult::Ok { line },
                Outcome::Err(line) => HwAckResult::Err { reason: line },
                Outcome::Timeout(_) => HwAckResult::Timeout,
            };
            let _ = tx.send(result);
        }
        AckSink::None => {} // fire-and-forget AIM: discard
    }
}
