use std::sync::OnceLock;

use regex::Regex;

/// Parsed output of a judge deliberation, independent of the production
/// `Verdict` struct in `state_machine::states` (which carries fields irrelevant
/// to the persona /test endpoint).
#[derive(Debug, Clone)]
pub struct ParsedDeliberation {
    pub deliberation: String,
    pub guilty: bool,
    /// The 2–4 word deciding factor from the `KEY_FACTOR:` marker, if present.
    pub key_factor: Option<String>,
}

static VERDICT_RE: OnceLock<Regex> = OnceLock::new();
static KEY_FACTOR_RE: OnceLock<Regex> = OnceLock::new();
static MARKER_LINE_RE: OnceLock<Regex> = OnceLock::new();

pub fn parse(text: &str) -> Option<ParsedDeliberation> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());
    let kre = KEY_FACTOR_RE.get_or_init(|| Regex::new(r"(?im)^\s*KEY_FACTOR:\s*(.+)$").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");
    let key_factor = kre
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|v| v.as_str().trim().to_string())
        .filter(|v| !v.is_empty());

    Some(ParsedDeliberation {
        deliberation: strip_marker_lines(text),
        guilty,
        key_factor,
    })
}

fn strip_marker_lines(text: &str) -> String {
    // Strip the VERDICT / KEY_FACTOR / REASON marker lines; also drop any stray
    // INTENSITY line a model might still emit so it's never shown or spoken.
    let re = MARKER_LINE_RE
        .get_or_init(|| Regex::new(r"(?im)^\s*(VERDICT|INTENSITY|KEY_FACTOR|REASON):.*$\n?").unwrap());
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

    #[test]
    fn parses_key_factor_and_strips_markers() {
        let p = parse("Clever technicality.\nVERDICT: ACQUITTED\nKEY_FACTOR: clever technicality\nREASON: The loophole held up.").unwrap();
        assert!(!p.guilty);
        assert_eq!(p.key_factor.as_deref(), Some("clever technicality"));
        assert!(!p.deliberation.contains("KEY_FACTOR"));
        assert!(!p.deliberation.contains("REASON"));
        assert!(p.deliberation.contains("technicality"));
    }

    #[test]
    fn key_factor_absent_is_none() {
        let p = parse("Nope.\nVERDICT: GUILTY").unwrap();
        assert!(p.key_factor.is_none());
    }
}
