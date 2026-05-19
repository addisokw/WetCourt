"""Closed-loop end-to-end performance harness for the WetCourt pipeline.

Launches the packaged UE renderer, drives N back-to-back charge/plea/verdict
cycles through the orchestrator's /operator/* HTTP and /ws endpoints, tails the
UE log file for face-stream diagnostics, and reports per-cycle timing with
PASS/FAIL gates.

Assumes:
- A2F-3D NIM (docker container `a2f_test`) is running on :52000.
- Orchestrator (`booth.exe`) is running on :8080.
- Packaged UE renderer EXE is built (default: C:\\WetCourtBooth\\Windows\\BoothRenderer.exe).
- sample_plea.wav exists at the repo root.

Usage:
  python e2e_harness.py --cycles 5
  python e2e_harness.py --cycles 3 --baseline   # observational, no gate fails

Gates (FAIL when violated, unless --baseline):
  - every cycle's UE log shows "animation_started"
  - zero "CreateA2FStream returned null" warnings across the run
  - median plea_to_first_audio_ms <= 5000

Exit code: 0 on PASS or in --baseline mode, 1 on any gate FAIL.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import threading
import time
import wave
from dataclasses import dataclass, field
from glob import glob
from pathlib import Path
from typing import Any, Optional

import requests
import websockets


REPO_ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_EXE = r"C:\WetCourtBooth\Windows\BoothRenderer.exe"
DEFAULT_LOG_GLOB = r"C:\WetCourtBooth\Windows\BoothRenderer\Saved\Logs\BoothRenderer*.log"
DEFAULT_PLEA = str(REPO_ROOT / "sample_plea.wav")
DEFAULT_ORCH = "http://localhost:8080"
DEFAULT_WS = "ws://localhost:8080/ws"

# Log substring oracles (BoothFaceActor.cpp source-of-truth lines noted).
LOG_AUDIO_START = "audio session start"          # line 167
LOG_ANIM_START = "animation_started"             # line 349
LOG_ANIM_END = "animation_ended"                 # line 358
LOG_AUDIO_END = "audio session end"              # line 302
LOG_CREATE_NULL = "CreateA2FStream returned null"  # line 190
LOG_NOT_REGISTERED = "not registered"            # line 207
# ACE plugin logs this from TickComponent when AudioComponent->Play() fires.
# This is the actual moment audio playback begins (not when audio bytes
# arrive at UE — those queue up via the A2F response stream first).
LOG_AUDIO_PLAY = "start playing audio"
LOG_TS_RE = re.compile(r"^\[(\d{4}\.\d{2}\.\d{2}-\d{2}\.\d{2}\.\d{2}:\d{3})\]")


@dataclass
class CycleRecord:
    """Per-cycle event timestamps (monotonic seconds) and computed metrics."""
    idx: int
    wall_start: float = 0.0
    wall_end: float = 0.0
    events: dict[str, float] = field(default_factory=dict)
    first_audio_byte_ts: Optional[float] = None
    log_findings: dict[str, bool] = field(default_factory=dict)
    log_lines: list[str] = field(default_factory=list)
    error: Optional[str] = None

    def metric(self, a: str, b: str) -> Optional[float]:
        ta, tb = self.events.get(a), self.events.get(b)
        if ta is None or tb is None:
            return None
        return (tb - ta) * 1000.0

    def metrics(self) -> dict[str, Optional[float]]:
        plea = self.events.get("plea_complete")
        first_audio = self.first_audio_byte_ts
        plea_to_audio = (first_audio - plea) * 1000.0 if (plea is not None and first_audio is not None) else None
        return {
            "stt_ms": self.metric("plea_complete", "TranscriptReady"),
            "llm_ttft_ms": self.metric("TranscriptReady", "DeliberationToken"),
            "llm_stream_ms": self.metric("DeliberationToken", "DeliberationComplete"),
            "synth_open_ms": self.metric("DeliberationComplete", "TtsEmotion"),
            "plea_to_first_audio_ms": plea_to_audio,
            "tts_stream_ms": self.metric("TtsAudio", "TtsEnd"),
        }


class UeLogTail:
    """Daemon thread that tails the newest matching UE log file.

    Stores every line plus its arrival monotonic timestamp. `findings(t0, t1)`
    returns a dict of substring presence for the window [t0, t1].
    """

    def __init__(self, log_glob: str, verbose: bool = False, min_mtime: float = 0.0):
        self.log_glob = log_glob
        self.verbose = verbose
        self.min_mtime = min_mtime  # only consider files with mtime >= this
        self._lines: list[tuple[float, str]] = []
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._path: Optional[Path] = None

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2)

    def _find_latest(self) -> Optional[Path]:
        matches = glob(self.log_glob)
        if not matches:
            return None
        if self.min_mtime > 0:
            matches = [m for m in matches if os.path.getmtime(m) >= self.min_mtime]
        if not matches:
            return None
        return Path(max(matches, key=os.path.getmtime))

    def _run(self):
        deadline = time.monotonic() + 30
        while not self._stop.is_set() and time.monotonic() < deadline:
            p = self._find_latest()
            if p is not None:
                self._path = p
                break
            time.sleep(0.5)
        if self._path is None:
            return
        if self.verbose:
            print(f"[log-tail] following {self._path}", flush=True)
        try:
            f = open(self._path, "r", errors="replace", encoding="utf-8", newline="")
        except OSError as e:
            print(f"[log-tail] open failed: {e}", file=sys.stderr)
            return
        f.seek(0, os.SEEK_END)
        while not self._stop.is_set():
            line = f.readline()
            if not line:
                time.sleep(0.05)
                continue
            ts = time.monotonic()
            with self._lock:
                self._lines.append((ts, line.rstrip("\r\n")))
        f.close()

    def findings(self, t0: float, t1: float) -> tuple[dict[str, bool], list[str]]:
        with self._lock:
            window = [(ts, ln) for (ts, ln) in self._lines if t0 <= ts <= t1]
        relevant = []
        result = {
            "audio_session_start": False,
            "animation_started": False,
            "animation_ended": False,
            "audio_session_end": False,
            "create_a2f_null": False,
            "provider_not_registered": False,
            "anim_start_lag_ms": None,
            "verdict_session_frames": None,
            # `first_frame_lag_ms` from the audio session end log line:
            # time from session start (JSON header arrival) to first PCM
            # byte landing at UE. NOT the same as audio-playback-start
            # because Play() is gated on the SoundStreaming queue filling
            # via A2F's response stream, not raw WS arrivals.
            "verdict_first_frame_lag_ms": None,
            # `audio_play_lag_ms` is the actual Play() moment from the
            # ACE plugin's "start playing audio" log line — when audio
            # playback truly begins. Compared to `anim_start_lag` to get
            # the real user-perceived audio-to-anim gap.
            "audio_play_lag_ms": None,
            # User-perceived gap: animation_started timestamp minus
            # audio Play() timestamp. ~0 means perfect sync.
            "audio_to_anim_gap_ms": None,
        }
        # Pivot the window on `tts_emotion:` (UE log line, fires only for the
        # verdict). Audio sessions / animations BEFORE that line are the charge;
        # AFTER are the verdict.
        emotion_idx = None
        for idx, (ts, ln) in enumerate(window):
            if "tts_emotion:" in ln:
                emotion_idx = idx
                # Don't break — there could be multiple cycles' emotions in the
                # window if pacing is wrong. Take the latest one for safety.
        verdict_anim_start_ts = None
        verdict_session_start_ts = None
        verdict_audio_play_ts = None
        for idx, (ts, ln) in enumerate(window):
            in_verdict_phase = emotion_idx is not None and idx >= emotion_idx
            if LOG_AUDIO_START in ln:
                result["audio_session_start"] = True
                relevant.append(ln)
                if in_verdict_phase and verdict_session_start_ts is None:
                    verdict_session_start_ts = ts
            if LOG_AUDIO_PLAY in ln and in_verdict_phase and verdict_audio_play_ts is None:
                verdict_audio_play_ts = ts
                relevant.append(ln)
            if LOG_ANIM_START in ln:
                result["animation_started"] = True
                relevant.append(ln)
                if in_verdict_phase:
                    verdict_anim_start_ts = ts
            if LOG_ANIM_END in ln:
                result["animation_ended"] = True
                relevant.append(ln)
            if LOG_AUDIO_END in ln:
                result["audio_session_end"] = True
                relevant.append(ln)
                if in_verdict_phase:
                    m = re.search(r"frames=(\d+)", ln)
                    if m:
                        result["verdict_session_frames"] = int(m.group(1))
                    m2 = re.search(r"first_frame_lag_ms=([\d.]+)", ln)
                    if m2:
                        result["verdict_first_frame_lag_ms"] = float(m2.group(1))
            if LOG_CREATE_NULL in ln:
                result["create_a2f_null"] = True
                relevant.append(ln)
            if LOG_NOT_REGISTERED in ln and ("provider" in ln.lower() or "ACE" in ln):
                result["provider_not_registered"] = True
                relevant.append(ln)
        if verdict_anim_start_ts is not None and verdict_session_start_ts is not None:
            result["anim_start_lag_ms"] = (verdict_anim_start_ts - verdict_session_start_ts) * 1000.0
        # If we never found a verdict-phase animation_started, the bug repro'd —
        # mark "verdict_animation_started" specifically.
        result["verdict_animation_started"] = (verdict_anim_start_ts is not None)
        # User-perceived gap: animation_started vs the actual Play() call.
        # The ACE plugin's "start playing audio" log line is the authoritative
        # audio-playback-start signal — Play() is gated on the SoundStreaming
        # queue filling via A2F's response stream, not raw WS arrivals.
        if verdict_audio_play_ts is not None and verdict_session_start_ts is not None:
            result["audio_play_lag_ms"] = (verdict_audio_play_ts - verdict_session_start_ts) * 1000.0
        if verdict_audio_play_ts is not None and verdict_anim_start_ts is not None:
            result["audio_to_anim_gap_ms"] = (verdict_anim_start_ts - verdict_audio_play_ts) * 1000.0
        return result, relevant

    def wait_for_substring(self, substring: str, t_after: float, timeout: float) -> Optional[str]:
        """Block until a log line containing `substring` appears at or after `t_after`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._lock:
                for (ts, ln) in self._lines:
                    if ts >= t_after and substring in ln:
                        return ln
            time.sleep(0.05)
        return None


class EventRecorder:
    """Subscribes to /ws and stores per-cycle first-occurrence timestamps.

    Also tracks the first binary frame received after each TtsAudio header.
    """

    def __init__(self):
        self.cycles: list[CycleRecord] = []
        self._current: Optional[CycleRecord] = None
        self._waiting_for_first_audio = False
        self._lock = asyncio.Lock()
        self.event_waiters: dict[str, asyncio.Event] = {}

    def begin_cycle(self, idx: int) -> CycleRecord:
        rec = CycleRecord(idx=idx, wall_start=time.monotonic())
        self._current = rec
        self._waiting_for_first_audio = False
        self._verdict_phase = False
        self.cycles.append(rec)
        self.event_waiters = {}
        return rec

    def end_cycle(self):
        if self._current is not None:
            self._current.wall_end = time.monotonic()
        self._current = None
        self._waiting_for_first_audio = False
        self._verdict_phase = False

    def mark_plea_complete(self):
        if self._current is not None:
            self._current.events.setdefault("plea_complete", time.monotonic())

    async def waiter(self, event_name: str) -> asyncio.Event:
        if event_name not in self.event_waiters:
            self.event_waiters[event_name] = asyncio.Event()
        # If already present, fire immediately.
        if self._current is not None and event_name in self._current.events:
            self.event_waiters[event_name].set()
        return self.event_waiters[event_name]

    def _record_event(self, name: str):
        if self._current is None:
            return
        self._current.events.setdefault(name, time.monotonic())
        if name in self.event_waiters:
            self.event_waiters[name].set()

    def on_text(self, payload: dict):
        t = payload.get("type")
        if not t:
            return
        # Convert snake_case to PascalCase for prettier metric keys.
        name = "".join(p.capitalize() for p in t.split("_"))
        # The orchestrator fires a `tts_audio`/`tts_end` pair for the *charge*
        # announcement (Speak(charge) in DisplayingCharge state) BEFORE the
        # deliberation tokens stream. We only care about the *verdict* TTS,
        # which is always preceded by `tts_emotion`. So we suppress the
        # pre-emotion TtsAudio/TtsEnd events from being recorded as the
        # canonical ones, then re-arm after TtsEmotion lands.
        if t == "tts_emotion":
            # Mark that the verdict TTS is about to start. Clear any prior
            # TtsAudio/TtsEnd/first_audio_byte_ts captured for the charge.
            if self._current is not None:
                self._current.events.pop("TtsAudio", None)
                self._current.events.pop("TtsEnd", None)
                self._current.first_audio_byte_ts = None
                # Also clear the waiters so the next TtsAudio re-triggers.
                for ev in ("TtsAudio", "TtsEnd"):
                    if ev in self.event_waiters:
                        self.event_waiters[ev] = asyncio.Event()
            self._waiting_for_first_audio = True  # arm for the verdict's first binary
            self._verdict_phase = True
            self._record_event(name)
            return
        self._record_event(name)
        if t == "tts_audio" and getattr(self, "_verdict_phase", False):
            self._waiting_for_first_audio = True

    def on_binary(self, _data: bytes):
        if self._current is None:
            return
        # Only record the first binary frame that arrives *after* tts_emotion
        # (i.e. during the verdict TTS phase). Pre-emotion binary frames are
        # the charge audio and we ignore them for timing purposes.
        if self._waiting_for_first_audio and getattr(self, "_verdict_phase", False) and self._current.first_audio_byte_ts is None:
            self._current.first_audio_byte_ts = time.monotonic()
            self._waiting_for_first_audio = False


async def ws_reader(ws, rec: EventRecorder, verbose: bool):
    suppress = {"deliberation_token"}  # too chatty
    async for msg in ws:
        if isinstance(msg, (bytes, bytearray)):
            rec.on_binary(bytes(msg))
        else:
            try:
                payload = json.loads(msg)
            except json.JSONDecodeError:
                continue
            if verbose:
                t = payload.get("type")
                if t not in suppress:
                    print(f"  [ws] {t}", flush=True)
            rec.on_text(payload)


async def wait_for_event(rec: EventRecorder, name: str, timeout: float, label: str):
    waiter = await rec.waiter(name)
    try:
        await asyncio.wait_for(waiter.wait(), timeout=timeout)
    except asyncio.TimeoutError as e:
        raise asyncio.TimeoutError(f"timeout waiting for {name} ({label}) after {timeout}s") from e


def read_plea_bytes(path: str) -> bytes:
    """Read the WAV file's raw bytes. The STT endpoint accepts WAV; the
    orchestrator just concatenates ws binary frames and forwards to STT."""
    with open(path, "rb") as f:
        return f.read()


def chunk_bytes(data: bytes, chunk_size: int = 65536):
    for i in range(0, len(data), chunk_size):
        yield data[i:i + chunk_size]


async def run_cycle(ws, http_session: requests.Session, orch: str,
                    plea: bytes, idx: int, rec: EventRecorder,
                    timeout: float, verbose: bool, wait_idle_first: bool = False):
    cycle = rec.begin_cycle(idx)
    print(f"\n=== cycle {idx} ===", flush=True)
    loop = asyncio.get_running_loop()

    # 1. /operator/start
    await loop.run_in_executor(None, lambda: http_session.post(f"{orch}/operator/start", timeout=5))
    await wait_for_event(rec, "ShowCharge", timeout, "after /operator/start")

    # 2. /operator/plea (cuts charge dwell short, enters AwaitingPlea)
    await loop.run_in_executor(None, lambda: http_session.post(f"{orch}/operator/plea", timeout=5))
    await wait_for_event(rec, "StartPleaRecording", timeout, "after /operator/plea")

    # 3. Stream plea audio over the ws. Each chunk preceded by the protocol
    # header text (no-op on server, present for protocol correctness).
    for chunk in chunk_bytes(plea):
        await ws.send(json.dumps({"type": "plea_audio_chunk"}))
        await ws.send(chunk)
    await ws.send(json.dumps({"type": "plea_audio_complete"}))
    rec.mark_plea_complete()
    if verbose:
        print(f"  plea sent: {len(plea)} bytes", flush=True)

    # 4. Wait through the pipeline.
    await wait_for_event(rec, "Transcribing", timeout, "STT start")
    await wait_for_event(rec, "TranscriptReady", timeout, "STT done")
    await wait_for_event(rec, "DeliberationToken", timeout, "LLM first token")
    await wait_for_event(rec, "DeliberationComplete", timeout, "LLM stream end")
    await wait_for_event(rec, "TtsEmotion", timeout, "emotion emit")
    await wait_for_event(rec, "TtsAudio", timeout, "tts header")
    # First binary frame: poll for first_audio_byte_ts.
    deadline = time.monotonic() + timeout
    while cycle.first_audio_byte_ts is None and time.monotonic() < deadline:
        await asyncio.sleep(0.02)
    if cycle.first_audio_byte_ts is None:
        raise asyncio.TimeoutError("timeout waiting for first audio binary frame")
    await wait_for_event(rec, "TtsEnd", timeout, "tts stream end")
    await wait_for_event(rec, "Verdict", timeout, "verdict")
    await wait_for_event(rec, "Cooldown", timeout * 2, "cooldown")
    # Capture Idle (the post-cooldown transition) within THIS cycle so the
    # next cycle doesn't race the inter-cycle settle vs the orchestrator's
    # idle transition.
    await wait_for_event(rec, "Idle", timeout, "post-cooldown idle")
    rec.end_cycle()


def evaluate_gates(cycle: CycleRecord, baseline: bool) -> tuple[bool, list[str]]:
    if baseline:
        return True, []
    fails = []
    if not cycle.log_findings.get("verdict_animation_started"):
        fails.append("verdict animation_started missing")
    if cycle.log_findings.get("create_a2f_null"):
        fails.append("CreateA2FStream returned null")
    if cycle.log_findings.get("provider_not_registered"):
        fails.append("provider not registered")
    lag = cycle.log_findings.get("anim_start_lag_ms")
    if lag is not None and lag > 2000:
        fails.append(f"anim_start_lag_ms={lag:.0f}")
    frames = cycle.log_findings.get("verdict_session_frames")
    if frames is not None and frames == 0:
        fails.append("verdict_session_frames=0 (stub)")
    m = cycle.metrics()
    pfa = m.get("plea_to_first_audio_ms")
    if pfa is None or pfa > 5000:
        fails.append(f"plea_to_first_audio_ms={pfa}")
    return (not fails), fails


def render_report(cycles: list[CycleRecord], baseline: bool):
    cols = ["#", "stt", "ttft", "llm", "plea2audio", "tts", "1st_frm", "play_lag", "anim_lag", "a2a_gap", "frames", "anim", "null", "verdict"]
    widths = [3, 6, 6, 6, 11, 6, 8, 9, 9, 8, 7, 5, 5, 8]
    header = "  ".join(c.ljust(w) for c, w in zip(cols, widths))
    print("\n" + header)
    print("  ".join("-" * w for w in widths))
    pfa_values: list[float] = []
    pass_count = 0
    for c in cycles:
        m = c.metrics()
        def fmt(v):
            return f"{v:.0f}" if isinstance(v, (int, float)) and v is not None else "-"
        ok, fails = evaluate_gates(c, baseline)
        if ok:
            pass_count += 1
        anim_glyph = "Y" if c.log_findings.get("verdict_animation_started") else "N"
        null_glyph = "Y" if c.log_findings.get("create_a2f_null") else "N"
        anim_lag = c.log_findings.get("anim_start_lag_ms")
        first_frame = c.log_findings.get("verdict_first_frame_lag_ms")
        play_lag = c.log_findings.get("audio_play_lag_ms")
        a2a_gap = c.log_findings.get("audio_to_anim_gap_ms")
        frames = c.log_findings.get("verdict_session_frames")
        verdict_glyph = "PASS" if ok else "FAIL"
        if c.error:
            verdict_glyph = "ERR"
        row = [
            str(c.idx).ljust(widths[0]),
            fmt(m["stt_ms"]).ljust(widths[1]),
            fmt(m["llm_ttft_ms"]).ljust(widths[2]),
            fmt(m["llm_stream_ms"]).ljust(widths[3]),
            fmt(m["plea_to_first_audio_ms"]).ljust(widths[4]),
            fmt(m["tts_stream_ms"]).ljust(widths[5]),
            fmt(first_frame).ljust(widths[6]),
            fmt(play_lag).ljust(widths[7]),
            fmt(anim_lag).ljust(widths[8]),
            fmt(a2a_gap).ljust(widths[9]),
            (str(frames) if frames is not None else "-").ljust(widths[10]),
            anim_glyph.ljust(widths[11]),
            null_glyph.ljust(widths[12]),
            verdict_glyph.ljust(widths[13]),
        ]
        print("  ".join(row))
        if isinstance(m["plea_to_first_audio_ms"], (int, float)) and m["plea_to_first_audio_ms"] is not None:
            pfa_values.append(m["plea_to_first_audio_ms"])
        if c.error:
            print(f"     err: {c.error}")
        if not ok and fails:
            print(f"     fails: {', '.join(fails)}")

    if pfa_values:
        med = statistics.median(pfa_values)
        mx = max(pfa_values)
        print(f"\nplea→first-audio: median={med:.0f}ms max={mx:.0f}ms (n={len(pfa_values)})")
    print(f"cycles: {pass_count}/{len(cycles)} pass ({'baseline mode' if baseline else 'gates ON'})")


def render_raw_diagnostics(cycles: list[CycleRecord], max_lines: int = 12):
    """Sanity-check dump for the first cycle: print every event and matched log line."""
    if not cycles:
        return
    print("\n=== raw diagnostics (cycle 1) ===")
    c = cycles[0]
    print("events:")
    for name, ts in sorted(c.events.items(), key=lambda x: x[1]):
        rel = (ts - c.wall_start) * 1000.0
        print(f"  +{rel:7.0f}ms  {name}")
    if c.first_audio_byte_ts is not None:
        rel = (c.first_audio_byte_ts - c.wall_start) * 1000.0
        print(f"  +{rel:7.0f}ms  <first audio binary frame>")
    print(f"log findings: {c.log_findings}")
    if c.log_lines:
        print("matched log lines:")
        for ln in c.log_lines[:max_lines]:
            print(f"  {ln}")
        if len(c.log_lines) > max_lines:
            print(f"  ... +{len(c.log_lines) - max_lines} more")


def find_existing_renderer() -> Optional[int]:
    """Return the PID of an existing BoothRenderer process, or None."""
    if sys.platform != "win32":
        return None
    try:
        out = subprocess.check_output(
            ["tasklist", "/FI", "IMAGENAME eq BoothRenderer.exe", "/FO", "CSV", "/NH"],
            text=True, stderr=subprocess.DEVNULL, timeout=5,
        )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return None
    for line in out.splitlines():
        line = line.strip().strip('"')
        if not line or "INFO:" in line.upper():
            continue
        parts = [p.strip().strip('"') for p in line.split('","')]
        if len(parts) >= 2 and parts[0].lower().startswith("boothrenderer"):
            try:
                return int(parts[1])
            except ValueError:
                continue
    return None


def launch_renderer(exe: str) -> subprocess.Popen:
    print(f"[harness] launching renderer: {exe}", flush=True)
    # No CREATE_NEW_PROCESS_GROUP — UE is a GUI app and CTRL_BREAK doesn't work.
    return subprocess.Popen([exe])


def stop_renderer(proc: Optional[subprocess.Popen] = None, pid: Optional[int] = None):
    target_pid = pid
    if proc is not None and proc.poll() is None:
        target_pid = proc.pid
    if target_pid is None:
        return
    print(f"[harness] stopping renderer (PID {target_pid})", flush=True)
    if sys.platform == "win32":
        # /F = force, /T = kill child processes too. UE doesn't respond to soft signals.
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(target_pid)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=10,
        )
        if proc is not None:
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        return
    if proc is not None:
        try:
            proc.terminate()
            proc.wait(timeout=3)
        except (subprocess.TimeoutExpired, OSError):
            try:
                proc.kill()
            except OSError:
                pass


async def wait_for_renderer(orch: str, ws_url: str, timeout: float = 30) -> bool:
    """Poll /health and wait for an Idle event on a probe ws subscription."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            r = requests.get(f"{orch}/health", timeout=2)
            if r.ok:
                break
        except requests.RequestException:
            pass
        await asyncio.sleep(0.5)
    else:
        return False
    # Probe ws for at most 10s — the EXE may still be loading UE assets.
    end = time.monotonic() + 15
    while time.monotonic() < end:
        try:
            async with websockets.connect(ws_url, open_timeout=3) as probe:
                first = await asyncio.wait_for(probe.recv(), timeout=3)
                try:
                    obj = json.loads(first)
                    if obj.get("type") == "idle":
                        return True
                except json.JSONDecodeError:
                    pass
        except (OSError, asyncio.TimeoutError, websockets.exceptions.WebSocketException):
            await asyncio.sleep(0.5)
    return True  # orchestrator is up; renderer ws probe is best-effort


async def main_async(args) -> int:
    plea = read_plea_bytes(args.plea_wav)
    rec = EventRecorder()

    proc: Optional[subprocess.Popen] = None
    attached_pid: Optional[int] = None
    launch_time = time.time()
    existing_pid = find_existing_renderer()
    if existing_pid is not None and not args.force_launch:
        print(f"[harness] attaching to existing BoothRenderer PID {existing_pid}", flush=True)
        attached_pid = existing_pid
        launch_time = 0.0  # accept the current log file
        # Verify orchestrator reachable.
        try:
            r = await asyncio.get_running_loop().run_in_executor(
                None, lambda: requests.get(f"{args.orch}/health", timeout=3)
            )
            if not r.ok:
                print("ERROR: orchestrator /health not OK", file=sys.stderr)
                return 1
        except requests.RequestException as e:
            print(f"ERROR: orchestrator not reachable: {e}", file=sys.stderr)
            return 1
    elif not args.no_launch:
        launch_time = time.time()
        proc = launch_renderer(args.exe)
        ok = await wait_for_renderer(args.orch, args.ws)
        if not ok:
            print("ERROR: orchestrator not reachable on /health", file=sys.stderr)
            stop_renderer(proc)
            return 1
        # Give UE a moment to connect and emit its initial logs.
        await asyncio.sleep(args.warmup)
    else:
        print("[harness] --no-launch: assuming renderer is already running", flush=True)
        launch_time = 0.0

    # Start tailing only AFTER the EXE has had time to rotate its log file.
    # We require mtime >= launch_time so we don't latch onto the old log.
    tail = UeLogTail(args.log_glob, verbose=args.verbose, min_mtime=launch_time)
    tail.start()
    # Give the tail thread a moment to find the file.
    await asyncio.sleep(1.0)

    try:
        async with websockets.connect(args.ws, open_timeout=5, max_size=None) as ws:
            # Drain the initial Idle event.
            try:
                await asyncio.wait_for(ws.recv(), timeout=3)
            except asyncio.TimeoutError:
                pass

            reader = asyncio.create_task(ws_reader(ws, rec, args.verbose))

            with requests.Session() as http:
                for i in range(1, args.cycles + 1):
                    cyc_start = time.monotonic()
                    try:
                        await run_cycle(ws, http, args.orch, plea, i, rec, args.cycle_timeout, args.verbose,
                                        wait_idle_first=(i > 1))
                    except asyncio.TimeoutError as e:
                        print(f"  cycle {i}: TIMEOUT: {e}", flush=True)
                        if rec.cycles and rec.cycles[-1].idx == i:
                            rec.cycles[-1].error = str(e)
                            rec.end_cycle()
                    except Exception as e:
                        print(f"  cycle {i}: ERROR: {e}", flush=True)
                        if rec.cycles and rec.cycles[-1].idx == i:
                            rec.cycles[-1].error = repr(e)
                            rec.end_cycle()
                    # Wait specifically for the VERDICT's animation_started.
                    # The cycle has TWO animation_starteds per fully-healthy cycle:
                    # one for the charge announcement, one for the verdict. We
                    # anchor the wait to the ws TtsEmotion event (verdict-only)
                    # so we don't get fooled by the charge's animation_started.
                    cur = rec.cycles[-1] if rec.cycles and rec.cycles[-1].idx == i else None
                    emotion_ts = cur.events.get("TtsEmotion") if cur else None
                    if emotion_ts is not None:
                        print(f"  [cycle {i}] waiting up to {args.anim_wait:.0f}s for VERDICT animation_started...", flush=True)
                        loop2 = asyncio.get_running_loop()
                        start_line = await loop2.run_in_executor(
                            None,
                            lambda: tail.wait_for_substring("animation_started", emotion_ts, args.anim_wait)
                        )
                        if start_line is None:
                            print(f"  [cycle {i}] no VERDICT animation_started within {args.anim_wait:.0f}s", flush=True)
                    cyc_end = time.monotonic()
                    findings, lines = tail.findings(cyc_start, cyc_end + 1.0)
                    if rec.cycles and rec.cycles[-1].idx == i:
                        rec.cycles[-1].log_findings = findings
                        rec.cycles[-1].log_lines = lines
                    # Small extra settle.
                    await asyncio.sleep(args.inter_cycle)

            reader.cancel()
            try:
                await reader
            except (asyncio.CancelledError, Exception):
                pass

            # Best-effort estop.
            try:
                http_estop = requests.post(f"{args.orch}/operator/estop", timeout=3)
                _ = http_estop  # noqa
            except requests.RequestException:
                pass
    finally:
        # Only kill what WE launched. Never kill an attached, pre-existing
        # renderer — the user owns its lifecycle.
        if proc is not None and not args.keep_exe:
            stop_renderer(proc=proc)
        elif attached_pid is not None:
            print(f"[harness] leaving attached renderer (PID {attached_pid}) running", flush=True)
        tail.stop()

    render_report(rec.cycles, args.baseline)
    if args.diagnostics or args.baseline:
        render_raw_diagnostics(rec.cycles)

    if args.baseline:
        return 0
    # Overall result
    all_ok = all(evaluate_gates(c, args.baseline)[0] for c in rec.cycles) and len(rec.cycles) == args.cycles
    return 0 if all_ok else 1


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cycles", type=int, default=5)
    ap.add_argument("--exe", default=DEFAULT_EXE)
    ap.add_argument("--orch", default=DEFAULT_ORCH)
    ap.add_argument("--ws", default=DEFAULT_WS)
    ap.add_argument("--plea-wav", default=DEFAULT_PLEA)
    ap.add_argument("--log-glob", default=DEFAULT_LOG_GLOB)
    ap.add_argument("--cycle-timeout", type=float, default=20.0,
                    help="per-event-await timeout (seconds)")
    ap.add_argument("--anim-wait", type=float, default=10.0,
                    help="seconds to wait for animation_started log per cycle")
    ap.add_argument("--inter-cycle", type=float, default=2.0,
                    help="settle seconds between cycles (lets Cooldown finish)")
    ap.add_argument("--warmup", type=float, default=8.0,
                    help="seconds to wait after launching the EXE for UE to fully boot")
    ap.add_argument("--baseline", action="store_true",
                    help="observational mode; never FAILs gates, always prints diagnostics")
    ap.add_argument("--diagnostics", action="store_true",
                    help="print raw event timeline + log excerpts for cycle 1")
    ap.add_argument("--no-launch", action="store_true",
                    help="don't launch the EXE; assume it's already running")
    ap.add_argument("--force-launch", action="store_true",
                    help="launch a new EXE even if BoothRenderer.exe is already running (rare; harness auto-attaches by default)")
    ap.add_argument("--keep-exe", action="store_true",
                    help="leave the EXE running on exit (only applies to harness-launched EXE; attached ones are never killed)")
    ap.add_argument("--verbose", action="store_true")
    return ap.parse_args()


def main() -> int:
    # Default stdout to UTF-8 on Windows; the cp1252 default mangles non-ASCII.
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except Exception:
        pass
    args = parse_args()
    return asyncio.run(main_async(args))


if __name__ == "__main__":
    sys.exit(main())
