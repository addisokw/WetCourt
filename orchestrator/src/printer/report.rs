//! Render a [`TrialRecord`] into an ESC/POS byte stream for the 80mm thermal
//! printer — the booth keepsake.
//!
//! The layout, top to bottom: a drawn court seal, the masthead, the docket
//! caption (charge + generated defendant alias + case no/time), the verbatim
//! transcript (plea, optional cross-examination, the judge's full
//! deliberation), the verdict, a reserved "moment of justice" photo slot on
//! guilty findings (filled by the vision still in M3), and a footer with a QR
//! code, an on-site-editable booth location, and the social tag.
//!
//! Everything is built through [`thermal_printer::escpos::Builder`], so the
//! result is testable without hardware: dump the bytes to a file, or send them
//! straight to a USB unit.

use thermal_printer::canvas::Canvas;
use thermal_printer::escpos::{Align, Builder, Font, QrEcc, WIDTH_DOTS_80MM};
use thermal_printer::raster::{self, Raster};
use thermal_printer::text::{cols_for, wrap};

use super::record::TrialRecord;
use crate::state_machine::states::{CrossExam, NO_DEFENSE};

/// Tunables the live system supplies. M2 reads these from a `[printer]` config
/// section so the booth location and QR target can be edited on-site (the booth
/// moves; the URL might point at a day-specific page) without a rebuild.
pub struct ReportOpts<'a> {
    pub width_dots: u32,
    /// Where to send people — encoded in the footer QR.
    pub qr_url: &'a str,
    /// Human-readable "find us here" line, printed under the QR. Editable on-site.
    pub booth_location: &'a str,
}

impl Default for ReportOpts<'_> {
    fn default() -> Self {
        Self {
            width_dots: WIDTH_DOTS_80MM,
            qr_url: "https://wetcourt.lol",
            booth_location: "Find the Wet Court near you",
        }
    }
}

/// Render the full keepsake transcript. Returns a finished [`Builder`]; call
/// `.bytes()` / `.build()` to get the ESC/POS stream, or hand it to a `Printer`.
pub fn render(rec: &TrialRecord, opts: &ReportOpts) -> Builder {
    let w = opts.width_dots;
    let cols = cols_for(w, Font::A);

    let mut b = Builder::new().with_width(w);
    b.init();

    seal(&mut b);
    masthead(&mut b);
    caption(&mut b, rec, cols);

    section(&mut b, "CHARGE:", &rec.charge, cols);
    rule(&mut b, cols);

    section(&mut b, "PLEA OF THE ACCUSED:", &plea_text(rec), cols);
    rule(&mut b, cols);

    if let Some(cx) = &rec.cross {
        cross(&mut b, cx, cols);
        rule(&mut b, cols);
    }

    let delib_label = format!("DELIBERATION OF THE HON. {}:", asciify(&rec.judge_name).to_uppercase());
    section(&mut b, &delib_label, &rec.deliberation, cols);

    heavy_rule(&mut b, cols);
    verdict(&mut b, rec);
    heavy_rule(&mut b, cols);

    if rec.guilty {
        moment_of_justice(&mut b, w, rec.still_jpeg.as_deref());
    }

    footer(&mut b, rec, opts);

    b.align(Align::Left).feed(2).cut();
    b
}

// ---- sections ---------------------------------------------------------------

/// The court seal: a procedurally drawn wax-seal emblem (rings, sunburst, a
/// central water-drop). A real dithered logo can later replace this via
/// `raster::from_image`; the emblem keeps the masthead "official" with no asset.
fn seal(b: &mut Builder) {
    let r = draw_seal();
    b.align(Align::Center)
        .raster_banded(&r.bits, r.width_bytes, r.height, 64)
        .feed(1);
}

fn masthead(b: &mut Builder) {
    b.align(Align::Center)
        .bold(true)
        .size(2, 2)
        .line("WET COURT")
        .size(1, 2)
        .line("OF APPEALS")
        .size(1, 1)
        .bold(false)
        .font(Font::B)
        .line("* OFFICIAL TRIAL RECORD *")
        .font(Font::A);
}

/// "IN THE MATTER OF / THE PEOPLE v. DEFENDANT #NNNN / aka ...", plus the case
/// number and timestamp row.
fn caption(b: &mut Builder, rec: &TrialRecord, cols: usize) {
    heavy_rule(b, cols);
    b.align(Align::Center)
        .bold(true)
        .line("IN THE MATTER OF")
        .line(&format!("THE PEOPLE v. DEFENDANT #{:04}", rec.case_no))
        .bold(false)
        .font(Font::B)
        .line(&format!("aka \"{}\"", asciify(&rec.docket_alias())))
        .line(&format!("Case No. {}   {}", rec.case_label(), rec.display_time()))
        .font(Font::A);
    heavy_rule(b, cols);
}

/// A bold left-aligned label followed by the word-wrapped body, indented two
/// spaces for readability. The transcript's verbatim sections all go through here.
fn section(b: &mut Builder, label: &str, body: &str, cols: usize) {
    b.align(Align::Left).bold(true).line(label).bold(false);
    for l in wrap(&asciify(body), cols.saturating_sub(2).max(1)) {
        b.line(&format!("  {l}"));
    }
}

fn cross(b: &mut Builder, cx: &CrossExam, cols: usize) {
    b.align(Align::Left).bold(true).line("CROSS-EXAMINATION:").bold(false);
    for l in wrap(&format!("THE COURT: {}", asciify(&cx.question)), cols) {
        b.line(&l);
    }
    for l in wrap(&format!("THE DEFENDANT: {}", asciify(&cx.answer)), cols) {
        b.line(&l);
    }
}

fn verdict(b: &mut Builder, rec: &TrialRecord) {
    b.align(Align::Center).bold(true).line("- VERDICT -").bold(false).feed(1);
    if rec.guilty {
        // Inverse + magnified — the line everyone photographs. Padding spaces
        // give the white-on-black bar some breathing room.
        b.inverse(true).size(2, 2).line("  GUILTY  ").size(1, 1).inverse(false);
    } else {
        b.size(2, 2).line("NOT GUILTY").size(1, 1);
    }
    b.feed(1)
        .font(Font::B)
        .line(&format!("\"{}\"", asciify(&rec.remarks)))
        .font(Font::A);
    // The deciding factor the judge named — the keepsake's "why".
    if let Some(kf) = rec.key_factor.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        b.feed(1)
            .font(Font::B)
            .line(&format!("WHAT DECIDED IT: {}", asciify(kf)))
            .font(Font::A);
    }
}

/// Reserved photo slot (guilty only). M1 draws a framed reticle placeholder so
/// the layout and paper feed match the final receipt; M3 replaces the frame
/// with the dithered firing-still from the vision service.
fn moment_of_justice(b: &mut Builder, w: u32, still: Option<&[u8]>) {
    b.align(Align::Center).bold(true).line("-- MOMENT OF JUSTICE --").bold(false);
    // Dither the captured blast frame to 1-bit at printer width; fall back to the
    // reticle placeholder when there's no still (capture off / failed).
    let captured = still.and_then(|bytes| raster::from_bytes(bytes, w, raster::Options::default()).ok());
    match captured {
        Some(r) => {
            b.align(Align::Center)
                .raster_banded(&r.bits, r.width_bytes, r.height, 64);
        }
        None => {
            let r = placeholder_frame(w);
            b.align(Align::Center)
                .raster_banded(&r.bits, r.width_bytes, r.height, 64)
                .font(Font::B)
                .line("[ evidentiary still - vision capture pending ]")
                .font(Font::A);
        }
    }
}

fn footer(b: &mut Builder, rec: &TrialRecord, opts: &ReportOpts) {
    b.feed(1).align(Align::Center);
    b.qr(opts.qr_url, 6, QrEcc::M).feed(1);
    b.bold(true).line("CATCH A TRIAL YOURSELF").bold(false);
    b.font(Font::B).line(&asciify(opts.booth_location)).font(Font::A);
    b.line("#WetCourtOfAppeals");
    b.feed(1)
        .font(Font::B)
        .line(&format!("Presiding: Hon. {}", asciify(&rec.judge_name)))
        .font(Font::A);
}

// ---- helpers ----------------------------------------------------------------

fn rule(b: &mut Builder, cols: usize) {
    b.align(Align::Left).line(&"-".repeat(cols));
}

fn heavy_rule(b: &mut Builder, cols: usize) {
    b.align(Align::Left).line(&"=".repeat(cols));
}

/// The plea body, with the silent-defendant sentinel turned into a court-style
/// note and a real plea wrapped in quotes.
fn plea_text(rec: &TrialRecord) -> String {
    if rec.plea.trim() == NO_DEFENSE {
        "[The accused offered no defense.]".to_string()
    } else {
        format!("\"{}\"", asciify(rec.plea.trim()))
    }
}

/// Fold the smart punctuation that LLM/STT output is full of down to the ASCII
/// the printer can render — otherwise [`Builder::text`] would stamp each curly
/// quote / em-dash as a literal `?`.
fn asciify(s: &str) -> String {
    let s = s.replace('\u{2026}', "..."); // ellipsis
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
            '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '\u{00A0}' => ' ',
            c if c.is_ascii() => c,
            _ => ' ', // anything else we can't print: a space beats a '?'
        })
        .collect()
}

// ---- drawn imagery ----------------------------------------------------------

/// The procedural court seal: two concentric rings, a sunburst of ticks, and a
/// filled water-drop at the centre. Pure geometry — no font, no asset.
fn draw_seal() -> Raster {
    use std::f32::consts::TAU;
    let size = 192i32;
    let mut c = Canvas::new(size as u32, size as u32);
    let (cx, cy) = (size / 2, size / 2);

    // Concentric rings.
    c.circle(cx, cy, 92, true);
    c.circle(cx, cy, 88, true);
    c.circle(cx, cy, 64, true);

    // Sunburst ticks between the inner ring and the outer rings.
    let ticks = 24;
    for i in 0..ticks {
        let a = TAU * (i as f32) / (ticks as f32);
        let (s, co) = a.sin_cos();
        let x0 = cx + (co * 68.0) as i32;
        let y0 = cy + (s * 68.0) as i32;
        let x1 = cx + (co * 86.0) as i32;
        let y1 = cy + (s * 86.0) as i32;
        c.line(x0, y0, x1, y1, true);
    }

    // Central water-drop: a filled disc with a triangular point on top.
    let dr = 24i32;
    let dcy = cy + 12;
    for yy in -dr..=dr {
        for xx in -dr..=dr {
            if xx * xx + yy * yy <= dr * dr {
                c.set(cx + xx, dcy + yy, true);
            }
        }
    }
    let tip_y = dcy - dr - 30;
    let base_y = dcy - dr / 2;
    for yy in tip_y..=base_y {
        let t = (yy - tip_y) as f32 / ((base_y - tip_y).max(1)) as f32;
        let half = (t * dr as f32) as i32;
        c.hline(cx - half, cx + half, yy, true);
    }

    c.raster()
}

/// The reserved-photo placeholder: a framed box with a centre reticle, so the
/// guilty receipt visibly holds space for the firing-still that M3 will drop in.
fn placeholder_frame(w: u32) -> Raster {
    let pw = (w * 5 / 6).max(8);
    let ph = 170u32;
    let mut c = Canvas::new(pw, ph);
    let (iw, ih) = (pw as i32, ph as i32);

    c.rect(2, 2, iw - 4, ih - 4, true);
    c.rect(6, 6, iw - 12, ih - 12, true);

    let (cx, cy) = (iw / 2, ih / 2);
    c.hline(cx - 22, cx + 22, cy, true);
    c.vline(cx, cy - 22, cy + 22, true);
    c.circle(cx, cy, 26, true);

    c.raster()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dump(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn renders_both_outcomes_to_files() {
        let opts = ReportOpts::default();
        let guilty = render(&TrialRecord::sample_guilty(), &opts).build();
        let acquitted = render(&TrialRecord::sample_acquitted(), &opts).build();

        // Sanity: real content, and the guilty receipt is longer (verdict bar +
        // photo slot) than the acquittal.
        assert!(guilty.len() > 200, "guilty receipt too short: {}", guilty.len());
        assert!(acquitted.len() > 200, "acquittal too short: {}", acquitted.len());
        assert!(guilty.len() > acquitted.len());

        let pg = dump("wetcourt_receipt_guilty.escpos", &guilty);
        let pa = dump("wetcourt_receipt_acquitted.escpos", &acquitted);
        eprintln!("wrote {} ({} bytes)", pg.display(), guilty.len());
        eprintln!("wrote {} ({} bytes)", pa.display(), acquitted.len());

        // Opt-in real-hardware proof: `WETCOURT_PRINT_USB=1 cargo test ...`.
        if std::env::var("WETCOURT_PRINT_USB").is_ok() {
            let printer = thermal_printer::Printer::connect().expect("open USB printer");
            printer.usb().write(&guilty).expect("print guilty receipt");
        }
    }

    #[test]
    fn asciify_folds_smart_punctuation() {
        let got = asciify("\u{201C}wet\u{201D} \u{2014} it\u{2019}s fine\u{2026}");
        assert_eq!(got, "\"wet\" - it's fine...");
    }

    #[test]
    fn silent_defendant_gets_a_note() {
        let mut rec = TrialRecord::sample_acquitted();
        rec.plea = NO_DEFENSE.to_string();
        assert_eq!(plea_text(&rec), "[The accused offered no defense.]");
    }
}
