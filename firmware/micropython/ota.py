# Wet Court — WiFi OTA file updates for the MicroPython NanoC6 fleet.
#
# The update flow is staged, verified, and atomic.
# The device runs a tiny token-gated TCP listener (separate from the
# orchestrator link — the fleet DIALS the orchestrator, so updates need their
# own inbound port). wetline.py polls it whenever the link is idle, so OTA
# works even with the orchestrator down. Client: ../micropython/otapush.py.
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
#   - boot.py is refused outright (the USB-recovery guarantee)
#   - every line requires the token from secrets.py; no OTA_TOKEN = disabled

import os
import socket
import time
import binascii
import hashlib

CHUNK_LIMIT = 3000            # raw bytes per PUT after b64 decode
LINE_LIMIT = 8192             # OTA line cap (b64 of a chunk fits comfortably)
_EAGAIN = (11, 35)            # lwip / BSD spellings (35 = host-side testing)
IDLE_DROP_MS = 30000          # kick a silent client so OTA can't wedge
ALLOWED_SUFFIXES = (".py", ".json")
FORBIDDEN = ("boot.py",)


def server_from_secrets():
    """OTAServer if secrets.py sets a non-empty OTA_TOKEN, else None."""
    import secrets
    token = getattr(secrets, "OTA_TOKEN", None)
    if not token:
        return None
    return OTAServer(token, getattr(secrets, "OTA_PORT", 8266))


def _sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            buf = f.read(1024)
            if not buf:
                break
            h.update(buf)
    return binascii.hexlify(h.digest()).decode()


def _safe_name(name):
    if not name or "/" in name or "\\" in name or name.startswith("."):
        return False
    if name in FORBIDDEN:
        return False
    for suffix in ALLOWED_SUFFIXES:
        if name.endswith(suffix):
            return True
    return False


class OTAServer:
    def __init__(self, token, port=8266):
        self.token = token
        self._sock = socket.socket()
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("0.0.0.0", port))
        self._sock.listen(1)
        self._sock.setblocking(False)
        self._client = None
        self._buf = bytearray()
        self._last_rx = 0
        self._manifest = None     # name -> (size, sha256)
        self._written = {}
        self._file = None
        self._file_path = None
        print("ota: listening on :%d" % port)

    # ------------------------------------------------------------- polling
    def poll(self):
        """Non-blocking service pass; call from idle points in the main loop."""
        if self._client is None:
            try:
                c, addr = self._sock.accept()
            except OSError:
                return
            c.setblocking(False)
            self._client = c
            self._buf = bytearray()
            self._last_rx = time.ticks_ms()
            print("ota: client", addr)
            return
        try:
            for _ in range(8):                     # drain a few chunks per pass
                data = self._client.recv(512)
                if data == b"":
                    raise OSError(-1, "closed")
                self._last_rx = time.ticks_ms()
                for b in data:
                    if b == 0x0A:
                        self._handle_line()
                        self._buf = bytearray()
                    elif len(self._buf) < LINE_LIMIT:
                        self._buf.append(b)
                    else:
                        raise OSError(-1, "line too long")
        except OSError as e:
            if not (e.args and e.args[0] in _EAGAIN):   # EAGAIN = drained
                self._drop_client("closed" if e.args and e.args[0] == -1 else str(e))
                return
        if time.ticks_diff(time.ticks_ms(), self._last_rx) > IDLE_DROP_MS:
            self._drop_client("idle")

    def _drop_client(self, why):
        print("ota: client dropped (%s)" % why)
        try:
            self._client.close()
        except OSError:
            pass
        self._client = None
        self._cleanup()            # dropped mid-update -> nothing changes

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
        if verb not in ("OTABEGIN", "OTAFILE", "OTAPUT", "OTACOMMIT", "OTAABORT"):
            self._send("ERR " + verb + " unsupported")
            return
        if len(parts) < 2 or parts[1] != self.token:
            self._send("ERR " + verb + " bad_token")
            return
        args = parts[2:]
        if verb == "OTABEGIN":
            self._cleanup()
            self._manifest = {}
            self._written = {}
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
                time.sleep_ms(500)         # let the ack flush
                import machine
                machine.reset()
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
            self._file = open(name + ".new", "wb" if off == 0 else "ab")
            self._file_path = name
        self._file.write(data)
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
        # Verify EVERYTHING before touching anything live.
        for name, (size, sha) in self._manifest.items():
            if self._written.get(name, 0) != size:
                return "ERR OTACOMMIT size %s" % name
            try:
                got = _sha256_file(name + ".new")
            except OSError:
                return "ERR OTACOMMIT missing %s" % name
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
        return "OK OTACOMMIT %d" % n       # caller acks, then resets the board

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
