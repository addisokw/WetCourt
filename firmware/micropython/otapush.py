#!/usr/bin/env python3
"""otapush.py — push firmware to a Wet Court board over WiFi (no cable).

Run from a board's firmware directory. Credentials come from that dir's
./secrets.py (NanoC6 fleet) or ./settings.toml (judge-face, CircuitPython):

    cd firmware/judge-neck
    python3 ../micropython/otapush.py 192.168.50.61              # main.py + shared libs
    python3 ../micropython/otapush.py 192.168.50.61 main.py      # just one file
    python3 ../micropython/otapush.py 192.168.50.61 secrets.py   # config change

    cd firmware/judge-face
    python3 ../micropython/otapush.py 192.168.50.77              # ./otafiles.txt set

With no file arguments, the set pushed is ./otafiles.txt (one name per line,
`#` comments) when the board dir has one, else main.py + the shared libs.

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


def read_config():
    """OTA_TOKEN/OTA_PORT from ./secrets.py, or ./settings.toml (CircuitPython)."""
    cfg = {}
    secrets = Path("secrets.py")
    toml = Path("settings.toml")
    if secrets.exists():
        exec(secrets.read_text(), cfg)
        source = secrets
    elif toml.exists():
        import tomllib
        cfg = tomllib.loads(toml.read_text())
        source = toml
    else:
        sys.exit("No ./secrets.py or ./settings.toml — run from a board dir "
                 "(e.g. firmware/judge-neck, firmware/judge-face)")
    token = cfg.get("OTA_TOKEN")
    if not token:
        sys.exit("OTA_TOKEN missing/empty in ./%s — OTA is disabled on "
                 "this board. Set it and redeploy once over USB (./deploy.sh)." % source)
    return token, int(cfg.get("OTA_PORT", 8266))


def default_files():
    """./otafiles.txt (one name per line, # comments) if present, else fleet set."""
    manifest = Path("otafiles.txt")
    if manifest.exists():
        names = [ln.strip() for ln in manifest.read_text().splitlines()]
        return [Path(n) for n in names if n and not n.startswith("#")]
    return [Path(p) for p in DEFAULT_FILES]


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
    token, port = read_config()

    paths = [Path(n) for n in names] if names else default_files()
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
