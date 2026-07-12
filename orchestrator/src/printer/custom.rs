//! Operator-authored custom prints: a block document (text / rule / feed / QR /
//! barcode / image) composed in the console's Print panel, rendered to ESC/POS.
//!
//! Two layout modes:
//! - **Continuous** (`length_mm: None`): print the blocks, feed, cut — like the
//!   trial keepsake.
//! - **Size-bounded** (`length_mm: Some(mm)`): guarantee an exact cut-to-cut
//!   strip length (e.g. an 80×50mm plaque insert). The renderer keeps a
//!   dot-exact height ledger, distributes leftover space into flex [`Feed`]
//!   spacers, optionally shrinks images to fit, and closes with a precise fill
//!   feed + partial cut. See `fit()` for the algorithm and the physical
//!   constraint (the head-to-cutter dead zone) it works around.
//!
//! Heights are deterministic on purpose: every advance is either an explicit
//! line spacing, an explicit dot feed, or a raster of known height — no
//! reliance on firmware-default spacing. QR codes are rasterized locally (not
//! the native GS ( k command) so their height is known and the console preview
//! can show the exact symbol.
//!
//! **Motion units.** All measurements here are in real 203-dpi dots, but the
//! printer interprets ESC J (feed) and ESC 3 (line spacing) values in its
//! *vertical motion unit*, which on the booth POS-80 is the Epson-default
//! 1/360" — measured: a 400-dot strip came out 28.2mm (= 400/360"), not
//! 50mm. `PrinterConfig::feed_units_per_inch` carries that unit; every
//! emitted vertical command converts dots → units, and bounded strips keep a
//! running unit ledger so the cut-to-cut total lands exactly on target.
//! Raster rows are physical 1/203" lines and need no conversion.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use thermal_printer::canvas::Canvas;
use thermal_printer::escpos::{Align, Barcode as Sym, Builder, Font};
use thermal_printer::raster::{self, Dither, Raster};
use thermal_printer::text::{cols_for, wrap};

use super::asciify;
use crate::config::PrinterConfig;

/// 203 dpi.
pub const DOTS_PER_MM: f32 = 203.0 / 25.4;

/// Hard ceiling on rendered output — a runaway doc melts a paper roll.
const MAX_BYTES: usize = 512 * 1024;
/// Tallest single image, in raster rows (~20cm of paper).
const MAX_IMAGE_ROWS: u32 = 1600;
/// Dots one fixed/empty "feed line" advances (matches the firmware default).
const FEED_LINE_DOTS: u32 = 30;
/// Extra leading between text lines, on top of the glyph height.
const TEXT_LEADING_DOTS: u32 = 6;
/// QR quiet zone, in modules, baked into the raster on all sides (spec says 4).
const QR_QUIET_MODULES: u32 = 4;

pub fn mm_to_dots(mm: f32) -> u32 {
    (mm * DOTS_PER_MM).round() as u32
}

/// Real 203-dpi dots → the printer's vertical motion units (ESC J / ESC 3).
fn to_units(dots: u32, units_per_inch: u32) -> u32 {
    ((dots as u64 * units_per_inch as u64 + 101) / 203) as u32
}

pub fn dots_to_mm(dots: u32) -> f32 {
    dots as f32 / DOTS_PER_MM
}

// ---- document schema ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintDoc {
    pub blocks: Vec<Block>,
    /// `None` = continuous; `Some(mm)` = exact cut-to-cut strip length.
    #[serde(default)]
    pub length_mm: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
        #[serde(default = "d_left")]
        align: String, // "left" | "center" | "right"
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        underline: bool,
        #[serde(default)]
        inverse: bool,
        #[serde(default = "d_font_a")]
        font: String, // "a" | "b"
        #[serde(default = "d_one")]
        size_w: u8, // 1..=8
        #[serde(default = "d_one")]
        size_h: u8, // 1..=8
    },
    Rule {
        #[serde(default)]
        heavy: bool, // '-' vs '='
    },
    Feed {
        #[serde(default = "d_one")]
        lines: u8, // 1..=10 (ignored while flexing)
        /// Bounded mode only: 0 = fixed, >=1 = spring weight absorbing leftover.
        #[serde(default)]
        flex: u8,
    },
    Qr {
        data: String,
        #[serde(default = "d_module")]
        module: u8, // dots per module, 1..=16
        #[serde(default = "d_ecc")]
        ecc: String, // "l" | "m" | "q" | "h"
    },
    Barcode {
        data: String,
        #[serde(default = "d_sym")]
        symbology: String, // "code128" | "code39" | "ean13" | "upca"
        #[serde(default = "d_bar_h")]
        height: u8, // 24..=200 dots
        #[serde(default = "d_bar_w")]
        width: u8, // module width 2..=6
    },
    Image {
        /// Base64 PNG/JPEG (no `data:` prefix).
        data_b64: String,
        /// `None` = the printer's configured default (`[printer] image_dither`).
        #[serde(default)]
        dither: Option<String>, // "fs" | "atkinson" | "bayer" | "none"
        #[serde(default = "d_100")]
        width_pct: u8, // 10..=100 of printable width
        /// Bounded mode only: allow scaling down to make the strip fit.
        #[serde(default)]
        shrink: bool,
        /// Tone overrides; `None` = the printer's configured defaults
        /// (`[printer] image_gamma` / `image_brightness`).
        #[serde(default)]
        gamma: Option<f32>, // 0.2..=4.0, <1 brightens
        #[serde(default)]
        brightness: Option<f32>, // -128..=128 luma, + lighter
        #[serde(default)]
        contrast: Option<f32>, // 0.2..=3.0, <1 flattens (lifts shadows)
    },
}

fn d_left() -> String { "left".into() }
fn d_font_a() -> String { "a".into() }
fn d_one() -> u8 { 1 }
fn d_module() -> u8 { 6 }
fn d_ecc() -> String { "m".into() }
fn d_sym() -> String { "code128".into() }
fn d_bar_h() -> u8 { 80 }
fn d_bar_w() -> u8 { 3 }
fn d_100() -> u8 { 100 }

// ---- validation ---------------------------------------------------------------

/// Cheap structural validation — run in the HTTP handler for an immediate 422
/// before the doc is queued. Rendering revalidates implicitly (decode etc.).
pub fn validate(doc: &PrintDoc) -> anyhow::Result<()> {
    anyhow::ensure!(!doc.blocks.is_empty(), "document has no blocks");
    anyhow::ensure!(doc.blocks.len() <= 100, "too many blocks (max 100)");
    if let Some(mm) = doc.length_mm {
        anyhow::ensure!(
            (20.0..=500.0).contains(&mm),
            "length_mm {mm} out of range (20-500)"
        );
    }
    let mut images = 0usize;
    for (i, blk) in doc.blocks.iter().enumerate() {
        let at = |msg: String| anyhow::anyhow!("block {}: {}", i + 1, msg);
        match blk {
            Block::Text { text, align, font, size_w, size_h, .. } => {
                anyhow::ensure!(text.len() <= 4000, at(format!("text too long ({} chars, max 4000)", text.len())));
                parse_align(align).map_err(at)?;
                parse_font(font).map_err(at)?;
                anyhow::ensure!((1..=8).contains(size_w) && (1..=8).contains(size_h), at("size out of range (1-8)".into()));
            }
            Block::Rule { .. } => {}
            Block::Feed { lines, flex } => {
                anyhow::ensure!((1..=10).contains(lines), at("feed lines out of range (1-10)".into()));
                anyhow::ensure!(*flex <= 10, at("flex weight out of range (0-10)".into()));
            }
            Block::Qr { data, module, ecc } => {
                anyhow::ensure!(!data.is_empty() && data.len() <= 512, at("QR data must be 1-512 bytes".into()));
                anyhow::ensure!((1..=16).contains(module), at("QR module out of range (1-16)".into()));
                parse_ecc(ecc).map_err(at)?;
            }
            Block::Barcode { data, symbology, height, width } => {
                validate_barcode(data, symbology).map_err(at)?;
                anyhow::ensure!((24..=200).contains(height), at("barcode height out of range (24-200)".into()));
                anyhow::ensure!((2..=6).contains(width), at("barcode width out of range (2-6)".into()));
            }
            Block::Image { data_b64, dither, width_pct, gamma, brightness, contrast, .. } => {
                images += 1;
                anyhow::ensure!(images <= 4, at("too many images (max 4)".into()));
                anyhow::ensure!((10..=100).contains(width_pct), at("width_pct out of range (10-100)".into()));
                if let Some(d) = dither {
                    parse_dither(d).map_err(at)?;
                }
                if let Some(g) = gamma {
                    anyhow::ensure!((0.2..=4.0).contains(g), at("gamma out of range (0.2-4.0)".into()));
                }
                if let Some(br) = brightness {
                    anyhow::ensure!((-128.0..=128.0).contains(br), at("brightness out of range (-128-128)".into()));
                }
                if let Some(c) = contrast {
                    anyhow::ensure!((0.2..=3.0).contains(c), at("contrast out of range (0.2-3.0)".into()));
                }
                // ~4MB decoded ceiling, checked on the base64 length (4/3 ratio).
                anyhow::ensure!(data_b64.len() <= 4 * 1024 * 1024 * 4 / 3, at("image too large (max 4MB)".into()));
            }
        }
    }
    Ok(())
}

fn validate_barcode(data: &str, symbology: &str) -> Result<(), String> {
    if data.is_empty() || data.len() > 64 {
        return Err("barcode data must be 1-64 chars".into());
    }
    if !data.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err("barcode data must be printable ASCII".into());
    }
    let digits = data.bytes().all(|b| b.is_ascii_digit());
    match symbology {
        "code128" => Ok(()),
        "code39" => {
            let ok = data.bytes().all(|b| {
                b.is_ascii_digit() || b.is_ascii_uppercase() || matches!(b, b' ' | b'-' | b'.' | b'$' | b'/' | b'+' | b'%')
            });
            ok.then_some(()).ok_or_else(|| "code39 allows A-Z 0-9 space -.$/+%".into())
        }
        "ean13" => (digits && matches!(data.len(), 12 | 13))
            .then_some(())
            .ok_or_else(|| "ean13 needs 12-13 digits".into()),
        "upca" => (digits && matches!(data.len(), 11 | 12))
            .then_some(())
            .ok_or_else(|| "upca needs 11-12 digits".into()),
        other => Err(format!("unknown symbology '{other}'")),
    }
}

fn parse_align(s: &str) -> Result<Align, String> {
    match s {
        "left" => Ok(Align::Left),
        "center" => Ok(Align::Center),
        "right" => Ok(Align::Right),
        other => Err(format!("unknown align '{other}'")),
    }
}

fn parse_font(s: &str) -> Result<Font, String> {
    match s {
        "a" => Ok(Font::A),
        "b" => Ok(Font::B),
        other => Err(format!("unknown font '{other}'")),
    }
}

fn parse_ecc(s: &str) -> Result<qrcode::EcLevel, String> {
    match s {
        "l" => Ok(qrcode::EcLevel::L),
        "m" => Ok(qrcode::EcLevel::M),
        "q" => Ok(qrcode::EcLevel::Q),
        "h" => Ok(qrcode::EcLevel::H),
        other => Err(format!("unknown ecc '{other}'")),
    }
}

pub(crate) fn parse_dither(s: &str) -> Result<Dither, String> {
    match s {
        "fs" => Ok(Dither::FloydSteinberg),
        "atkinson" => Ok(Dither::Atkinson),
        "bayer" => Ok(Dither::Bayer),
        "none" => Ok(Dither::None),
        other => Err(format!("unknown dither '{other}'")),
    }
}

fn glyph_height(f: Font) -> u32 {
    match f {
        Font::A => 24,
        Font::B => 17,
    }
}

// ---- preview helpers (console round-trips) --------------------------------------

/// Dither an uploaded image exactly as an [`Block::Image`] would print it —
/// the console preview shows these very pixels.
pub fn preview_image_raster(
    bytes: &[u8],
    width_dots: u32,
    pct: u8,
    dither: &str,
    gamma: f32,
    brightness: f32,
    contrast: f32,
) -> anyhow::Result<Raster> {
    anyhow::ensure!((10..=100).contains(&pct), "width_pct out of range (10-100)");
    anyhow::ensure!(bytes.len() <= 4 * 1024 * 1024, "image too large (max 4MB)");
    let opts = raster::Options {
        dither: parse_dither(dither).map_err(anyhow::Error::msg)?,
        gamma,
        brightness,
        contrast,
    };
    image_raster(bytes, width_dots, pct as u32, opts)
}

/// The exact QR raster a [`Block::Qr`] prints, for pixel-true previews.
pub fn preview_qr_raster(data: &str, module: u8, ecc: &str) -> anyhow::Result<Raster> {
    anyhow::ensure!(!data.is_empty() && data.len() <= 512, "QR data must be 1-512 bytes");
    anyhow::ensure!((1..=16).contains(&module), "QR module out of range (1-16)");
    qr_raster(data, module as u32, parse_ecc(ecc).map_err(anyhow::Error::msg)?)
}

// ---- prepared blocks (exact heights) -------------------------------------------

/// A block resolved to its final form: wrapped lines, rendered rasters, and an
/// exact height in dots. The fit pass mutates flex feeds and shrinkable images.
enum Prep {
    Lines {
        lines: Vec<String>,
        align: Align,
        bold: bool,
        underline: bool,
        inverse: bool,
        font: Font,
        size_w: u8,
        size_h: u8,
        /// Explicit ESC 3 spacing = advance per line.
        spacing: u32,
    },
    Raster {
        raster: Raster,
        /// Present on shrinkable images: original encoded bytes + dither, so the
        /// fit pass can re-raster at a smaller width.
        shrink: Option<ShrinkSrc>,
    },
    Barcode {
        data: String,
        sym: Sym,
        height: u8,
        width: u8,
    },
    Feed {
        dots: u32,
        flex: u8,
    },
}

struct ShrinkSrc {
    bytes: Vec<u8>,
    opts: raster::Options,
}

impl Prep {
    fn height(&self, bounded: bool) -> u32 {
        match self {
            Prep::Lines { lines, spacing, .. } => lines.len() as u32 * spacing,
            Prep::Raster { raster, .. } => raster.height as u32,
            // Bounded mode disables HRI, so the symbol height is exact. In
            // continuous mode HRI adds an uncounted line — nobody's measuring.
            Prep::Barcode { height, .. } => {
                *height as u32 + if bounded { 0 } else { FEED_LINE_DOTS }
            }
            Prep::Feed { dots, .. } => *dots,
        }
    }
}

fn prepare(blk: &Block, width_dots: u32, cfg: &PrinterConfig) -> anyhow::Result<Prep> {
    Ok(match blk {
        Block::Text { text, align, bold, underline, inverse, font, size_w, size_h } => {
            let font = parse_font(font).map_err(anyhow::Error::msg)?;
            let align = parse_align(align).map_err(anyhow::Error::msg)?;
            let cols = (cols_for(width_dots, font) / *size_w as usize).max(1);
            let mut lines = Vec::new();
            for raw in text.lines() {
                lines.extend(wrap(&asciify(raw), cols));
            }
            if lines.is_empty() {
                lines.push(String::new());
            }
            let spacing = glyph_height(font) * *size_h as u32 + TEXT_LEADING_DOTS;
            Prep::Lines {
                lines,
                align,
                bold: *bold,
                underline: *underline,
                inverse: *inverse,
                font,
                size_w: *size_w,
                size_h: *size_h,
                spacing,
            }
        }
        Block::Rule { heavy } => {
            let ch = if *heavy { "=" } else { "-" };
            Prep::Lines {
                lines: vec![ch.repeat(cols_for(width_dots, Font::A))],
                align: Align::Left,
                bold: false,
                underline: false,
                inverse: false,
                font: Font::A,
                size_w: 1,
                size_h: 1,
                spacing: glyph_height(Font::A) + TEXT_LEADING_DOTS,
            }
        }
        Block::Feed { lines, flex } => Prep::Feed {
            dots: *lines as u32 * FEED_LINE_DOTS,
            flex: *flex,
        },
        Block::Qr { data, module, ecc } => Prep::Raster {
            raster: qr_raster(data, *module as u32, parse_ecc(ecc).map_err(anyhow::Error::msg)?)?,
            shrink: None,
        },
        Block::Barcode { data, symbology, height, width } => {
            let sym = match symbology.as_str() {
                "code128" => Sym::Code128,
                "code39" => Sym::Code39,
                "ean13" => Sym::Ean13,
                "upca" => Sym::UpcA,
                other => anyhow::bail!("unknown symbology '{other}'"),
            };
            Prep::Barcode { data: data.clone(), sym, height: *height, width: *width }
        }
        Block::Image { data_b64, dither, width_pct, shrink, gamma, brightness, contrast } => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_b64.trim())
                .map_err(|e| anyhow::anyhow!("image base64: {e}"))?;
            anyhow::ensure!(bytes.len() <= 4 * 1024 * 1024, "image too large (max 4MB)");
            let opts = raster::Options {
                dither: parse_dither(dither.as_deref().unwrap_or(&cfg.image_dither))
                    .map_err(anyhow::Error::msg)?,
                gamma: gamma.unwrap_or(cfg.image_gamma),
                brightness: brightness.unwrap_or(cfg.image_brightness),
                contrast: contrast.unwrap_or(cfg.image_contrast),
            };
            let raster = image_raster(&bytes, width_dots, *width_pct as u32, opts)?;
            Prep::Raster {
                raster,
                shrink: shrink.then(|| ShrinkSrc { bytes, opts }),
            }
        }
    })
}

fn image_raster(bytes: &[u8], width_dots: u32, pct: u32, opts: raster::Options) -> anyhow::Result<Raster> {
    let target = (width_dots * pct / 100).max(8);
    let r = raster::from_bytes(bytes, target, opts)?;
    anyhow::ensure!(
        (r.height as u32) <= MAX_IMAGE_ROWS,
        "image renders {} rows tall (max {}) — reduce width_pct or crop",
        r.height,
        MAX_IMAGE_ROWS
    );
    Ok(r)
}

/// Rasterize a QR symbol at `module` dots per module with a spec quiet zone on
/// all sides, so the height is exact and the console can preview the real code.
fn qr_raster(data: &str, module: u32, ecc: qrcode::EcLevel) -> anyhow::Result<Raster> {
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), ecc)
        .map_err(|e| anyhow::anyhow!("QR encode: {e}"))?;
    let n = code.width() as u32; // modules per side
    let side = (n + 2 * QR_QUIET_MODULES) * module;
    let mut c = Canvas::new(side, side);
    let colors = code.to_colors();
    for y in 0..n {
        for x in 0..n {
            if colors[(y * n + x) as usize] == qrcode::Color::Dark {
                let x0 = ((QR_QUIET_MODULES + x) * module) as i32;
                let y0 = ((QR_QUIET_MODULES + y) * module) as i32;
                c.fill_rect(x0, y0, module as i32, module as i32, true);
            }
        }
    }
    Ok(c.raster())
}

// ---- size-bounded fit -----------------------------------------------------------

/// What the fit pass resolved — returned so callers (and tests) can see the ledger.
pub struct Layout {
    /// Content height in dots (bounded: equals the budget when flex absorbed all).
    pub content_dots: u32,
    /// Trailing fill feed emitted before the cut (bounded mode only).
    pub fill_dots: u32,
}

/// Make the prepared blocks fit `budget` dots exactly-or-under:
/// shrink `shrink: true` images on overflow, grow flex feeds on underflow.
fn fit(preps: &mut [Prep], budget: u32) -> anyhow::Result<()> {
    // Overflow: shrink images (up to 3 refinement passes for rounding).
    for _ in 0..3 {
        let used: u32 = preps.iter().map(|p| p.height(true)).sum();
        if used <= budget {
            break;
        }
        let over = used - budget;
        let shrinkable: u32 = preps
            .iter()
            .filter_map(|p| match p {
                Prep::Raster { raster, shrink: Some(_) } => Some(raster.height as u32),
                _ => None,
            })
            .sum();
        if shrinkable == 0 {
            return Err(overflow_error(preps, used, budget));
        }
        anyhow::ensure!(
            shrinkable > over,
            "content overflows the strip by {:.1}mm and shrinkable images ({:.1}mm) can't absorb it",
            dots_to_mm(over),
            dots_to_mm(shrinkable)
        );
        let factor = (shrinkable - over) as f32 / shrinkable as f32;
        for p in preps.iter_mut() {
            if let Prep::Raster { raster, shrink: Some(src) } = p {
                // Height scales ~linearly with width; re-raster at the reduced width.
                let cur_w = raster.width_bytes as u32 * 8;
                let new_w = ((cur_w as f32 * factor) as u32).max(8);
                *raster = raster::from_bytes(&src.bytes, new_w, src.opts)?;
            }
        }
    }
    let used: u32 = preps.iter().map(|p| p.height(true)).sum();
    if used > budget {
        return Err(overflow_error(preps, used, budget));
    }

    // Underflow: distribute leftover into flex feeds by weight.
    let leftover = budget - used;
    let total_weight: u32 = preps
        .iter()
        .filter_map(|p| match p {
            Prep::Feed { flex, .. } if *flex > 0 => Some(*flex as u32),
            _ => None,
        })
        .sum();
    if total_weight > 0 && leftover > 0 {
        let mut given = 0u32;
        let mut last_flex: Option<&mut u32> = None;
        for p in preps.iter_mut() {
            if let Prep::Feed { dots, flex } = p {
                if *flex > 0 {
                    let share = leftover * *flex as u32 / total_weight;
                    *dots += share;
                    given += share;
                    last_flex = Some(dots);
                }
            }
        }
        // Rounding remainder goes to the last spring so the ledger sums exactly.
        if let Some(dots) = last_flex {
            *dots += leftover - given;
        }
    }
    Ok(())
}

fn overflow_error(preps: &[Prep], used: u32, budget: u32) -> anyhow::Error {
    let per_block: Vec<String> = preps
        .iter()
        .enumerate()
        .map(|(i, p)| format!("block {}: {:.1}mm", i + 1, dots_to_mm(p.height(true))))
        .collect();
    anyhow::anyhow!(
        "content is {:.1}mm but only {:.1}mm is printable — trim {:.1}mm ({})",
        dots_to_mm(used),
        dots_to_mm(budget),
        dots_to_mm(used - budget),
        per_block.join(", ")
    )
}

// ---- rendering ------------------------------------------------------------------

/// Render a custom document to ESC/POS bytes. Pure — no I/O, testable like
/// [`super::report::render`].
pub fn render_custom(doc: &PrintDoc, cfg: &PrinterConfig) -> anyhow::Result<Vec<u8>> {
    Ok(render_with_layout(doc, cfg)?.0)
}

/// As [`render_custom`] but also returns the resolved [`Layout`] ledger.
pub fn render_with_layout(doc: &PrintDoc, cfg: &PrinterConfig) -> anyhow::Result<(Vec<u8>, Layout)> {
    validate(doc)?;
    let w = cfg.width_dots;
    let bounded = doc.length_mm.is_some();

    let mut preps = doc
        .blocks
        .iter()
        .enumerate()
        .map(|(i, blk)| prepare(blk, w, cfg).map_err(|e| anyhow::anyhow!("block {}: {e}", i + 1)))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let mut layout = Layout { content_dots: 0, fill_dots: 0 };
    if let Some(mm) = doc.length_mm {
        let length_dots = mm_to_dots(mm);
        // The blade sits downstream of the head: the strip's top
        // `head_to_cutter_dots` can never carry print, and the last printed dot
        // must advance that far to clear the blade before the closing cut.
        let dead = cfg.head_to_cutter_dots;
        anyhow::ensure!(
            length_dots > dead,
            "strip length {:.1}mm is shorter than the {:.1}mm head-to-cutter dead zone",
            mm,
            dots_to_mm(dead)
        );
        fit(&mut preps, length_dots - dead)?;
        layout.content_dots = preps.iter().map(|p| p.height(true)).sum();
        // The cutter feeds `cut_advance_dots` on its own before the blade
        // drops, so the commanded fill stops that far short of the target.
        layout.fill_dots = length_dots
            .saturating_sub(cfg.cut_advance_dots)
            .saturating_sub(layout.content_dots);
    } else {
        layout.content_dots = preps.iter().map(|p| p.height(false)).sum();
    }

    let upi = cfg.feed_units_per_inch.max(1);
    let mut b = Builder::new().with_width(w);
    b.init();
    // Two-channel advance ledger: raster rows are physical dots; text/feed
    // commands advance in motion units. The closing fill is computed in units
    // against the exact command total, so per-command rounding never drifts
    // the cut-to-cut length.
    let mut raster_dots = 0u32;
    let mut cmd_units = 0u32;
    for p in &preps {
        let adv = emit(&mut b, p, bounded, upi);
        raster_dots += adv.raster_dots;
        cmd_units += adv.cmd_units;
    }
    if let Some(mm) = doc.length_mm {
        let feed_dots = mm_to_dots(mm).saturating_sub(cfg.cut_advance_dots);
        let target_units = to_units(feed_dots.saturating_sub(raster_dots), upi);
        feed_units_exact(&mut b, target_units.saturating_sub(cmd_units));
        b.partial_cut();
    } else {
        b.align(Align::Left).feed(2).cut();
    }

    let bytes = b.build();
    anyhow::ensure!(
        bytes.len() <= MAX_BYTES,
        "rendered output is {}KB (max {}KB)",
        bytes.len() / 1024,
        MAX_BYTES / 1024
    );
    Ok((bytes, layout))
}

/// Paper advanced by one emitted prep, split by unit system: raster/barcode
/// rows are physical 1/203" dots; ESC 3 / ESC J values are motion units.
struct Advance {
    raster_dots: u32,
    cmd_units: u32,
}

fn emit(b: &mut Builder, p: &Prep, bounded: bool, upi: u32) -> Advance {
    match p {
        Prep::Lines { lines, align, bold, underline, inverse, font, size_w, size_h, spacing } => {
            // Spacing in motion units can exceed ESC 3's u8 (e.g. 8× Font A at
            // 1/360"): set what fits, top up each line with an explicit feed.
            let s_units = to_units(*spacing, upi);
            let s0 = s_units.min(255);
            let extra = s_units - s0;
            b.align(*align)
                .font(*font)
                .bold(*bold)
                .underline(if *underline { 2 } else { 0 })
                .inverse(*inverse)
                .size(*size_w, *size_h)
                .line_spacing(Some(s0 as u8));
            for l in lines {
                b.line(l);
                feed_units_exact(b, extra);
            }
            b.bold(false)
                .underline(0)
                .inverse(false)
                .size(1, 1)
                .font(Font::A)
                .line_spacing(None);
            Advance { raster_dots: 0, cmd_units: lines.len() as u32 * s_units }
        }
        Prep::Raster { raster, .. } => {
            b.align(Align::Center)
                .raster_banded(&raster.bits, raster.width_bytes, raster.height, 64);
            Advance { raster_dots: raster.height as u32, cmd_units: 0 }
        }
        Prep::Barcode { data, sym, height, width } => {
            b.align(Align::Center).barcode_style(*height, *width);
            if bounded {
                // barcode_style enables HRI text; its height is firmware-defined,
                // so bounded strips drop it to keep the ledger exact.
                b.raw(&[0x1D, b'H', 0]);
            }
            b.barcode(*sym, data);
            // GS h height is specced in dots; verify against calipers if a
            // bounded strip with a barcode comes out long/short.
            Advance { raster_dots: *height as u32, cmd_units: 0 }
        }
        Prep::Feed { dots, .. } => {
            let units = to_units(*dots, upi);
            feed_units_exact(b, units);
            Advance { raster_dots: 0, cmd_units: units }
        }
    }
}

/// Feed an exact number of motion units (ESC J caps at 255 per call).
fn feed_units_exact(b: &mut Builder, mut units: u32) {
    while units > 0 {
        let step = units.min(255) as u8;
        b.feed_dots(step);
        units -= step as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PrinterConfig {
        PrinterConfig::default()
    }

    fn text(s: &str) -> Block {
        Block::Text {
            text: s.into(),
            align: "left".into(),
            bold: false,
            underline: false,
            inverse: false,
            font: "a".into(),
            size_w: 1,
            size_h: 1,
        }
    }

    /// A tiny in-memory PNG for image-block tests.
    fn tiny_png() -> String {
        let img = image::RgbImage::from_fn(32, 32, |x, y| {
            image::Rgb(if (x / 4 + y / 4) % 2 == 0 { [0, 0, 0] } else { [255, 255, 255] })
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
    }

    #[test]
    fn continuous_doc_with_every_block_renders() {
        let doc = PrintDoc {
            blocks: vec![
                Block::Text {
                    text: "WET COURT".into(),
                    align: "center".into(),
                    bold: true,
                    underline: false,
                    inverse: true,
                    font: "a".into(),
                    size_w: 2,
                    size_h: 2,
                },
                Block::Rule { heavy: true },
                text("hello \u{201C}world\u{201D} with smart quotes"),
                Block::Feed { lines: 2, flex: 0 },
                Block::Qr { data: "https://wetcourt.lol".into(), module: 6, ecc: "m".into() },
                Block::Barcode { data: "CASE-0042".into(), symbology: "code128".into(), height: 80, width: 3 },
                Block::Image { data_b64: tiny_png(), dither: Some("fs".into()), width_pct: 50, shrink: false, gamma: None, brightness: None, contrast: None },
            ],
            length_mm: None,
        };
        let bytes = render_custom(&doc, &cfg()).unwrap();
        assert!(bytes.len() > 200, "suspiciously small: {}", bytes.len());
        // Ends with feed(2) + cut (GS V 66 n).
        let tail = &bytes[bytes.len() - 4..];
        assert_eq!(&tail[..3], &[0x1D, b'V', 66]);
    }

    /// Walk an ESC/POS stream and total the physical paper advance in real
    /// 203-dpi dots, honoring the printer's motion unit for ESC 3 / ESC J.
    /// Covers exactly the commands the bounded renderer emits.
    fn physical_advance_dots(bytes: &[u8], upi: u32) -> f64 {
        let units_to_dots = |n: u32| n as f64 * 203.0 / upi as f64;
        let mut i = 0usize;
        let mut spacing_units = 0u32; // always set via ESC 3 before any LF
        let mut total = 0f64;
        while i < bytes.len() {
            match bytes[i] {
                0x0A => {
                    total += units_to_dots(spacing_units);
                    i += 1;
                }
                0x1B => match bytes[i + 1] {
                    b'@' | b'2' => i += 2,
                    b'3' => {
                        spacing_units = bytes[i + 2] as u32;
                        i += 3;
                    }
                    b'J' => {
                        total += units_to_dots(bytes[i + 2] as u32);
                        i += 3;
                    }
                    b'a' | b'M' | b'E' | b'-' => i += 3,
                    other => panic!("unexpected ESC {other:#x}"),
                },
                0x1D => match bytes[i + 1] {
                    b'B' | b'!' | b'h' | b'w' | b'H' => i += 3,
                    b'V' => {
                        assert_eq!(bytes[i + 2], 1, "bounded strips must end in a bare partial cut");
                        i += 3;
                    }
                    b'k' => {
                        let len = bytes[i + 3] as usize;
                        i += 4 + len; // advance accounted via GS h, tracked by caller
                    }
                    b'v' => {
                        let wb = bytes[i + 4] as usize | (bytes[i + 5] as usize) << 8;
                        let h = bytes[i + 6] as usize | (bytes[i + 7] as usize) << 8;
                        total += h as f64;
                        i += 8 + wb * h;
                    }
                    other => panic!("unexpected GS {other:#x}"),
                },
                _ => i += 1, // text byte (buffered; advance happens at LF)
            }
        }
        total
    }

    #[test]
    fn bounded_strip_advances_exactly_its_length_in_motion_units() {
        // Mixed doc: magnified text, rule, fixed + flex feeds, QR raster.
        let doc = PrintDoc {
            blocks: vec![
                Block::Feed { lines: 1, flex: 1 },
                Block::Text {
                    text: "WET COURT".into(),
                    align: "center".into(),
                    bold: true,
                    underline: false,
                    inverse: false,
                    font: "a".into(),
                    size_w: 2,
                    size_h: 2,
                    },
                Block::Rule { heavy: false },
                Block::Feed { lines: 2, flex: 0 },
                Block::Qr { data: "https://wetcourt.lol".into(), module: 3, ecc: "m".into() },
                Block::Feed { lines: 1, flex: 1 },
            ],
            length_mm: Some(80.0),
        };
        let c = cfg();
        let bytes = render_custom(&doc, &c).unwrap();
        let advance = physical_advance_dots(&bytes, c.feed_units_per_inch);
        let target = mm_to_dots(80.0) as f64;
        // Per-command unit rounding is the only slack allowed (< 1 dot each).
        assert!(
            (advance - target).abs() < 3.0,
            "strip advances {advance:.1} dots, want {target}"
        );
    }

    #[test]
    fn cut_advance_is_subtracted_from_the_closing_fill() {
        // A cutter that self-feeds before the blade drops (dev LAN printer:
        // ~2.65mm = 21 dots) must shorten the commanded feed by that much so
        // the physical cut-to-cut still lands on the target.
        let doc = PrintDoc { blocks: vec![text("one line")], length_mm: Some(50.0) };
        let mut c = cfg();
        c.cut_advance_dots = 21;
        let (bytes, layout) = render_with_layout(&doc, &c).unwrap();
        let advance = physical_advance_dots(&bytes, c.feed_units_per_inch);
        let target = (mm_to_dots(50.0) - 21) as f64;
        assert!(
            (advance - target).abs() < 3.0,
            "strip advances {advance:.1} dots, want {target}"
        );
        assert_eq!(layout.fill_dots, mm_to_dots(50.0) - 21 - layout.content_dots);
    }

    #[test]
    fn oversize_line_spacing_tops_up_with_feeds() {
        // 8×-tall Font A: spacing = 198 dots = 351 units at 1/360" — past
        // ESC 3's u8 range, so each line must be topped up with ESC J.
        let doc = PrintDoc {
            blocks: vec![Block::Text {
                text: "BIG".into(),
                align: "left".into(),
                bold: false,
                underline: false,
                inverse: false,
                font: "a".into(),
                size_w: 1,
                size_h: 8,
            }],
            length_mm: Some(60.0),
        };
        let c = cfg();
        let bytes = render_custom(&doc, &c).unwrap();
        let advance = physical_advance_dots(&bytes, c.feed_units_per_inch);
        let target = mm_to_dots(60.0) as f64;
        assert!((advance - target).abs() < 3.0, "advance {advance:.1}, want {target}");
    }

    #[test]
    fn bounded_ledger_sums_to_exact_length() {
        let doc = PrintDoc {
            blocks: vec![
                Block::Feed { lines: 1, flex: 1 },
                text("centered on a plaque"),
                Block::Feed { lines: 1, flex: 1 },
            ],
            length_mm: Some(50.0),
        };
        let c = cfg();
        let (bytes, layout) = render_with_layout(&doc, &c).unwrap();
        let length_dots = mm_to_dots(50.0);
        assert_eq!(layout.content_dots + layout.fill_dots, length_dots);
        assert_eq!(layout.content_dots, length_dots - c.head_to_cutter_dots);
        assert_eq!(layout.fill_dots, c.head_to_cutter_dots);
        // Ends with a bare partial cut, not feed_and_cut.
        assert_eq!(&bytes[bytes.len() - 3..], &[0x1D, b'V', 1]);
    }

    #[test]
    fn bounded_overflow_names_block_heights() {
        let doc = PrintDoc {
            blocks: vec![text(&"a very long line of testimony ".repeat(30))],
            length_mm: Some(25.0),
        };
        let err = render_custom(&doc, &cfg()).unwrap_err().to_string();
        assert!(err.contains("printable"), "unexpected error: {err}");
        assert!(err.contains("block 1"), "unexpected error: {err}");
    }

    #[test]
    fn bounded_shrinks_marked_images_to_fit() {
        let doc = PrintDoc {
            blocks: vec![
                text("photo strip"),
                Block::Image { data_b64: tiny_png(), dither: Some("none".into()), width_pct: 100, shrink: true, gamma: None, brightness: None, contrast: None },
            ],
            length_mm: Some(50.0),
        };
        let c = cfg();
        // Full-width square image = 576 rows, way over a 50mm budget (~290 dots).
        let (_, layout) = render_with_layout(&doc, &c).unwrap();
        assert!(layout.content_dots <= mm_to_dots(50.0) - c.head_to_cutter_dots);
    }

    #[test]
    fn unshrinkable_overflow_errors() {
        let doc = PrintDoc {
            blocks: vec![
                Block::Image { data_b64: tiny_png(), dither: Some("none".into()), width_pct: 100, shrink: false, gamma: None, brightness: None, contrast: None },
            ],
            length_mm: Some(50.0),
        };
        assert!(render_custom(&doc, &cfg()).is_err());
    }

    #[test]
    fn validation_rejects_bad_blocks() {
        let bad = |blocks: Vec<Block>, length_mm: Option<f32>| {
            validate(&PrintDoc { blocks, length_mm }).is_err()
        };
        assert!(bad(vec![], None));
        assert!(bad(vec![Block::Qr { data: String::new(), module: 6, ecc: "m".into() }], None));
        assert!(bad(vec![Block::Barcode { data: "12ab".into(), symbology: "ean13".into(), height: 80, width: 3 }], None));
        assert!(bad(vec![text("x")], Some(5.0)));
        let mut t = text("x");
        if let Block::Text { size_w, .. } = &mut t {
            *size_w = 9;
        }
        assert!(bad(vec![t], None));
    }

    #[test]
    fn strip_shorter_than_dead_zone_errors() {
        let doc = PrintDoc { blocks: vec![text("x")], length_mm: Some(20.0) };
        let mut c = cfg();
        c.head_to_cutter_dots = 200; // pretend a long head-blade distance
        let err = render_custom(&doc, &c).unwrap_err().to_string();
        assert!(err.contains("dead zone"), "unexpected error: {err}");
    }

    #[test]
    fn qr_raster_is_square_and_quiet_zoned() {
        let r = qr_raster("https://wetcourt.lol", 4, qrcode::EcLevel::M).unwrap();
        // Width and height match (square symbol incl. quiet zone).
        assert_eq!(r.width_bytes as u32 * 8 >= r.height as u32, true);
        assert!(r.height > 100, "QR too small: {}", r.height);
        // Quiet zone: the first rows must be blank.
        let quiet_rows = (QR_QUIET_MODULES * 4) as usize * r.width_bytes as usize;
        assert!(r.bits[..quiet_rows].iter().all(|&b| b == 0));
    }
}
