//! RFC 2833/4733 telephone-event parsing. The HT801 sends each digit as a
//! stream of event packets sharing one RTP timestamp; the final three carry
//! the end bit. We emit one digit per distinct (timestamp) on its first
//! end-bit packet.

pub struct DtmfParser {
    last_end_ts: Option<u32>,
}

impl DtmfParser {
    pub fn new() -> Self {
        Self { last_end_ts: None }
    }

    /// Feed one telephone-event RTP payload (with its RTP timestamp).
    /// Returns a digit exactly once per completed key press.
    pub fn push(&mut self, rtp_timestamp: u32, payload: &[u8]) -> Option<char> {
        if payload.len() < 4 {
            return None;
        }
        let event = payload[0];
        let end = payload[1] & 0x80 != 0;
        if !end {
            return None;
        }
        if self.last_end_ts == Some(rtp_timestamp) {
            return None; // retransmitted end packet
        }
        self.last_end_ts = Some(rtp_timestamp);
        event_to_char(event)
    }
}

fn event_to_char(event: u8) -> Option<char> {
    Some(match event {
        0..=9 => (b'0' + event) as char,
        10 => '*',
        11 => '#',
        12..=15 => (b'A' + event - 12) as char,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(event: u8, end: bool, duration: u16) -> Vec<u8> {
        let e = if end { 0x80u8 } else { 0 } | 10; // volume 10
        vec![event, e, (duration >> 8) as u8, duration as u8]
    }

    #[test]
    fn digit_emitted_once_per_press() {
        let mut p = DtmfParser::new();
        // Interim packets: no emission.
        assert_eq!(p.push(1000, &pkt(1, false, 160)), None);
        assert_eq!(p.push(1000, &pkt(1, false, 320)), None);
        // End retransmitted three times: exactly one emission.
        assert_eq!(p.push(1000, &pkt(1, true, 480)), Some('1'));
        assert_eq!(p.push(1000, &pkt(1, true, 480)), None);
        assert_eq!(p.push(1000, &pkt(1, true, 480)), None);
        // Next press (new timestamp) emits again.
        assert_eq!(p.push(2600, &pkt(2, true, 480)), Some('2'));
    }

    #[test]
    fn event_mapping() {
        let mut p = DtmfParser::new();
        assert_eq!(p.push(1, &pkt(10, true, 1)), Some('*'));
        assert_eq!(p.push(2, &pkt(11, true, 1)), Some('#'));
        assert_eq!(p.push(3, &pkt(12, true, 1)), Some('A'));
        assert_eq!(p.push(4, &pkt(16, true, 1)), None); // flash etc. ignored
    }

    #[test]
    fn short_payload_ignored() {
        let mut p = DtmfParser::new();
        assert_eq!(p.push(1, &[1, 0x80]), None);
    }
}
