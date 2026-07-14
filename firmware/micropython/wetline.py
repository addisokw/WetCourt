# Wet Court device line-protocol client (MicroPython, NanoC6).
#
# The role-agnostic half of a device firmware: brings up WiFi, dials the
# orchestrator, completes the HELLO handshake, then services newline-delimited
# commands, acking exactly once per command (see ../../protocol/README.md).
# The role name and its verb handlers come from each board's main.py.
#
# SHARED by all the MicroPython boards (judge-neck, turret, squirt, gavel,
# swear-in): this file lives once here in firmware/micropython/ and every
# board's deploy.sh copies it onto the device — edit it here, not per board.
#
# Boards with local inputs/outputs (the swear-in button) pass run(..., tick=fn):
# tick(send) is called continuously — send is a line-sender while the link is
# up, None while it is down — so firmware can scan inputs, animate LEDs, and
# emit unsolicited events (e.g. "BUTTON") without owning the serve loop.
#
# The orchestrator is found one of two ways:
#   - secrets.ORCH_HOST set (hostname or IP): dial it directly — a hard
#     override that never listens for beacons (use on show rigs, or when two
#     orchestrators share a LAN).
#   - ORCH_HOST absent/empty: listen for the orchestrator's UDP discovery
#     beacon (`WETCOURT <spec> <tcp_port>` broadcast on ORCH_BEACON_PORT,
#     default 8091) — IP from the datagram's sender, port from the payload.
#     Re-discovers after repeated dial failures, so the fleet follows the
#     orchestrator across machines/addresses with no reflash.
#
# Status RGB LED (NanoC6 onboard WS2812):
#   red   = WiFi down / associating
#   amber = WiFi up, discovering / dialing the orchestrator
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

_EAGAIN = (11, 35)   # lwip / BSD spellings (35 shows up in host-side testing)
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


_BEACON_MAGIC = b"WETCOURT"
_DISCOVER_MS = 8000            # ~4 beacon intervals before giving up a round


def _discover(ota, tick):
    """Listen for the orchestrator's UDP discovery beacon.

    Returns (host, port) from the first well-formed `WETCOURT <spec> <port>`
    datagram — host is the sender's IP, port comes from the payload — or None
    after ~8 s of silence. OTA (and the board's tick hook) stay serviced so a
    board parked in discovery can still be updated.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("0.0.0.0", getattr(secrets, "ORCH_BEACON_PORT", 8091)))
        s.settimeout(0.1)
        deadline = time.ticks_add(time.ticks_ms(), _DISCOVER_MS)
        while time.ticks_diff(deadline, time.ticks_ms()) > 0:
            if ota:
                ota.poll()
            if tick:
                tick(None)
            try:
                data, src = s.recvfrom(64)
            except OSError:
                continue                     # timeout: keep listening
            parts = data.split()
            if len(parts) == 3 and parts[0] == _BEACON_MAGIC:
                try:
                    return (src[0], int(parts[2]))
                except ValueError:
                    pass                     # malformed port: keep listening
    finally:
        s.close()
    return None


def _connect(role, version, target):
    """Dial + HELLO. Returns a non-blocking socket, or raises OSError."""
    addr = socket.getaddrinfo(target[0], target[1])[0][-1]
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


def _serve(sock, wlan, handlers, ota, tick):
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
            if not (e.args and e.args[0] in _EAGAIN):   # EAGAIN = no data yet
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
            if ota:
                ota.poll()
            time.sleep_ms(10)
        if tick:
            tick(send)                                   # link up: may emit events
        now = time.ticks_ms()
        if time.ticks_diff(now, wifi_check) > 2000:
            wifi_check = now
            if not wlan.isconnected():
                raise OSError(-1, "wifi dropped")


def _make_ota():
    """OTA update listener (see ota.py) — None when disabled or not deployed."""
    try:
        import ota
        return ota.server_from_secrets()
    except Exception as e:
        print("ota: disabled (%s)" % e)
        return None


def run(role, version, handlers, tick=None):
    """Run forever: WiFi -> dial -> HELLO -> serve, reconnecting on failure.

    The OTA listener is polled at every idle point WiFi allows, so firmware
    can be pushed even while the orchestrator is down. `tick(send)`, if given,
    is called at the same points: send is live while connected, None while
    down (events have nowhere to go; local outputs should show "not ready").
    """
    try:
        network.hostname(role)     # mDNS: the board answers <role>.local
    except (AttributeError, ValueError):
        pass                       # port without hostname(); IP still works
    wlan = network.WLAN(network.STA_IF)
    ota = _make_ota()
    # Explicit ORCH_HOST is a hard override; unset/empty means "discover".
    static_host = getattr(secrets, "ORCH_HOST", None) or None
    target = (static_host, getattr(secrets, "ORCH_PORT", 8090)) if static_host else None
    fails = 0                      # consecutive dial failures → re-discover
    while True:
        if not _ensure_wifi(wlan):
            continue                       # _ensure_wifi already waited ~15 s
        _led(_AMBER)
        if not static_host and (target is None or fails >= 3):
            target = _discover(ota, tick)
            fails = 0
            if target is None:
                continue                   # no beacon heard; recheck WiFi, retry
            print("discovered orchestrator at %s:%d" % target)
        try:
            sock = _connect(role, version, target)
        except OSError as e:
            print("link:", e)
            fails += 1
            for _ in range(20):            # ~2 s backoff, OTA stays serviced
                if ota:
                    ota.poll()
                if tick:
                    tick(None)             # link down: no event sink
                time.sleep_ms(100)
            continue
        fails = 0
        _led(_GREEN)
        print("connected to orchestrator")
        try:
            _serve(sock, wlan, handlers, ota, tick)
        except OSError as e:
            print("link dropped:", e)
        try:
            sock.close()
        except OSError:
            pass
