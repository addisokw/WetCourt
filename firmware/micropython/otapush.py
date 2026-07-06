#!/usr/bin/env python3
"""otapush.py — push firmware to a Wet Court NanoC6 over WiFi (no cable).

Run from a board's firmware directory (it reads OTA_TOKEN/OTA_PORT from the
./secrets.py you deployed to that board):

    cd firmware/judge-neck
    python3 ../micropython/otapush.py 192.168.50.61              # main.py + shared libs
    python3 ../micropython/otapush.py 192.168.50.61 main.py      # just one file
    python3 ../micropython/otapush.py 192.168.50.61 secrets.py   # config change

Files stage as *.new on the board, every
sha256 is verified, and only then are they swapped in and the board reset —
a dropped connection mid-update changes nothing. The device refuses boot.py;
USB (deploy.sh) remains the recovery path.
"""

import base64
import hashlib
import socket
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent      # firmware/micropython/
CHUNK = 2048
DEFAULT_FILES = ["main.py", HERE / "wetline.py", HERE / "ota.py"]


def read_secrets():
    cfg = {}
    path = Path("secrets.py")
    if not path.exists():
        sys.exit("No ./secrets.py — run from a board dir (e.g. firmware/judge-neck)")
    exec(path.read_text(), cfg)
    token = cfg.get("OTA_TOKEN")
    if not token:
        sys.exit("OTA_TOKEN missing/empty in ./secrets.py — OTA is disabled on "
                 "this board. Set it and redeploy once over USB (./deploy.sh).")
    return token, int(cfg.get("OTA_PORT", 8266))


class Link:
    def __init__(self, host, port):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""

    def cmd(self, line, expect):
        self.sock.sendall(line.encode() + b"\n")
        while b"\n" not in self.buf:
            data = self.sock.recv(1024)
            if not data:
                sys.exit("device closed the connection")
            self.buf += data
        reply, self.buf = self.buf.split(b"\n", 1)
        reply = reply.decode().strip()
        if not reply.startswith("OK " + expect):
            sys.exit("device said: %s" % reply)
        return reply


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    if not args:
        sys.exit(__doc__)
    host, names = args[0], args[1:]
    token, port = read_secrets()

    paths = [Path(n) for n in names] if names else [Path(p) for p in DEFAULT_FILES]
    for p in paths:
        if not p.exists():
            sys.exit("no such file: %s" % p)

    link = Link(host, port)
    link.cmd("OTABEGIN %s" % token, "OTABEGIN")
    blobs = {}
    for p in paths:
        data = p.read_bytes()
        name = p.name
        sha = hashlib.sha256(data).hexdigest()
        blobs[name] = data
        link.cmd("OTAFILE %s %s %d %s" % (token, name, len(data), sha), "OTAFILE")
    for name, data in blobs.items():
        for off in range(0, len(data), CHUNK):
            b64 = base64.b64encode(data[off:off + CHUNK]).decode()
            link.cmd("OTAPUT %s %s %d %s" % (token, name, off, b64), "OTAPUT")
        print("  staged %-14s %5d bytes" % (name, len(data)))
    reply = link.cmd("OTACOMMIT %s" % token, "OTACOMMIT")
    print("%s — board verified all hashes, swapped, and is resetting" % reply)


if __name__ == "__main__":
    main()
