//! Image → 1-bit raster conversion for thermal printing.
//!
//! Pipeline: load → grayscale → scale to printer width → tone-adjust
//! (gamma / contrast / brightness) → dither → pack to MSB-first rows.
//! The packed output feeds [`escpos::Builder::raster`].

#[cfg(feature = "image")]
use anyhow::{Context, Result};
#[cfg(feature = "image")]
use image::imageops::FilterType;
#[cfg(feature = "image")]
use std::path::Path;

/// Dithering strategy — the biggest lever on the printed "look".
#[derive(Clone, Copy, Debug)]
pub enum Dither {
    /// Hard threshold at mid-gray. Graphic, high-contrast, no texture.
    None,
    /// Floyd–Steinberg: fine photographic stipple (the default).
    FloydSteinberg,
    /// Atkinson: sparser, punchier, more white space — classic Mac dither.
    Atkinson,
    /// Ordered 8×8 Bayer: regular cross-hatch / halftone grid, retro feel.
    Bayer,
}

/// Tone + dither controls applied before packing.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Gamma. `<1.0` brightens mid-tones, `>1.0` darkens. (Thermal heads run
    /// dark, so ~0.7–0.8 is often a good starting point.)
    pub gamma: f32,
    /// Contrast multiplier around mid-gray. `1.0` = none, `>1.0` = punchier.
    pub contrast: f32,
    /// Brightness offset in luma units, added last. `+` = lighter.
    pub brightness: f32,
    pub dither: Dither,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            contrast: 1.0,
            brightness: 0.0,
            dither: Dither::FloydSteinberg,
        }
    }
}

pub struct Raster {
    /// Packed bits, row-major, MSB-first. Bit set = black dot.
    pub bits: Vec<u8>,
    pub width_bytes: u16,
    pub height: u16,
}

/// 8×8 Bayer threshold matrix (values 0..63), used for ordered dithering.
#[cfg(feature = "image")]
#[rustfmt::skip]
const BAYER8: [[u8; 8]; 8] = [
    [ 0, 32,  8, 40,  2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44,  4, 36, 14, 46,  6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [ 3, 35, 11, 43,  1, 33,  9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47,  7, 39, 13, 45,  5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Load an image file and convert it for printing at `target_width` dots.
#[cfg(feature = "image")]
pub fn from_path<P: AsRef<Path>>(path: P, target_width: u32, opts: Options) -> Result<Raster> {
    let img = image::open(path.as_ref())
        .with_context(|| format!("opening image {}", path.as_ref().display()))?;
    Ok(from_image(&img, target_width, opts))
}

/// Same as [`from_path`] but from encoded image bytes in memory (e.g. a JPEG
/// frame captured from the vision service).
#[cfg(feature = "image")]
pub fn from_bytes(bytes: &[u8], target_width: u32, opts: Options) -> Result<Raster> {
    let img = image::load_from_memory(bytes).context("decoding captured image bytes")?;
    Ok(from_image(&img, target_width, opts))
}

/// Same as [`from_path`] but from an already-decoded image.
#[cfg(feature = "image")]
pub fn from_image(img: &image::DynamicImage, target_width: u32, opts: Options) -> Raster {
    // Scale to fit the printable width, preserving aspect ratio.
    let (w0, h0) = (img.width().max(1), img.height().max(1));
    let target_width = target_width.max(8);
    let target_height = ((target_width as u64 * h0 as u64) / w0 as u64).max(1) as u32;
    let scaled = img.resize_exact(target_width, target_height, FilterType::Lanczos3);

    let gray = scaled.to_luma8();
    let (w, h) = (gray.width(), gray.height());

    // Tone-adjust into an f32 working buffer: contrast → brightness → gamma.
    let inv_gamma = if opts.gamma > 0.0 { opts.gamma } else { 1.0 };
    let mut buf: Vec<f32> = gray
        .pixels()
        .map(|p| {
            let mut v = p[0] as f32;
            v = (v - 128.0) * opts.contrast + 128.0;
            v += opts.brightness;
            v = v.clamp(0.0, 255.0);
            // Gamma in normalized space.
            255.0 * (v / 255.0).powf(inv_gamma)
        })
        .collect();

    let mut out_black = vec![false; (w * h) as usize];
    let idx = |x: i64, y: i64| (y as u32 * w + x as u32) as usize;

    match opts.dither {
        Dither::None => {
            for i in 0..buf.len() {
                out_black[i] = buf[i] < 128.0;
            }
        }
        Dither::Bayer => {
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    // Map matrix 0..63 → threshold 4..252 (centered).
                    let t = (BAYER8[(y % 8) as usize][(x % 8) as usize] as f32 + 0.5) * 4.0;
                    out_black[i] = buf[i] < t;
                }
            }
        }
        Dither::FloydSteinberg | Dither::Atkinson => {
            let atkinson = matches!(opts.dither, Dither::Atkinson);
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    let i = idx(x, y);
                    let old = buf[i];
                    let black = old < 128.0;
                    out_black[i] = black;
                    let new = if black { 0.0 } else { 255.0 };
                    let err = old - new;
                    let mut diffuse = |dx: i64, dy: i64, f: f32| {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx >= 0 && nx < w as i64 && ny >= 0 && ny < h as i64 {
                            buf[idx(nx, ny)] += err * f;
                        }
                    };
                    if atkinson {
                        // Atkinson diffuses 6/8 of the error (1/8 each) — the
                        // "lost" 2/8 is what gives it its airy, contrasty look.
                        let f = 1.0 / 8.0;
                        diffuse(1, 0, f);
                        diffuse(2, 0, f);
                        diffuse(-1, 1, f);
                        diffuse(0, 1, f);
                        diffuse(1, 1, f);
                        diffuse(0, 2, f);
                    } else {
                        diffuse(1, 0, 7.0 / 16.0);
                        diffuse(-1, 1, 3.0 / 16.0);
                        diffuse(0, 1, 5.0 / 16.0);
                        diffuse(1, 1, 1.0 / 16.0);
                    }
                }
            }
        }
    }

    // Pack to MSB-first rows.
    let width_bytes = ((w + 7) / 8) as u16;
    let mut bits = vec![0u8; width_bytes as usize * h as usize];
    for y in 0..h {
        for x in 0..w {
            if out_black[(y * w + x) as usize] {
                let byte = (y * width_bytes as u32 + x / 8) as usize;
                bits[byte] |= 0x80 >> (x % 8);
            }
        }
    }

    Raster {
        bits,
        width_bytes,
        height: h as u16,
    }
}
