//! The printer service: one task, one queue, one printer. Trial keepsakes and
//! operator custom prints share the channel so jobs serialize naturally.
//! Rendering is cheap and pure (image dithering less so — still CPU-only), the
//! USB/net write is blocking, so each job is rendered and sent on a blocking
//! thread.
//!
//! `mode` (from `[printer]` config) decides how far it goes:
//! - `off`   — drop the job (the casebook still logged trials upstream).
//! - `mock`  — render and log the byte count, but never open the device.
//! - `real`  — render and send to the printer.

use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::config::PrinterConfig;
use crate::display::events::DisplayEvent;
use crate::display::DisplayMessage;

use super::custom::{self, render_custom, PrintDoc};
use super::{render, ReportOpts, TrialRecord};

/// A unit of work for the printer task.
pub enum PrintJob {
    /// A finalized trial keepsake — fire-and-forget from the state machine.
    Trial(TrialRecord),
    /// An operator custom print; `reply` carries the outcome back to the HTTP
    /// handler so the console's Print button gets a definitive answer.
    Custom {
        doc: PrintDoc,
        reply: oneshot::Sender<Result<CustomOutcome, String>>,
    },
}

/// What actually happened to a custom job, mode-aware.
#[derive(Debug, Clone, Copy)]
pub enum CustomOutcome {
    /// `mode = "off"`: rendered nothing, sent nothing.
    Off,
    /// `mode = "mock"`: rendered, no I/O.
    Mock { bytes: usize },
    Printed { bytes: usize },
}

/// Spawn the printer task and return the job sender. A small buffer is plenty —
/// trials are seconds apart at minimum and a backed-up printer should drop,
/// not stall the trial loop.
pub fn spawn(
    cfg: PrinterConfig,
    // Operator feedback channel: printer problems (not ready, print failed)
    // surface as an Error banner on the console instead of only a log line.
    bcast: broadcast::Sender<DisplayMessage>,
) -> mpsc::Sender<PrintJob> {
    let (tx, mut rx) = mpsc::channel::<PrintJob>(8);
    tokio::spawn(async move {
        info!(mode = %cfg.mode, width = cfg.width_dots, "printer service up");
        while let Some(job) = rx.recv().await {
            let cfg = cfg.clone();
            let b = bcast.clone();
            match job {
                PrintJob::Trial(rec) => {
                    let case_no = rec.case_no;
                    let bc = b.clone();
                    match tokio::task::spawn_blocking(move || print_trial(&cfg, &rec, &bc)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            error!("keepsake print failed: {e:#}");
                            let _ = b.send(DisplayMessage::Json(DisplayEvent::Error {
                                message: format!("keepsake for case {case_no} failed to print: {e:#}"),
                            }));
                        }
                        Err(e) => error!("keepsake print task panicked: {e}"),
                    }
                }
                PrintJob::Custom { doc, reply } => {
                    let bc = b.clone();
                    let out = match tokio::task::spawn_blocking(move || print_custom(&cfg, &doc, &bc)).await {
                        Ok(Ok(outcome)) => Ok(outcome),
                        Ok(Err(e)) => {
                            error!("custom print failed: {e:#}");
                            Err(format!("{e:#}"))
                        }
                        Err(e) => {
                            error!("custom print task panicked: {e}");
                            Err(format!("print task panicked: {e}"))
                        }
                    };
                    let _ = reply.send(out); // handler may have timed out; fine
                }
            }
        }
        warn!("printer channel closed; printer service exiting");
    });
    tx
}

fn print_trial(
    cfg: &PrinterConfig,
    rec: &TrialRecord,
    bcast: &broadcast::Sender<DisplayMessage>,
) -> anyhow::Result<()> {
    if cfg.mode == "off" {
        return Ok(());
    }
    let opts = ReportOpts {
        width_dots: cfg.width_dots,
        qr_url: &cfg.qr_url,
        booth_location: &cfg.booth_location,
        image_gamma: cfg.image_gamma,
        image_brightness: cfg.image_brightness,
        image_contrast: cfg.image_contrast,
        image_dither: custom::parse_dither(&cfg.image_dither).unwrap_or_else(|e| {
            warn!("bad [printer] image_dither ({e}); falling back to Floyd-Steinberg");
            thermal_printer::raster::Dither::FloydSteinberg
        }),
        upside_down: cfg.upside_down,
    };
    let bytes = render(rec, &opts).build();
    let label = format!("keepsake for case {}", rec.case_no);
    match emit(cfg, &bytes, &label, bcast)? {
        Emitted::Mock => info!(case_no = rec.case_no, bytes = bytes.len(), "keepsake rendered (mock; no I/O)"),
        Emitted::Printed => info!(case_no = rec.case_no, bytes = bytes.len(), "keepsake printed"),
    }
    Ok(())
}

fn print_custom(
    cfg: &PrinterConfig,
    doc: &PrintDoc,
    bcast: &broadcast::Sender<DisplayMessage>,
) -> anyhow::Result<CustomOutcome> {
    if cfg.mode == "off" {
        return Ok(CustomOutcome::Off);
    }
    let bytes = render_custom(doc, cfg)?;
    match emit(cfg, &bytes, "custom print", bcast)? {
        Emitted::Mock => {
            info!(bytes = bytes.len(), "custom print rendered (mock; no I/O)");
            Ok(CustomOutcome::Mock { bytes: bytes.len() })
        }
        Emitted::Printed => {
            info!(bytes = bytes.len(), "custom print sent");
            Ok(CustomOutcome::Printed { bytes: bytes.len() })
        }
    }
}

enum Emitted {
    Mock,
    Printed,
}

/// The shared transport tail: mode gate, transport pick, status preflight
/// (with an operator banner), and the blocking write. `mode = "off"` never
/// reaches here.
fn emit(
    cfg: &PrinterConfig,
    bytes: &[u8],
    label: &str,
    bcast: &broadcast::Sender<DisplayMessage>,
) -> anyhow::Result<Emitted> {
    if cfg.mode != "real" {
        return Ok(Emitted::Mock);
    }

    let printer = match cfg.transport.as_str() {
        "net" => {
            if cfg.net_addr.is_empty() {
                anyhow::bail!("printer.transport = \"net\" but printer.net_addr is not set");
            }
            thermal_printer::Printer::connect_net(&cfg.net_addr)?
        }
        "usb" => thermal_printer::Printer::connect()?,
        other => {
            anyhow::bail!("unknown printer.transport '{other}' (expected \"usb\" or \"net\")")
        }
    };
    // Preflight: paper-out / cover-open would otherwise swallow the output
    // silently (the write can still "succeed"). Warn the operator, then
    // attempt the write anyway — some conditions (near-end) still print.
    match printer.transport().query_status() {
        Ok(st) if !st.is_ready() => {
            let msg = format!(
                "printer not ready (paper_out={:?} cover_open={:?} online={:?}) — {label} may not print",
                st.paper_out, st.cover_open, st.online
            );
            warn!("{msg}");
            let _ = bcast.send(DisplayMessage::Json(DisplayEvent::Error { message: msg }));
        }
        Ok(_) => {}
        Err(e) => warn!("printer status query failed (printing anyway): {e:#}"),
    }
    printer.transport().write(bytes)?;
    Ok(Emitted::Printed)
}
