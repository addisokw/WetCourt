//! `tp` — a playground CLI for exploring the thermal printer.
//!
//! Usage:
//!   tp info                 Show what we connected to (no printing)
//!   tp hello                Minimal "it's alive" print
//!   tp text                 Typography demo (fonts, sizes, styles)
//!   tp image <path> [--threshold]   Print an image (dithered by default)
//!   tp qr <data>            Print a QR code
//!   tp barcode <data>       Print a CODE128 barcode
//!   tp paper                Feed/cut/spacing tricks demo
//!   tp all                  Run the full capability tour
//!   tp dump <out.bin> <cmd> Write the bytes for <cmd> to a file (no printer)
//!
//! Global flags: --width <dots>  (default 576 for 80mm)
//!               --net <host[:port]>  LAN printer instead of USB (port: 9100)
//!               --usb           force USB even when tp.conf sets net_addr

use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::path::Path;
use thermal_printer::{
    canvas::Canvas,
    config::{self, parse_dither},
    escpos::Builder,
    raster, text, Align, Barcode, Config, Font, Printer, QrEcc,
};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Load defaults from tp.conf (in the working dir), then let flags override.
    let cfg_path = Path::new(config::DEFAULT_PATH);
    let cfg = Config::load(cfg_path)?;

    // Pull out an optional `--width N` from anywhere in the args (overrides config).
    let mut width = cfg.width_dots;
    if let Some(i) = args.iter().position(|a| a == "--width") {
        width = args
            .get(i + 1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("--width needs a number"))?;
        args.drain(i..=i + 1);
    }

    // Transport: tp.conf's `net_addr` is the default; `--net host[:port]`
    // overrides it, `--usb` forces the cable even when the conf says LAN.
    let mut net: Option<String> = cfg.net_addr.clone();
    if let Some(i) = args.iter().position(|a| a == "--net") {
        net = Some(
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| anyhow!("--net needs a host[:port]"))?,
        );
        args.drain(i..=i + 1);
    }
    if let Some(i) = args.iter().position(|a| a == "--usb") {
        net = None;
        args.remove(i);
    }

    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let rest = &args[args.len().min(1)..];

    // These commands don't need hardware.
    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            print_help();
            return Ok(());
        }
        "dump" => return dump(rest, width),
        "config" => return show_config(cfg_path, &cfg),
        _ => {}
    }

    let printer = match &net {
        Some(addr) => Printer::connect_net(addr)?,
        None => Printer::connect()?,
    }
    .with_width(width);

    match cmd.as_str() {
        "info" => {
            println!("connected: {}", printer.transport().describe());
            println!("width: {} dots", printer.width_dots());
            if printer.transport().has_status_channel() {
                println!("status channel: available");
            } else {
                println!("status channel: none (no bulk-IN endpoint)");
            }
        }
        "status" => {
            let s = printer.transport().query_status()?;
            let yn = |b: Option<bool>| match b {
                Some(true) => "yes",
                Some(false) => "no",
                None => "?",
            };
            println!("ready:               {}", if s.is_ready() { "yes" } else { "NO" });
            println!("online:              {}", yn(s.online));
            println!("cover open:          {}", yn(s.cover_open));
            println!("paper out:           {}", yn(s.paper_out));
            println!("paper near-end:      {}", yn(s.paper_near_end));
            println!("paper feed button:   {}", yn(s.paper_feeding));
            println!("cutter error:        {}", yn(s.cutter_error));
            println!("recoverable error:   {}", yn(s.recoverable_error));
            println!("unrecoverable error: {}", yn(s.unrecoverable_error));
            let raw: Vec<String> = s
                .raw
                .iter()
                .map(|b| b.map(|x| format!("{x:02x}")).unwrap_or_else(|| "--".into()))
                .collect();
            println!("raw DLE EOT 1..4:    {}", raw.join(" "));
        }
        "hello" => printer.print(|b| {
            scene_hello(b);
        })?,
        "text" => printer.print(|b| {
            scene_text(b);
        })?,
        "qr" => {
            let data = rest.first().map(String::as_str).unwrap_or("https://anthropic.com");
            printer.print(|b| scene_qr(b, data))?;
        }
        "barcode" => {
            let data = rest.first().map(String::as_str).unwrap_or("ART-0001");
            printer.print(|b| scene_barcode(b, data))?;
        }
        "paper" => printer.print(|b| {
            scene_paper(b);
        })?,
        "feedcut" => printer.print(|b| {
            b.feed(2).cut();
        })?,
        "print" | "cat" => {
            // Read from a file arg (that isn't a flag) or stdin.
            let file = rest.iter().find(|a| !a.starts_with('-'));
            let input = match file {
                Some(path) => std::fs::read_to_string(path)
                    .map_err(|e| anyhow!("reading {path}: {e}"))?,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let plain = rest.iter().any(|a| a == "--raw" || a == "--plain");
            let do_cut = rest.iter().any(|a| a == "--cut");
            let bytes = print_text(&printer, &input, plain, do_cut)?;
            println!("printed {bytes} bytes ({})", if plain { "plain" } else { "markup" });
        }
        "watch" => {
            let path = rest
                .iter()
                .find(|a| !a.starts_with('-'))
                .ok_or_else(|| anyhow!("usage: tp watch <file> [--raw] [--cut] [--interval MS]"))?;
            let plain = rest.iter().any(|a| a == "--raw" || a == "--plain");
            let do_cut = rest.iter().any(|a| a == "--cut");
            let interval = flag_val(rest, "--interval").unwrap_or(500) as u64;
            watch_file(&printer, path, plain, do_cut, interval)?;
        }
        #[cfg(feature = "image")]
        "image" => {
            let path = rest
                .first()
                .ok_or_else(|| anyhow!("usage: tp image <path> [tone/dither flags]"))?;
            let opts = parse_image_opts(rest, cfg.image)?;
            let r = raster::from_path(path, printer.width_dots(), opts)?;
            let band = cfg.image_band_rows;
            let warmup = cfg.warmup_feed_dots;
            // 1) Send the image + a terminated caption line (LF-terminated, so
            //    nothing is left un-printed in the line buffer).
            printer.print(|b| {
                b.init().align(Align::Center);
                if warmup > 0 {
                    b.feed_dots(warmup);
                }
                b.raster_banded(&r.bits, r.width_bytes, r.height, band);
                b.feed(1).align(Align::Center).line(path);
            })?;
            // 2) Let the raster physically finish printing while the handle stays open.
            let dots = r.height as u64 + warmup as u64 + 200;
            printer.transport().drain(dots * 1000 / 250 + 800);
            // 3) Send the cut as its own small write — the printer is now idle, so
            //    this takes the same reliable path as `tp feedcut`. Bundling the cut
            //    into the giant raster transfer is what was dropping it.
            printer.print(|b| {
                b.feed(2).cut();
            })?;
            printer.transport().drain(900);
            println!(
                "printed {} ({}x{} dots, dither={:?} gamma={} contrast={} brightness={})",
                path,
                r.width_bytes as u32 * 8,
                r.height,
                opts.dither,
                opts.gamma,
                opts.contrast,
                opts.brightness,
            );
        }
        "gen" => {
            let pattern = rest.first().map(String::as_str).unwrap_or("rings");
            let height = flag_val(rest, "--height").unwrap_or(printer.width_dots());
            let seed = flag_val(rest, "--seed").unwrap_or(1) as u64;
            let canvas = gen_pattern(pattern, printer.width_dots(), height, seed)?;
            let r = canvas.raster();
            printer.print(|b| {
                b.init().align(Align::Center);
                b.raster_banded(&r.bits, r.width_bytes, r.height, cfg.image_band_rows);
                b.feed(1).line(pattern);
            })?;
            printer.transport().drain(r.height as u64 * 1000 / 250 + 800);
            printer.print(|b| {
                b.feed(2).cut();
            })?;
            printer.transport().drain(1000);
            println!("generated `{pattern}` ({}x{} dots, seed {seed})", canvas.width, canvas.height);
        }
        "all" => printer.print(|b| {
            scene_hello(b);
            scene_text(b);
            scene_barcode(b, "ART-0001");
            scene_qr(b, "https://anthropic.com");
            scene_paper(b);
        })?,
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            std::process::exit(2);
        }
    }
    Ok(())
}

// ---- scenes (reusable so `all` can chain them) -----------------------------

fn scene_hello(b: &mut Builder) {
    b.init()
        .align(Align::Center)
        .size(2, 2)
        .bold(true)
        .line("HELLO")
        .bold(false)
        .size(1, 1)
        .line("thermal printer is alive")
        .feed(1)
        .align(Align::Left);
}

fn scene_text(b: &mut Builder) {
    b.init().align(Align::Left);
    b.size(2, 1).line("Typography").size(1, 1).feed(1);

    b.bold(true).line("bold").bold(false);
    b.underline(1).line("underline").underline(0);
    b.inverse(true).line(" inverse ").inverse(false);
    b.upside_down(true).line("upside down").upside_down(false);
    b.feed(1);

    b.line("Font A (48 col):");
    b.font(Font::A).line("0123456789 ABCDEFGHIJKLMNOP");
    b.line("Font B (64 col):");
    b.font(Font::B).line("0123456789 ABCDEFGHIJKLMNOPQRSTUVWX");
    b.font(Font::A).feed(1);

    b.line("Sizes:");
    for s in 1..=4u8 {
        b.size(s, s).line(&format!("{s}x"));
    }
    b.size(1, 1);

    b.feed(1)
        .align(Align::Center)
        .line("-- center --")
        .align(Align::Right)
        .line("-- right --")
        .align(Align::Left)
        .feed(1);
}

fn scene_barcode(b: &mut Builder, data: &str) {
    b.init()
        .align(Align::Center)
        .line("CODE128")
        .barcode_style(80, 3)
        .barcode(Barcode::Code128, data)
        .feed(2)
        .align(Align::Left);
}

fn scene_qr(b: &mut Builder, data: &str) {
    b.init()
        .align(Align::Center)
        .line("QR")
        .qr(data, 7, QrEcc::M)
        .feed(1)
        .line(data)
        .feed(2)
        .align(Align::Left);
}

fn scene_paper(b: &mut Builder) {
    b.init().line("Line spacing sweep:");
    for sp in [20u8, 40, 60, 80] {
        b.line_spacing(Some(sp)).line(&format!("spacing = {sp} dots"));
    }
    b.line_spacing(None).feed(1);

    b.line("Fine feed (dots), then a clean cut below:")
        .feed_dots(60)
        .line("v v v")
        // cut() feeds past the blade first, so this line clears it cleanly.
        .cut();
}

// ---- image flag parsing ----------------------------------------------------

/// Parse tone/dither flags for the `image` command, starting from `base`
/// (the config defaults) and overriding per flag:
///   --gamma F  --contrast F  --brightness F
///   --dither <none|fs|atkinson|bayer>   (--threshold = alias for none)
#[cfg(feature = "image")]
fn parse_image_opts(rest: &[String], base: raster::Options) -> Result<raster::Options> {
    let mut o = base;
    let val = |i: usize| -> Result<f32> {
        rest.get(i + 1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("{} needs a number", rest[i]))
    };
    let mut i = 1; // rest[0] is the path
    while i < rest.len() {
        match rest[i].as_str() {
            "--gamma" => {
                o.gamma = val(i)?;
                i += 1;
            }
            "--contrast" => {
                o.contrast = val(i)?;
                i += 1;
            }
            "--brightness" => {
                o.brightness = val(i)?;
                i += 1;
            }
            "--threshold" => o.dither = parse_dither("none")?,
            "--dither" => {
                o.dither = parse_dither(
                    rest.get(i + 1)
                        .map(String::as_str)
                        .ok_or_else(|| anyhow!("--dither needs none|fs|atkinson|bayer"))?,
                )?;
                i += 1;
            }
            other => return Err(anyhow!("unknown image flag: {other}")),
        }
        i += 1;
    }
    Ok(o)
}

/// Parse `--flag N` into a u32 if present.
fn flag_val(rest: &[String], flag: &str) -> Option<u32> {
    let i = rest.iter().position(|a| a == flag)?;
    rest.get(i + 1).and_then(|s| s.parse().ok())
}

// ---- generative patterns ---------------------------------------------------

/// Tiny deterministic PRNG (xorshift64*) so patterns are reproducible per seed.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n as u64) as u32
    }
}

/// Render a named generative pattern into a fresh canvas.
fn gen_pattern(name: &str, w: u32, h: u32, seed: u64) -> Result<Canvas> {
    let mut c = Canvas::new(w, h);
    let (wf, hf) = (w as f32, h as f32);
    match name {
        // Concentric rings from the center.
        "rings" => {
            let (cx, cy) = (wf / 2.0, hf / 2.0);
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                    c.set(x, y, (d / 12.0).floor() as i32 % 2 == 0);
                }
            }
        }
        // Interference of two circular wave sources.
        "waves" => {
            let s1 = (wf * 0.30, hf * 0.35);
            let s2 = (wf * 0.72, hf * 0.62);
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let d1 = ((x as f32 - s1.0).powi(2) + (y as f32 - s1.1).powi(2)).sqrt();
                    let d2 = ((x as f32 - s2.0).powi(2) + (y as f32 - s2.1).powi(2)).sqrt();
                    let v = (d1 / 9.0).sin() + (d2 / 9.0).sin();
                    c.set(x, y, v > 0.0);
                }
            }
        }
        // Random Truchet tiles: each cell gets one of two diagonal arcs.
        "truchet" => {
            let mut rng = Rng::new(seed);
            let tile = 24i32;
            let mut gy = 0;
            while gy < h as i32 {
                let mut gx = 0;
                while gx < w as i32 {
                    let flip = rng.below(2) == 1;
                    for t in 0..tile {
                        // Two quarter-arcs approximated by diagonals across the tile.
                        if flip {
                            c.set(gx + t, gy + t, true);
                        } else {
                            c.set(gx + t, gy + tile - 1 - t, true);
                        }
                    }
                    gx += tile;
                }
                gy += tile;
            }
        }
        // Escape-time Mandelbrot, thresholded to 1-bit.
        "mandelbrot" => {
            for py in 0..h as i32 {
                for px in 0..w as i32 {
                    let x0 = (px as f32 / wf) * 3.0 - 2.1;
                    let y0 = (py as f32 / hf) * 2.6 - 1.3;
                    let (mut x, mut y, mut i) = (0.0f32, 0.0f32, 0u32);
                    while x * x + y * y <= 4.0 && i < 64 {
                        let xt = x * x - y * y + x0;
                        y = 2.0 * x * y + y0;
                        x = xt;
                        i += 1;
                    }
                    // Banded escape count → stippled boundary.
                    c.set(px, py, i % 2 == 0 && i < 64);
                }
            }
        }
        "checker" => {
            let n = 16i32;
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    c.set(x, y, ((x / n) + (y / n)) % 2 == 0);
                }
            }
        }
        // White noise at a chosen density.
        "noise" => {
            let mut rng = Rng::new(seed);
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    c.set(x, y, rng.below(100) < 45);
                }
            }
        }
        other => {
            return Err(anyhow!(
                "unknown pattern `{other}` (rings|waves|truchet|mandelbrot|checker|noise)"
            ));
        }
    }
    Ok(c)
}

/// Rough time for the printer to physically emit a built command stream, so we
/// can hold the USB handle open until it finishes. Counts line feeds (~one text
/// row each) at ~250 dots/s, plus a safety base.
fn estimate_drain_ms(b: &Builder) -> u64 {
    let lines = b.bytes().iter().filter(|&&x| x == 0x0A).count() as u64;
    lines * 120 + 1200
}

/// Render text/markup and print it, draining so the trailing cut isn't dropped.
/// Returns the number of bytes sent. Shared by `print` and `watch`.
fn print_text(printer: &Printer, input: &str, plain: bool, do_cut: bool) -> Result<usize> {
    let mut b = printer.builder();
    if plain {
        text::render_plain(&mut b, input, printer.width_dots());
    } else {
        text::render_markup(&mut b, input, printer.width_dots());
    }
    b.feed(2);
    printer.send(&b)?;
    printer.transport().drain(estimate_drain_ms(&b));
    if do_cut {
        printer.print(|c| {
            c.cut();
        })?;
        printer.transport().drain(1000);
    }
    Ok(b.bytes().len())
}

/// Poll `path` for changes and reprint on each save. Dependency-free: watches
/// mtime + size, with a one-tick debounce so a burst of editor writes prints once.
fn watch_file(
    printer: &Printer,
    path: &str,
    plain: bool,
    do_cut: bool,
    interval_ms: u64,
) -> Result<()> {
    use std::time::{Duration, SystemTime};
    let p = Path::new(path);
    let sig = |p: &Path| -> Option<(SystemTime, u64)> {
        let m = std::fs::metadata(p).ok()?;
        Some((m.modified().ok()?, m.len()))
    };

    println!("watching {path} (every {interval_ms}ms) — Ctrl-C to stop");
    let _ = std::io::stdout().flush();
    let print_now = |label: &str| -> Result<()> {
        match std::fs::read_to_string(p) {
            Ok(s) => {
                let n = print_text(printer, &s, plain, do_cut)?;
                println!("  printed {n} bytes ({label})");
            }
            Err(e) => eprintln!("  read error: {e}"),
        }
        let _ = std::io::stdout().flush();
        Ok(())
    };

    let mut last = sig(p);
    if last.is_some() {
        print_now("initial")?; // print current contents on start
    }
    let mut dirty = false;
    loop {
        std::thread::sleep(Duration::from_millis(interval_ms));
        let cur = sig(p);
        if cur != last {
            // Something changed — record it and wait one more tick to settle.
            last = cur;
            dirty = true;
        } else if dirty {
            dirty = false;
            if cur.is_some() {
                print_now("changed")?;
            }
        }
    }
}

/// `tp config`: show the active config, and write a documented sample if none exists.
fn show_config(path: &Path, cfg: &Config) -> Result<()> {
    if path.exists() {
        println!("config: {} (loaded)", path.display());
    } else {
        std::fs::write(path, Config::SAMPLE)?;
        println!("config: wrote sample to {} (edit and re-run)", path.display());
    }
    println!("  width_dots       = {}", cfg.width_dots);
    println!(
        "  net_addr         = {}",
        cfg.net_addr.as_deref().unwrap_or("(unset — USB)")
    );
    println!("  warmup_feed_dots = {}", cfg.warmup_feed_dots);
    println!("  image_band_rows  = {}", cfg.image_band_rows);
    println!("  image_gamma      = {}", cfg.image.gamma);
    println!("  image_contrast   = {}", cfg.image.contrast);
    println!("  image_brightness = {}", cfg.image.brightness);
    println!("  image_dither     = {}", config::dither_name(cfg.image.dither));
    Ok(())
}

// ---- offline byte dumping (no hardware) ------------------------------------

fn dump(rest: &[String], width: u32) -> Result<()> {
    let out = rest
        .first()
        .ok_or_else(|| anyhow!("usage: tp dump <out.bin> <hello|text|qr|barcode|paper> [arg]"))?;
    let which = rest.get(1).map(String::as_str).unwrap_or("hello");
    let arg = rest.get(2).map(String::as_str);

    let mut b = Builder::new().with_width(width);
    match which {
        "hello" => scene_hello(&mut b),
        "text" => scene_text(&mut b),
        "qr" => scene_qr(&mut b, arg.unwrap_or("https://anthropic.com")),
        "barcode" => scene_barcode(&mut b, arg.unwrap_or("ART-0001")),
        "paper" => scene_paper(&mut b),
        other => return Err(anyhow!("unknown scene: {other}")),
    }
    let bytes = b.build();
    std::fs::write(out, &bytes)?;
    println!("wrote {} bytes to {out}", bytes.len());
    Ok(())
}

fn print_help() {
    print!(
        "{}",
        r#"tp — thermal printer playground

USAGE:
  tp <command> [args] [--width DOTS] [--net HOST[:PORT]] [--usb]

COMMANDS:
  info                    Show connection details (no printing)
  status                  Poll printer status (paper/cover/errors)
  hello                   Minimal "it's alive" print
  text                    Typography demo
  image <path> [flags]    Print an image. Flags:
                            --dither none|fs|atkinson|bayer  (default fs)
                            --gamma F       <1 brighter, >1 darker (try 0.7)
                            --contrast F    1=none, >1 punchier
                            --brightness F  luma offset, + = lighter
                            --threshold     alias for --dither none
  print [file] [--raw] [--cut]   Print text/markup from a file or stdin
  watch <file> [--raw] [--cut] [--interval MS]   Reprint the file on every save
  qr <data>               Print a QR code
  barcode <data>          Print a CODE128 barcode
  paper                   Feed / cut / line-spacing demo
  feedcut                 Feed a bit and cut (advance & tear)
  gen <pattern> [--height N] [--seed N]
                          Generative art: rings|waves|truchet|mandelbrot|checker|noise
  all                     Full capability tour
  config                  Show active config; writes sample tp.conf if missing
  dump <out.bin> <scene> [arg]   Write ESC/POS bytes to a file (no hardware)

GLOBAL:
  --width DOTS            Printable width (overrides config; 576/512)
  --net HOST[:PORT]       LAN printer over raw TCP (port defaults to 9100)
  --usb                   Force USB even when tp.conf sets net_addr

Image tone & dither defaults — and the printer's `net_addr` — live in tp.conf
(run `tp config`). CLI flags override them per-print.
"#
    );
}
