# Wet Court judge-face — input layer: the orchestrator link + demo mode.
#
# OrchestratorLink speaks the Wet Court device line protocol (see
# ../../protocol/README.md): dial the host over TCP through the AirLift
# ESP32, identify with `HELLO judge-face`, then service commands:
#
#   FACE <phase>       set the eye phase (idle/listening/deliberating/verdict:*)
#   AUDIO <0.0-1.0>    live mic envelope (~20-30 Hz while listening)
#   PERSONA <slug>     switch the judge persona
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
_RETRY_MS = 8000


class DemoSource:
    _SCRIPT = (("idle", 5.0), ("listening", 8.0), ("deliberating", 5.0),
               ("verdict:guilty", 2.5), ("idle", 4.0), ("listening", 7.0),
               ("deliberating", 4.0), ("verdict:innocent", 4.0))

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
        self._enabled = bool(config.WIFI_SSID and config.ORCH_HOST)
        if not self._enabled:
            print("link: no WIFI_SSID/ORCH_HOST in settings.toml — demo mode only")

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
                self._drop(now)
                return False
        if ticks_diff(now, self._next_try) < 0:
            return False
        try:
            self._connect()
            self._last_alive = ticks_ms()
            return True
        except Exception as e:                 # any failure → backoff, keep animating
            print("link:", e)
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

    def _connect(self):
        if self._esp is None:
            self._init_hw()
        if not self._esp.is_connected:
            self._esp.connect_AP(config.WIFI_SSID, config.WIFI_PASS)
            print("wifi: up,", self._esp.pretty_ip(self._esp.ip_address))
        sock = self._pool.socket(self._pool.AF_INET, self._pool.SOCK_STREAM)
        sock.settimeout(4)
        sock.connect((config.ORCH_HOST, config.ORCH_PORT))
        sock.send(b"HELLO judge-face " + config.FW_VERSION.encode() + b"\n")

        # Await WELCOME / BYE (handshake is the one blocking read, ≤4 s).
        first = bytearray()
        while b"\n" not in first:
            n = sock.recv_into(self._rbuf)
            if n <= 0:
                raise OSError("handshake: connection closed")
            first += self._rbuf[:n]
            if len(first) > 128:
                raise OSError("handshake: garbage")
        if first.split(b"\n")[0].strip() != b"WELCOME":
            sock.close()
            raise OSError("handshake: rejected: " + str(bytes(first.split(b"\n")[0])))

        sock.settimeout(0)                     # non-blocking from here on
        self._sock = sock
        self._line = bytearray()
        print("orchestrator: connected")

    def _drop(self, now):
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None
        self._next_try = ticks_add(now, _RETRY_MS)

    def _service(self, eye, now):
        n = 0
        try:
            n = self._sock.recv_into(self._rbuf)
        except OSError as e:
            if e.errno not in (11, 110, 116):  # EAGAIN / timeouts = no data yet
                raise
        if n > 0:
            self._last_alive = now
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
