//! A tongue-in-cheek invoice generator — bill a friend for emotional labor or a
//! coworker for doing their job. Shows off composing the library: a `Canvas`
//! logo, ESC/POS typography, aligned line items, and a QR footer.
//!
//! Run:
//!   cargo run --example invoice -- friend "Alex"
//!   cargo run --example invoice -- coworker "Dana from Accounting"
//!
//! The recipient name and invoice type are optional (defaults below).

use anyhow::Result;
use std::f32::consts::PI;
use std::time::{SystemTime, UNIX_EPOCH};
use thermal_printer::{
    canvas::Canvas,
    escpos::{Align, Builder, QrEcc},
    Printer,
};

struct Item {
    desc: &'static str,
    qty: f32,
    rate_cents: u32,
    note: &'static str,
}

struct Profile {
    name: &'static str,
    tagline: &'static str,
    address: &'static str,
    prefix: &'static str,
    discount_label: &'static str,
    tax_label: &'static str,
    tax_pct: u32,
    terms: &'static str,
    footer: &'static str,
    qr: &'static str,
    items: &'static [Item],
}

fn friend() -> Profile {
    Profile {
        name: "BOSOM BUDDIES LLC",
        tagline: "Premium Friendship Solutions",
        address: "1 Ride-or-Die Blvd, Your Corner",
        prefix: "FR",
        discount_label: "BFF LOYALTY DISCOUNT",
        tax_label: "EMOTIONAL LABOR TAX",
        tax_pct: 8,
        terms: "Net 30 hugs",
        footer: "Payable in tacos or equivalent affection.",
        qr: "https://example.com/pay/friendship",
        items: &[
            Item { desc: "Emotional support (after-hours)", qty: 3.5, rate_cents: 4500, note: "/hr" },
            Item { desc: "Listening to the same story, again", qty: 4.0, rate_cents: 1200, note: "" },
            Item { desc: "Talking you off the texting ledge", qty: 2.0, rate_cents: 4000, note: "" },
            Item { desc: "Helping move a couch up 4 flights", qty: 1.0, rate_cents: 15000, note: "" },
            Item { desc: "Being your plus-one (last minute)", qty: 1.0, rate_cents: 7500, note: "" },
            Item { desc: "Pretending to like your new haircut", qty: 3.0, rate_cents: 800, note: "" },
            Item { desc: "'I'm 5 mins away' (was 25)", qty: 3.0, rate_cents: 0, note: "comp" },
            Item { desc: "Validating questionable decisions", qty: 6.0, rate_cents: 1500, note: "" },
        ],
    }
}

fn coworker() -> Profile {
    Profile {
        name: "SYNERGY SOLUTIONS GROUP",
        tagline: "Corporate Companionship, LLC",
        address: "4th Floor, By The Broken Printer",
        prefix: "CW",
        discount_label: "SYNERGY REBATE",
        tax_label: "CORPORATE OVERHEAD",
        tax_pct: 12,
        terms: "Net 30 (business) days",
        footer: "Consider this a teambuilding exercise.",
        qr: "https://example.com/pay/synergy",
        items: &[
            Item { desc: "Covering your 9AM standup", qty: 1.0, rate_cents: 3500, note: "" },
            Item { desc: "Looking busy during your demo", qty: 1.5, rate_cents: 5000, note: "/hr" },
            Item { desc: "'Per my last email' diplomacy", qty: 2.0, rate_cents: 2500, note: "" },
            Item { desc: "Reply-all damage control", qty: 1.0, rate_cents: 8000, note: "" },
            Item { desc: "Strategic coffee-run intelligence", qty: 1.0, rate_cents: 1000, note: "" },
            Item { desc: "Pretending your idea was great", qty: 4.0, rate_cents: 2000, note: "" },
            Item { desc: "Not mentioning the printer incident", qty: 1.0, rate_cents: 50000, note: "NDA" },
            Item { desc: "Unjamming the printer (again)", qty: 2.0, rate_cents: 4500, note: "" },
        ],
    }
}

/// Tiny deterministic PRNG so we can shuffle/select line items.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn money(cents: i64) -> String {
    let neg = cents < 0;
    let c = cents.unsigned_abs();
    format!("{}${}.{:02}", if neg { "-" } else { "" }, c / 100, c % 100)
}

fn qty_str(q: f32) -> String {
    if q.fract() == 0.0 {
        format!("{}", q as i64)
    } else {
        format!("{q:.1}")
    }
}

/// Left text + right text padded to `cols`, with leader spaces between.
fn row(left: &str, right: &str, cols: usize) -> String {
    let gap = cols.saturating_sub(left.len() + right.len()).max(1);
    format!("{left}{}{right}", " ".repeat(gap))
}

/// A faux certification seal, drawn into a 1-bit canvas.
fn seal() -> Canvas {
    let mut c = Canvas::new(150, 150);
    let (cx, cy) = (75, 75);
    c.circle(cx, cy, 72, true);
    c.circle(cx, cy, 68, true);
    c.circle(cx, cy, 44, true);
    // Radial ticks around the rim.
    for k in 0..36 {
        let a = k as f32 * PI / 18.0;
        let (s, co) = (a.sin(), a.cos());
        c.line(
            (cx as f32 + 46.0 * co) as i32,
            (cy as f32 + 46.0 * s) as i32,
            (cx as f32 + 66.0 * co) as i32,
            (cy as f32 + 66.0 * s) as i32,
            true,
        );
    }
    // A 6-point star in the middle.
    for k in 0..6 {
        let a = k as f32 * PI / 3.0;
        c.line(cx, cy, (cx as f32 + 36.0 * a.cos()) as i32, (cy as f32 + 36.0 * a.sin()) as i32, true);
    }
    c
}

fn build_invoice(b: &mut Builder, p: &Profile, recipient: &str, cols: usize, rng: &mut Rng) {
    b.init();

    // --- logo seal ---
    let s = seal();
    let r = s.raster();
    b.align(Align::Center)
        .raster_banded(&r.bits, r.width_bytes, r.height, 128)
        .feed(1);

    // --- letterhead ---
    b.size(1, 2).bold(true).line(p.name).bold(false).size(1, 1);
    b.line(p.tagline).line(p.address).feed(1);

    // --- meta ---
    let inv_no = 1000 + rng.below(9000);
    b.align(Align::Left).line(&"=".repeat(cols));
    b.bold(true).size(2, 1).line("INVOICE").size(1, 1).bold(false);
    b.line(&format!("No.    {}-{:04}", p.prefix, inv_no));
    b.line(&format!("Bill to: {recipient}"));
    b.line(&format!("Terms:   {}", p.terms));
    b.line(&"=".repeat(cols));

    // --- line items: pick 5-6 of them ---
    let want = 5 + rng.below(2);
    let mut idx: Vec<usize> = (0..p.items.len()).collect();
    // Fisher-Yates shuffle.
    for i in (1..idx.len()).rev() {
        idx.swap(i, rng.below(i + 1));
    }
    idx.truncate(want);

    let mut subtotal: i64 = 0;
    b.line(&row("QTY ITEM", "AMOUNT", cols));
    b.line(&"-".repeat(cols));
    for &i in &idx {
        let it = &p.items[i];
        let amount = (it.qty * it.rate_cents as f32).round() as i64;
        subtotal += amount;

        // Description line (bold), wrapped to width.
        b.bold(true).line(it.desc).bold(false);
        // Detail line: "  2 x $40.00 .......... $80.00"
        let left = format!("  {} x {}{}", qty_str(it.qty), money(it.rate_cents as i64), it.note);
        let right = if amount == 0 { "FREE".to_string() } else { money(amount) };
        b.line(&row(&left, &right, cols));
    }
    b.line(&"-".repeat(cols));

    // --- totals ---
    let discount = (subtotal as f32 * 0.10).round() as i64; // 10% loyalty
    let taxable = subtotal - discount;
    let tax = (taxable as f32 * p.tax_pct as f32 / 100.0).round() as i64;
    let total = taxable + tax;

    b.line(&row("Subtotal", &money(subtotal), cols));
    b.line(&row(p.discount_label, &money(-discount), cols));
    b.line(&row(&format!("{} ({}%)", p.tax_label, p.tax_pct), &money(tax), cols));
    b.line(&"-".repeat(cols));
    b.size(1, 2).bold(true).line(&row("TOTAL DUE", &money(total), cols / 2));
    b.size(1, 1).bold(false);
    b.line(&"=".repeat(cols)).feed(1);

    // --- footer + "pay here" QR ---
    b.align(Align::Center)
        .line(p.footer)
        .feed(1)
        .qr(p.qr, 6, QrEcc::M)
        .line("scan to settle up")
        .feed(1)
        .bold(true)
        .line("*** THIS INVOICE IS A JOKE ***")
        .bold(false)
        .line("(probably)")
        .align(Align::Left);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = args.first().map(String::as_str).unwrap_or("friend");
    let (profile, default_name) = match kind {
        "coworker" | "work" => (coworker(), "ESTEEMED COLLEAGUE"),
        _ => (friend(), "A VALUED FRIEND"),
    };
    let recipient = args.get(1).cloned().unwrap_or_else(|| default_name.to_string());

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B9);
    let mut rng = Rng::new(seed);

    let printer = Printer::connect()?;
    let cols = (printer.width_dots() / 12) as usize;

    let mut b = printer.builder();
    build_invoice(&mut b, &profile, &recipient, cols, &mut rng);
    b.feed(2);
    printer.send(&b)?;

    // Hold the handle open until printing finishes, then cut separately.
    let lines = b.bytes().iter().filter(|&&x| x == 0x0A).count() as u64;
    printer.transport().drain(lines * 120 + 2500);
    printer.print(|c| {
        c.cut();
    })?;
    printer.transport().drain(1000);

    println!("printed {kind} invoice for \"{recipient}\"");
    Ok(())
}
