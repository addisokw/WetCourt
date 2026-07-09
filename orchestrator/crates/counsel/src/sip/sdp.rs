//! Minimal SDP for one use case: PCMU/8000 + RFC2833 telephone-event over a
//! single audio stream. Hand-rolled — parsing is a handful of line prefixes
//! and building is a format string; the full grammar buys nothing here.

use anyhow::{bail, Context, Result};

#[derive(Debug, PartialEq)]
pub struct RemoteMedia {
    pub ip: String,
    pub port: u16,
    /// Negotiated telephone-event payload type, if offered.
    pub dtmf_pt: Option<u8>,
}

/// Parse the peer's SDP (offer or answer): connection address, audio port,
/// PCMU presence, telephone-event payload type.
pub fn parse(body: &str) -> Result<RemoteMedia> {
    let mut ip = None;
    let mut port = None;
    let mut fmts: Vec<u8> = Vec::new();
    let mut dtmf_pt = None;

    for line in body.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("c=") {
            // c=IN IP4 192.168.50.216
            let addr = rest.split_whitespace().nth(2);
            if let Some(a) = addr {
                ip = Some(a.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("m=audio ") {
            // m=audio 5004 RTP/AVP 0 101
            let mut parts = rest.split_whitespace();
            port = parts.next().and_then(|p| p.parse::<u16>().ok());
            fmts = parts.skip(1).filter_map(|f| f.parse().ok()).collect();
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            // a=rtpmap:101 telephone-event/8000
            let mut parts = rest.split_whitespace();
            let pt = parts.next().and_then(|p| p.parse::<u8>().ok());
            let codec = parts.next().unwrap_or("");
            if codec.to_ascii_lowercase().starts_with("telephone-event") {
                dtmf_pt = pt;
            }
        }
    }

    let ip = ip.context("SDP has no c= connection line")?;
    let port = port.context("SDP has no m=audio line")?;
    if !fmts.contains(&0) {
        bail!("peer does not offer PCMU (fmts: {fmts:?})");
    }
    Ok(RemoteMedia { ip, port, dtmf_pt })
}

/// Build our SDP (offer or answer): PCMU + telephone-event 101, ptime 20.
pub fn build(advertise_ip: &str, rtp_port: u16, session_id: u32) -> String {
    format!(
        "v=0\r\n\
         o=counsel {session_id} {session_id} IN IP4 {advertise_ip}\r\n\
         s=wetcourt lawyer line\r\n\
         c=IN IP4 {advertise_ip}\r\n\
         t=0 0\r\n\
         m=audio {rtp_port} RTP/AVP 0 101\r\n\
         a=rtpmap:0 PCMU/8000\r\n\
         a=rtpmap:101 telephone-event/8000\r\n\
         a=fmtp:101 0-15\r\n\
         a=ptime:20\r\n\
         a=sendrecv\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape of a Grandstream HT801 offer (trimmed).
    const HT801_OFFER: &str = "v=0\r\n\
        o=defendant 8000 8000 IN IP4 192.168.50.216\r\n\
        s=SIP Call\r\n\
        c=IN IP4 192.168.50.216\r\n\
        t=0 0\r\n\
        m=audio 5004 RTP/AVP 0 101\r\n\
        a=sendrecv\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=ptime:20\r\n\
        a=rtpmap:101 telephone-event/8000\r\n\
        a=fmtp:101 0-15\r\n";

    #[test]
    fn parses_ht801_style_offer() {
        let m = parse(HT801_OFFER).unwrap();
        assert_eq!(
            m,
            RemoteMedia {
                ip: "192.168.50.216".into(),
                port: 5004,
                dtmf_pt: Some(101)
            }
        );
    }

    #[test]
    fn parses_offer_without_dtmf() {
        let body = "v=0\r\nc=IN IP4 10.0.0.5\r\nm=audio 7078 RTP/AVP 0 8\r\na=rtpmap:0 PCMU/8000\r\n";
        let m = parse(body).unwrap();
        assert_eq!(m.dtmf_pt, None);
        assert_eq!(m.port, 7078);
    }

    #[test]
    fn rejects_no_pcmu() {
        let body = "v=0\r\nc=IN IP4 10.0.0.5\r\nm=audio 7078 RTP/AVP 8\r\n";
        assert!(parse(body).is_err());
    }

    #[test]
    fn build_roundtrips_through_parse() {
        let sdp = build("192.168.50.10", 40000, 42);
        let m = parse(&sdp).unwrap();
        assert_eq!(m.ip, "192.168.50.10");
        assert_eq!(m.port, 40000);
        assert_eq!(m.dtmf_pt, Some(101));
    }
}
