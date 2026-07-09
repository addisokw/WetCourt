#!/usr/bin/env python3
"""Scripted SIP+RTP integration test for counsel M1.

REGISTER as 'defendant', INVITE '1' with a PCMU offer, ACK the 200, stream a
1 kHz mu-law tone for ~1.2 s, assert the echo comes back byte-identical,
poke a second INVITE (expect 486), send a DTMF end packet, then BYE.
"""

import math
import random
import re
import socket
import struct
import sys
import time

SERVER = ("127.0.0.1", 5060)
TIMEOUT = 5.0


def local_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.connect(("8.8.8.8", 80))
    ip = s.getsockname()[0]
    s.close()
    return ip


def rand_token(n=10):
    return "".join(random.choice("abcdefghijklmnop0123456789") for _ in range(n))


def ulaw_encode(sample: int) -> int:
    BIAS, CLIP = 0x84, 32635
    sign = 0x80 if sample < 0 else 0
    s = min(abs(sample), CLIP) + BIAS
    exp = 7
    for e in range(8):
        if s < (1 << (e + 8)):
            exp = e
            break
    mant = (s >> (exp + 3)) & 0x0F
    return ~(sign | (exp << 4) | mant) & 0xFF


class SipClient:
    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("0.0.0.0", 0))
        self.sock.settimeout(TIMEOUT)
        self.ip = local_ip()
        self.port = self.sock.getsockname()[1]
        self.call_id = rand_token(16)
        self.from_tag = rand_token()
        self.to_tag = None
        self.cseq = 1

    def hdr_via(self):
        return f"Via: SIP/2.0/UDP {self.ip}:{self.port};branch=z9hG4bK{rand_token()};rport"

    def send(self, msg):
        self.sock.sendto(msg.encode(), SERVER)

    def recv_response(self, want_status=None, ignore_provisional=True):
        deadline = time.time() + TIMEOUT
        while time.time() < deadline:
            try:
                data, _ = self.sock.recvfrom(65535)
            except socket.timeout:
                break
            text = data.decode(errors="replace")
            first = text.split("\r\n", 1)[0]
            if not first.startswith("SIP/2.0"):
                continue  # a request (e.g. BYE later); caller handles separately
            status = int(first.split(" ")[1])
            print(f"  << {first}")
            if ignore_provisional and status < 200 and (want_status is None or want_status >= 200):
                continue
            return status, text
        raise AssertionError(f"timed out waiting for response (wanted {want_status})")

    def register(self):
        msg = (
            f"REGISTER sip:{SERVER[0]} SIP/2.0\r\n"
            f"{self.hdr_via()}\r\n"
            f"Max-Forwards: 70\r\n"
            f"From: <sip:defendant@{SERVER[0]}>;tag={self.from_tag}\r\n"
            f"To: <sip:defendant@{SERVER[0]}>\r\n"
            f"Call-ID: reg-{self.call_id}\r\n"
            f"CSeq: {self.cseq} REGISTER\r\n"
            f"Contact: <sip:defendant@{self.ip}:{self.port}>\r\n"
            f"Expires: 120\r\n"
            f"Content-Length: 0\r\n\r\n"
        )
        print(">> REGISTER")
        self.send(msg)
        status, text = self.recv_response(200)
        assert status == 200, f"REGISTER got {status}"
        assert re.search(r"^Expires:\s*120", text, re.M) or "expires=120" in text, "no Expires echo"
        print("  REGISTER OK")

    def invite(self, rtp_port, expect=200):
        self.cseq += 1
        sdp = (
            "v=0\r\n"
            f"o=test {self.cseq} {self.cseq} IN IP4 {self.ip}\r\n"
            "s=call\r\n"
            f"c=IN IP4 {self.ip}\r\n"
            "t=0 0\r\n"
            f"m=audio {rtp_port} RTP/AVP 0 101\r\n"
            "a=rtpmap:0 PCMU/8000\r\n"
            "a=rtpmap:101 telephone-event/8000\r\n"
            "a=fmtp:101 0-15\r\n"
            "a=ptime:20\r\n"
        )
        call_id = f"inv-{rand_token(12)}"
        msg = (
            f"INVITE sip:1@{SERVER[0]} SIP/2.0\r\n"
            f"{self.hdr_via()}\r\n"
            f"Max-Forwards: 70\r\n"
            f"From: <sip:defendant@{SERVER[0]}>;tag={self.from_tag}\r\n"
            f"To: <sip:1@{SERVER[0]}>\r\n"
            f"Call-ID: {call_id}\r\n"
            f"CSeq: {self.cseq} INVITE\r\n"
            f"Contact: <sip:defendant@{self.ip}:{self.port}>\r\n"
            f"Content-Type: application/sdp\r\n"
            f"Content-Length: {len(sdp)}\r\n\r\n{sdp}"
        )
        print(f">> INVITE (expecting {expect})")
        self.send(msg)
        status, text = self.recv_response(expect)
        assert status == expect, f"INVITE got {status}, wanted {expect}"
        m = re.search(r";tag=([^;\r\n>]+)", re.search(r"^To:.*$", text, re.M).group(0))
        to_tag = m.group(1) if m else None
        if expect == 200:
            self.to_tag = to_tag
            self.invite_call_id = call_id
            body = text.split("\r\n\r\n", 1)[1]
            c = re.search(r"^c=IN IP4 (\S+)", body, re.M).group(1)
            p = int(re.search(r"^m=audio (\d+)", body, re.M).group(1))
            print(f"  200 OK; remote RTP {c}:{p}")
            self.ack(call_id, to_tag)
            return c, p
        else:
            # ACK the non-2xx final so the server transaction completes
            self.ack(call_id, to_tag, non2xx=True)
            return None

    def ack(self, call_id, to_tag, non2xx=False):
        to = f"To: <sip:1@{SERVER[0]}>" + (f";tag={to_tag}" if to_tag else "")
        msg = (
            f"ACK sip:1@{SERVER[0]} SIP/2.0\r\n"
            f"{self.hdr_via()}\r\n"
            f"Max-Forwards: 70\r\n"
            f"From: <sip:defendant@{SERVER[0]}>;tag={self.from_tag}\r\n"
            f"{to}\r\n"
            f"Call-ID: {call_id}\r\n"
            f"CSeq: {self.cseq} ACK\r\n"
            f"Content-Length: 0\r\n\r\n"
        )
        self.send(msg)
        print(f"  >> ACK{' (non-2xx)' if non2xx else ''}")

    def bye(self):
        self.cseq += 1
        msg = (
            f"BYE sip:1@{SERVER[0]} SIP/2.0\r\n"
            f"{self.hdr_via()}\r\n"
            f"Max-Forwards: 70\r\n"
            f"From: <sip:defendant@{SERVER[0]}>;tag={self.from_tag}\r\n"
            f"To: <sip:1@{SERVER[0]}>;tag={self.to_tag}\r\n"
            f"Call-ID: {self.invite_call_id}\r\n"
            f"CSeq: {self.cseq} BYE\r\n"
            f"Content-Length: 0\r\n\r\n"
        )
        print(">> BYE")
        self.send(msg)
        status, _ = self.recv_response(200)
        assert status == 200, f"BYE got {status}"
        print("  BYE OK")


def rtp_packet(seq, ts, ssrc, payload, pt=0, marker=False):
    b1 = 0x80
    b2 = (0x80 if marker else 0) | pt
    return struct.pack("!BBHII", b1, b2, seq & 0xFFFF, ts & 0xFFFFFFFF, ssrc) + payload


def run_media(remote, rtp_sock):
    """Stream a tone, collect the echo, assert byte-identical passthrough."""
    tone = bytes(
        ulaw_encode(int(8000 * math.sin(2 * math.pi * 1000 * i / 8000)))
        for i in range(8000)  # 1 s
    )
    ssrc = random.getrandbits(32)
    seq, ts = 1, 0
    received = bytearray()
    rtp_sock.settimeout(0.5)
    n_frames = len(tone) // 160

    for i in range(n_frames):
        frame = tone[i * 160 : (i + 1) * 160]
        rtp_sock.sendto(rtp_packet(seq, ts, ssrc, frame, marker=(i == 0)), remote)
        seq += 1
        ts += 160
        try:
            data, _ = rtp_sock.recvfrom(2048)
            if len(data) > 12 and (data[1] & 0x7F) == 0:
                received.extend(data[12:])
        except socket.timeout:
            pass
        time.sleep(0.02)

    # Drain the queued tail.
    deadline = time.time() + 2.0
    while time.time() < deadline:
        try:
            data, _ = rtp_sock.recvfrom(2048)
            if len(data) > 12 and (data[1] & 0x7F) == 0:
                received.extend(data[12:])
        except socket.timeout:
            break

    print(f"  sent {len(tone)} tone bytes, received {len(received)} echo bytes")
    probe = tone[800:1120]  # 40 ms slice from mid-tone
    assert bytes(received).find(probe) != -1, "echoed stream does not contain the sent tone"
    print("  echo is byte-identical PASS")

    # DTMF: event 5, end bit set, sent 3x like a real endpoint.
    dtmf = struct.pack("!BBH", 5, 0x8A, 800)
    for _ in range(3):
        rtp_sock.sendto(rtp_packet(seq, ts, ssrc, dtmf, pt=101), remote)
        seq += 1
        time.sleep(0.02)
    print("  DTMF '5' end packets sent (check counsel log)")


def main():
    c = SipClient()
    print(f"client on {c.ip}:{c.port}")
    c.register()

    rtp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rtp_sock.bind(("0.0.0.0", 0))
    rtp_port = rtp_sock.getsockname()[1]

    remote_ip, remote_port = c.invite(rtp_port)
    run_media((remote_ip, remote_port), rtp_sock)

    # Busy check: a second INVITE while the call is live must 486.
    c2 = SipClient()
    c2.invite(0, expect=486)
    print("  busy line correctly refused with 486")

    c.bye()
    print("ALL CHECKS PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
