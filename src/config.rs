//! Small file-based config so the art project gets a consistent look without
//! passing flags every time. Plain `key = value` lines (`#` comments), no deps.
//!
//! Looked up as `tp.conf` in the current directory. Missing keys fall back to
//! defaults; missing file → all defaults. `tp config` writes a documented sample.

use crate::escpos::WIDTH_DOTS_80MM;
use crate::raster::{Dither, Options};
use anyhow::{anyhow, Result};
use std::path::Path;

/// Default config filename, resolved relative to the working directory.
pub const DEFAULT_PATH: &str = "tp.conf";

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Printable width in dots (576 for most 80mm, 512 for some clones).
    pub width_dots: u32,
    /// Blank dots fed before a raster, so any cold-start artifact lands on margin.
    pub warmup_feed_dots: u8,
    /// Rows per raster band when printing images (smaller = safer on cheap heads).
    pub image_band_rows: u16,
    /// Default tone/dither for the `image` command (CLI flags override these).
    pub image: Options,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width_dots: WIDTH_DOTS_80MM,
            warmup_feed_dots: 24,
            image_band_rows: 128,
            // Thermal heads run dark, so a brightening gamma is a sane default.
            image: Options {
                gamma: 0.7,
                contrast: 1.0,
                brightness: 0.0,
                dither: Dither::FloydSteinberg,
            },
        }
    }
}

impl Config {
    /// Load from `path`, falling back to defaults if the file is absent.
    /// A malformed file is a hard error (better to tell the user than silently ignore).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        let mut c = Self::default();
        for (n, raw) in s.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| anyhow!("{}: line {}: expected `key = value`", DEFAULT_PATH, n + 1))?;
            let (k, v) = (k.trim(), v.trim());
            let num = |v: &str| v.parse::<f32>().map_err(|_| anyhow!("line {}: `{v}` is not a number", n + 1));
            match k {
                "width_dots" => c.width_dots = num(v)? as u32,
                "warmup_feed_dots" => c.warmup_feed_dots = num(v)? as u8,
                "image_band_rows" => c.image_band_rows = num(v)? as u16,
                "image_gamma" => c.image.gamma = num(v)?,
                "image_contrast" => c.image.contrast = num(v)?,
                "image_brightness" => c.image.brightness = num(v)?,
                "image_dither" => c.image.dither = parse_dither(v)?,
                other => return Err(anyhow!("line {}: unknown key `{other}`", n + 1)),
            }
        }
        Ok(c)
    }

    /// A documented sample config, for `tp config` to write out.
    pub const SAMPLE: &'static str = "\
# thermal printer config (tp.conf). Plain key = value, '#' starts a comment.
# CLI flags on `tp image` override the image_* values below.

width_dots        = 576    # 576 for most 80mm heads, 512 for some clones
warmup_feed_dots  = 24     # blank feed before an image (hides cold-start band)
image_band_rows   = 128    # raster band height; lower if images smear/banding

image_gamma       = 0.7    # <1 brightens mid-tones, >1 darkens
image_contrast    = 1.0    # 1 = none, >1 = punchier
image_brightness  = 0.0    # luma offset, + = lighter
image_dither      = fs     # none | fs | atkinson | bayer
";
}

pub fn parse_dither(v: &str) -> Result<Dither> {
    Ok(match v {
        "none" | "threshold" => Dither::None,
        "fs" | "floyd" => Dither::FloydSteinberg,
        "atkinson" => Dither::Atkinson,
        "bayer" => Dither::Bayer,
        other => return Err(anyhow!("unknown dither `{other}` (none|fs|atkinson|bayer)")),
    })
}

/// Format a dither back to its config token (for printing the active config).
pub fn dither_name(d: Dither) -> &'static str {
    match d {
        Dither::None => "none",
        Dither::FloydSteinberg => "fs",
        Dither::Atkinson => "atkinson",
        Dither::Bayer => "bayer",
    }
}
