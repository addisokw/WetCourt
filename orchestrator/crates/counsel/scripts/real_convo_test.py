#!/usr/bin/env python3
"""Full-stack call test against real inference (Spark over Tailscale).

Plays a Kokoro-synthesized 'caller' clip into the line, adaptively:
  1. wait for the IVR prompt, press '1' to cut it
  2. sit through the hold gag + greeting until sustained quiet (= listening)
  3. play the caller clip ("soup in the fountain" confession)
  4. wait for the lawyer's reply burst
Asserts the bursts happened. The counsel log shows the actual transcript
and reply text.
"""

import struct
import sys
import time
import socket
import wave

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
sys.path.insert(0, SP)
from sip_echo_test import SipClient, rtp_packet, ulaw_encode

FRAME = 0.02
SILENCE = bytes([0xFF] * 160)


def load_caller_frames():
    w = wave.open(f"{SP}/caller.wav", "rb")
    assert w.getframerate() == 24000 and w.getnchannels() == 1
    raw = w.readframes(w.getnframes())
    samples = [
        int.from_bytes(raw[i : i + 2], "little", signed=True)
        for i in range(0, len(raw), 2)
    ]
    # crude 3:1 decimation with 3-sample boxcar (fine for STT)
    down = [
        (samples[i] + samples[i + 1] + samples[i + 2]) // 3
        for i in range(0, len(samples) - 3, 3)
    ]
    ulaw = bytes(ulaw_encode(s) for s in down)
    frames = [ulaw[i : i + 160] for i in range(0, len(ulaw) - 160, 160)]
    print(f"caller clip: {len(frames)} frames ({len(frames)*0.02:.1f}s)")
    return frames


def energetic(payload):
    loud = sum(1 for b in payload if b not in (0xFF, 0x7F, 0xFE, 0x7E))
    return loud > len(payload) * 0.3


class Line:
    """Continuous 20 ms RTP pump with a playout queue."""

    def __init__(self, sock, remote):
        self.sock, self.remote = sock, remote
        self.seq, self.ts = 1, 0
        self.queue = []
        self.last_sound = None
        self.heard_any = False
        self.t0 = time.time()

    def tick(self):
        payload = self.queue.pop(0) if self.queue else SILENCE
        self.sock.sendto(rtp_packet(self.seq, self.ts, 99, payload), self.remote)
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

    def run_until(self, cond, timeout, label):
        start = time.time()
        n = 0
        while time.time() - start < timeout:
            self.tick()
            n += 1
            if cond():
                print(f"  [{time.time()-self.t0:5.1f}s] {label}")
                return True
            target = start + n * FRAME
            d = target - time.time()
            if d > 0:
                time.sleep(d)
        return False

    def dtmf(self, digit):
        pl = struct.pack("!BBH", digit, 0x8A, 800)
        for _ in range(3):
            self.sock.sendto(rtp_packet(self.seq, self.ts, 99, pl, pt=101), self.remote)
            self.seq += 1


def main():
    c = SipClient()
    print(f"client on {c.ip}:{c.port}")
    c.register()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", 0))
    sock.setblocking(False)
    remote = c.invite(sock.getsockname()[1])

    line = Line(sock, remote)
    caller_frames = load_caller_frames()

    assert line.run_until(
        lambda: line.heard_any, 30, "IVR prompt started"
    ), "no IVR audio within 30s"
    line.run_until(lambda: False, 1.0, "")  # let the prompt establish
    line.dtmf(1)
    print("  pressed 1 (guilty)")
    # Hold music -> queue announcement -> greeting; music fills every gap,
    # so the first sustained quiet means the lawyer is listening.
    assert line.run_until(
        lambda: line.last_sound and time.time() - line.last_sound > 1.5,
        120,
        "hold + greeting finished",
    ), "line never went quiet after hold/greeting"

    print(">> playing caller clip")
    line.queue = list(caller_frames)
    line.heard_any = False
    assert line.run_until(
        lambda: not line.queue, 60, "caller clip played"
    ), "clip playout stalled"

    line.heard_any = False
    assert line.run_until(
        lambda: line.heard_any, 45, "lawyer reply started"
    ), "no reply within 45s"
    assert line.run_until(
        lambda: line.last_sound and time.time() - line.last_sound > 1.5,
        90,
        "lawyer reply finished",
    ), "reply never went quiet"

    c.bye()
    print("REAL CONVERSATION PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL: {e}")
        sys.exit(1)
