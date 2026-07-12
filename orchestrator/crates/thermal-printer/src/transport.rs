//! Transports for a generic ESC/POS printer.
//!
//! - [`Usb`] talks to the printer's bulk-OUT endpoint directly via libusb
//!   (rusb), so it does not depend on a CUPS queue or any OS print path.
//! - [`Net`] talks raw TCP to a LAN printer on the JetDirect port (9100) —
//!   the same byte stream, no driver anywhere.
//! - [`Transport`] wraps either so callers don't care which cable it is.

use anyhow::{anyhow, Context, Result};
use rusb::{Device, DeviceHandle, Direction, GlobalContext, TransferType};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Your printer, discovered earlier: `usb://Printer/POS-80?serial=6746046E3632`.
pub const DEFAULT_VID: u16 = 0x1FC9;
pub const DEFAULT_PID: u16 = 0x2016;

/// A connected printer ready to receive ESC/POS bytes.
pub struct Usb {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    ep_out: u8,
    ep_in: Option<u8>,
    timeout: Duration,
    claimed: bool,
}

impl Usb {
    /// Open the default printer (VID/PID above). Falls back to the first device
    /// that exposes a printer-class interface if the exact IDs aren't found.
    pub fn open_default() -> Result<Self> {
        Self::open(DEFAULT_VID, DEFAULT_PID).or_else(|_| Self::open_any_printer())
    }

    /// Open a specific device by vendor/product id.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        let device = rusb::devices()?
            .iter()
            .find(|d| {
                d.device_descriptor()
                    .map(|desc| desc.vendor_id() == vid && desc.product_id() == pid)
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("no USB device {vid:04x}:{pid:04x} found"))?;
        Self::from_device(device)
    }

    /// Open the first device that advertises a printer interface class (7).
    pub fn open_any_printer() -> Result<Self> {
        for device in rusb::devices()?.iter() {
            if let Ok(config) = device.active_config_descriptor() {
                let is_printer = config
                    .interfaces()
                    .flat_map(|i| i.descriptors())
                    .any(|d| d.class_code() == 7);
                if is_printer {
                    return Self::from_device(device);
                }
            }
        }
        Err(anyhow!("no USB printer-class device found"))
    }

    fn from_device(device: Device<GlobalContext>) -> Result<Self> {
        let config = device
            .active_config_descriptor()
            .context("reading active USB config")?;

        // Find a printer interface and its bulk endpoints. Generic POS printers
        // expose one interface with a bulk-OUT (data to printer) and sometimes a
        // bulk-IN (status back from printer).
        let mut found = None;
        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                let mut ep_out = None;
                let mut ep_in = None;
                for ep in desc.endpoint_descriptors() {
                    if ep.transfer_type() == TransferType::Bulk {
                        match ep.direction() {
                            Direction::Out => ep_out = Some(ep.address()),
                            Direction::In => ep_in = Some(ep.address()),
                        }
                    }
                }
                if let Some(out) = ep_out {
                    found = Some((desc.interface_number(), out, ep_in));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (iface, ep_out, ep_in) =
            found.ok_or_else(|| anyhow!("no bulk-OUT endpoint on this device"))?;

        let handle = device.open().context(
            "opening USB device (on macOS, make sure no CUPS print job is mid-flight)",
        )?;

        // macOS doesn't support auto-detaching kernel drivers; ignore the error.
        let _ = handle.set_auto_detach_kernel_driver(true);

        let claimed = match handle.claim_interface(iface) {
            Ok(()) => true,
            Err(e) => {
                // Some macOS setups won't let us claim if a printing class driver
                // grabbed it. Writes can still succeed in many cases, so warn and
                // continue rather than hard-fail.
                eprintln!(
                    "warning: could not claim interface {iface} ({e}); attempting writes anyway"
                );
                false
            }
        };

        Ok(Self {
            handle,
            iface,
            ep_out,
            ep_in,
            timeout: Duration::from_secs(5),
            claimed,
        })
    }

    /// Write raw bytes to the printer (chunked so big rasters don't stall).
    pub fn write(&self, data: &[u8]) -> Result<()> {
        const CHUNK: usize = 4096;
        for chunk in data.chunks(CHUNK) {
            let mut sent = 0;
            while sent < chunk.len() {
                let n = self
                    .handle
                    .write_bulk(self.ep_out, &chunk[sent..], self.timeout)
                    .context("bulk write to printer failed")?;
                if n == 0 {
                    return Err(anyhow!("printer accepted 0 bytes (stalled?)"));
                }
                sent += n;
            }
        }
        Ok(())
    }

    /// Query real-time printer status via `DLE EOT n` (n = 1..4) and decode it.
    /// Works even mid-print on printers that support real-time status.
    pub fn query_status(&self) -> Result<Status> {
        if self.ep_in.is_none() {
            return Err(anyhow!("this printer has no bulk-IN endpoint for status"));
        }
        let q = |n: u8| -> Option<u8> {
            self.write(&[0x10, 0x04, n]).ok()?;
            self.read_one(Duration::from_millis(800))
        };
        Ok(Status::decode([q(1), q(2), q(3), q(4)]))
    }

    fn read_one(&self, timeout: Duration) -> Option<u8> {
        let ep = self.ep_in?;
        let mut buf = [0u8; 1];
        match self.handle.read_bulk(ep, &mut buf, timeout) {
            Ok(n) if n >= 1 => Some(buf[0]),
            _ => None,
        }
    }

    /// Read up to `max` status bytes, if the printer has a bulk-IN endpoint.
    pub fn read_status(&self, max: usize) -> Result<Vec<u8>> {
        let ep = self
            .ep_in
            .ok_or_else(|| anyhow!("this printer has no bulk-IN endpoint for status"))?;
        let mut buf = vec![0u8; max];
        let n = self
            .handle
            .read_bulk(ep, &mut buf, self.timeout)
            .context("bulk read from printer failed")?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Block for `ms` milliseconds to let the printer finish physically printing
    /// before the handle is dropped (closing the pipe can discard buffered
    /// commands like a trailing cut that haven't been mechanically executed yet).
    pub fn drain(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    pub fn set_timeout(&mut self, d: Duration) {
        self.timeout = d;
    }

    pub fn has_status_channel(&self) -> bool {
        self.ep_in.is_some()
    }

    /// A short human-readable description of what we connected to.
    pub fn describe(&self) -> String {
        format!(
            "iface {} | OUT ep 0x{:02x} | IN ep {} | claimed: {}",
            self.iface,
            self.ep_out,
            self.ep_in
                .map(|e| format!("0x{e:02x}"))
                .unwrap_or_else(|| "none".into()),
            self.claimed,
        )
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        if self.claimed {
            let _ = self.handle.release_interface(self.iface);
        }
    }
}

/// Default raw-printing TCP port (HP JetDirect convention; POS LAN boards
/// speak the same ESC/POS bytes over it).
pub const NET_PORT: u16 = 9100;

/// A LAN printer reached over raw TCP. The socket is bidirectional, so
/// `DLE EOT` real-time status works the same as over USB bulk-IN (on printers
/// that implement it; unanswered queries just decode to `None`s).
pub struct Net {
    stream: TcpStream,
    peer: String,
}

impl Net {
    /// Connect to `host[:port]`; the port defaults to 9100.
    pub fn connect(addr: &str) -> Result<Self> {
        let full = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{NET_PORT}") };
        let sock = full
            .to_socket_addrs()
            .with_context(|| format!("resolving printer address {full}"))?
            .next()
            .ok_or_else(|| anyhow!("printer address {full} resolved to nothing"))?;
        let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(4))
            .with_context(|| format!("connecting to printer at {full}"))?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        Ok(Self { stream, peer: full })
    }

    /// Write raw bytes to the printer.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        // `impl Write for &TcpStream` keeps the &self surface identical to Usb.
        (&self.stream)
            .write_all(data)
            .with_context(|| format!("TCP write to printer {} failed", self.peer))
    }

    /// Query real-time printer status via `DLE EOT n` (n = 1..4) and decode it.
    pub fn query_status(&self) -> Result<Status> {
        // Drop any stale unread bytes so the four answers pair with the four
        // queries below rather than with some earlier traffic.
        self.stream.set_read_timeout(Some(Duration::from_millis(10))).ok();
        let mut junk = [0u8; 32];
        while matches!((&self.stream).read(&mut junk), Ok(n) if n > 0) {}

        let q = |n: u8| -> Option<u8> {
            self.write(&[0x10, 0x04, n]).ok()?;
            self.read_one(Duration::from_millis(800))
        };
        Ok(Status::decode([q(1), q(2), q(3), q(4)]))
    }

    fn read_one(&self, timeout: Duration) -> Option<u8> {
        self.stream.set_read_timeout(Some(timeout)).ok();
        let mut buf = [0u8; 1];
        match (&self.stream).read(&mut buf) {
            Ok(n) if n >= 1 => Some(buf[0]),
            _ => None,
        }
    }

    /// See [`Usb::drain`] — same purpose: let the mech finish before the
    /// socket closes and the printer's buffer is at the mercy of its firmware.
    pub fn drain(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    pub fn describe(&self) -> String {
        format!("tcp {} (raw/JetDirect)", self.peer)
    }
}

/// Either cable, one surface — what [`crate::Printer`] holds.
pub enum Transport {
    Usb(Usb),
    Net(Net),
}

impl Transport {
    pub fn write(&self, data: &[u8]) -> Result<()> {
        match self {
            Transport::Usb(u) => u.write(data),
            Transport::Net(n) => n.write(data),
        }
    }

    pub fn query_status(&self) -> Result<Status> {
        match self {
            Transport::Usb(u) => u.query_status(),
            Transport::Net(n) => n.query_status(),
        }
    }

    /// Whether status queries have any chance of an answer (USB needs a
    /// bulk-IN endpoint; a TCP socket is always bidirectional).
    pub fn has_status_channel(&self) -> bool {
        match self {
            Transport::Usb(u) => u.has_status_channel(),
            Transport::Net(_) => true,
        }
    }

    pub fn drain(&self, ms: u64) {
        match self {
            Transport::Usb(u) => u.drain(ms),
            Transport::Net(n) => n.drain(ms),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Transport::Usb(u) => u.describe(),
            Transport::Net(n) => n.describe(),
        }
    }
}

/// Decoded real-time printer status. Fields are `None` when the printer didn't
/// answer that particular query (some clones only implement a subset).
#[derive(Debug, Clone, Copy, Default)]
pub struct Status {
    pub online: Option<bool>,
    pub cover_open: Option<bool>,
    pub paper_feeding: Option<bool>,
    pub paper_near_end: Option<bool>,
    pub paper_out: Option<bool>,
    pub cutter_error: Option<bool>,
    pub recoverable_error: Option<bool>,
    pub unrecoverable_error: Option<bool>,
    /// Raw bytes from DLE EOT 1..4 (None where unanswered).
    pub raw: [Option<u8>; 4],
}

impl Status {
    /// Decode the four DLE EOT response bytes. Bit meanings per the ESC/POS
    /// real-time status spec; every byte has fixed bits (1,4 set; 0,7 clear),
    /// which we use as a sanity check that we got a real status byte.
    pub fn decode(raw: [Option<u8>; 4]) -> Self {
        let valid = |b: Option<u8>| b.filter(|x| x & 0x93 == 0x12);
        let bit = |b: Option<u8>, mask: u8| valid(b).map(|x| x & mask != 0);

        Self {
            // DLE EOT 1: bit3 set = offline.
            online: valid(raw[0]).map(|x| x & 0x08 == 0),
            // DLE EOT 2: bit2 = cover open, bit3 = paper fed by button.
            cover_open: bit(raw[1], 0x04),
            paper_feeding: bit(raw[1], 0x08),
            // DLE EOT 3: bit3 cutter err, bit5 unrecoverable, bit6 recoverable.
            cutter_error: bit(raw[2], 0x08),
            unrecoverable_error: bit(raw[2], 0x20),
            recoverable_error: bit(raw[2], 0x40),
            // DLE EOT 4: bits2,3 = paper near-end; bits5,6 = paper end.
            paper_near_end: bit(raw[3], 0x0C),
            paper_out: bit(raw[3], 0x60),
            raw,
        }
    }

    /// True if the printer reports everything ready to print.
    pub fn is_ready(&self) -> bool {
        self.online != Some(false)
            && self.cover_open != Some(true)
            && self.paper_out != Some(true)
            && self.unrecoverable_error != Some(true)
    }
}
