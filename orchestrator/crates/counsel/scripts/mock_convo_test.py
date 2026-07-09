#!/usr/bin/env python3
"""M2/M3 integration test: full voice loop against counsel in mock mode.

Adaptive, phone-shaped flow (continuous 20 ms RTP; silence when quiet):
  1. press '1' shortly after answer (cuts the IVR prompt)
  2. audio runs continuously: hold music -> queue announcement -> music beat
     -> greeting tone; wait for the first sustained quiet = lawyer listening
  3. "speak" (loud frames), go quiet; VAD endpoints, mock turn runs
  4. expect the reply tone
"""

import struct
import sys
import time
import socket

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
sys.path.insert(0, SP)
from sip_echo_test import SipClient, rtp_packet, ulaw_encode

FRAME = 0.02
SILENCE = bytes([0xFF] * 160)
SPEECH = bytes(ulaw_encode(3000 if i % 2 == 0 else -3000) for i in range(160))


def energetic(p):
    return sum(1 for b in p if b not in (0xFF, 0x7F, 0xFE, 0x7E)) > len(p) * 0.3


class Pump:
    def __init__(self, sock, remote):
        self.sock, self.remote = sock, remote
        self.seq = self.ts = 0
        self.t0 = time.time()
        self.last_sound = None
        self.heard_any = False

    def t(self):
        return time.time() - self.t0

    def dtmf(self, digit):
        pl = struct.pack("!BBH", digit, 0x8A, 800)
        for _ in range(3):
            self.sock.sendto(rtp_packet(self.seq, self.ts, 9, pl, pt=101), self.remote)
            self.seq += 1

    def run(self, seconds, payload=SILENCE):
        n = 0
        base = time.time()
        while time.time() - base < seconds:
            self.sock.sendto(rtp_packet(self.seq, self.ts, 9, payload), self.remote)
            self.seq += 1
            self.ts += 160
            while True:
                try:
                    data, _ = self.sock.recvfrom(2048)
                except BlockingIOError:
                    break
                if len(data) > 12 and (data[1] & 0x7F) == 0 and energetic(data[12:]):
                    self.last_sound = time.time()
                    self.heard_any = True
            n += 1
            d = base + n * FRAME - time.time()
            if d > 0:
                time.sleep(d)

    def until(self, cond, timeout, label):
        end = time.time() + timeout
        while time.time() < end:
            self.run(0.1)
            if cond():
                print(f"  [{self.t():5.1f}s] {label}")
                return True
        raise AssertionError(f"timeout waiting for: {label}")


def main():
    c = SipClient()
    print(f"client on {c.ip}:{c.port}")
    c.register()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", 0))
    sock.setblocking(False)
    remote = c.invite(sock.getsockname()[1])

    p = Pump(sock, remote)
    p.until(lambda: p.heard_any, 10, "IVR prompt started")
    p.run(1.0)
    p.dtmf(1)
    print(f"  [{p.t():5.1f}s] pressed 1 (guilty)")

    # Hold music -> announcement -> greeting; first sustained quiet = listening.
    p.until(
        lambda: p.last_sound and time.time() - p.last_sound > 1.3,
        60,
        "hold + greeting finished, line quiet",
    )

    p.run(1.4, payload=SPEECH)
    print(f"  [{p.t():5.1f}s] spoke for 1.4s")
    p.heard_any = False
    p.until(lambda: p.heard_any, 15, "lawyer reply heard")

    c.bye()
    print("MOCK CONVERSATION PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
