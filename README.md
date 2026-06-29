# thermal_printer

Drive a generic 80mm ESC/POS thermal printer directly over USB from Rust — no
CUPS, no OS print queue. Built as a playground for exploring the printer's
capabilities and as the reusable foundation for an art project.

It talks to the printer's USB bulk endpoint via libusb (`rusb`), so you get full
control of the raw ESC/POS byte stream: text styling, raster images with
dithering, barcodes, native QR, generative bitmaps, piped text/markup, real-time
status polling, and paper/feed/cut control.

## Hardware

Developed against a generic `POS-80` 80mm printer:

| | |
|---|---|
| USB Vendor ID  | `0x1FC9` |
| USB Product ID | `0x2016` |
| Print width    | 576 dots (≈72mm @ 203dpi) |

`Usb::open_default()` tries those IDs first, then falls back to the first
USB printer-class device it finds, so other 80mm units have a good chance of
working. If text prints clipped or off-center, your head is probably 512 dots
wide — set `width_dots = 512` in `tp.conf` (or pass `--width 512`).

## Requirements

- Rust (2021 edition)
- libusb 1.0
  - **macOS:** `brew install libusb`
  - **Linux:** `libusb-1.0-0-dev` (or your distro's equivalent)
  - **Windows:** nothing to install — `libusb1-sys` vendors and builds libusb
    with the MSVC toolchain. See the Windows section below for the one-time
    driver step.

If `cargo build` can't find libusb on macOS, point pkg-config at Homebrew:

```sh
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
```

### Windows

This crate talks to the printer's USB endpoints directly via libusb, which on
Windows means the device must be bound to the **WinUSB** driver. A printer
installed normally binds to Windows' built-in `usbprint.sys` instead, and libusb
can't claim it — so do this one-time setup:

1. Download [Zadig](https://zadig.akeo.ie/).
2. **Options → List All Devices**, then pick your printer (the generic `POS-80`
   shows up as VID `1FC9` / PID `2016`).
3. Choose **WinUSB** as the target driver and click **Replace Driver**
   (or **Install Driver**).
4. Run `tp info` to confirm the connection.

To go back to printing through the normal Windows print spooler later, uninstall
the WinUSB driver from Device Manager (the device reverts to `usbprint.sys`).
There's no need for the macOS/Homebrew `PKG_CONFIG_PATH` step on Windows.

## Build

```sh
cargo build --release      # binary at target/release/tp
```

The examples below use the debug binary (`target/debug/tp`); swap in the release
path for faster image processing.

## Quick start

```sh
tp info                    # confirm the connection (no printing)
tp hello                   # minimal "it's alive" print
tp all                     # full capability tour on one receipt
```

## Commands

| Command | Description |
|---|---|
| `tp info` | Show connection details (endpoints, width). No printing. |
| `tp status` | Poll real-time status (paper/cover/errors). No printing. |
| `tp hello` | Minimal "it's alive" print. |
| `tp text` | Typography demo: fonts, sizes, bold/underline/inverse/upside-down. |
| `tp image <path> [flags]` | Print an image (see below). |
| `tp print [file] [--raw] [--cut]` | Print text/markup from a file or stdin (see below). |
| `tp watch <file> [--raw] [--cut] [--interval MS]` | Reprint a file every time it's saved. |
| `tp gen <pattern> [--height N] [--seed N]` | Generative bitmap art (see below). |
| `tp qr <data>` | Print a native QR code. |
| `tp barcode <data>` | Print a CODE128 barcode. |
| `tp paper` | Feed / cut / line-spacing demo. |
| `tp feedcut` | Feed a bit and cut — advance & tear anytime. |
| `tp all` | Full capability tour. |
| `tp config` | Show active config; writes a sample `tp.conf` if missing. |
| `tp dump <out.bin> <scene> [arg]` | Write a scene's ESC/POS bytes to a file (no hardware). |

Global flag `--width DOTS` overrides the configured printable width.

### Printing images

```sh
tp image photo.jpg                          # uses defaults from tp.conf
tp image photo.jpg --gamma 0.7              # brighter mid-tones (thermal runs dark)
tp image photo.jpg --gamma 0.7 --contrast 1.3   # brighter + punchier
tp image photo.jpg --dither atkinson        # airy, high-contrast look
tp image photo.jpg --dither bayer           # retro halftone grid
tp image photo.jpg --threshold              # hard black/white, no texture
```

The pipeline is: load → grayscale → scale to printer width → tone-adjust
(gamma → contrast → brightness) → dither → 1-bit raster.

**Dither modes** (the biggest lever on the look):

| Mode | Look |
|---|---|
| `fs` (Floyd–Steinberg, default) | Fine photographic stipple, max detail. |
| `atkinson` | Sparser, punchier, more white space (classic Mac dither). |
| `bayer` | Regular cross-hatch / halftone grid, retro. |
| `none` / `--threshold` | Hard black/white, graphic poster look. |

**Tone controls:** `--gamma` (`<1` brightens, `>1` darkens), `--contrast`
(`1` = none), `--brightness` (luma offset, `+` = lighter). CLI flags override the
config defaults per-print.

### Printing text & markup

`tp print` reads from a file argument or stdin and prints it, word-wrapped to the
printer width. Good for live/performative printing — pipe anything in:

```sh
tp print poem.md --cut             # print a file, then cut
cat log.txt | tp print --raw       # plain text, no markup, just wrapping
printf '# HELLO\n- live\n.qr https://example.com\n.cut\n' | tp print
```

By default it interprets a small, receipt-friendly **Markdown** subset:
`#`/`##`/`###` headings, `-`/`*`/`+` bullets, `---` rules, ``` fenced code
(switches to the denser Font B), and `| ... |` tables. `--raw` disables this.

**Live reprinting:** `tp watch <file>` prints the file once on start, then again
every time you save it — handy for iterating on a layout or for a performance
where edits print as you type. It polls mtime/size (no dependencies) with a
one-tick debounce, so a burst of editor writes prints only once. Same `--raw` /
`--cut` flags as `print`; tune the poll rate with `--interval MS` (default 500).

It also understands `.directives` (one per line) aimed at live printing:

| Directive | Effect |
|---|---|
| `.center` / `.left` / `.right` | Set alignment |
| `.bold on\|off` | Toggle bold |
| `.font a\|b` | Switch font |
| `.size W H` | Character magnification (1–8) |
| `.rule` | Horizontal rule |
| `.feed [n]` | Feed n lines (default 1) |
| `.qr <data>` | Print a QR code |
| `.barcode <data>` | Print a CODE128 barcode |
| `.cut` | Feed and cut |

### Generative imagery

`tp gen` draws a pattern straight into a 1-bit bitmap (no image file) and prints
it. Random patterns take `--seed` for reproducibility; `--height` controls length
(default = square).

```sh
tp gen waves                 # two-source interference moiré
tp gen rings                 # concentric circles
tp gen truchet --seed 7      # random diagonal tiles
tp gen mandelbrot            # escape-time fractal, stippled
tp gen checker
tp gen noise --seed 3 --height 300
```

Patterns: `rings`, `waves`, `truchet`, `mandelbrot`, `checker`, `noise`. These
are demos over the `canvas::Canvas` primitives (pixels, lines, rects, circles) —
the building blocks for your own generative work.

### Status polling

`tp status` issues `DLE EOT 1..4` and decodes the reply — useful for installations
that need to react to running out of paper or an open cover:

```text
ready:               yes
online:              yes
cover open:          no
paper out:           no
paper near-end:      no
...
raw DLE EOT 1..4:    16 12 12 12
```

In code, `Status::is_ready()` is a one-call gate before a print.

## Examples

Standalone programs in `examples/` show how to compose the library. Run them
with `cargo run --example <name>`:

```sh
cargo run --example invoice -- friend "Alex"
cargo run --example invoice -- coworker "Dana from Accounting"
```

- **invoice** — a tongue-in-cheek invoice generator (bill a friend for emotional
  labor or a coworker for doing their job). Draws a faux certification seal with
  `Canvas`, lays out randomized line items with totals/tax, and prints a "pay
  here" QR. A compact tour of canvas + typography + QR working together.

## Configuration

Defaults live in `tp.conf` in the working directory. Run `tp config` once to
write a documented sample, then edit it — every `tp image` picks it up, and CLI
flags still override per-print.

```ini
width_dots        = 576    # 576 for most 80mm heads, 512 for some clones
warmup_feed_dots  = 24     # blank feed before an image (hides cold-start band)
image_band_rows   = 128    # raster band height; lower if images smear/banding

image_gamma       = 0.7    # <1 brightens mid-tones, >1 darkens
image_contrast    = 1.0    # 1 = none, >1 = punchier
image_brightness  = 0.0    # luma offset, + = lighter
image_dither      = fs     # none | fs | atkinson | bayer
```

## Using it as a library

The binary is a thin shell over the library — depend on the crate directly from
another project (e.g. a larger art project) while keeping this as its own repo.

### Adding it to another project

Reference it as a **git dependency pinned to a tag** — no need to publish to
crates.io. Only the library is built for consumers; the `tp` binary and
`examples/` are ignored.

```toml
# your-project/Cargo.toml
[dependencies]
thermal_printer = { git = "https://github.com/you/thermal-printer", tag = "v0.1.0" }

# If you only draw via Canvas / hand-build ESC/POS, skip the heavy image stack:
# thermal_printer = { git = "...", tag = "v0.1.0", default-features = false }
```

Pin to a `tag` (or `rev`) for reproducible builds. To pull in a newer library
version later: tag a release here, bump the tag, then `cargo update -p thermal_printer`.

**Co-developing both at once?** Override the git dep with your local checkout so
you can edit the library live without re-tagging or touching `[dependencies]`:

```toml
# your-project/Cargo.toml — at the bottom
[patch."https://github.com/you/thermal-printer"]
thermal_printer = { path = "../thermal-printer" }
```

Remove the `[patch]` block to snap back to the pinned tag. (No remote yet? Use a
plain `path = "../thermal-printer"` dependency, or add this repo as a git
submodule and path-depend on it.)

### API sketch

```rust
use thermal_printer::{Printer, Align, Barcode, QrEcc};

let printer = Printer::connect()?;          // opens USB

printer.print(|b| {
    b.init()
        .align(Align::Center)
        .size(2, 2).bold(true).line("GALLERY")
        .bold(false).size(1, 1)
        .qr("https://example.com", 7, QrEcc::M)
        .feed(2)
        .cut();                              // feeds past the blade, then cuts
})?;
```

Layers:

- `transport::Usb` — raw bulk USB I/O (no CUPS / OS print path); `query_status()`
  returns a decoded `Status`.
- `escpos::Builder` — fluent ESC/POS command construction.
- `raster` — image → 1-bit dithered raster (file loading behind the `image` feature).
- `canvas::Canvas` — 1-bit drawing surface for generative bitmaps.
- `text` — word-wrap + Markdown/directive rendering for piped text.
- `config::Config` — load the same `tp.conf` look the CLI uses.
- `Printer` — convenience wrapper that owns a transport + builds/sends commands.

Inspect what would be sent without hardware:

```sh
tp dump out.bin text          # writes the raw ESC/POS bytes
xxd out.bin | head
```

## Notes & gotchas

Learned the hard way against cheap ESC/POS hardware:

- **Terminate the last line before cutting.** A dangling `text()` with no line
  feed sits unflushed in the line buffer and swallows the following cut command.
  Use `line()` (LF-terminated) before `cut()`.
- **Cut after a big raster as a separate write.** Bundling the cut into the same
  giant raster transfer can drop it. The `image` command prints the raster,
  waits for it to physically finish (`usb().drain(ms)`), then sends the cut on
  its own — the same reliable path as `tp feedcut`.
- **The cutter sits ~1cm above the print head.** `cut()` feeds past the blade
  first so your last line isn't sliced through. Tune `CUTTER_CLEARANCE_DOTS` in
  `escpos.rs` if your mechanism differs.
- **Cold-start band.** The first few mm of a raster can print dark. `warmup_feed_dots`
  pushes a blank margin out first; banding (`image_band_rows`) keeps the head
  synced with the paper feed.
- **macOS** can't auto-detach kernel drivers; the transport ignores that and
  claims the interface directly. Make sure no CUPS job is mid-flight.
