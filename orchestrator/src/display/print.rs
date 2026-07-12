//! `/operator/print/*` — the console's custom-print plane: submit a block
//! document to the printer queue, get pixel-true image/QR previews, and CRUD
//! the named template store.
//!
//! These routes live on their own sub-router with a raised body limit (base64
//! images blow past axum's 2MB default); everything else keeps the small one.

use std::io::Cursor;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, State as AxumState};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::info;

use crate::printer::custom::{self, PrintDoc};
use crate::printer::service::{CustomOutcome, PrintJob};
use crate::printer::templates;

use super::AppState;

/// How long the Print button waits for the queue + render + write. Generous:
/// a slow USB write behind a queued keepsake is the worst realistic case.
const PRINT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/operator/print", post(print_custom))
        .route("/operator/print/config", get(printer_info))
        .route("/operator/print/preview_image", post(preview_image))
        .route("/operator/print/preview_qr", post(preview_qr))
        .route("/operator/print/templates", get(list_templates))
        .route(
            "/operator/print/templates/{name}",
            get(get_template).put(put_template).delete(delete_template),
        )
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
}

/// The tunables the console's preview must mirror to model heights exactly.
#[derive(Serialize)]
struct PrinterInfo {
    mode: String,
    width_dots: u32,
    head_to_cutter_dots: u32,
    image_gamma: f32,
    image_brightness: f32,
    image_contrast: f32,
    image_dither: String,
}

async fn printer_info(AxumState(s): AxumState<AppState>) -> Response {
    Json(PrinterInfo {
        mode: s.printer_cfg.mode.clone(),
        width_dots: s.printer_cfg.width_dots,
        head_to_cutter_dots: s.printer_cfg.head_to_cutter_dots,
        image_gamma: s.printer_cfg.image_gamma,
        image_brightness: s.printer_cfg.image_brightness,
        image_contrast: s.printer_cfg.image_contrast,
        image_dither: s.printer_cfg.image_dither.clone(),
    })
    .into_response()
}

// ---- printing -----------------------------------------------------------------

#[derive(Serialize)]
struct PrintResult {
    /// "printed" | "mock" | "off" — mode-aware so the panel can say
    /// "nothing came out because the printer is off" as status, not error.
    status: &'static str,
    bytes: usize,
}

async fn print_custom(
    AxumState(s): AxumState<AppState>,
    Json(doc): Json<PrintDoc>,
) -> Response {
    if let Err(e) = custom::validate(&doc) {
        return (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response();
    }
    info!(blocks = doc.blocks.len(), length_mm = ?doc.length_mm, "operator: custom print");
    let (reply, rx) = oneshot::channel();
    if s.print_job_tx.send(PrintJob::Custom { doc, reply }).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "printer service is down").into_response();
    }
    match tokio::time::timeout(PRINT_TIMEOUT, rx).await {
        Ok(Ok(Ok(outcome))) => {
            let (status, bytes) = match outcome {
                CustomOutcome::Off => ("off", 0),
                CustomOutcome::Mock { bytes } => ("mock", bytes),
                CustomOutcome::Printed { bytes } => ("printed", bytes),
            };
            Json(PrintResult { status, bytes }).into_response()
        }
        // Render or transport failure — the message is operator-readable
        // (overflow ledgers, "printer not ready", ...).
        Ok(Ok(Err(msg))) => (StatusCode::BAD_GATEWAY, msg).into_response(),
        Ok(Err(_)) => (StatusCode::INTERNAL_SERVER_ERROR, "printer service dropped the job").into_response(),
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, "print timed out (queue backed up?)").into_response(),
    }
}

// ---- previews -------------------------------------------------------------------

#[derive(Deserialize)]
struct PreviewImageReq {
    data_b64: String,
    /// `None` = the printer's configured default dither.
    #[serde(default)]
    dither: Option<String>,
    #[serde(default = "d_100")]
    width_pct: u8,
    /// Tone overrides; absent/null = the printer's configured defaults.
    #[serde(default)]
    gamma: Option<f32>,
    #[serde(default)]
    brightness: Option<f32>,
    #[serde(default)]
    contrast: Option<f32>,
}

fn d_100() -> u8 { 100 }

async fn preview_image(
    AxumState(s): AxumState<AppState>,
    Json(req): Json<PreviewImageReq>,
) -> Response {
    let bytes = match base64::engine::general_purpose::STANDARD.decode(req.data_b64.trim()) {
        Ok(b) => b,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, format!("image base64: {e}")).into_response(),
    };
    let width = s.printer_cfg.width_dots;
    let gamma = req.gamma.unwrap_or(s.printer_cfg.image_gamma);
    let brightness = req.brightness.unwrap_or(s.printer_cfg.image_brightness);
    let contrast = req.contrast.unwrap_or(s.printer_cfg.image_contrast);
    let dither = req.dither.unwrap_or_else(|| s.printer_cfg.image_dither.clone());
    // Dithering a large photo is CPU-bound — keep it off the async workers.
    let out = tokio::task::spawn_blocking(move || {
        custom::preview_image_raster(&bytes, width, req.width_pct, &dither, gamma, brightness, contrast)
            .map(|r| raster_png(&r))
    })
    .await;
    match out {
        Ok(Ok(png)) => png_response(png),
        Ok(Err(e)) => (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("preview task panicked: {e}")).into_response(),
    }
}

#[derive(Deserialize)]
struct PreviewQrReq {
    data: String,
    #[serde(default = "d_module")]
    module: u8,
    #[serde(default = "d_ecc")]
    ecc: String,
}

fn d_module() -> u8 { 6 }
fn d_ecc() -> String { "m".into() }

async fn preview_qr(Json(req): Json<PreviewQrReq>) -> Response {
    match custom::preview_qr_raster(&req.data, req.module, &req.ecc) {
        Ok(r) => png_response(raster_png(&r)),
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response(),
    }
}

/// 1-bit raster → grayscale PNG at raster resolution (black dot = black pixel).
fn raster_png(r: &thermal_printer::raster::Raster) -> Vec<u8> {
    let w = r.width_bytes as u32 * 8;
    let img = image::GrayImage::from_fn(w, r.height as u32, |x, y| {
        let idx = y as usize * r.width_bytes as usize + (x / 8) as usize;
        let black = r.bits[idx] & (0x80 >> (x % 8)) != 0;
        image::Luma([if black { 0u8 } else { 255 }])
    });
    let mut buf = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("in-memory PNG encode");
    buf.into_inner()
}

fn png_response(png: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "image/png")], png).into_response()
}

// ---- templates --------------------------------------------------------------------

async fn list_templates(AxumState(s): AxumState<AppState>) -> Response {
    Json(s.print_templates.read().await.list()).into_response()
}

async fn get_template(AxumState(s): AxumState<AppState>, Path(name): Path<String>) -> Response {
    match s.print_templates.read().await.get(&name) {
        Some(doc) => Json(doc.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "no such template").into_response(),
    }
}

async fn put_template(
    AxumState(s): AxumState<AppState>,
    Path(name): Path<String>,
    Json(doc): Json<PrintDoc>,
) -> Response {
    if let Err(e) = templates::validate_name(&name).and_then(|_| custom::validate(&doc)) {
        return (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response();
    }
    match s.print_templates.write().await.put(&name, doc) {
        Ok(()) => {
            info!(%name, "print template saved");
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn delete_template(AxumState(s): AxumState<AppState>, Path(name): Path<String>) -> Response {
    match s.print_templates.write().await.remove(&name) {
        Ok(true) => {
            info!(%name, "print template deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such template").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}
