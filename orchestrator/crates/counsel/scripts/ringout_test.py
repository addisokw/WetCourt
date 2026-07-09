#!/usr/bin/env python3
"""M4 test: register as the ATA, trigger POST /call, answer the incoming
INVITE, and verify the lawyer speaks an opening line (mock tone)."""

import re
import socket
import sys
import threading
import time
import urllib.request

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
sys.path.insert(0, SP)
from sip_echo_test import SipClient, rtp_packet, rand_token

SILENCE = bytes([0xFF] * 160)


def energetic(p):
    return sum(1 for b in p if b not in (0xFF, 0x7F, 0xFE, 0x7E)) > len(p) * 0.3


def http_call(result):
    req = urllib.request.Request(
        __import__("os").environ.get("CALL_URL", "http://127.0.0.1:8092/call"),
        data=b'{"reason": "the verdict is in and it is not looking great"}',
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            result["status"] = r.status
            result["body"] = r.read().decode()
    except urllib.error.HTTPError as e:
        result["status"] = e.code
        result["body"] = e.read().decode()


def main():
    c = SipClient()
    print(f"phone on {c.ip}:{c.port}")
    c.register()

    rtp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rtp_sock.bind(("0.0.0.0", 0))
    rtp_port = rtp_sock.getsockname()[1]

    # Fire the control-plane call in a thread; it blocks until answered.
    result = {}
    t = threading.Thread(target=http_call, args=(result,))
    t.start()

    # Wait for the INVITE on our SIP socket.
    c.sock.settimeout(10)
    invite, addr = None, None
    while True:
        data, addr = c.sock.recvfrom(65535)
        text = data.decode(errors="replace")
        if text.startswith("INVITE"):
            invite = text
            print("  << INVITE received (phone is 'ringing')")
            break

    via = re.search(r"^Via: (.+)$", invite, re.M).group(1)
    from_h = re.search(r"^From: (.+)$", invite, re.M).group(1)
    to_h = re.search(r"^To: (.+)$", invite, re.M).group(1)
    call_id = re.search(r"^Call-ID: (.+)$", invite, re.M).group(1)
    cseq = re.search(r"^CSeq: (.+)$", invite, re.M).group(1)
    to_tag = rand_token()

    sdp = (
        "v=0\r\n"
        f"o=phone 1 1 IN IP4 {c.ip}\r\n"
        "s=answer\r\n"
        f"c=IN IP4 {c.ip}\r\n"
        "t=0 0\r\n"
        f"m=audio {rtp_port} RTP/AVP 0 101\r\n"
        "a=rtpmap:0 PCMU/8000\r\n"
        "a=rtpmap:101 telephone-event/8000\r\n"
    )

    def respond(code, name, body=""):
        extra = ""
        if body:
            extra = f"Content-Type: application/sdp\r\nContent-Length: {len(body)}\r\n"
        else:
            extra = "Content-Length: 0\r\n"
        msg = (
            f"SIP/2.0 {code} {name}\r\n"
            f"Via: {via}\r\n"
            f"From: {from_h}\r\n"
            f"To: {to_h};tag={to_tag}\r\n"
            f"Call-ID: {call_id}\r\n"
            f"CSeq: {cseq}\r\n"
            f"Contact: <sip:defendant@{c.ip}:{c.port}>\r\n"
            f"{extra}\r\n{body}"
        )
        c.sock.sendto(msg.encode(), addr)

    respond(180, "Ringing")
    time.sleep(1.0)  # let it ring a moment
    respond(200, "OK", sdp)
    print("  >> 180 + 200 OK sent (answered)")

    # Expect ACK.
    data, _ = c.sock.recvfrom(65535)
    assert data.decode(errors="replace").startswith("ACK"), "no ACK after 200"
    print("  << ACK")

    # Counsel's SDP offer tells us where to send RTP.
    offer_body = invite.split("\r\n\r\n", 1)[1]
    rip = re.search(r"^c=IN IP4 (\S+)", offer_body, re.M).group(1)
    rport = int(re.search(r"^m=audio (\d+)", offer_body, re.M).group(1))
    remote = (rip, rport)

    # Pump silence, listen for the lawyer's opening (mock tone burst).
    rtp_sock.setblocking(False)
    heard = 0
    seq = ts = 0
    start = time.time()
    while time.time() - start < 15:
        rtp_sock.sendto(rtp_packet(seq, ts, 55, SILENCE), remote)
        seq += 1
        ts += 160
        while True:
            try:
                data, _ = rtp_sock.recvfrom(2048)
            except BlockingIOError:
                break
            if len(data) > 12 and (data[1] & 0x7F) == 0 and energetic(data[12:]):
                heard += 1
        if heard >= 40:
            break
        time.sleep(0.02)
    print(f"  heard {heard} energetic frames from the lawyer")
    assert heard >= 40, "no opening line heard"

    t.join(timeout=40)
    print(f"  POST /call → {result.get('status')} {result.get('body')}")
    assert result.get("status") == 200, f"unexpected /call result: {result}"

    # Hang up from the phone side.
    c.to_tag, c.invite_call_id = to_tag, call_id
    # counsel's dialog remote target is our Contact; BYE goes the other way —
    # send a BYE as the phone: swap From/To (we are the callee).
    bye = (
        f"BYE sip:1@{SERVER_IP}:5060 SIP/2.0\r\n"
        f"Via: SIP/2.0/UDP {c.ip}:{c.port};branch=z9hG4bK{rand_token()};rport\r\n"
        f"Max-Forwards: 70\r\n"
        f"From: {to_h};tag={to_tag}\r\n"
        f"To: {from_h}\r\n"
        f"Call-ID: {call_id}\r\n"
        f"CSeq: 2 BYE\r\n"
        f"Content-Length: 0\r\n\r\n"
    )
    c.sock.sendto(bye.encode(), addr)
    data, _ = c.sock.recvfrom(65535)
    first = data.decode(errors="replace").split("\r\n", 1)[0]
    print(f"  BYE → {first}")
    print("RING-OUT TEST PASSED")


SERVER_IP = "127.0.0.1"

if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
