//! The printer service: a task that turns finalized [`TrialRecord`]s into
//! physical receipts. The state machine hands it a record per completed verdict
//! over a channel; rendering is cheap and pure, the USB write is blocking, so
//! each receipt is rendered and sent on a blocking thread.
//!
//! `mode` (from `[printer]` config) decides how far it goes:
//! - `off`   — drop the record (the casebook still logged it upstream).
//! - `mock`  — render and log the byte count, but never open the USB device.
//! - `real`  — render and send to the printer.

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::config::PrinterConfig;

use super::{render, ReportOpts, TrialRecord};

/// Spawn the printer task and return the sender the state machine pushes
/// finalized records into. A small buffer is plenty — trials are seconds apart
/// at minimum and a backed-up printer should drop, not stall the trial loop.
pub fn spawn(cfg: PrinterConfig) -> mpsc::Sender<TrialRecord> {
    let (tx, mut rx) = mpsc::channel::<TrialRecord>(8);
    tokio::spawn(async move {
        info!(mode = %cfg.mode, width = cfg.width_dots, "printer service up");
        while let Some(rec) = rx.recv().await {
            let cfg = cfg.clone();
            match tokio::task::spawn_blocking(move || print_one(&cfg, &rec)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => error!("keepsake print failed: {e:#}"),
                Err(e) => error!("keepsake print task panicked: {e}"),
            }
        }
        warn!("printer channel closed; printer service exiting");
    });
    tx
}

fn print_one(cfg: &PrinterConfig, rec: &TrialRecord) -> anyhow::Result<()> {
    if cfg.mode == "off" {
        return Ok(());
    }

    let opts = ReportOpts {
        width_dots: cfg.width_dots,
        qr_url: &cfg.qr_url,
        booth_location: &cfg.booth_location,
    };
    let bytes = render(rec, &opts).build();

    if cfg.mode != "real" {
        info!(case_no = rec.case_no, bytes = bytes.len(), "keepsake rendered (mock; no USB)");
        return Ok(());
    }

    let printer = thermal_printer::Printer::connect()?;
    printer.usb().write(&bytes)?;
    info!(case_no = rec.case_no, bytes = bytes.len(), "keepsake printed");
    Ok(())
}
