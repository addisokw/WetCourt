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

#[derive(Default)]
pub struct Builder {
    buf: Vec<u8>,
    width_dots: u32,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            width_dots: WIDTH_DOTS_80MM,
        }
    }

    pub fn with_width(mut self, width_dots: u32) -> Self {
        self.width_dots = width_dots;
        self
    }

    pub fn width_dots(&self) -> u32 {
        self.width_dots
    }

    /// Reset the printer to a known state (clears styles, buffer).
    pub fn init(&mut self) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'@']);
        self
    }

    // ---- text styling -------------------------------------------------------

    pub fn align(&mut self, a: Align) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'a', a as u8]);
        self
    }

    pub fn font(&mut self, f: Font) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'M', f as u8]);
        self
    }

    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'E', on as u8]);
        self
    }

    pub fn underline(&mut self, dots: u8) -> &mut Self {
        // 0 = off, 1 = 1-dot, 2 = 2-dot thick.
        self.buf.extend_from_slice(&[ESC, b'-', dots.min(2)]);
        self
    }

    /// White-on-black inverted text.
    pub fn inverse(&mut self, on: bool) -> &mut Self {
        self.buf.extend_from_slice(&[GS, b'B', on as u8]);
        self
    }

    /// Upside-down (180°) text — fun for art layouts.
    pub fn upside_down(&mut self, on: bool) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'{', on as u8]);
        self
    }

    /// Character magnification, 1..=8 in each axis (GS ! n).
    pub fn size(&mut self, w: u8, h: u8) -> &mut Self {
        let w = w.clamp(1, 8) - 1;
        let h = h.clamp(1, 8) - 1;
        self.buf.extend_from_slice(&[GS, b'!', (w << 4) | h]);
        self
    }

    /// Line spacing in dots (ESC 3 n). Pass `None` to restore the default (ESC 2).
    pub fn line_spacing(&mut self, dots: Option<u8>) -> &mut Self {
        match dots {
            Some(n) => self.buf.extend_from_slice(&[ESC, b'3', n]),
            None => self.buf.extend_from_slice(&[ESC, b'2']),
        }
        self
    }

    // ---- text content -------------------------------------------------------

    /// Append text (CP437-ish; non-ASCII is dropped to '?').
    pub fn text(&mut self, s: &str) -> &mut Self {
        for b in s.bytes() {
            self.buf.push(if b < 0x80 { b } else { b'?' });
        }
        self
    }

    pub fn line(&mut self, s: &str) -> &mut Self {
        self.text(s);
        self.buf.push(LF);
        self
    }

    pub fn feed(&mut self, lines: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'd', lines]);
        self
    }

    /// Feed n dots (fine vertical control, ESC J n).
    pub fn feed_dots(&mut self, dots: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'J', dots]);
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
        self.buf.extend_from_slice(&[ESC, b'e', lines]);
        self
    }

    /// Reverse-feed `n` dots (ESC K n) — fine reverse motion. Same caveat as
    /// [`Self::reverse_feed`]: ignored on the dev POS-80, verify before relying.
    pub fn reverse_feed_dots(&mut self, dots: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'K', dots]);
        self
    }

    // ---- paper / cut --------------------------------------------------------

    /// Full cut (GS V 0). Many 80mm heads only do partial — see [`Self::partial_cut`].
    pub fn full_cut(&mut self) -> &mut Self {
        self.buf.extend_from_slice(&[GS, b'V', 0]);
        self
    }

    /// Partial cut leaving a small tab (GS V 1).
    pub fn partial_cut(&mut self) -> &mut Self {
        self.buf.extend_from_slice(&[GS, b'V', 1]);
        self
    }

    /// Feed `dots` then partial cut in one op (GS V 66 n) — cleaner tear-off.
    pub fn feed_and_cut(&mut self, dots: u8) -> &mut Self {
        self.buf.extend_from_slice(&[GS, b'V', 66, dots]);
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
    pub fn barcode_style(&mut self, height: u8, width: u8) -> &mut Self {
        self.buf.extend_from_slice(&[GS, b'h', height.max(1)]);
        self.buf.extend_from_slice(&[GS, b'w', width.clamp(2, 6)]);
        // HRI text below the bars (GS H 2).
        self.buf.extend_from_slice(&[GS, b'H', 2]);
        self
    }

    /// Print a 1D barcode using the length-prefixed form (GS k m n d...).
    pub fn barcode(&mut self, sym: Barcode, data: &str) -> &mut Self {
        let bytes = data.as_bytes();
        let len = bytes.len().min(255) as u8;
        self.buf.extend_from_slice(&[GS, b'k', sym as u8, len]);
        self.buf.extend_from_slice(&bytes[..len as usize]);
        self
    }

    // ---- QR (native model 2) ------------------------------------------------

    /// Print a QR code with the given module size (1..=16) and ECC level.
    pub fn qr(&mut self, data: &str, module: u8, ecc: QrEcc) -> &mut Self {
        let module = module.clamp(1, 16);
        // Select model 2.
        self.buf
            .extend_from_slice(&[GS, b'(', b'k', 4, 0, 49, 65, 50, 0]);
        // Module size.
        self.buf
            .extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 67, module]);
        // Error correction.
        self.buf
            .extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 69, ecc as u8]);
        // Store data: length = payload + 3 (the cn fn m prefix bytes).
        let bytes = data.as_bytes();
        let store_len = (bytes.len() + 3) as u16;
        self.buf.extend_from_slice(&[
            GS,
            b'(',
            b'k',
            (store_len & 0xFF) as u8,
            (store_len >> 8) as u8,
            49,
            80,
            48,
        ]);
        self.buf.extend_from_slice(bytes);
        // Print the stored symbol.
        self.buf
            .extend_from_slice(&[GS, b'(', b'k', 3, 0, 49, 81, 48]);
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
        let band = band.max(1);
        let row_len = width_bytes as usize;
        let mut y = 0u16;
        while y < height {
            let h = band.min(height - y);
            let start = y as usize * row_len;
            let end = start + h as usize * row_len;
            self.raster(&bits[start..end], width_bytes, h);
            y += h;
        }
        self
    }

    /// Append a 1-bit raster image (GS v 0). `bits` is row-major, MSB-first,
    /// `width_bytes` bytes per row, `height` rows. Black pixel = bit set.
    /// For tall images prefer [`Self::raster_banded`].
    pub fn raster(&mut self, bits: &[u8], width_bytes: u16, height: u16) -> &mut Self {
        self.buf.extend_from_slice(&[
            GS,
            b'v',
            b'0',
            0, // normal mode
            (width_bytes & 0xFF) as u8,
            (width_bytes >> 8) as u8,
            (height & 0xFF) as u8,
            (height >> 8) as u8,
        ]);
        self.buf.extend_from_slice(bits);
        self
    }

    // ---- output -------------------------------------------------------------

    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    /// Consume the builder and return the byte stream.
    pub fn build(&self) -> Vec<u8> {
        self.buf.clone()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}
