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

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

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
    // F4: live coupon frequency (0=off,1=rare,2=sometimes,3=always), read fresh
    // per receipt so the operator's dropdown takes effect without a restart.
    coupon_frequency: Arc<AtomicU8>,
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
                    let level = coupon_frequency.load(Ordering::Relaxed);
                    match tokio::task::spawn_blocking(move || print_trial(&cfg, &rec, level, &bc)).await {
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

/// Map a coupon-frequency string to a level (0=off,1=rare,2=sometimes,3=always).
/// Unknown values map to 0 (off). Shared with the operator endpoint.
pub fn coupon_level(frequency: &str) -> u8 {
    match frequency {
        "rare" => 1,
        "sometimes" => 2,
        "always" => 3,
        _ => 0,
    }
}

/// Inverse of [`coupon_level`], for reporting the current setting.
pub fn coupon_level_str(level: u8) -> &'static str {
    match level {
        1 => "rare",
        2 => "sometimes",
        3 => "always",
        _ => "off",
    }
}

/// Decide whether this receipt gets a coupon, per the live coupon level
/// (0=off,1=rare ~1/6,2=sometimes ~1/3,3=always).
fn roll_coupon(level: u8) -> Option<super::report::CouponCopy> {
    use rand::Rng;
    let p = match level {
        3 => 1.0,
        2 => 1.0 / 3.0,
        1 => 1.0 / 6.0,
        _ => 0.0,
    };
    (p > 0.0 && rand::thread_rng().gen_bool(p)).then(super::report::random_coupon)
}

fn print_trial(
    cfg: &PrinterConfig,
    rec: &TrialRecord,
    coupon_level: u8,
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
        // Roll once per trial (so both keepsake copies match) whether to append a
        // "bad lawyer" coupon, per the runtime-switchable [printer] frequency.
        coupon: roll_coupon(coupon_level),
    };
    let bytes = render(rec, &opts).build();
    // The booth prints two copies — one to hang on the backdrop, one for the
    // defendant. The render is deterministic, so render once and write the same
    // bytes per copy; each ends in its own cut, so the copies come off as
    // separate strips. A mid-run transport failure aborts the rest (better to
    // stop than jam), so the operator banner from emit() still fires.
    let copies = cfg.keepsake_copies.max(1);
    for n in 1..=copies {
        let label = if copies > 1 {
            format!("keepsake for case {} (copy {n}/{copies})", rec.case_no)
        } else {
            format!("keepsake for case {}", rec.case_no)
        };
        match emit(cfg, &bytes, &label, bcast)? {
            Emitted::Mock => info!(case_no = rec.case_no, copy = n, copies, bytes = bytes.len(), "keepsake rendered (mock; no I/O)"),
            Emitted::Printed => info!(case_no = rec.case_no, copy = n, copies, bytes = bytes.len(), "keepsake printed"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_cfg(copies: u32) -> PrinterConfig {
        PrinterConfig { mode: "mock".into(), keepsake_copies: copies, ..Default::default() }
    }

    // Mock mode does no I/O, so this exercises the render-once/emit-per-copy
    // loop end to end: it must render cleanly for each copy and never panic.
    #[test]
    fn keepsake_prints_each_requested_copy() {
        let (tx, _rx) = broadcast::channel(4);
        let rec = TrialRecord::sample_guilty();
        for copies in [1u32, 2, 3] {
            assert!(print_trial(&mock_cfg(copies), &rec, 0, &tx).is_ok());
        }
    }

    // A 0 (or unset-to-0) copy count must still print one, not zero — the
    // keepsake is the point of the trial. `off` mode prints none regardless.
    #[test]
    fn copy_count_is_clamped_and_off_prints_nothing() {
        let (tx, _rx) = broadcast::channel(4);
        let rec = TrialRecord::sample_acquitted();
        assert!(print_trial(&mock_cfg(0), &rec, 0, &tx).is_ok());
        let off = PrinterConfig { mode: "off".into(), keepsake_copies: 2, ..Default::default() };
        assert!(print_trial(&off, &rec, 0, &tx).is_ok());
    }
}
