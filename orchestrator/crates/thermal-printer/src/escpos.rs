//! A small, fluent ESC/POS command builder.
//!
//! You build up a byte buffer with chainable calls, then hand the bytes to the
//! USB transport. Keeping command construction separate from I/O makes it easy
//! to unit-test, preview (dump bytes to a file), or retarget later.

/// 80mm printers print 576 dots wide at 203 dpi. Some clones top out at 512;
/// override via [`Builder::with_width`] if your output looks clipped/offset.
pub const WIDTH_DOTS_80MM: u32 = 576;

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;
const LF: u8 = 0x0A;

/// Dots to feed before cutting so the last line clears the blade (~1cm @ 203dpi).
const CUTTER_CLEARANCE_DOTS: u8 = 110;

#[derive(Clone, Copy)]
pub enum Align {
    Left = 0,
    Center = 1,
    Right = 2,
}

#[derive(Clone, Copy)]
pub enum Font {
    /// Font A: ~12x24 dots, 48 cols on 80mm.
    A = 0,
    /// Font B: ~9x17 dots, 64 cols on 80mm — smaller/denser.
    B = 1,
}

/// QR error-correction level (recoverable area: L≈7%, M≈15%, Q≈25%, H≈30%).
#[derive(Clone, Copy)]
pub enum QrEcc {
    L = 48,
    M = 49,
    Q = 50,
    H = 51,
}

/// Common 1D symbologies. CODE128 is the most flexible for arbitrary data.
#[derive(Clone, Copy)]
pub enum Barcode {
    UpcA = 65,
    Ean13 = 67,
    Code39 = 69,
    Code128 = 73,
}

/// The text style state a flipped segment prints under. Captured when the
/// segment closes and re-emitted in full before it in the reversed stream —
/// segment reordering would otherwise tear apart stateful on/off pairs
/// (`bold(true) … bold(false)`).
#[derive(Clone, Copy)]
struct Style {
    align: u8,
    font: u8,
    bold: bool,
    underline: u8,
    inverse: bool,
    /// Packed GS ! magnification value.
    size: u8,
    /// `Some(n)` = ESC 3 n; `None` = firmware default (ESC 2).
    spacing: Option<u8>,
}

impl Default for Style {
    fn default() -> Self {
        Self { align: 0, font: 0, bold: false, underline: 0, inverse: false, size: 0, spacing: None }
    }
}

/// One vertical slice of a flipped document: a printed line, a feed, or a
/// whole raster/QR/barcode block, plus the style it prints under.
struct Segment {
    style: Style,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct Builder {
    buf: Vec<u8>,
    width_dots: u32,
    /// 180°-rotate the whole document for an upside-down-mounted printer.
    /// Content is recorded as [`Segment`]s and replayed in reverse by
    /// [`Self::build`] under ESC { 1 (per-line 180° rotation); raster bits are
    /// pre-rotated in software (GS v 0 ignores upside-down mode per spec).
    /// In flip mode the stream only exists via `build()` — `bytes()` is empty.
    flip: bool,
    cur: Style,
    /// Unclosed content: text awaiting its LF, or glue commands (barcode
    /// style, raw bytes) that must travel with the next content segment.
    pending: Vec<u8>,
    segments: Vec<Segment>,
    /// Cut commands — always emitted last, never reversed.
    trailer: Vec<u8>,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            width_dots: WIDTH_DOTS_80MM,
            ..Self::default()
        }
    }

    pub fn with_width(mut self, width_dots: u32) -> Self {
        self.width_dots = width_dots;
        self
    }

    /// Enable 180° document rotation (see the `flip` field). Set before any
    /// content is appended.
    pub fn with_flip(mut self, flip: bool) -> Self {
        self.flip = flip;
        self
    }

    /// Append style-setter bytes: dropped in flip mode (the [`Style`] snapshot
    /// carries the state instead), appended verbatim otherwise.
    fn style_bytes(&mut self, bytes: &[u8]) {
        if !self.flip {
            self.buf.extend_from_slice(bytes);
        }
    }

    /// Append content bytes and close the segment they complete.
    fn content(&mut self, bytes: &[u8]) {
        if self.flip {
            self.pending.extend_from_slice(bytes);
            let bytes = std::mem::take(&mut self.pending);
            self.segments.push(Segment { style: self.cur, bytes });
        } else {
            self.buf.extend_from_slice(bytes);
        }
    }

    pub fn width_dots(&self) -> u32 {
        self.width_dots
    }

    /// Reset the printer to a known state (clears styles, buffer).
    pub fn init(&mut self) -> &mut Self {
        if self.flip {
            self.cur = Style::default();
        } else {
            self.buf.extend_from_slice(&[ESC, b'@']);
        }
        self
    }

    // ---- text styling -------------------------------------------------------

    pub fn align(&mut self, a: Align) -> &mut Self {
        self.cur.align = a as u8;
        self.style_bytes(&[ESC, b'a', a as u8]);
        self
    }

    pub fn font(&mut self, f: Font) -> &mut Self {
        self.cur.font = f as u8;
        self.style_bytes(&[ESC, b'M', f as u8]);
        self
    }

    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.cur.bold = on;
        self.style_bytes(&[ESC, b'E', on as u8]);
        self
    }

    pub fn underline(&mut self, dots: u8) -> &mut Self {
        // 0 = off, 1 = 1-dot, 2 = 2-dot thick.
        self.cur.underline = dots.min(2);
        self.style_bytes(&[ESC, b'-', dots.min(2)]);
        self
    }

    /// White-on-black inverted text.
    pub fn inverse(&mut self, on: bool) -> &mut Self {
        self.cur.inverse = on;
        self.style_bytes(&[GS, b'B', on as u8]);
        self
    }

    /// Upside-down (180°) text — fun for art layouts. Ignored in flip mode,
    /// which owns ESC { for the whole document.
    pub fn upside_down(&mut self, on: bool) -> &mut Self {
        self.style_bytes(&[ESC, b'{', on as u8]);
        self
    }

    /// Character magnification, 1..=8 in each axis (GS ! n).
    pub fn size(&mut self, w: u8, h: u8) -> &mut Self {
        let w = w.clamp(1, 8) - 1;
        let h = h.clamp(1, 8) - 1;
        self.cur.size = (w << 4) | h;
        self.style_bytes(&[GS, b'!', (w << 4) | h]);
        self
    }

    /// Line spacing in dots (ESC 3 n). Pass `None` to restore the default (ESC 2).
    pub fn line_spacing(&mut self, dots: Option<u8>) -> &mut Self {
        self.cur.spacing = dots;
        match dots {
            Some(n) => self.style_bytes(&[ESC, b'3', n]),
            None => self.style_bytes(&[ESC, b'2']),
        }
        self
    }

    // ---- text content -------------------------------------------------------

    /// Append text (CP437-ish; non-ASCII is dropped to '?').
    pub fn text(&mut self, s: &str) -> &mut Self {
        let sink = if self.flip { &mut self.pending } else { &mut self.buf };
        for b in s.bytes() {
            sink.push(if b < 0x80 { b } else { b'?' });
        }
        self
    }

    pub fn line(&mut self, s: &str) -> &mut Self {
        self.text(s);
        self.content(&[LF]);
        self
    }

    pub fn feed(&mut self, lines: u8) -> &mut Self {
        self.content(&[ESC, b'd', lines]);
        self
    }

    /// Feed n dots (fine vertical control, ESC J n).
    pub fn feed_dots(&mut self, dots: u8) -> &mut Self {
        self.content(&[ESC, b'J', dots]);
        self
    }

    /// Reverse-feed `n` lines (ESC e n) — the counterpart to [`Self::feed`],
    /// meant to pull paper back up over the head.
    ///
    /// **Unsupported on the dev POS-80** (VID 1FC9 / PID 2016): the command is
    /// silently ignored — the drivetrain only moves forward. This matches the
    /// general reality for generic ESC/POS clones, where reverse feed is either
    /// absent or so slip-prone that re-registration is unusable. Kept for spec
    /// completeness and on the chance other hardware honors it; don't rely on it
    /// without confirming on your specific unit (dump the bytes, or just watch
    /// whether the paper actually retracts).
    pub fn reverse_feed(&mut self, lines: u8) -> &mut Self {
        self.content(&[ESC, b'e', lines]);
        self
    }

    /// Reverse-feed `n` dots (ESC K n) — fine reverse motion. Same caveat as
    /// [`Self::reverse_feed`]: ignored on the dev POS-80, verify before relying.
    pub fn reverse_feed_dots(&mut self, dots: u8) -> &mut Self {
        self.content(&[ESC, b'K', dots]);
        self
    }

    // ---- paper / cut --------------------------------------------------------

    /// Append cut bytes: held for the trailer in flip mode (the cut stays the
    /// physically-last command no matter how content reorders).
    fn cut_bytes(&mut self, bytes: &[u8]) {
        if self.flip {
            self.trailer.extend_from_slice(bytes);
        } else {
            self.buf.extend_from_slice(bytes);
        }
    }

    /// Full cut (GS V 0). Many 80mm heads only do partial — see [`Self::partial_cut`].
    pub fn full_cut(&mut self) -> &mut Self {
        self.cut_bytes(&[GS, b'V', 0]);
        self
    }

    /// Partial cut leaving a small tab (GS V 1).
    pub fn partial_cut(&mut self) -> &mut Self {
        self.cut_bytes(&[GS, b'V', 1]);
        self
    }

    /// Feed `dots` then partial cut in one op (GS V 66 n) — cleaner tear-off.
    pub fn feed_and_cut(&mut self, dots: u8) -> &mut Self {
        self.cut_bytes(&[GS, b'V', 66, dots]);
        self
    }

    /// Cut clearing the blade. The cutter sits ~1cm above the print head, so
    /// this feeds enough to push the last line past the blade before cutting.
    /// Use this instead of [`Self::partial_cut`] for normal "end of receipt" cuts.
    pub fn cut(&mut self) -> &mut Self {
        self.feed_and_cut(CUTTER_CLEARANCE_DOTS)
    }

    // ---- barcodes -----------------------------------------------------------

    /// Set barcode height in dots (GS h) and module width 2..=6 (GS w).
    /// In flip mode these glue onto the following [`Self::barcode`]'s segment
    /// (they aren't part of the [`Style`] snapshot).
    pub fn barcode_style(&mut self, height: u8, width: u8) -> &mut Self {
        let sink = if self.flip { &mut self.pending } else { &mut self.buf };
        sink.extend_from_slice(&[GS, b'h', height.max(1)]);
        sink.extend_from_slice(&[GS, b'w', width.clamp(2, 6)]);
        // HRI text below the bars (GS H 2).
        sink.extend_from_slice(&[GS, b'H', 2]);
        self
    }

    /// Print a 1D barcode using the length-prefixed form (GS k m n d...).
    /// Flip-mode caveat: the symbol itself scans fine mirrored (CODE128 & co.
    /// are bidirectional), but the printer may not rotate the HRI text.
    pub fn barcode(&mut self, sym: Barcode, data: &str) -> &mut Self {
        let bytes = data.as_bytes();
        let len = bytes.len().min(255) as u8;
        let mut cmd = vec![GS, b'k', sym as u8, len];
        cmd.extend_from_slice(&bytes[..len as usize]);
        self.content(&cmd);
        self
    }

    // ---- QR (native model 2) ------------------------------------------------

    /// Print a QR code with the given module size (1..=16) and ECC level.
    /// One segment in flip mode; QR symbols scan at any rotation, so the
    /// symbol itself needs no software rotation.
    pub fn qr(&mut self, data: &str, module: u8, ecc: QrEcc) -> &mut Self {
        let module = module.clamp(1, 16);
        let mut cmd = Vec::new();
        // Select model 2.
        cmd.extend_from_slice(&[GS, b'(', b'k', 4, 0, 49, 65, 50, 0]);
        // Module size.
        cmd.extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 67, module]);
        // Error correction.
        cmd.extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 69, ecc as u8]);
        // Store data: length = payload + 3 (the cn fn m prefix bytes).
        let bytes = data.as_bytes();
        let store_len = (bytes.len() + 3) as u16;
        cmd.extend_from_slice(&[
            GS,
            b'(',
            b'k',
            (store_len & 0xFF) as u8,
            (store_len >> 8) as u8,
            49,
            80,
            48,
        ]);
        cmd.extend_from_slice(bytes);
        // Print the stored symbol.
        cmd.extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 81, 48]);
        self.content(&cmd);
        self
    }

    // ---- raster images ------------------------------------------------------

    /// Append a tall raster as a series of horizontal bands (multiple GS v 0
    /// blocks of `band` rows). Cheap printers underrun on a single huge raster —
    /// causing a dark bar at the top and sometimes dropping the command that
    /// follows (e.g. the cut). Banding keeps the head synced with paper feed.
    pub fn raster_banded(
        &mut self,
        bits: &[u8],
        width_bytes: u16,
        height: u16,
        band: u16,
    ) -> &mut Self {
        // In flip mode the rotated image's bands must stay one segment (one
        // visual unit) so reversal can't interleave them with other content.
        let rotated;
        let bits = if self.flip {
            rotated = rotate180(bits, width_bytes as usize, height as usize);
            &rotated[..]
        } else {
            bits
        };
        let band = band.max(1);
        let row_len = width_bytes as usize;
        let mut cmd = Vec::new();
        let mut y = 0u16;
        while y < height {
            let h = band.min(height - y);
            let start = y as usize * row_len;
            let end = start + h as usize * row_len;
            raster_cmd(&mut cmd, &bits[start..end], width_bytes, h);
            y += h;
        }
        self.content(&cmd);
        self
    }

    /// Append a 1-bit raster image (GS v 0). `bits` is row-major, MSB-first,
    /// `width_bytes` bytes per row, `height` rows. Black pixel = bit set.
    /// For tall images prefer [`Self::raster_banded`].
    pub fn raster(&mut self, bits: &[u8], width_bytes: u16, height: u16) -> &mut Self {
        let rotated;
        let bits = if self.flip {
            rotated = rotate180(bits, width_bytes as usize, height as usize);
            &rotated[..]
        } else {
            bits
        };
        let mut cmd = Vec::new();
        raster_cmd(&mut cmd, bits, width_bytes, height);
        self.content(&cmd);
        self
    }

    // ---- output -------------------------------------------------------------

    /// In flip mode raw bytes glue onto the next content segment (like
    /// [`Self::barcode_style`]) — use only for prefix commands.
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        let sink = if self.flip { &mut self.pending } else { &mut self.buf };
        sink.extend_from_slice(bytes);
        self
    }

    /// Return the byte stream. In flip mode this assembles the reversed
    /// document: init, ESC { 1, the content segments last-to-first (each
    /// prefixed by its style snapshot), then the cut trailer.
    pub fn build(&self) -> Vec<u8> {
        if !self.flip {
            return self.buf.clone();
        }
        let mut out = vec![ESC, b'@', ESC, b'{', 1];
        for seg in self.segments.iter().rev() {
            let s = &seg.style;
            out.extend_from_slice(&[ESC, b'a', s.align]);
            out.extend_from_slice(&[ESC, b'M', s.font]);
            out.extend_from_slice(&[ESC, b'E', s.bold as u8]);
            out.extend_from_slice(&[ESC, b'-', s.underline]);
            out.extend_from_slice(&[GS, b'B', s.inverse as u8]);
            out.extend_from_slice(&[GS, b'!', s.size]);
            match s.spacing {
                Some(n) => out.extend_from_slice(&[ESC, b'3', n]),
                None => out.extend_from_slice(&[ESC, b'2']),
            }
            out.extend_from_slice(&seg.bytes);
        }
        out.extend_from_slice(&self.trailer);
        out
    }

    /// The raw unassembled buffer — empty in flip mode; use [`Self::build`].
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// Append one GS v 0 command for `bits` to `out`.
fn raster_cmd(out: &mut Vec<u8>, bits: &[u8], width_bytes: u16, height: u16) {
    out.extend_from_slice(&[
        GS,
        b'v',
        b'0',
        0, // normal mode
        (width_bytes & 0xFF) as u8,
        (width_bytes >> 8) as u8,
        (height & 0xFF) as u8,
        (height >> 8) as u8,
    ]);
    out.extend_from_slice(bits);
}

/// Rotate a packed 1-bit raster 180°: rows reversed, and each row's bytes
/// reversed with their bits mirrored. Rotation is of the padded byte width —
/// content whose dot width isn't a multiple of 8 shifts left by the padding
/// (≤7 dots), invisible on centered or full-width imagery.
fn rotate180(bits: &[u8], width_bytes: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; bits.len()];
    for y in 0..height {
        let src = &bits[y * width_bytes..(y + 1) * width_bytes];
        let dst = (height - 1 - y) * width_bytes;
        for (i, &b) in src.iter().enumerate() {
            out[dst + (width_bytes - 1 - i)] = b.reverse_bits();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate180_mirrors_rows_and_bits() {
        // 2 rows × 2 bytes: a dot at top-left must land at bottom-right.
        let bits = [0b1000_0000, 0b0000_0000, 0b0000_0011, 0b0000_0000];
        let r = rotate180(&bits, 2, 2);
        assert_eq!(r, [0b0000_0000, 0b1100_0000, 0b0000_0000, 0b0000_0001]);
        // Involution: rotating twice restores the original.
        assert_eq!(rotate180(&r, 2, 2), bits);
    }

    #[test]
    fn flip_build_reverses_lines_and_restores_styles() {
        let mut b = Builder::new().with_flip(true);
        b.init();
        b.bold(true).line("FIRST").bold(false).line("second");
        b.feed(2).cut();
        let out = b.build();

        // Header: init + upside-down mode on.
        assert_eq!(&out[..5], &[ESC, b'@', ESC, b'{', 1]);
        // Content order reversed: "second" prints before "FIRST".
        let first = out.windows(5).position(|w| w == b"FIRST").unwrap();
        let second = out.windows(6).position(|w| w == b"second").unwrap();
        assert!(second < first, "flip must reverse line order");
        // Each segment re-emits its style: "FIRST" is preceded by bold-on,
        // "second" by bold-off — even though bold-off was issued before it.
        let style_before = |pos: usize| {
            let head = &out[..pos];
            let bold_at = head
                .windows(2)
                .rposition(|w| w == [ESC, b'E'])
                .expect("bold state before every segment");
            head[bold_at + 2]
        };
        assert_eq!(style_before(first), 1, "FIRST prints bold");
        assert_eq!(style_before(second), 0, "second prints unbolded");
        // The cut stays physically last.
        assert_eq!(&out[out.len() - 4..out.len() - 1], &[GS, b'V', 66]);
        // The feed(2) sits at the front of the content (visual bottom margin).
        let feed = out.windows(3).position(|w| w == [ESC, b'd', 2]).unwrap();
        assert!(feed < second);
    }

    #[test]
    fn flip_rasters_are_rotated_and_stay_one_segment() {
        // 2-row raster with a distinctive top row, banded at 1 row per band.
        let bits = [0b1111_0000, 0b0000_0000];
        let mut b = Builder::new().with_flip(true);
        b.line("caption").raster_banded(&bits, 1, 2, 1);
        let out = b.build();
        // Reversal puts the raster before the caption text.
        let cap = out.windows(7).position(|w| w == b"caption").unwrap();
        let ras = out.windows(3).position(|w| w == [GS, b'v', b'0']).unwrap();
        assert!(ras < cap);
        // Both bands travel together, in rotated order: the (rotated) first
        // band is the old bottom row, the second carries the mirrored top row.
        let band2 = out[ras + 3..].windows(3).position(|w| w == [GS, b'v', b'0']).unwrap();
        let band1_pixel = out[ras + 8];
        let band2_pixel = out[ras + 3 + band2 + 8];
        assert_eq!(band1_pixel, 0b0000_0000);
        assert_eq!(band2_pixel, 0b0000_1111);
    }

    #[test]
    fn no_flip_build_is_untouched() {
        let mk = |flip: bool| {
            let mut b = Builder::new().with_flip(flip);
            b.init();
            b.align(Align::Center).bold(true).line("X").bold(false).feed(1).cut();
            b.build()
        };
        let plain = mk(false);
        // Plain mode: byte-for-byte the historical stream.
        let mut want = vec![ESC, b'@', ESC, b'a', 1, ESC, b'E', 1];
        want.extend_from_slice(b"X");
        want.push(0x0A);
        want.extend_from_slice(&[ESC, b'E', 0, ESC, b'd', 1, GS, b'V', 66, 110]);
        assert_eq!(plain, want);
    }
}
