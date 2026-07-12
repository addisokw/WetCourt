//! Drive a generic 80mm ESC/POS thermal printer over USB or the LAN.
//!
//! Layers:
//! - [`transport::Usb`] — raw bulk USB I/O (no CUPS / OS print path needed).
//! - [`transport::Net`] — raw TCP to a LAN printer (JetDirect port 9100).
//! - [`escpos::Builder`] — fluent ESC/POS command construction.
//! - [`raster`] — image → 1-bit dithered raster.
//! - [`Printer`] — convenience wrapper that owns a transport + a builder.

pub mod canvas;
pub mod config;
pub mod escpos;
pub mod raster;
pub mod text;
pub mod transport;

use anyhow::Result;
pub use config::Config;
pub use escpos::{Align, Barcode, Builder, Font, QrEcc};
pub use transport::{Net, Status, Transport, Usb};

/// High-level handle: builds a command stream then flushes it to the transport.
pub struct Printer {
    transport: Transport,
    width_dots: u32,
}

impl Printer {
    /// Connect to the default USB printer.
    pub fn connect() -> Result<Self> {
        Ok(Self {
            transport: Transport::Usb(Usb::open_default()?),
            width_dots: escpos::WIDTH_DOTS_80MM,
        })
    }

    /// Connect to a LAN printer at `host[:port]` (port defaults to 9100).
    pub fn connect_net(addr: &str) -> Result<Self> {
        Ok(Self {
            transport: Transport::Net(Net::connect(addr)?),
            width_dots: escpos::WIDTH_DOTS_80MM,
        })
    }

    pub fn with_width(mut self, dots: u32) -> Self {
        self.width_dots = dots;
        self
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    pub fn width_dots(&self) -> u32 {
        self.width_dots
    }

    /// Start a fresh command builder pre-sized to this printer's width.
    pub fn builder(&self) -> Builder {
        Builder::new().with_width(self.width_dots)
    }

    /// Send a finished builder to the printer.
    pub fn send(&self, b: &Builder) -> Result<()> {
        self.transport.write(b.bytes())
    }

    /// Build → send in one shot via a closure.
    pub fn print<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Builder),
    {
        let mut b = self.builder();
        f(&mut b);
        self.send(&b)
    }
}
