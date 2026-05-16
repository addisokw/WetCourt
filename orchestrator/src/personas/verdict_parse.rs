use std::sync::OnceLock;

use regex::Regex;

/// Parsed output of a judge deliberation, independent of the production
/// `Verdict` struct in `state_machine::states` (which carries fields irrelevant
/// to the persona /test endpoint).
#[derive(Debug, Clone)]
pub struct ParsedDeliberation {
    pub deliberation: String,
    pub guilty: bool,
    pub intensity: u8,
}

static VERDICT_RE: OnceLock<Regex> = OnceLock::new();
static INTENSITY_RE: OnceLock<Regex> = OnceLock::new();
static MARKER_LINE_RE: OnceLock<Regex> = OnceLock::new();

pub fn parse(text: &str) -> Option<ParsedDeliberation> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());
    let ire = INTENSITY_RE.get_or_init(|| Regex::new(r"(?i)INTENSITY:\s*([1-5])").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");
    let intensity = ire
        .captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(if guilty { 3 } else { 0 });

    Some(ParsedDeliberation {
        deliberation: strip_marker_lines(text),
        guilty,
        intensity,
    })
}

fn strip_marker_lines(text: &str) -> String {
    let re = MARKER_LINE_RE
        .get_or_init(|| Regex::new(r"(?im)^\s*(VERDICT|INTENSITY):.*$\n?").unwrap());
    re.replace_all(text, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guilty() {
        let p = parse("Pathetic.\nVERDICT: GUILTY\nINTENSITY: 4").unwrap();
        assert!(p.guilty);
        assert_eq!(p.intensity, 4);
        assert!(!p.deliberation.contains("VERDICT"));
    }

    #[test]
    fn defaults_intensity_on_guilty() {
        let p = parse("Pathetic.\nVERDICT: GUILTY").unwrap();
        assert_eq!(p.intensity, 3);
    }

    #[test]
    fn acquitted() {
        let p = parse("Clever.\nVERDICT: ACQUITTED").unwrap();
        assert!(!p.guilty);
        assert_eq!(p.intensity, 0);
    }

    #[test]
    fn no_marker_returns_none() {
        assert!(parse("no marker").is_none());
    }
}
