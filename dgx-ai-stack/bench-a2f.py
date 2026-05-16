#!/usr/bin/env python3
"""A2F-3D feasibility benchmark.

Streams an audio fixture into the audio2face container's /v1/face/stream WS,
collects the blendshape frames it emits, and reports the load-bearing numbers
for the feasibility decision:

  - frame rate (Hz) actually produced
  - end-to-end latency from last PCM byte sent to last blendshape frame received
  - GPU power / utilization during the run (proxy for VRAM use on GB10 since
    unified memory makes nvidia-smi memory.used N/A — same approach as
    sample-benchmark.py)
  - per-container memory delta from docker stats

Two run modes:

  --solo     : push a single audio fixture through A2F only. Measures the
               service in isolation.

  --with-trial : kick off a full /operator/start trial against the running
               orchestrator and capture A2F load while the rest of the stack
               (LLM, Kokoro, parakeet) is also busy. This is the realistic
               concurrency picture.

Run on the Mac with AI_STACK_HOST set, or directly on the Spark.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import subprocess
import sys
import time
import wave
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional

# websockets is the standard asyncio WS client. Add to your venv via
# `pip install websockets numpy` if not already there.
import websockets
import numpy as np

DEFAULT_SSH_HOST = os.environ.get("AI_STACK_HOST", "")
DEFAULT_A2F_URL = os.environ.get("A2F_URL", "ws://localhost:9000/v1/face/stream")
DEFAULT_A2F_HEALTH_URL = os.environ.get("A2F_HEALTH_URL", "http://localhost:9000/health")
DEFAULT_ORCH_URL = os.environ.get("ORCH_URL", "http://localhost:8080")

PCM_SAMPLE_RATE_HZ = 24000
PCM_BYTES_PER_SAMPLE = 2
TARGET_CHUNK_MS = 50  # match Kokoro's typical chunk pacing


@dataclass
class RunResult:
    audio_seconds: float
    bytes_sent: int
    frames_received: int
    first_frame_latency_ms: Optional[float]
    last_byte_to_last_frame_ms: Optional[float]
    frame_rate_hz: Optional[float]
    wallclock_s: float


def load_pcm_24k_mono(path: Path) -> bytes:
    """Read a wav and return raw int16 little-endian @ 24 kHz mono."""
    with wave.open(str(path), "rb") as w:
        nch, sw, sr, nframes = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
        raw = w.readframes(nframes)
    if sw != 2:
        raise SystemExit(f"{path}: expected 16-bit PCM, got {sw*8}-bit")
    a = np.frombuffer(raw, dtype=np.int16)
    if nch > 1:
        a = a.reshape(-1, nch).mean(axis=1).astype(np.int16)
    if sr != PCM_SAMPLE_RATE_HZ:
        # cheap linear resample — fine for benching the inference, not for audio fidelity
        from math import floor
        n_out = floor(len(a) * PCM_SAMPLE_RATE_HZ / sr)
        x_out = np.linspace(0, len(a) - 1, n_out)
        a = np.interp(x_out, np.arange(len(a)), a).astype(np.int16)
    return a.tobytes()


async def stream_once(url: str, pcm: bytes, chunk_ms: int = TARGET_CHUNK_MS) -> RunResult:
    """Push pcm at realtime pace; collect all frames; return measurements."""
    bytes_per_chunk = int(PCM_SAMPLE_RATE_HZ * chunk_ms / 1000) * PCM_BYTES_PER_SAMPLE
    audio_seconds = len(pcm) / (PCM_SAMPLE_RATE_HZ * PCM_BYTES_PER_SAMPLE)

    frames: List[dict] = []
    frame_ts: List[float] = []  # arrival wallclock for each frame
    last_byte_t: List[float] = []

    async def reader(ws):
        async for msg in ws:
            now = time.perf_counter()
            frame_ts.append(now)
            try:
                frames.append(json.loads(msg))
            except Exception:
                pass

    t0 = time.perf_counter()
    async with websockets.connect(url, max_size=2**22) as ws:
        rd = asyncio.create_task(reader(ws))
        # Push at realtime — match what Kokoro+orchestrator would do in production.
        target_per_chunk = chunk_ms / 1000.0
        offset = 0
        sent = 0
        while offset < len(pcm):
            chunk = pcm[offset:offset + bytes_per_chunk]
            offset += len(chunk)
            t_send = time.perf_counter()
            await ws.send(chunk)
            sent += len(chunk)
            # Pace.
            sleep_for = target_per_chunk - (time.perf_counter() - t_send)
            if sleep_for > 0:
                await asyncio.sleep(sleep_for)
        last_byte_t.append(time.perf_counter())
        # Give the server a generous window for tail frames.
        try:
            await asyncio.wait_for(rd, timeout=audio_seconds + 2.0)
        except asyncio.TimeoutError:
            rd.cancel()
        # Close cleanly.
    t1 = time.perf_counter()

    first_ms = (frame_ts[0] - t0) * 1000.0 if frame_ts else None
    last_byte_to_last_frame = None
    if last_byte_t and frame_ts:
        last_byte_to_last_frame = (frame_ts[-1] - last_byte_t[0]) * 1000.0
    fps = (len(frames) / audio_seconds) if audio_seconds > 0 else None

    return RunResult(
        audio_seconds=audio_seconds,
        bytes_sent=sent,
        frames_received=len(frames),
        first_frame_latency_ms=first_ms,
        last_byte_to_last_frame_ms=last_byte_to_last_frame,
        frame_rate_hz=fps,
        wallclock_s=t1 - t0,
    )


# ---- GPU / container sampling (cloned from sample-benchmark.py) -------------

@dataclass
class GpuSample:
    t: float
    power_w: float
    util_pct: float
    temp_c: float


class SparkMonitor:
    GPU_QUERY = "power.draw,utilization.gpu,temperature.gpu"

    def __init__(self, ssh_host: str, sample_ms: int = 250):
        self.ssh_host = ssh_host
        self.sample_ms = sample_ms
        self.proc: Optional[subprocess.Popen] = None
        self.samples: List[GpuSample] = []
        import threading
        self._thread: Optional[threading.Thread] = None
        self._lock = threading.Lock()

    def start(self):
        if not self.ssh_host:
            return
        import threading
        cmd = [
            "ssh", "-o", "ServerAliveInterval=10", "-o", "BatchMode=yes",
            self.ssh_host,
            f"nvidia-smi --query-gpu={self.GPU_QUERY} "
            f"--format=csv,noheader,nounits -lms {self.sample_ms}",
        ]
        self.proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                     text=True, bufsize=1)
        self._thread = threading.Thread(target=self._reader, daemon=True)
        self._thread.start()

    def _reader(self):
        for line in self.proc.stdout:  # type: ignore[union-attr]
            line = line.strip()
            if not line:
                continue
            try:
                pw, ut, tp = [x.strip() for x in line.split(",")]
                def f(x: str) -> float:
                    if x in ("[N/A]", "N/A", ""):
                        return float("nan")
                    return float(x)
                with self._lock:
                    self.samples.append(GpuSample(time.perf_counter(), f(pw), f(ut), f(tp)))
            except ValueError:
                continue

    def stop(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def window(self, t_start: float, t_end: float):
        with self._lock:
            wnd = [s for s in self.samples if t_start <= s.t <= t_end]
        if not wnd:
            return None
        def stats(xs):
            xs = [x for x in xs if x == x]  # filter NaN
            if not xs:
                return None
            return {"min": min(xs), "max": max(xs), "mean": statistics.mean(xs)}
        return {
            "n": len(wnd),
            "power_w": stats([s.power_w for s in wnd]),
            "util_pct": stats([s.util_pct for s in wnd]),
            "temp_c": stats([s.temp_c for s in wnd]),
        }


def docker_mem_snapshot(ssh_host: str, names: List[str]) -> dict:
    if not ssh_host or not names:
        return {}
    fmt = "{{.Name}}\t{{.MemUsage}}\t{{.MemPerc}}"
    cmd = ["ssh", "-o", "BatchMode=yes", ssh_host,
           f"docker stats --no-stream --format '{fmt}' {' '.join(names)}"]
    try:
        out = subprocess.check_output(cmd, text=True, timeout=10, stderr=subprocess.DEVNULL)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return {}
    snap = {}
    for line in out.strip().splitlines():
        parts = line.split("\t")
        if len(parts) >= 3:
            snap[parts[0]] = {"mem": parts[1], "pct": parts[2]}
    return snap


# ---- Orchestrator trigger ---------------------------------------------------

def trigger_trial(orch_url: str):
    import urllib.request
    req = urllib.request.Request(f"{orch_url.rstrip('/')}/operator/start", method="POST")
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            r.read()
    except Exception as e:
        print(f"WARN: could not POST /operator/start: {e}", file=sys.stderr)


# ---- Main -------------------------------------------------------------------

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--a2f-url", default=DEFAULT_A2F_URL)
    p.add_argument("--audio", default="sample_plea.wav")
    p.add_argument("--also", action="append", default=[],
                   help="extra audio fixtures to bench (e.g. --also judges_ruling.wav)")
    p.add_argument("--ssh-host", default=DEFAULT_SSH_HOST,
                   help="user@host for nvidia-smi sampling. Empty disables GPU monitoring.")
    p.add_argument("--containers", default="audio2face,kokoro,llama-server,parakeet,litellm",
                   help="comma-separated docker container names to snapshot memory for")
    p.add_argument("--with-trial", action="store_true",
                   help="POST /operator/start on orchestrator immediately before streaming, to "
                        "measure A2F under realistic concurrency")
    p.add_argument("--orch-url", default=DEFAULT_ORCH_URL)
    args = p.parse_args()

    audio_paths = [Path(args.audio)] + [Path(p) for p in args.also]
    for ap in audio_paths:
        if not ap.exists():
            raise SystemExit(f"missing audio fixture: {ap}")

    monitor = SparkMonitor(args.ssh_host) if args.ssh_host else None
    if monitor:
        monitor.start()
        time.sleep(2.0)  # idle baseline window

    containers = [c for c in args.containers.split(",") if c]
    mem_before = docker_mem_snapshot(args.ssh_host, containers)

    results: List[tuple[Path, RunResult, dict]] = []
    for ap in audio_paths:
        print(f"\n=== {ap.name} ===")
        pcm = load_pcm_24k_mono(ap)
        if args.with_trial:
            trigger_trial(args.orch_url)
        t_start = time.perf_counter()
        try:
            res = asyncio.run(stream_once(args.a2f_url, pcm))
        except Exception as e:
            print(f"  failed: {e}")
            continue
        t_end = time.perf_counter()
        gpu = monitor.window(t_start, t_end) if monitor else None
        print(f"  audio_s         : {res.audio_seconds:.2f}")
        print(f"  bytes_sent      : {res.bytes_sent}")
        print(f"  frames_received : {res.frames_received}")
        print(f"  frame_rate_hz   : {res.frame_rate_hz:.1f}" if res.frame_rate_hz else "  frame_rate_hz   : ?")
        print(f"  first_frame_ms  : {res.first_frame_latency_ms:.0f}" if res.first_frame_latency_ms else "  first_frame_ms  : ?")
        print(f"  last_byte→last_frame_ms : {res.last_byte_to_last_frame_ms:.0f}" if res.last_byte_to_last_frame_ms else "  last_byte→last_frame_ms : ?")
        if gpu and gpu.get("power_w"):
            p_w = gpu["power_w"]; u = gpu["util_pct"]
            print(f"  gpu_power_w     : mean {p_w['mean']:.1f} / peak {p_w['max']:.1f}")
            if u:
                print(f"  gpu_util_pct    : mean {u['mean']:.0f} / peak {u['max']:.0f}")
        results.append((ap, res, gpu or {}))

    mem_after = docker_mem_snapshot(args.ssh_host, containers)
    if monitor:
        monitor.stop()

    print("\n=== Memory snapshot ===")
    print(f"{'container':16s}  {'before':30s}  {'after':30s}")
    for c in containers:
        b = mem_before.get(c, {}).get("mem", "-")
        a = mem_after.get(c, {}).get("mem", "-")
        print(f"{c:16s}  {b:30s}  {a:30s}")

    if not results:
        print("\nNo successful runs.", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
