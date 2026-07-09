#!/usr/bin/env python3
"""M3 test: IVR prompt plays on answer; DTMF '2' cuts it; greeting follows."""

import struct
import sys
import time
import socket

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
sys.path.insert(0, SP)
from sip_echo_test import SipClient, rtp_packet

FRAME = 0.02
SILENCE = bytes([0xFF] * 160)


def energetic(p):
    return sum(1 for b in p if b not in (0xFF, 0x7F, 0xFE, 0x7E)) > len(p) * 0.3


def main():
    c = SipClient()
    c.register()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", 0))
    sock.setblocking(False)
    remote = c.invite(sock.getsockname()[1])

    seq, ts = 1, 0
    heard = []  # (t, energetic)
    start = time.time()
    dtmf_sent_at = None

    for tick in range(int(12 / FRAME)):
        t = tick * FRAME
        # After 3 s of the IVR prompt, press '2' (three end packets).
        if 3.0 <= t < 3.06 and dtmf_sent_at is None:
            for _ in range(3):
                sock.sendto(
                    rtp_packet(seq, ts, 7, struct.pack("!BBH", 2, 0x8A, 800), pt=101),
                    remote,
                )
                seq += 1
            dtmf_sent_at = t
            print(f"  [{t:.1f}s] sent DTMF '2'")
        sock.sendto(rtp_packet(seq, ts, 7, SILENCE), remote)
        seq += 1
        ts += 160
        while True:
            try:
                data, _ = sock.recvfrom(2048)
            except BlockingIOError:
                break
            if len(data) > 12 and (data[1] & 0x7F) == 0:
                heard.append((time.time() - start, energetic(data[12:])))
        nxt = start + (tick + 1) * FRAME
        d = nxt - time.time()
        if d > 0:
            time.sleep(d)

    def sound(t0, t1):
        return sum(1 for (t, e) in heard if e and t0 <= t < t1)

    ivr = sound(0.5, 3.0)
    # After the key press the prompt is cut; mock greeting tone (1.2 s)
    # follows within ~a second.
    post = sound(3.2, 7.0)
    print(f"energetic frames — ivr window: {ivr}, post-DTMF window: {post}")
    assert ivr >= 60, f"IVR prompt not heard ({ivr})"
    assert post >= 40, f"greeting after DTMF not heard ({post})"
    c.bye()
    print("IVR TEST PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
