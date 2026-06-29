//! Light markup renderer for performative / piped printing.
//!
//! Converts a text stream into ESC/POS via [`Builder`]. Supports a small,
//! receipt-friendly subset of Markdown plus a few `.directives` aimed at live
//! printing. Anything it doesn't recognize is wrapped and printed as plain text,
//! so piping a raw `.md` file Just Works.
//!
//! Markdown:  `# / ## / ###` headings, `- / * / +` bullets, `---` rules,
//!            ``` fenced code (switches to Font B), `| ... |` tables.
//! Directives (one per line):
//!   .center / .left / .right     set alignment
//!   .bold on|off                 toggle bold
//!   .font a|b                    switch font
//!   .size W H                    character magnification (1..8)
//!   .rule                        horizontal rule
//!   .feed [n]                    feed n lines (default 1)
//!   .qr <data>                   print a QR code
//!   .barcode <data>              print a CODE128 barcode
//!   .cut                         feed and cut

use crate::escpos::{Align, Barcode, Builder, Font, QrEcc};

/// Characters per line for a given font at this printer width.
/// Font A glyphs are ~12 dots wide, Font B ~9.
pub fn cols_for(width_dots: u32, font: Font) -> usize {
    let per = match font {
        Font::A => 12,
        Font::B => 9,
    };
    (width_dots as usize / per).max(1)
}

/// Word-wrap `s` to `cols`, hard-splitting any word longer than a line.
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if word.len() > cols {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut w = word;
            while w.len() > cols {
                lines.push(w[..cols].to_string());
                w = &w[cols..];
            }
            cur = w.to_string();
        } else if cur.is_empty() {
            cur = word.to_string();
        } else if cur.len() + 1 + word.len() <= cols {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Render plain text: just wrap and print, no markup interpretation.
pub fn render_plain(b: &mut Builder, input: &str, width_dots: u32) {
    let cols = cols_for(width_dots, Font::A);
    b.init().align(Align::Left).font(Font::A);
    for raw in input.lines() {
        if raw.trim().is_empty() {
            b.feed(1);
        } else {
            for l in wrap(raw, cols) {
                b.line(&l);
            }
        }
    }
}

/// Render the markup subset described in the module docs.
pub fn render_markup(b: &mut Builder, input: &str, width_dots: u32) {
    let cols_a = cols_for(width_dots, Font::A);
    let cols_b = cols_for(width_dots, Font::B);
    let mut in_code = false;

    b.init().align(Align::Left).font(Font::A);

    for raw in input.lines() {
        let trimmed = raw.trim_end();

        // Fenced code blocks toggle Font B and print verbatim.
        if trimmed.trim_start().starts_with("```") {
            in_code = !in_code;
            b.font(if in_code { Font::B } else { Font::A });
            continue;
        }
        if in_code {
            for chunk in hard_chunks(trimmed, cols_b) {
                b.line(&chunk);
            }
            continue;
        }

        // `.directives`
        if let Some(rest) = trimmed.strip_prefix('.') {
            apply_directive(b, rest, width_dots);
            continue;
        }

        let t = trimmed.trim_start();

        if let Some(h) = t.strip_prefix("# ") {
            b.feed(1).align(Align::Center).bold(true).size(1, 2);
            for l in wrap(&strip_inline(h), cols_a / 2) {
                b.line(&l);
            }
            b.size(1, 1).bold(false).align(Align::Left);
        } else if let Some(h) = t.strip_prefix("## ").or_else(|| t.strip_prefix("### ")) {
            b.feed(1).bold(true);
            for l in wrap(&strip_inline(h), cols_a) {
                b.line(&l);
            }
            b.bold(false);
        } else if is_rule(t) {
            rule(b, cols_a);
        } else if t.starts_with('|') && t.ends_with('|') {
            // Markdown table row. The `|---|:--|` separator becomes a rule;
            // content rows print verbatim in Font B so columns roughly line up.
            if t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                rule(b, cols_b);
            } else {
                b.font(Font::B);
                for l in hard_chunks(&strip_inline(t), cols_b) {
                    b.line(&l);
                }
                b.font(Font::A);
            }
        } else if let Some(item) = strip_bullet(t) {
            let lines = wrap(&strip_inline(item), cols_a.saturating_sub(2).max(1));
            for (i, l) in lines.iter().enumerate() {
                b.line(&format!("{}{}", if i == 0 { "* " } else { "  " }, l));
            }
        } else if t.is_empty() {
            b.feed(1);
        } else {
            for l in wrap(&strip_inline(t), cols_a) {
                b.line(&l);
            }
        }
    }
    if in_code {
        b.font(Font::A);
    }
}

// ---- helpers ---------------------------------------------------------------

fn apply_directive(b: &mut Builder, rest: &str, width_dots: u32) {
    let mut it = rest.splitn(2, ' ');
    let cmd = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("").trim();
    match cmd {
        "center" => {
            b.align(Align::Center);
        }
        "left" => {
            b.align(Align::Left);
        }
        "right" => {
            b.align(Align::Right);
        }
        "bold" => {
            b.bold(arg != "off");
        }
        "font" => {
            b.font(if arg.eq_ignore_ascii_case("b") {
                Font::B
            } else {
                Font::A
            });
        }
        "size" => {
            let mut p = arg.split_whitespace();
            let w = p.next().and_then(|x| x.parse().ok()).unwrap_or(1);
            let h = p.next().and_then(|x| x.parse().ok()).unwrap_or(w);
            b.size(w, h);
        }
        "rule" => rule(b, cols_for(width_dots, Font::A)),
        "feed" => {
            b.feed(arg.parse().unwrap_or(1));
        }
        "qr" => {
            b.align(Align::Center)
                .qr(arg, 7, QrEcc::M)
                .feed(1)
                .align(Align::Left);
        }
        "barcode" => {
            b.align(Align::Center)
                .barcode_style(80, 3)
                .barcode(Barcode::Code128, arg)
                .feed(1)
                .align(Align::Left);
        }
        "cut" => {
            b.feed(2).cut();
        }
        _ => {} // unknown directive: ignore
    }
}

fn rule(b: &mut Builder, cols: usize) {
    b.line(&"-".repeat(cols));
}

fn is_rule(t: &str) -> bool {
    (t.len() >= 3 && t.chars().all(|c| c == '-'))
        || (t.len() >= 3 && t.chars().all(|c| c == '*'))
        || (t.len() >= 3 && t.chars().all(|c| c == '='))
}

fn strip_bullet(t: &str) -> Option<&str> {
    t.strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
}

/// Remove inline markdown that the printer can't render (`**bold**`, `` `code` ``).
fn strip_inline(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// Split a string into verbatim chunks of at most `cols` bytes (ASCII).
fn hard_chunks(s: &str, cols: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let cols = cols.max(1);
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + cols).min(bytes.len());
        out.push(String::from_utf8_lossy(&bytes[i..end]).into_owned());
        i = end;
    }
    out
}
