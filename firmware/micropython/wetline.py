# Wet Court device line-protocol client (MicroPython, NanoC6).
#
# The role-agnostic half of a device firmware: brings up WiFi, dials the
# orchestrator, completes the HELLO handshake, then services newline-delimited
# commands, acking exactly once per command (see ../../protocol/README.md).
# The role name and its verb handlers come from each board's main.py.
#
# SHARED by all the MicroPython boards (judge-neck, turret, squirt, gavel):
# this file lives once here in firmware/micropython/ and every board's
# deploy.sh copies it onto the device — edit it here, not per board.
#
# Status RGB LED (NanoC6 onboard WS2812):
#   red   = WiFi down / associating
#   amber = WiFi up, dialing the orchestrator
#   green = link up, serving commands

import network
import socket
import time

from machine import Pin
from neopixel import NeoPixel

import secrets

_RED = (16, 0, 0)
_AMBER = (16, 8, 0)
_GREEN = (0, 12, 0)

_EAGAIN = 11
_np = None


def _led(color):
    global _np
    if _np is None:
        Pin(19, Pin.OUT).value(1)          # RGB power-enable must be HIGH first
        _np = NeoPixel(Pin(20), 1)
    _np[0] = color
    _np.write()


def dispatch(line, send, handlers):
    """One command line -> exactly one ack via send().

    A handler takes the arg tokens (list of str) and returns None for OK or a
    short reason string for ERR. PING and unknown verbs are handled here.
    """
    parts = line.split()
    if not parts:
        return
    verb = parts[0]
    if verb == "PING":
        send("OK PING")
        return
    handler = handlers.get(verb)
    if handler is None:
        send("ERR " + verb + " unsupported")
        return
    err = handler(parts[1:])
    send(("OK " + verb) if err is None else ("ERR " + verb + " " + err))


def _ensure_wifi(wlan):
    if wlan.isconnected():
        return True
    _led(_RED)
    wlan.active(True)
    wlan.connect(secrets.WIFI_SSID, secrets.WIFI_PASS)
    deadline = time.ticks_add(time.ticks_ms(), 15000)
    while not wlan.isconnected() and time.ticks_diff(deadline, time.ticks_ms()) > 0:
        time.sleep_ms(200)
    return wlan.isconnected()


def _connect(role, version):
    """Dial + HELLO. Returns a non-blocking socket, or raises OSError."""
    addr = socket.getaddrinfo(secrets.ORCH_HOST, secrets.ORCH_PORT)[0][-1]
    s = socket.socket()
    try:
        s.settimeout(4)
        s.connect(addr)
        s.send(b"HELLO " + role.encode() + b" " + version.encode() + b"\n")
        s.settimeout(3)                    # handshake read deadline
        first = b""
        while b"\n" not in first:
            chunk = s.recv(64)
            if not chunk:
                raise OSError(-1, "closed during handshake")
            first += chunk
            if len(first) > 128:
                raise OSError(-1, "handshake garbage")
        if first.split(b"\n", 1)[0].strip() != b"WELCOME":
            raise OSError(-1, "rejected: %s" % first.split(b"\n", 1)[0])
    except Exception:
        s.close()
        raise
    s.setblocking(False)
    return s


def _serve(sock, wlan, handlers):
    """Service the link until it drops (always exits by raising OSError)."""
    buf = bytearray()

    def send(msg):
        sock.send(msg.encode() + b"\n")

    wifi_check = time.ticks_ms()
    while True:
        data = None
        try:
            data = sock.recv(128)
            if data == b"":
                raise OSError(-1, "peer closed")
        except OSError as e:
            if not (e.args and e.args[0] == _EAGAIN):   # EAGAIN = no data yet
                raise
        if data:
            for b in data:
                if b == 0x0A:                            # \n
                    try:
                        line = bytes(buf).decode().strip()
                    except ValueError:
                        line = ""                        # binary garbage: skip
                    buf = bytearray()
                    if line:
                        dispatch(line, send, handlers)
                elif len(buf) < 96:
                    buf.append(b)
                else:
                    buf = bytearray()                    # drop a runaway line
        else:
            time.sleep_ms(10)
        now = time.ticks_ms()
        if time.ticks_diff(now, wifi_check) > 2000:
            wifi_check = now
            if not wlan.isconnected():
                raise OSError(-1, "wifi dropped")


def run(role, version, handlers):
    """Run forever: WiFi -> dial -> HELLO -> serve, reconnecting on failure."""
    wlan = network.WLAN(network.STA_IF)
    while True:
        if not _ensure_wifi(wlan):
            continue                       # _ensure_wifi already waited ~15 s
        _led(_AMBER)
        try:
            sock = _connect(role, version)
        except OSError as e:
            print("link:", e)
            time.sleep(2)                  # backoff before redial
            continue
        _led(_GREEN)
        print("connected to orchestrator")
        try:
            _serve(sock, wlan, handlers)
        except OSError as e:
            print("link dropped:", e)
        try:
            sock.close()
        except OSError:
            pass
