#!/usr/bin/env python3
"""Turn this machine into the booth phone — mic/speakers over real SIP/RTP.

Usage:
  python3 softphone.py                 # register + dial the lawyer (off-hook)
  python3 softphone.py --listen        # register + wait for ring-out, auto-answer
  python3 softphone.py --server IP     # counsel host (default 127.0.0.1)

Keys during the call: 0-9 * #  send DTMF ("press 1 if guilty...")
                      q        hang up

Exercises the exact path the HT801 uses (SIP register/INVITE, G.711 RTP,
RFC2833 DTMF). Calls are recorded server-side under counsel's recordings/.

Requires: pip install sounddevice
"""

import argparse
import queue
import re
import select
import socket
import struct
import sys
import termios
import threading
import time
import tty

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
sys.path.insert(0, SP)
import sip_echo_test as sip
from sip_echo_test import SipClient, rtp_packet, rand_token, ulaw_encode

try:
    import sounddevice as sd
except ImportError:
    print("softphone needs the sounddevice module:  pip install sounddevice")
    sys.exit(1)

FRAME = 160  # samples per 20 ms @ 8 kHz


def ulaw_decode(b):
    u = ~b & 0xFF
    t = ((u & 0x0F) << 3) + 0x84
    t <<= (u & 0x70) >> 4
    return (0x84 - t) if (u & 0x80) else (t - 0x84)


ULAW_TABLE = [ulaw_decode(i) for i in range(256)]
DTMF_EVENTS = {**{str(d): d for d in range(10)}, "*": 10, "#": 11}


class Call:
    def __init__(self, sip_client, rtp_sock, remote):
        self.c = sip_client
        self.rtp = rtp_sock
        self.remote = remote
        self.seq, self.ts = 1, 0
        self.done = threading.Event()
        self.spk = queue.Queue(maxsize=50)  # decoded i16 frames for playback

    def send_dtmf(self, digit):
        ev = DTMF_EVENTS[digit]
        pl = struct.pack("!BBH", ev, 0x8A, 800)
        for _ in range(3):
            self.rtp.sendto(rtp_packet(self.seq, self.ts, 7, pl, pt=101), self.remote)
            self.seq += 1
        print(f"  [dtmf {digit}]")

    def mic_loop(self):
        """Mic → µ-law RTP; the blocking 20 ms reads pace the sender."""
        with sd.RawInputStream(samplerate=8000, channels=1, dtype="int16",
                               blocksize=FRAME) as stream:
            while not self.done.is_set():
                data, _ = stream.read(FRAME)
                pcm = struct.unpack(f"<{FRAME}h", bytes(data))
                payload = bytes(ulaw_encode(s) for s in pcm)
                self.rtp.sendto(
                    rtp_packet(self.seq, self.ts, 7, payload), self.remote
                )
                self.seq += 1
                self.ts += FRAME

    def rtp_recv_loop(self):
        self.rtp.settimeout(0.5)
        while not self.done.is_set():
            try:
                data, _ = self.rtp.recvfrom(2048)
            except socket.timeout:
                continue
            if len(data) > 12 and (data[1] & 0x7F) == 0:
                pcm = struct.pack(
                    f"<{len(data)-12}h", *(ULAW_TABLE[b] for b in data[12:])
                )
                try:
                    self.spk.put_nowait(pcm)
                except queue.Full:
                    pass  # drop rather than build latency

    def speaker_loop(self):
        silence = b"\x00" * FRAME * 2
        with sd.RawOutputStream(samplerate=8000, channels=1, dtype="int16",
                                blocksize=FRAME) as stream:
            while not self.done.is_set():
                try:
                    pcm = self.spk.get(timeout=0.1)
                except queue.Empty:
                    pcm = silence
                stream.write(pcm)

    def sip_recv_loop(self):
        """Answer in-dialog requests (BYE from the lawyer ends the call)."""
        self.c.sock.settimeout(0.5)
        while not self.done.is_set():
            try:
                data, addr = self.c.sock.recvfrom(65535)
            except socket.timeout:
                continue
            text = data.decode(errors="replace")
            if text.startswith("BYE"):
                reply_to_request(self.c, text, addr, 200, "OK")
                print("\n  lawyer hung up")
                self.done.set()

    def key_loop(self):
        old = termios.tcgetattr(sys.stdin)
        try:
            tty.setcbreak(sys.stdin.fileno())
            while not self.done.is_set():
                r, _, _ = select.select([sys.stdin], [], [], 0.2)
                if not r:
                    continue
                key = sys.stdin.read(1)
                if key == "q":
                    print("\n  hanging up")
                    self.done.set()
                elif key in DTMF_EVENTS:
                    self.send_dtmf(key)
        finally:
            termios.tcsetattr(sys.stdin, termios.TCSADRAIN, old)

    def hangup_sip(self):
        """Best-effort BYE so the lawyer stops talking (dial-out path)."""
        if getattr(self.c, "to_tag", None) and getattr(self.c, "invite_call_id", None):
            try:
                self.c.bye()
            except Exception:
                pass

    def run(self):
        # Not daemon: we join these on exit so the PortAudio streams close on
        # their own threads. Letting daemon threads die during interpreter
        # shutdown while a stream is mid-callback segfaults.
        threads = [
            threading.Thread(target=f)
            for f in (self.mic_loop, self.rtp_recv_loop, self.speaker_loop,
                      self.sip_recv_loop)
        ]
        for t in threads:
            t.start()
        print("connected — talk into your mic  |  0-9*# = DTMF, q = hang up")
        try:
            self.key_loop()
        except KeyboardInterrupt:
            pass
        finally:
            self.done.set()
            # Let each loop see `done` and exit its `with` block (streams close
            # cleanly here, on the thread that opened them).
            for t in threads:
                t.join(timeout=2.0)
            self.hangup_sip()
            self.rtp.close()


def reply_to_request(c, req_text, addr, code, name):
    """Minimal response echoing the request's dialog headers."""
    def h(name_):
        m = re.search(rf"^{name_}: (.+)$", req_text, re.M)
        return m.group(1) if m else ""
    msg = (
        f"SIP/2.0 {code} {name}\r\n"
        f"Via: {h('Via')}\r\n"
        f"From: {h('From')}\r\n"
        f"To: {h('To')}\r\n"
        f"Call-ID: {h('Call-ID')}\r\n"
        f"CSeq: {h('CSeq')}\r\n"
        f"Content-Length: 0\r\n\r\n"
    )
    c.sock.sendto(msg.encode(), addr)


def listen_and_answer(c, rtp_sock, rtp_port):
    """Wait for counsel's ring-out INVITE, ring briefly, answer with SDP."""
    print("registered; waiting for the lawyer to call (trigger via the "
          "console Lawyer tab or POST /call)... ctrl-c to quit")
    c.sock.settimeout(2.0)
    while True:
        try:
            data, addr = c.sock.recvfrom(65535)
        except socket.timeout:
            continue
        text = data.decode(errors="replace")
        if not text.startswith("INVITE"):
            continue
        print("  RING RING (answering in 2s)")
        to_tag = rand_token()
        # patch a to-tag into the To header for our responses
        patched = re.sub(r"^(To: .+)$", rf"\1;tag={to_tag}", text, count=1, flags=re.M)
        sdp = (
            "v=0\r\n"
            f"o=softphone 1 1 IN IP4 {c.ip}\r\n"
            "s=answer\r\n"
            f"c=IN IP4 {c.ip}\r\n"
            "t=0 0\r\n"
            f"m=audio {rtp_port} RTP/AVP 0 101\r\n"
            "a=rtpmap:0 PCMU/8000\r\n"
            "a=rtpmap:101 telephone-event/8000\r\n"
        )
        def respond(code, name, body=""):
            def h(name_):
                m = re.search(rf"^{name_}: (.+)$", patched, re.M)
                return m.group(1) if m else ""
            extra = (f"Content-Type: application/sdp\r\nContent-Length: {len(body)}\r\n"
                     if body else "Content-Length: 0\r\n")
            msg = (
                f"SIP/2.0 {code} {name}\r\n"
                f"Via: {h('Via')}\r\n"
                f"From: {h('From')}\r\n"
                f"To: {h('To')}\r\n"
                f"Call-ID: {h('Call-ID')}\r\n"
                f"CSeq: {h('CSeq')}\r\n"
                f"Contact: <sip:{c.ip}:{c.port}>\r\n"
                f"{extra}\r\n{body}"
            )
            c.sock.sendto(msg.encode(), addr)
        respond(180, "Ringing")
        time.sleep(2.0)
        respond(200, "OK", sdp)
        # counsel's offer says where to send our RTP
        body = text.split("\r\n\r\n", 1)[1]
        rip = re.search(r"^c=IN IP4 (\S+)", body, re.M).group(1)
        rport = int(re.search(r"^m=audio (\d+)", body, re.M).group(1))
        return (rip, rport)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="127.0.0.1")
    ap.add_argument("--listen", action="store_true",
                    help="wait for a ring-out instead of dialing")
    args = ap.parse_args()
    sip.SERVER = (args.server, 5060)

    c = SipClient()
    print(f"softphone on {c.ip}:{c.port} → counsel at {args.server}:5060")
    c.register()

    rtp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rtp_sock.bind(("0.0.0.0", 0))
    rtp_port = rtp_sock.getsockname()[1]

    if args.listen:
        remote = listen_and_answer(c, rtp_sock, rtp_port)
    else:
        print("dialing the lawyer...")
        remote = c.invite(rtp_port)

    Call(c, rtp_sock, remote).run()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print()
