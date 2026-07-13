"""Desktop end-to-end test for the judge-face OTA port (CPython, no hardware).

Run: python3 firmware/judge-face/test_ota_desktop.py

Runs the REAL firmware/judge-face/ota.py server against a fake socket pool
(thin wrappers over CPython sockets, which already share the native
CircuitPython socketpool semantics the S3 uses), and drives it with the REAL
firmware/micropython/otapush.py over localhost TCP — both halves of the wire
exercised, plus unit checks for the pure-Python sha256 fallback and the
failure paths.
"""
import hashlib
import os
import secrets as pysecrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "firmware" / "judge-face"))

import ota  # noqa: E402  (the real firmware module)

TOKEN = "test-token-123"
PORT = 18266


# ---- fake socketpool plumbing --------------------------------------------------

class FakeSocket:
    """socketpool-flavored wrapper over a real CPython socket."""

    def __init__(self, raw):
        self._raw = raw

    def settimeout(self, t):
        assert t == 0
        self._raw.setblocking(False)

    def bind(self, addr):
        self._raw.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._raw.bind(addr)

    def listen(self, n):
        self._raw.listen(n)

    def accept(self):
        c, addr = self._raw.accept()   # raises BlockingIOError (OSError) if none
        return FakeSocket(c), addr

    def recv_into(self, buf):
        # CPython matches native socketpool here: no data raises
        # OSError(EAGAIN), EOF returns 0 (the firmware drops the client).
        return self._raw.recv_into(buf)

    def send(self, data):
        return self._raw.send(data)

    def close(self):
        self._raw.close()


class FakePool:
    AF_INET = socket.AF_INET
    SOCK_STREAM = socket.SOCK_STREAM

    def socket(self, *a):
        return FakeSocket(socket.socket(socket.AF_INET, socket.SOCK_STREAM))


class FakeLink:
    def __init__(self):
        self.radio_epoch = 1
        self._pool = FakePool()
        self.up = True

    def radio(self):
        return (None, self._pool) if self.up else (None, None)


def pump(srv, link, stop, period=0.005):
    """The code.py loop, sped up (accept probes are still 500ms-gated)."""
    while not stop.is_set():
        srv.poll(link, ota.ticks_ms())
        time.sleep(period)


# ---- tests --------------------------------------------------------------------

def test_pure_sha256_matches_hashlib():
    for size in (0, 1, 55, 56, 64, 65, 1000, 5000):
        data = pysecrets.token_bytes(size)
        py = ota._PySHA256()
        # feed in awkward chunk sizes
        for i in range(0, len(data), 37):
            py.update(data[i:i + 37])
        assert py.digest() == hashlib.sha256(data).digest(), f"size {size}"
        # digest() must be repeatable
        assert py.digest() == hashlib.sha256(data).digest()
    print("ok: pure sha256 matches hashlib (incl. padding edges)")


def test_sha256_probe_shapes():
    """ota._sha256 must cope with CircuitPython's new()-only hashlib.

    Regression: the S3's core hashlib has no .sha256() constructor — only
    hashlib.new("sha256") — and the resulting AttributeError in OTAFILE
    killed every real push while this suite stayed green on CPython.
    """
    import importlib
    import types
    real = sys.modules["hashlib"]
    stub = types.ModuleType("hashlib")
    stub.new = lambda name, data=b"": real.new(name, data)   # no .sha256 attr
    try:
        sys.modules["hashlib"] = stub
        importlib.reload(ota)
        h = ota._sha256()
        h.update(b"wet court")
        assert h.digest() == real.sha256(b"wet court").digest()
    finally:
        sys.modules["hashlib"] = real
        importlib.reload(ota)
    print("ok: _sha256 handles CircuitPython's new()-only hashlib shape")


def start_server(tmp):
    os.chdir(tmp)
    srv = ota.OTAServer(TOKEN, PORT)
    link = FakeLink()
    stop = threading.Event()
    t = threading.Thread(target=pump, args=(srv, link, stop), daemon=True)
    t.start()
    # wait for the listener (rebind window is 5s; first bind is immediate)
    deadline = time.time() + 8
    while srv._sock is None and time.time() < deadline:
        time.sleep(0.05)
    assert srv._sock is not None, "listener never bound"
    return srv, link, stop


def raw_cmd(sock, line):
    sock.sendall(line.encode() + b"\n")
    buf = b""
    while b"\n" not in buf:
        buf += sock.recv(1024)
    return buf.split(b"\n")[0].decode()


def test_end_to_end_push(tmp):
    # a board dir with settings.toml + otafiles.txt + a payload file
    (tmp / "settings.toml").write_text(
        f'OTA_TOKEN = "{TOKEN}"\nOTA_PORT = {PORT}\n')
    payload = pysecrets.token_bytes(9000) .hex().encode()   # ~18KB text
    (tmp / "eye_face.py").write_bytes(b"# old\n")
    (tmp / "new_eye.py").write_bytes(payload)
    (tmp / "otafiles.txt").write_text("# comment\nnew_eye.py\n")

    r = subprocess.run(
        [sys.executable, str(REPO / "firmware/micropython/otapush.py"), "127.0.0.1"],
        cwd=tmp, capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, f"otapush failed:\n{r.stdout}\n{r.stderr}"
    assert "OK OTACOMMIT 1" in r.stdout, r.stdout
    assert (tmp / "new_eye.py").read_bytes() == payload
    assert not (tmp / "new_eye.py.new").exists()
    print("ok: real otapush.py end-to-end (settings.toml + otafiles.txt)")


def test_failure_paths(tmp):
    c = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    assert raw_cmd(c, "OTABEGIN wrong-token") == "ERR OTABEGIN bad_token"
    assert raw_cmd(c, f"OTABEGIN {TOKEN}") == "OK OTABEGIN"
    assert raw_cmd(c, f"OTAFILE {TOKEN} boot.py 4 abcd") == "ERR OTAFILE bad_name"
    assert raw_cmd(c, f"OTAFILE {TOKEN} ../evil.py 4 abcd") == "ERR OTAFILE bad_name"
    assert raw_cmd(c, f"OTAFILE {TOKEN} x.txt 4 abcd") == "ERR OTAFILE bad_name"
    # declare a real file, then: bad offset, sha mismatch at commit
    import base64
    body = base64.b64encode(b"data").decode()
    assert raw_cmd(c, f"OTAFILE {TOKEN} x.py 4 {'0'*64}") == "OK OTAFILE"
    assert raw_cmd(c, f"OTAPUT {TOKEN} x.py 2 {body}") == "ERR OTAPUT bad_off"
    assert raw_cmd(c, f"OTAPUT {TOKEN} x.py 0 {body}") == "OK OTAPUT 4"
    assert raw_cmd(c, f"OTACOMMIT {TOKEN}").startswith("ERR OTACOMMIT sha256")
    # abort cleans staging
    assert raw_cmd(c, f"OTAABORT {TOKEN}") == "OK OTAABORT"
    assert not (tmp / "x.py.new").exists(), "abort left staging behind"
    assert not (tmp / "x.py").exists(), "failed commit touched live files"
    c.close()
    print("ok: token gate, name safety, bad offset, sha reject, abort cleanup")


def test_drop_mid_update_cleans_staging(tmp, srv):
    import base64
    c = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    assert raw_cmd(c, f"OTABEGIN {TOKEN}") == "OK OTABEGIN"
    body = base64.b64encode(b"half").decode()
    assert raw_cmd(c, f"OTAFILE {TOKEN} y.py 8 {'0'*64}") == "OK OTAFILE"
    assert raw_cmd(c, f"OTAPUT {TOKEN} y.py 0 {body}") == "OK OTAPUT 4"
    c.close()                                   # vanish mid-update
    deadline = time.time() + 5
    while srv._client is not None and time.time() < deadline:
        time.sleep(0.05)
    assert srv._client is None, "server never noticed the dropped client"
    assert not (tmp / "y.py.new").exists(), "drop left staging behind"
    print("ok: dropped connection mid-update cleans staging")


def test_handler_exception_drops_client_not_listener(tmp, srv):
    """A command-handler bug must cost one client, never the listener."""
    orig = srv._file_decl
    srv._file_decl = lambda args: (_ for _ in ()).throw(RuntimeError("boom"))
    try:
        c = socket.create_connection(("127.0.0.1", PORT), timeout=10)
        assert raw_cmd(c, f"OTABEGIN {TOKEN}") == "OK OTABEGIN"
        c.sendall(f"OTAFILE {TOKEN} z.py 4 {'0'*64}\n".encode())
        c.settimeout(5)
        assert c.recv(1024) == b"", "expected the client to be dropped"
        c.close()
    finally:
        srv._file_decl = orig
    assert srv._sock is not None, "listener died with the client"
    c = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    assert raw_cmd(c, f"OTABEGIN {TOKEN}") == "OK OTABEGIN"
    assert raw_cmd(c, f"OTAABORT {TOKEN}") == "OK OTAABORT"
    c.close()
    assert "boom" in str(ota.LAST_OTA_ERROR), "OTALOG never learned why"
    print("ok: handler exception drops the client, listener + OTALOG survive")


def test_radio_epoch_rearms(tmp, srv, link):
    old_sock = srv._sock
    link.radio_epoch += 1                       # simulate the link rebuilding its pool
    deadline = time.time() + 8                  # teardown + rebind (5s window)
    while (srv._sock is old_sock or srv._sock is None) and time.time() < deadline:
        time.sleep(0.05)
    assert srv._sock is not None and srv._sock is not old_sock, "listener not re-armed"
    c = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    assert raw_cmd(c, f"OTABEGIN {TOKEN}") == "OK OTABEGIN"
    assert raw_cmd(c, f"OTAABORT {TOKEN}") == "OK OTAABORT"
    c.close()
    print("ok: listener re-arms after a radio epoch bump")


def main():
    test_pure_sha256_matches_hashlib()
    test_sha256_probe_shapes()
    tmp = Path(tempfile.mkdtemp(prefix="wc_ota_"))
    srv, link, stop = start_server(tmp)
    try:
        test_end_to_end_push(tmp)
        test_failure_paths(tmp)
        test_drop_mid_update_cleans_staging(tmp, srv)
        test_handler_exception_drops_client_not_listener(tmp, srv)
        test_radio_epoch_rearms(tmp, srv, link)
    finally:
        stop.set()
    print("\nALL OTA TESTS PASSED")


if __name__ == "__main__":
    main()
