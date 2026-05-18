"""Smoke client for the Audio2Face-3D NIM.

Streams 16 kHz s16le mono PCM at localhost:52000 and prints frames received,
TTFB, and per-frame timing stats. Used as the hardware-acceptance gate for
the renderer host (see ../README.md Phase 1.6).

Usage:
    python smoke_a2f.py [WAV_PATH]

If WAV_PATH is omitted, a 2-second 200 Hz sawtooth is synthesized so the
script needs no external files to verify the gRPC path.

Requires: nvidia-audio2face-3d, nvidia-ace, grpcio (>=1.67), numpy.
"""

from __future__ import annotations

import argparse
import math
import statistics
import struct
import sys
import time
import wave
from pathlib import Path

import grpc

from nvidia_ace import audio_pb2
from nvidia_audio2face_3d import audio2face_pb2_grpc, messages_pb2

SAMPLE_RATE = 16000
BITS_PER_SAMPLE = 16
CHANNELS = 1
CHUNK_MS = 33  # ~one a2f frame; matches a2f's 30 fps animation cadence


def synth_pcm(duration_s: float = 2.0) -> bytes:
    """Generate a sawtooth as test stimulus when no WAV file is given."""
    n = int(SAMPLE_RATE * duration_s)
    out = bytearray(n * 2)
    period = SAMPLE_RATE // 200
    for i in range(n):
        v = int((i % period) / period * 32000 - 16000)
        struct.pack_into("<h", out, i * 2, v)
    return bytes(out)


def load_wav_pcm(path: Path) -> bytes:
    with wave.open(str(path), "rb") as w:
        if w.getframerate() != SAMPLE_RATE:
            sys.exit(
                f"smoke_a2f: {path} is {w.getframerate()} Hz; NIM wants {SAMPLE_RATE} Hz mono s16le. "
                f"Resample first (e.g. `ffmpeg -i in.wav -ar 16000 -ac 1 -sample_fmt s16 out.wav`)."
            )
        if w.getnchannels() != 1 or w.getsampwidth() != 2:
            sys.exit(
                f"smoke_a2f: {path} must be mono 16-bit; got channels={w.getnchannels()} "
                f"sample_width={w.getsampwidth()}."
            )
        return w.readframes(w.getnframes())


def request_iter(pcm: bytes, chunk_bytes: int, log_send):
    header = messages_pb2.AudioWithEmotionStreamHeader(
        audio_header=audio_pb2.AudioHeader(
            audio_format=audio_pb2.AudioHeader.AUDIO_FORMAT_PCM,
            channel_count=CHANNELS,
            samples_per_second=SAMPLE_RATE,
            bits_per_sample=BITS_PER_SAMPLE,
        ),
    )
    yield messages_pb2.AudioWithEmotionStream(audio_stream_header=header)

    for offset in range(0, len(pcm), chunk_bytes):
        chunk = pcm[offset : offset + chunk_bytes]
        log_send(offset, len(chunk))
        yield messages_pb2.AudioWithEmotionStream(
            audio_with_emotion=messages_pb2.AudioWithEmotion(audio_buffer=chunk),
        )

    yield messages_pb2.AudioWithEmotionStream(
        end_of_audio=messages_pb2.AudioWithEmotionStream.EndOfAudio(),
    )


def percentile(xs: list[float], p: float) -> float:
    if not xs:
        return float("nan")
    s = sorted(xs)
    k = max(0, min(len(s) - 1, int(math.ceil(p / 100 * len(s))) - 1))
    return s[k]


def run(target: str, pcm: bytes) -> int:
    chunk_bytes = SAMPLE_RATE * CHUNK_MS // 1000 * 2  # *2 for s16
    audio_seconds = len(pcm) / (SAMPLE_RATE * 2)
    print(
        f"smoke_a2f: target={target} audio={audio_seconds:.2f}s "
        f"chunk={CHUNK_MS}ms ({chunk_bytes} bytes)",
        flush=True,
    )

    send_times: list[float] = []
    recv_times: list[float] = []

    def log_send(offset, n):
        send_times.append(time.perf_counter())

    with grpc.insecure_channel(target) as ch:
        stub = audio2face_pb2_grpc.A2FControllerServiceStub(ch)
        t0 = time.perf_counter()
        try:
            for resp in stub.ProcessAudioStream(request_iter(pcm, chunk_bytes, log_send)):
                recv_times.append(time.perf_counter())
                if resp.HasField("animation_data_stream_header"):
                    bs = list(resp.animation_data_stream_header.skel_animation_header.blend_shapes)
                    print(f"  header: {len(bs)} blendshape names, e.g. {bs[:4]}", flush=True)
                elif resp.HasField("status"):
                    s = resp.status
                    if s.code != 0:
                        print(f"  status: code={s.code} message={s.message!r}", flush=True)
                elif resp.HasField("event"):
                    print(f"  event: type={resp.event.event_type}", flush=True)
        except grpc.RpcError as e:
            print(f"smoke_a2f: gRPC error: code={e.code()} details={e.details()!r}", flush=True)
            return 2

    t1 = time.perf_counter()
    if not recv_times:
        print("smoke_a2f: no responses received", flush=True)
        return 3

    frames = [r for r in recv_times[1:] if True]  # exclude header
    ttfb_ms = (recv_times[0] - t0) * 1000
    if len(frames) >= 2:
        gaps = [(b - a) * 1000 for a, b in zip(frames[:-1], frames[1:])]
        gap_p50 = statistics.median(gaps)
        gap_p99 = percentile(gaps, 99)
    else:
        gap_p50 = gap_p99 = float("nan")

    total_ms = (t1 - t0) * 1000
    rt_factor = audio_seconds / (total_ms / 1000) if total_ms > 0 else float("nan")

    print()
    print(f"  TTFB (first message)     : {ttfb_ms:8.1f} ms")
    print(f"  Total round-trip         : {total_ms:8.1f} ms  ({rt_factor:.2f}x realtime)")
    print(f"  Messages received        : {len(recv_times)}")
    print(f"  Animation frames         : {len(recv_times) - 1}")
    print(f"  Frame inter-arrival p50  : {gap_p50:8.1f} ms")
    print(f"  Frame inter-arrival p99  : {gap_p99:8.1f} ms")

    if ttfb_ms > 500 or rt_factor < 0.8:
        print("smoke_a2f: WARN — performance below expected (TTFB>500ms or <0.8x RT)", flush=True)
        return 1
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("wav", nargs="?", help="16 kHz mono s16le WAV; synthesized if omitted")
    p.add_argument("--target", default="localhost:52000", help="NIM gRPC endpoint")
    p.add_argument("--seconds", type=float, default=2.0, help="duration when synthesizing")
    a = p.parse_args()

    pcm = load_wav_pcm(Path(a.wav)) if a.wav else synth_pcm(a.seconds)
    return run(a.target, pcm)


if __name__ == "__main__":
    sys.exit(main())
