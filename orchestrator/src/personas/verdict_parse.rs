use std::sync::OnceLock;

use regex::Regex;

/// Parsed output of a judge deliberation, independent of the production
/// `Verdict` struct in `state_machine::states` (which carries fields irrelevant
/// to the persona /test endpoint).
#[derive(Debug, Clone)]
pub struct ParsedDeliberation {
    pub deliberation: String,
    pub guilty: bool,
}

static VERDICT_RE: OnceLock<Regex> = OnceLock::new();
static MARKER_LINE_RE: OnceLock<Regex> = OnceLock::new();

pub fn parse(text: &str) -> Option<ParsedDeliberation> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");

    Some(ParsedDeliberation {
        deliberation: strip_marker_lines(text),
        guilty,
    })
}

fn strip_marker_lines(text: &str) -> String {
    // Strip the VERDICT marker line; also drop any stray INTENSITY line a model
    // might still emit so it's never shown or spoken.
    let re = MARKER_LINE_RE
        .get_or_init(|| Regex::new(r"(?im)^\s*(VERDICT|INTENSITY):.*$\n?").unwrap());
    re.replace_all(text, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guilty() {
        let p = parse("Pathetic.\nVERDICT: GUILTY").unwrap();
        assert!(p.guilty);
        assert!(!p.deliberation.contains("VERDICT"));
    }

    #[test]
    fn strips_stray_intensity_line() {
        let p = parse("Pathetic.\nVERDICT: GUILTY\nINTENSITY: 4").unwrap();
        assert!(p.guilty);
        assert!(!p.deliberation.contains("INTENSITY"));
    }

    #[test]
    fn acquitted() {
        let p = parse("Clever.\nVERDICT: ACQUITTED").unwrap();
        assert!(!p.guilty);
    }

    #[test]
    fn no_marker_returns_none() {
        assert!(parse("no marker").is_none());
    }
}
