//! A 1-bit drawing canvas for generative imagery — draw straight into a bitmap
//! and hand it to the printer, no image file needed.
//!
//! Bits are MSB-first, row-major, `1 = black dot`, matching the ESC/POS raster
//! format, so [`Canvas::raster`] is a cheap view for [`escpos::Builder::raster_banded`].

use crate::raster::Raster;

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    row_bytes: usize,
    bits: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let row_bytes = ((width + 7) / 8) as usize;
        Self {
            width,
            height,
            row_bytes,
            bits: vec![0u8; row_bytes * height as usize],
        }
    }

    pub fn clear(&mut self, black: bool) {
        let v = if black { 0xFF } else { 0x00 };
        self.bits.iter_mut().for_each(|b| *b = v);
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, black: bool) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let idx = y as usize * self.row_bytes + (x as usize) / 8;
        let mask = 0x80u8 >> (x as usize % 8);
        if black {
            self.bits[idx] |= mask;
        } else {
            self.bits[idx] &= !mask;
        }
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return false;
        }
        let idx = y as usize * self.row_bytes + (x as usize) / 8;
        self.bits[idx] & (0x80 >> (x as usize % 8)) != 0
    }

    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, black: bool) {
        for x in x0.min(x1)..=x0.max(x1) {
            self.set(x, y, black);
        }
    }

    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, black: bool) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.set(x, y, black);
        }
    }

    /// Bresenham line.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, black: bool) {
        let (mut x0, mut y0) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.set(x0, y0, black);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, black: bool) {
        self.hline(x, x + w - 1, y, black);
        self.hline(x, x + w - 1, y + h - 1, black);
        self.vline(x, y, y + h - 1, black);
        self.vline(x + w - 1, y, y + h - 1, black);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, black: bool) {
        for yy in y..y + h {
            self.hline(x, x + w - 1, yy, black);
        }
    }

    /// Midpoint circle outline.
    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, black: bool) {
        if r < 0 {
            return;
        }
        let (mut x, mut y) = (r, 0);
        let mut err = 1 - r;
        while x >= y {
            for (px, py) in [
                (cx + x, cy + y),
                (cx + y, cy + x),
                (cx - y, cy + x),
                (cx - x, cy + y),
                (cx - x, cy - y),
                (cx - y, cy - x),
                (cx + y, cy - x),
                (cx + x, cy - y),
            ] {
                self.set(px, py, black);
            }
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }

    pub fn bits(&self) -> &[u8] {
        &self.bits
    }

    pub fn row_bytes(&self) -> u16 {
        self.row_bytes as u16
    }

    /// View this canvas as a printable [`Raster`].
    pub fn raster(&self) -> Raster {
        Raster {
            bits: self.bits.clone(),
            width_bytes: self.row_bytes as u16,
            height: self.height as u16,
        }
    }
}
