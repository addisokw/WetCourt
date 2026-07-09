#!/usr/bin/env python3
"""M2 integration test: full voice loop against counsel in mock-inference mode.

Behaves like a phone: continuous 20 ms RTP frames (silence when quiet).
Timeline:
  phase A (0-3 s):   expect the greeting (mock TTS = 440 Hz tone burst)
  phase B (3-4.5 s): we "speak" (loud frames)
  phase C (4.5-9 s): silence; VAD endpoints, mock turn runs, expect reply tone
Assert: sound received in A, sound received in C after our speech ended.
"""

import sys
import time
import socket

sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from sip_echo_test import SipClient, rtp_packet, ulaw_encode  # reuse the M1 client

FRAME = 0.02


def energetic(payload):
    # µ-law silence is 0xFF; tone bytes scatter. Count non-quiet bytes.
    loud = sum(1 for b in payload if abs((b ^ 0xFF)) > 4 and b not in (0xFF, 0x7F))
    return loud > len(payload) * 0.3


def main():
    c = SipClient()
    print(f"client on {c.ip}:{c.port}")
    c.register()

    rtp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rtp_sock.bind(("0.0.0.0", 0))
    rtp_sock.setblocking(False)
    remote = c.invite(rtp_sock.getsockname()[1])

    speech_frame = bytes(
        ulaw_encode(3000 if i % 2 == 0 else -3000) for i in range(160)
    )
    silence_frame = bytes([0xFF] * 160)

    ssrc, seq, ts = 12345, 1, 0
    timeline = []  # (t, energetic) for received frames
    start = time.time()
    total_ticks = int(9.0 / FRAME)

    for tick in range(total_ticks):
        t = tick * FRAME
        speaking = 3.0 <= t < 4.5
        payload = speech_frame if speaking else silence_frame
        rtp_sock.sendto(rtp_packet(seq, ts, ssrc, payload), remote)
        seq += 1
        ts += 160
        while True:
            try:
                data, _ = rtp_sock.recvfrom(2048)
            except BlockingIOError:
                break
            if len(data) > 12 and (data[1] & 0x7F) == 0:
                timeline.append((time.time() - start, energetic(data[12:])))
        # pace
        next_at = start + (tick + 1) * FRAME
        delay = next_at - time.time()
        if delay > 0:
            time.sleep(delay)

    def sound_between(t0, t1):
        return sum(1 for (t, e) in timeline if e and t0 <= t < t1)

    greeting = sound_between(0.0, 3.0)
    during_c = sound_between(5.0, 9.0)
    print(f"energetic frames — greeting window: {greeting}, reply window: {during_c}")
    assert greeting >= 30, f"no greeting audio heard ({greeting} frames)"
    assert during_c >= 30, f"no reply audio after speech ({during_c} frames)"

    c.bye()
    print("MOCK CONVERSATION PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
