# Wet Court judge-face — WiFi OTA file updates (CircuitPython port of
# ../micropython/ota.py; same wire protocol, same otapush.py client).
#
# The update flow is staged, verified, and atomic. The device runs a tiny
# token-gated TCP listener (separate from the orchestrator link — the face
# DIALS the orchestrator, so updates need their own inbound port). code.py
# polls it every frame; the poll self-rate-limits, so OTA works even with the
# orchestrator down (WiFi comes up on the link's first connect attempt).
#
# Wire (ASCII lines, house style — one ack per command):
#   OTABEGIN  <token>                          → OK OTABEGIN
#   OTAFILE   <token> <name> <size> <sha256>   → OK OTAFILE      (declare, repeat)
#   OTAPUT    <token> <name> <off> <b64>       → OK OTAPUT <newoff>
#   OTACOMMIT <token>                          → OK OTACOMMIT <n>  (then reset)
#   OTAABORT  <token>                          → OK OTAABORT
#
# Safety:
#   - chunks stage to <name>.new — running code is untouched until commit
#   - commit verifies EVERY declared file's size + sha256 before swapping any
#   - a dropped connection mid-update aborts the staging; nothing changes
#   - boot.py is refused outright (it owns the USB-recovery arbitration)
#   - every line requires OTA_TOKEN from settings.toml; unset = disabled
#
# CircuitPython deltas from the NanoC6 version:
#   - The filesystem is only code-writable when boot.py remounted it (default;
#     hold UP at reset for USB deploy mode instead). OTABEGIN probes and
#     reports `read_only_fs` so a push against USB mode fails clearly.
#   - Sockets come from the OrchestratorLink's native socketpool; the
#     listener is (re)armed lazily once WiFi is up, and torn down if WiFi
#     drops or the link ever rebuilds its pool (radio_epoch).
#   - sha256 accumulates over the chunks as they arrive. The S3 has native
#     hashlib so this is cheap; the pure-Python fallback below keeps the
#     desktop harness and hashlib-less builds (the retired M4) working.
#     Commit then checks size + running digest.

import os

try:
    from adafruit_ticks import ticks_ms, ticks_diff, ticks_add
except ImportError:  # desktop test harness
    import time

    def ticks_ms():
        return int(time.monotonic() * 1000)

    def ticks_diff(a, b):
        return a - b

    def ticks_add(a, b):
        return a + b

import binascii

# sha256 construction, probed ONCE at import (a surprise mid-update raises
# from _file_decl and used to take the whole listener down — see _service):
#   - CPython (desktop harness): hashlib.sha256()
#   - CircuitPython core hashlib (S3): ONLY hashlib.new("sha256") exists —
#     there are no named constructors, so hasattr picks the right shape
#   - no hashlib at all (the retired M4's SAMD51): pure-Python fallback
try:
    import hashlib

    if hasattr(hashlib, "sha256"):
        def _sha256():
            return hashlib.sha256()
    else:
        hashlib.new("sha256")     # unsupported algo raises here, not mid-update
        def _sha256():
            return hashlib.new("sha256")
except Exception:
    def _sha256():
        return _PySHA256()

CHUNK_LIMIT = 3000            # raw bytes per PUT after b64 decode
LINE_LIMIT = 8192             # OTA line cap (b64 of a chunk fits comfortably)
_EAGAIN = (11, 35, 110, 116)  # lwIP / BSD spellings of "no data yet"
IDLE_DROP_MS = 30000          # kick a silent client so OTA can't wedge
ACCEPT_EVERY_MS = 500         # accept probes are cheap natively; still no rush
REBIND_EVERY_MS = 5000        # how often to look for the radio while unbound
ALLOWED_SUFFIXES = (".py", ".json")
FORBIDDEN = ("boot.py",)

# Last noteworthy server event (client drop reason, handler exception),
# readable via OTALOG — the remote counterpart of the serial "ota:" prints.
LAST_OTA_ERROR = None


def _note_error(why):
    global LAST_OTA_ERROR
    LAST_OTA_ERROR = why


def server_from_settings():
    """OTAServer if settings.toml sets a non-empty OTA_TOKEN, else None."""
    token = os.getenv("OTA_TOKEN")
    if not token:
        print("ota: no OTA_TOKEN in settings.toml — OTA disabled")
        return None
    return OTAServer(token, int(os.getenv("OTA_PORT") or 8266))


def _safe_name(name):
    if not name or "/" in name or "\\" in name or name.startswith("."):
        return False
    if name in FORBIDDEN:
        return False
    for suffix in ALLOWED_SUFFIXES:
        if name.endswith(suffix):
            return True
    return False


def _fs_writable():
    """Probe: can code write the filesystem right now (boot.py remounted it)?"""
    try:
        with open(".ota_probe", "wb"):
            pass
        os.remove(".ota_probe")
        return True
    except OSError:
        return False


class OTAServer:
    def __init__(self, token, port=8266):
        self.token = token
        self.port = port
        self._epoch = None            # link.radio_epoch the listener was built on
        self._sock = None             # bound listener, or None while radio is down
        self._next_bind = ticks_ms()
        self._next_accept = ticks_ms()
        self._client = None
        self._rbuf = bytearray(512)
        self._buf = bytearray()
        self._last_rx = 0
        self._manifest = None         # name -> (size, sha256)
        self._written = {}
        self._hashes = {}             # name -> running sha256 over staged bytes
        self._file = None
        self._file_path = None

    # ------------------------------------------------------------- polling
    def poll(self, link, now):
        """Non-blocking service pass; call every frame (self-rate-limits)."""
        if link is None:
            return
        try:
            self._poll(link, now)
        except Exception as e:        # never let OTA take down the eye
            print("ota:", e)
            _note_error("poll: %r" % e)
            self._teardown()

    def _poll(self, link, now):
        # A listener from an older pool epoch is dead hardware — drop it; it
        # re-arms on the rebind window. (No teardown on a WiFi dip: an lwIP
        # listener bound to 0.0.0.0 survives reassociation, and tearing down
        # mid-session costs an active OTA client.)
        if self._sock is not None and link.radio_epoch != self._epoch:
            self._teardown("radio reset")
        if self._sock is None:
            if ticks_diff(now, self._next_bind) < 0:
                return
            self._next_bind = ticks_add(now, REBIND_EVERY_MS)
            self._bind(link)
            return
        if self._client is None:
            if ticks_diff(now, self._next_accept) < 0:
                return
            self._next_accept = ticks_add(now, ACCEPT_EVERY_MS)
            try:
                c, addr = self._sock.accept()
            except OSError:
                return
            c.settimeout(0)
            self._client = c
            self._buf = bytearray()
            self._last_rx = now
            print("ota: client", addr)
            return
        self._service(now)

    def _bind(self, link):
        radio, pool = link.radio()
        if pool is None:
            return
        try:
            s = pool.socket(pool.AF_INET, pool.SOCK_STREAM)
            try:
                # Without this, rebinding while a dropped client's socket
                # sits in TIME_WAIT fails for minutes (lwIP EADDRINUSE).
                s.setsockopt(pool.SOL_SOCKET, pool.SO_REUSEADDR, 1)
            except (AttributeError, OSError):
                pass
            s.bind(("0.0.0.0", self.port))
            s.listen(1)
            s.settimeout(0)
        except Exception as e:
            print("ota: listen failed:", e)
            return
        self._sock = s
        self._epoch = link.radio_epoch
        print("ota: listening on :%d" % self.port)

    def _service(self, now):
        got = False
        try:
            for _ in range(8):                 # drain a few chunks per pass
                n = self._client.recv_into(self._rbuf)
                if n == 0:                     # EOF: peer closed cleanly
                    self._drop_client("closed")
                    return
                got = True
                self._last_rx = now
                for b in memoryview(self._rbuf)[:n]:
                    if b == 0x0A:
                        try:
                            self._handle_line()
                        except Exception as e:
                            # A handler bug must cost one client, not the
                            # listener (proven the hard way: the S3's hashlib
                            # shape raised here and the resulting teardown +
                            # TIME_WAIT blacked out OTA for minutes per try).
                            self._drop_client("handler: %r" % e)
                            return
                        self._buf = bytearray()
                    elif len(self._buf) < LINE_LIMIT:
                        self._buf.append(b)
                    else:
                        self._drop_client("line too long")
                        return
        except OSError as e:
            if e.errno not in _EAGAIN:
                self._drop_client(str(e))
                return
        if got:
            return
        if ticks_diff(now, self._last_rx) > IDLE_DROP_MS:
            self._drop_client("idle")

    def _teardown(self, why=None):
        if why:
            print("ota: listener down (%s)" % why)
        if self._client is not None:
            self._drop_client(why or "teardown")
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None
        self._epoch = None

    def _drop_client(self, why):
        print("ota: client dropped (%s)" % why)
        if why not in ("closed", "idle"):  # keep routine reaps out of OTALOG
            _note_error("drop: %s" % why)
        try:
            self._client.close()
        except OSError:
            pass
        self._client = None
        self._cleanup()               # dropped mid-update -> nothing changes

    def _send(self, msg):
        try:
            self._client.send(msg.encode() + b"\n")
        except OSError:
            pass

    # ------------------------------------------------------------ commands
    def _handle_line(self):
        try:
            line = bytes(self._buf).decode().strip()
        except ValueError:
            return
        if not line:
            return
        parts = line.split()
        verb = parts[0]
        if verb not in ("OTABEGIN", "OTAFILE", "OTAPUT", "OTACOMMIT", "OTAABORT", "OTALOG"):
            self._send("ERR " + verb + " unsupported")
            return
        if len(parts) < 2 or parts[1] != self.token:
            self._send("ERR " + verb + " bad_token")
            return
        if verb == "OTALOG":
            # Remote diagnostics: the orchestrator-link's last failure + live
            # signal strength. The serial console for a board that's zip-tied
            # into the booth.
            rssi = None
            try:
                import wifi
                rssi = wifi.radio.ap_info.rssi if wifi.radio.ap_info else None
            except Exception:
                pass
            boot = None
            try:
                # Why the last reboot happened (POWER_ON / BROWNOUT /
                # SOFTWARE / WATCHDOG / RESET_PIN...) — a manual RESET press
                # overwrites it, so query BEFORE pressing the button.
                import microcontroller
                boot = str(microcontroller.cpu.reset_reason).split(".")[-1]
            except Exception:
                pass
            try:
                import inputs
                self._send("OK OTALOG fails=%d last=%s probe=%s rssi=%s ota=%s boot=%s"
                           % (inputs.LINK_FAILS, inputs.LAST_LINK_ERROR,
                              inputs.NET_PROBE, rssi, LAST_OTA_ERROR, boot))
            except Exception as e:
                self._send("OK OTALOG unavailable (%s)" % e)
            return
        args = parts[2:]
        if verb == "OTABEGIN":
            if not _fs_writable():
                # boot.py didn't remount: the host owns the drive (USB deploy
                # mode). Reset the board without UP held to enable OTA.
                self._send("ERR OTABEGIN read_only_fs")
                return
            self._cleanup()
            self._manifest = {}
            self._written = {}
            self._hashes = {}
            self._send("OK OTABEGIN")
        elif verb == "OTAFILE":
            self._send(self._file_decl(args) or "OK OTAFILE")
        elif verb == "OTAPUT":
            self._send(self._put(args))
        elif verb == "OTACOMMIT":
            msg = self._commit()
            self._send(msg)
            if msg.startswith("OK"):
                print("ota: updated, resetting")
                import time
                time.sleep(0.5)       # let the ack flush
                try:
                    import microcontroller
                    microcontroller.reset()
                except ImportError:   # desktop test harness
                    print("ota: (no microcontroller module - reset skipped)")
        elif verb == "OTAABORT":
            self._cleanup()
            self._send("OK OTAABORT")

    def _file_decl(self, args):
        if self._manifest is None:
            return "ERR OTAFILE no_begin"
        if len(args) < 3:
            return "ERR OTAFILE need_name_size_sha"
        name, size_s, sha = args[0], args[1], args[2]
        if not _safe_name(name):
            return "ERR OTAFILE bad_name"
        try:
            size = int(size_s)
        except ValueError:
            return "ERR OTAFILE bad_size"
        if size < 0:
            return "ERR OTAFILE bad_size"
        self._manifest[name] = (size, sha.lower())
        self._written[name] = 0
        self._hashes[name] = _sha256()
        return None

    def _put(self, args):
        if self._manifest is None:
            return "ERR OTAPUT no_begin"
        if len(args) < 3:
            return "ERR OTAPUT need_name_off_data"
        name, off_s, b64 = args[0], args[1], args[2]
        if name not in self._manifest:
            return "ERR OTAPUT not_declared"
        try:
            off = int(off_s)
            data = binascii.a2b_base64(b64)
        except ValueError:
            return "ERR OTAPUT bad_args"
        if len(data) > CHUNK_LIMIT:
            return "ERR OTAPUT too_long"
        if off != self._written[name]:
            return "ERR OTAPUT bad_off"
        if self._file_path != name:
            if self._file:
                self._file.close()
            try:
                self._file = open(name + ".new", "wb" if off == 0 else "ab")
            except OSError:
                self._file = None
                self._file_path = None
                return "ERR OTAPUT read_only_fs"
            self._file_path = name
        self._file.write(data)
        self._hashes[name].update(data)
        self._written[name] += len(data)
        return "OK OTAPUT %d" % self._written[name]

    def _commit(self):
        if self._manifest is None:
            return "ERR OTACOMMIT no_begin"
        if not self._manifest:
            return "ERR OTACOMMIT empty"
        if self._file:
            self._file.close()
            self._file = None
            self._file_path = None
        # Verify EVERYTHING before touching anything live. (Digests were
        # accumulated as chunks arrived — see the header note on why commit
        # doesn't re-read flash here like the NanoC6 does.)
        for name, (size, sha) in self._manifest.items():
            if self._written.get(name, 0) != size:
                return "ERR OTACOMMIT size %s" % name
            try:
                os.stat(name + ".new")
            except OSError:
                return "ERR OTACOMMIT missing %s" % name
            got = binascii.hexlify(self._hashes[name].digest()).decode()
            if got != sha:
                return "ERR OTACOMMIT sha256 %s" % name
        # Swap in (remove+rename per file; all content already verified).
        for name in self._manifest:
            try:
                os.remove(name)
            except OSError:
                pass
            os.rename(name + ".new", name)
        n = len(self._manifest)
        self._manifest = None
        self._written = {}
        self._hashes = {}
        return "OK OTACOMMIT %d" % n   # caller acks, then resets the board

    def _cleanup(self):
        if self._file:
            self._file.close()
        self._file = None
        self._file_path = None
        if self._manifest:
            for name in self._manifest:
                try:
                    os.remove(name + ".new")
                except OSError:
                    pass
        self._manifest = None
        self._written = {}
        self._hashes = {}


# --------------------------------------------------------------------------
# Pure-Python SHA-256 fallback for builds without hashlib (the retired M4's
# SAMD51 among them — the S3 has it native, so this is dormant there). Slow
# (~KB/s scale) but digests accumulate per 2KB chunk as they arrive, so no
# single ack stalls long enough to time out.
_K = (
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1,
    0x923F82A4, 0xAB1C5ED5, 0xD807AA98, 0x12835B01, 0x243185BE, 0x550C7DC3,
    0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174, 0xE49B69C1, 0xEFBE4786,
    0x0FC19DC6, 0x240CA1CC, 0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA,
    0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147,
    0x06CA6351, 0x14292967, 0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13,
    0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85, 0xA2BFE8A1, 0xA81A664B,
    0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070,
    0x19A4C116, 0x1E376C08, 0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A,
    0x5B9CCA4F, 0x682E6FF3, 0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208,
    0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
)


class _PySHA256:
    def __init__(self):
        self._h = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
                   0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19]
        self._pending = b""
        self._length = 0

    def update(self, data):
        self._length += len(data)
        buf = self._pending + bytes(data)
        n = len(buf) // 64 * 64
        for i in range(0, n, 64):
            self._block(buf[i:i + 64])
        self._pending = buf[n:]

    def digest(self):
        # Pad a copy so digest() is repeatable.
        length = self._length
        buf = self._pending + b"\x80"
        buf += b"\x00" * ((56 - len(buf) % 64) % 64)
        buf += (length * 8).to_bytes(8, "big")
        h = list(self._h)
        for i in range(0, len(buf), 64):
            self._block(buf[i:i + 64], h)
        return b"".join(x.to_bytes(4, "big") for x in h)

    def _block(self, block, h=None):
        if h is None:
            h = self._h
        w = list(int.from_bytes(block[i:i + 4], "big") for i in range(0, 64, 4))
        for i in range(16, 64):
            s0 = (_ror(w[i - 15], 7) ^ _ror(w[i - 15], 18) ^ (w[i - 15] >> 3))
            s1 = (_ror(w[i - 2], 17) ^ _ror(w[i - 2], 19) ^ (w[i - 2] >> 10))
            w.append((w[i - 16] + s0 + w[i - 7] + s1) & 0xFFFFFFFF)
        a, b, c, d, e, f, g, hh = h
        for i in range(64):
            s1 = _ror(e, 6) ^ _ror(e, 11) ^ _ror(e, 25)
            ch = (e & f) ^ (~e & g)
            t1 = (hh + s1 + ch + _K[i] + w[i]) & 0xFFFFFFFF
            s0 = _ror(a, 2) ^ _ror(a, 13) ^ _ror(a, 22)
            maj = (a & b) ^ (a & c) ^ (b & c)
            t2 = (s0 + maj) & 0xFFFFFFFF
            hh, g, f, e, d, c, b, a = g, f, e, (d + t1) & 0xFFFFFFFF, c, b, a, (t1 + t2) & 0xFFFFFFFF
        h[0] = (h[0] + a) & 0xFFFFFFFF
        h[1] = (h[1] + b) & 0xFFFFFFFF
        h[2] = (h[2] + c) & 0xFFFFFFFF
        h[3] = (h[3] + d) & 0xFFFFFFFF
        h[4] = (h[4] + e) & 0xFFFFFFFF
        h[5] = (h[5] + f) & 0xFFFFFFFF
        h[6] = (h[6] + g) & 0xFFFFFFFF
        h[7] = (h[7] + hh) & 0xFFFFFFFF


def _ror(x, n):
    return ((x >> n) | (x << (32 - n))) & 0xFFFFFFFF
