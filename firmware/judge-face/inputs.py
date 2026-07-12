# Wet Court judge-face — input layer: the orchestrator link + demo mode.
#
# OrchestratorLink speaks the Wet Court device line protocol (see
# ../../protocol/README.md): dial the host over TCP through the AirLift
# ESP32, identify with `HELLO judge-face`, then service commands:
#
#   FACE <phase>       set the eye phase (idle/listening/deliberating/verdict:*)
#   AUDIO <0.0-1.0>    live mic envelope (~20-30 Hz while listening)
#   PERSONA <slug>     switch the judge persona
#   AIM <pan> <tilt>   neck pose in degrees (host mirrors judge-neck AIM);
#                      drives the catchlight parallax, moves no servos here
#   PANEL <pattern>    legacy alias (idle/thinking/verdict), kept for the
#                      current orchestrator: thinking→deliberating,
#                      verdict→verdict:guilty
#   PING               keepalive
#
# Rendering must never block on the network, so all socket reads are
# non-blocking and connection attempts are rate-limited. The unavoidable
# exception on the M4: WiFi association + the HELLO handshake are synchronous
# in the esp32spi API and can stall a few seconds — they run at most once per
# backoff window, and code.py clamps dt so the animation doesn't leap.
#
# DemoSource fakes the same inputs (brief §5): cycles the phases, rotates
# personas, and synthesizes a speech-like audio envelope, so the eye is fully
# developable with no orchestrator on the network.

from adafruit_ticks import ticks_ms, ticks_diff, ticks_add

import config
import personas
from eye_face import snoise

_PANEL_MAP = {"idle": "idle", "thinking": "deliberating", "verdict": "verdict:guilty"}

# Last link failure, readable over the OTA channel (`OTALOG` in ota.py) —
# remote serial-console-lite for a board that's zip-tied into the booth.
LAST_LINK_ERROR = None
LINK_FAILS = 0
NET_PROBE = {}


def _note_failure(e):
    global LAST_LINK_ERROR, LINK_FAILS
    LAST_LINK_ERROR = repr(e)
    LINK_FAILS += 1


def _net_probe(esp, pool):
    """One-shot layer-by-layer dial diagnosis, reported via OTALOG.

    Separates NINA hostname resolution (esp32spi resolves even dotted-quad
    strings through REQ_HOST_BY_NAME) from the raw START_CLIENT_TCP connect.
    """
    out = {}
    try:
        out["dns"] = repr(bytes(esp.get_host_by_name(config.ORCH_HOST)))
    except Exception as e:
        out["dns"] = "ERR " + repr(e)
    try:
        s = pool.socket(pool.AF_INET, pool.SOCK_STREAM)
        try:
            ip = bytes(int(x) for x in config.ORCH_HOST.split("."))
            esp.socket_connect(s._socknum, ip, config.ORCH_PORT, 0)  # 0 = TCP
            out["raw_tcp"] = "OK"
        finally:
            try:
                s.close()
            except Exception:
                pass
    except Exception as e:
        out["raw_tcp"] = "ERR " + repr(e)
    return out
# Connection attempts block the render loop (sync esp32spi API), so space
# them well apart: a down orchestrator costs one ≤3 s hitch per window, and
# a live one is picked up within ~20 s. Consecutive failures back off
# exponentially (20 → 40 → 80 s) so absent infrastructure barely hitches.
_RETRY_MS = 20000
_RETRY_MAX_MS = 80000
_CONNECT_TIMEOUT_S = 3
# Re-dial after this much rx silence. The NINA identifies sockets by small
# recycled numbers, and with the OTA server allocating sockets on the same
# radio, a dead link socket can inherit a live socket's number — making the
# socket_connected probe (and recv) lie indefinitely. End-to-end liveness
# can't lean on the host either (it's silent between trials and doesn't
# answer device pings), so: too quiet for too long → assume nothing and
# reconnect. Costs a sub-second hitch every window while idle; any trial
# traffic resets the clock.
_RX_REFRESH_MS = 120000


class DemoSource:
    # No verdict phases here: the guilty strobe is a deliberate rapid red
    # flash (synced to the squirt when the host commands it) and reads as a
    # glitch when the idle demo rehearses it. Trigger verdicts via FACE.
    _SCRIPT = (("idle", 6.0), ("listening", 9.0), ("deliberating", 5.0))

    def __init__(self):
        self._i = -1
        self._t = 0.0
        self._clock = 0.0
        self._pi = 0

    def reset(self):
        """Called while the real link is up, so demo re-enters cleanly later."""
        self._i = -1

    def update(self, eye, dt):
        self._clock += dt
        if self._i < 0:                        # (re)entering demo mode
            self._i = 0
            self._t = 0.0
            eye.set_phase(self._SCRIPT[0][0])
        self._t += dt
        phase, dur = self._SCRIPT[self._i]
        if self._t >= dur:
            self._t = 0.0
            self._i = (self._i + 1) % len(self._SCRIPT)
            if self._i == 0:                   # full cycle done: next judge
                self._pi = (self._pi + 1) % len(personas.ORDER)
                eye.set_persona(personas.ORDER[self._pi])
            phase = self._SCRIPT[self._i][0]
            eye.set_phase(phase)
        if phase == "listening":               # bursty speech-like envelope
            e = max(0.0, (snoise(self._clock * 1.9, 7.7) - 0.35) * 1.7)
            eye.set_audio(min(1.0, e * e * 1.4))


class OrchestratorLink:
    def __init__(self):
        self._esp = None
        self._pool = None
        self._sock = None
        self._rbuf = bytearray(128)
        self._line = bytearray()
        self._next_try = ticks_ms()
        self._last_alive = ticks_ms()
        self._last_rx = ticks_ms()
        self._fails = 0
        self._enabled = bool(config.WIFI_SSID and config.ORCH_HOST)
        # Bumped whenever the AirLift is (re)created or hard-reset, so the OTA
        # server knows its listener socket died with the old radio state.
        self.radio_epoch = 0
        if not self._enabled:
            print("link: no WIFI_SSID/ORCH_HOST in settings.toml — demo mode only")

    def radio(self):
        """(esp, pool) once WiFi is associated, else (None, None).

        The OTA server binds its listener through this; the is_connected probe
        is an SPI round-trip, so callers rate-limit themselves.
        """
        if self._esp is None or self._pool is None:
            return None, None
        try:
            if not self._esp.is_connected:
                return None, None
        except Exception:
            return None, None
        return self._esp, self._pool

    def poll(self, eye, now):
        """Service the link; returns True while connected. Never raises."""
        if not self._enabled:
            return False
        if self._sock is not None:
            try:
                self._service(eye, now)
                return True
            except OSError as e:
                print("link: dropped:", e)
                _note_failure(e)
                planned = str(e) == "idle refresh"
                self._drop(now)
                if planned:            # host presumed live: skip the backoff
                    self._next_try = now
                    self._fails = 0
                return False
        if ticks_diff(now, self._next_try) < 0:
            return False
        try:
            self._connect()
            self._last_alive = ticks_ms()
            self._fails = 0
            return True
        except Exception as e:                 # any failure → backoff, keep animating
            print("link:", e)
            _note_failure(e)
            global NET_PROBE
            if not NET_PROBE and self._esp is not None and self._pool is not None:
                try:
                    NET_PROBE = _net_probe(self._esp, self._pool)
                except Exception as pe:
                    NET_PROBE = {"probe": "ERR " + repr(pe)}
                print("link: net probe:", NET_PROBE)
            if "not responding" in str(e) and self._esp is not None:
                # AirLift wedged mid-transaction (busy pin stuck) — only a
                # hard reset recovers it; WiFi reassociates on the next try.
                try:
                    self._esp.reset()
                    self.radio_epoch += 1     # every socket died with the radio
                    print("link: AirLift reset")
                except Exception:
                    pass
            self._drop(ticks_ms())
            return False

    # ------------------------------------------------------------ internals
    def _init_hw(self):
        import board
        import busio
        from digitalio import DigitalInOut
        from adafruit_esp32spi import adafruit_esp32spi
        import adafruit_connection_manager

        spi = busio.SPI(board.SCK, board.MOSI, board.MISO)
        self._esp = adafruit_esp32spi.ESP_SPIcontrol(
            spi, DigitalInOut(board.ESP_CS), DigitalInOut(board.ESP_BUSY),
            DigitalInOut(board.ESP_RESET))
        self._pool = adafruit_connection_manager.get_radio_socketpool(self._esp)
        self.radio_epoch += 1

    def _connect(self):
        if self._esp is None:
            self._init_hw()
        if not self._esp.is_connected:
            self._esp.connect_AP(config.WIFI_SSID, config.WIFI_PASS)
            print("wifi: up,", self._esp.pretty_ip(self._esp.ip_address))

        # No reachability pre-check: an earlier ping gate here (meant to spare
        # the NINA from wedging on a dead host) turned out to poison the dial
        # itself — a ping immediately before START_CLIENT_TCP made the NINA
        # NAK the connect ("Expected 01 but got 00"), so the face never got a
        # SYN out while the ping-free NanoC6 boards connected fine. Dial
        # directly like they do; connect has its own timeout, and poll()
        # hard-resets a wedged AirLift.
        sock = self._pool.socket(self._pool.AF_INET, self._pool.SOCK_STREAM)
        try:
            sock.settimeout(_CONNECT_TIMEOUT_S)
            # Don't use sock.connect((host, port)): this bundle's socketpool
            # mangles a dotted-quad string dest on its way to the NINA, which
            # NAKs START_CLIENT_TCP ("Expected 01 but got 00") — the SYN never
            # leaves the radio. Resolving to 4 IP bytes and connecting at the
            # esp layer works (proven by the OTALOG net probe on this unit);
            # the Socket wrapper stays valid for send/recv since connection
            # state lives in the NINA, keyed by socket number.
            parts = config.ORCH_HOST.split(".")
            if len(parts) == 4 and all(p.isdigit() and int(p) < 256 for p in parts):
                ip = bytes(int(p) for p in parts)
            else:
                ip = bytes(self._esp.get_host_by_name(config.ORCH_HOST))
            self._esp.socket_connect(sock._socknum, ip, config.ORCH_PORT, 0)  # 0 = TCP
            sock.send(b"HELLO judge-face " + config.FW_VERSION.encode() + b"\n")

            # Await WELCOME / BYE (handshake is the one blocking read, ≤3 s).
            first = bytearray()
            while b"\n" not in first:
                n = sock.recv_into(self._rbuf)
                if n <= 0:
                    raise OSError("handshake: connection closed")
                first += self._rbuf[:n]
                if len(first) > 128:
                    raise OSError("handshake: garbage")
            if first.split(b"\n")[0].strip() != b"WELCOME":
                raise OSError("handshake: rejected: " + str(bytes(first.split(b"\n")[0])))
        except BaseException:
            # NINA socket numbers are a tiny pool; a failed dial must not
            # leak one (enough leaks and every future dial dies too).
            try:
                sock.close()
            except Exception:
                pass
            raise

        sock.settimeout(0)                     # non-blocking from here on
        self._sock = sock
        self._line = bytearray()
        self._last_rx = ticks_ms()
        print("orchestrator: connected")

    def _drop(self, now):
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None
        delay = min(_RETRY_MS * (1 << min(self._fails, 4)), _RETRY_MAX_MS)
        self._fails += 1
        self._next_try = ticks_add(now, delay)

    def _service(self, eye, now):
        n = 0
        try:
            n = self._sock.recv_into(self._rbuf)
        except OSError as e:
            if e.errno not in (11, 110, 116):  # EAGAIN / timeouts = no data yet
                raise
        if n > 0:
            self._last_alive = now
            self._last_rx = now
            for b in memoryview(self._rbuf)[:n]:
                if b == 0x0A:                  # \n
                    line = bytes(self._line).decode().strip()
                    self._line = bytearray()
                    if line:
                        self._dispatch(line, eye)
                elif len(self._line) < 96:
                    self._line.append(b)
                else:
                    self._line = bytearray()   # overflow: drop the runaway line
        elif ticks_diff(now, self._last_rx) > _RX_REFRESH_MS:
            # See _RX_REFRESH_MS: the NINA state below can lie about a stale
            # socket, so long silence forces a fresh dial regardless.
            raise OSError("idle refresh")
        elif ticks_diff(now, self._last_alive) > 2000:
            # Quiet for a while — make sure the peer is still there. The
            # socknum probe is bundle-version dependent, hence the hasattr.
            self._last_alive = now
            if hasattr(self._sock, "_socknum") and \
                    not self._esp.socket_connected(self._sock._socknum):
                raise OSError("peer closed")

    def _send(self, s):
        self._sock.send(s.encode() + b"\n")

    def _dispatch(self, line, eye):
        parts = line.split()
        verb = parts[0]
        arg = parts[1] if len(parts) > 1 else None
        if verb == "PING":
            self._send("OK PING")
        elif verb == "FACE":
            if arg is None:
                self._send("ERR FACE missing_args")
                return
            try:
                eye.set_phase(arg)
                self._send("OK FACE")
            except ValueError:
                self._send("ERR FACE unknown_phase")
        elif verb == "PANEL":                  # legacy vocabulary
            phase = _PANEL_MAP.get(arg)
            if phase is None:
                self._send("ERR PANEL unknown_pattern" if arg else "ERR PANEL missing_args")
                return
            eye.set_phase(phase)
            self._send("OK PANEL")
        elif verb == "AUDIO":
            try:
                eye.set_audio(float(arg))
                self._send("OK AUDIO")
            except (TypeError, ValueError):
                self._send("ERR AUDIO bad_level")
        elif verb == "AIM":
            try:
                eye.set_aim(float(parts[1]), float(parts[2]))
                self._send("OK AIM")
            except (IndexError, ValueError):
                self._send("ERR AIM bad_args")
        elif verb == "PERSONA":
            if arg is None:
                self._send("ERR PERSONA missing_args")
                return
            try:
                eye.set_persona(arg)
                self._send("OK PERSONA")
            except ValueError:
                self._send("ERR PERSONA unknown_persona")
        else:
            self._send("ERR " + verb + " unsupported")
