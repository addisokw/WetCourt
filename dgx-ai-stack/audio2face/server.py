"""Audio2Face-3D streaming WS server.

Accepts 24 kHz s16le PCM as binary WS frames on /v1/face/stream and emits
JSON blendshape frames (ARKit-52 weights) at ~30 Hz.

Modes (env A2F_MODE):
  - cuda      : real A2F-3D on GPU. Falls back to `amplitude` if import fails.
  - cpu       : real A2F-3D forced to CPU (slow; for diagnostic comparison).
  - amplitude : synthetic stub. jaw_open weight tracks RMS amplitude of the
                last audio window. Lets us verify the WS plumbing end-to-end
                before A2F itself is known-working on this hardware.

The output frame shape is deliberately renderer-agnostic — plain ARKit-52
weights anchored to an `audio_offset_ms` in the input stream. UE5 LiveLink,
three.js, and Unity ARKit blendshape sinks all consume this shape.
"""
import asyncio
import json
import logging
import os
import time
from typing import Optional

import numpy as np
import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect

# ---- Config -----------------------------------------------------------------

A2F_MODE = os.environ.get("A2F_MODE", "cuda").lower()
A2F_CHARACTER = os.environ.get("A2F_CHARACTER", "claire")
PCM_SAMPLE_RATE_HZ = 24000
PCM_BYTES_PER_SAMPLE = 2  # s16le, mono
FRAME_RATE_HZ = float(os.environ.get("A2F_FRAME_RATE_HZ", "30"))
SAMPLES_PER_FRAME = int(PCM_SAMPLE_RATE_HZ / FRAME_RATE_HZ)
ARKIT_52 = [
    "browDownLeft", "browDownRight", "browInnerUp", "browOuterUpLeft", "browOuterUpRight",
    "cheekPuff", "cheekSquintLeft", "cheekSquintRight",
    "eyeBlinkLeft", "eyeBlinkRight", "eyeLookDownLeft", "eyeLookDownRight",
    "eyeLookInLeft", "eyeLookInRight", "eyeLookOutLeft", "eyeLookOutRight",
    "eyeLookUpLeft", "eyeLookUpRight", "eyeSquintLeft", "eyeSquintRight",
    "eyeWideLeft", "eyeWideRight",
    "jawForward", "jawLeft", "jawOpen", "jawRight",
    "mouthClose", "mouthDimpleLeft", "mouthDimpleRight", "mouthFrownLeft", "mouthFrownRight",
    "mouthFunnel", "mouthLeft", "mouthLowerDownLeft", "mouthLowerDownRight",
    "mouthPressLeft", "mouthPressRight", "mouthPucker", "mouthRight", "mouthRollLower",
    "mouthRollUpper", "mouthShrugLower", "mouthShrugUpper", "mouthSmileLeft", "mouthSmileRight",
    "mouthStretchLeft", "mouthStretchRight", "mouthUpperUpLeft", "mouthUpperUpRight",
    "noseSneerLeft", "noseSneerRight", "tongueOut",
]
JAW_OPEN_IDX = ARKIT_52.index("jawOpen")
assert len(ARKIT_52) == 52

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("a2f")

# ---- Model loading ----------------------------------------------------------

class A2FRunner:
    """Wraps either real A2F-3D inference or the amplitude stub."""

    def __init__(self, mode: str):
        self.mode = mode
        self.model = None
        if mode in ("cuda", "cpu"):
            self.mode = self._try_load(mode)
        log.info("A2F runner ready: mode=%s character=%s", self.mode, A2F_CHARACTER)

    def _try_load(self, target_mode: str) -> str:
        try:
            # The exact import shape of nvidia-audio2face-3d may differ between
            # releases. This block is best-effort; on any failure we drop to the
            # amplitude stub so the WS plumbing is still exercisable.
            import nvidia_audio_2face_3d as a2f  # type: ignore
            device = "cuda" if target_mode == "cuda" else "cpu"
            self.model = a2f.load_model(character=A2F_CHARACTER, device=device)  # type: ignore
            log.info("loaded nvidia-audio2face-3d on %s", device)
            return target_mode
        except Exception as e:  # noqa: BLE001
            log.warning("could not load nvidia-audio2face-3d (%s); falling back to amplitude stub", e)
            return "amplitude"

    def infer_frame(self, pcm_window: np.ndarray) -> np.ndarray:
        """Return 52 float weights for one ~33 ms window of int16 PCM."""
        if self.mode == "amplitude":
            return self._amplitude_stub(pcm_window)
        # Real path. The exact call signature depends on the package version;
        # this is intentionally minimal and will likely need tweaking once we
        # see what the import surface actually looks like on the Spark.
        try:
            weights = self.model.infer(pcm_window.astype(np.float32) / 32768.0)  # type: ignore[attr-defined]
            return np.asarray(weights, dtype=np.float32).reshape(-1)[:52]
        except Exception as e:  # noqa: BLE001
            log.warning("a2f infer failed (%s); emitting amplitude frame", e)
            return self._amplitude_stub(pcm_window)

    @staticmethod
    def _amplitude_stub(pcm_window: np.ndarray) -> np.ndarray:
        # RMS in 0..1 range, soft-clipped. Drives jaw_open and a couple of
        # supporting mouth weights so a renderer sees something move during
        # speech and rests during silence.
        if pcm_window.size == 0:
            return np.zeros(52, dtype=np.float32)
        x = pcm_window.astype(np.float32) / 32768.0
        rms = float(np.sqrt(np.mean(x * x)))
        # Gentle compressor so consonant transients don't slam the jaw.
        jaw = min(1.0, rms * 6.0)
        weights = np.zeros(52, dtype=np.float32)
        weights[JAW_OPEN_IDX] = jaw
        # Slight lip motion paired with jaw for visual plausibility.
        for name in ("mouthFunnel", "mouthLowerDownLeft", "mouthLowerDownRight"):
            weights[ARKIT_52.index(name)] = jaw * 0.3
        return weights


# ---- App --------------------------------------------------------------------

app = FastAPI()
runner = A2FRunner(A2F_MODE)


@app.get("/health")
def health():
    return {
        "status": "ok",
        "mode": runner.mode,
        "character": A2F_CHARACTER,
        "frame_rate_hz": FRAME_RATE_HZ,
        "sample_rate_hz": PCM_SAMPLE_RATE_HZ,
    }


@app.websocket("/v1/face/stream")
async def face_stream(ws: WebSocket):
    """Receive PCM s16le binary frames, emit JSON blendshape frames.

    Protocol:
      client → server : binary frames of arbitrary-length s16le @ 24 kHz, mono
      server → client : JSON {frame_idx, audio_offset_ms, weights:[52 floats]}
      either side closes when done.
    """
    await ws.accept()
    log.info("ws client connected")
    buf = bytearray()
    frame_idx = 0
    samples_consumed = 0
    bytes_per_frame = SAMPLES_PER_FRAME * PCM_BYTES_PER_SAMPLE
    try:
        while True:
            msg = await ws.receive()
            if msg.get("type") == "websocket.disconnect":
                break
            if "bytes" in msg and msg["bytes"] is not None:
                buf.extend(msg["bytes"])
            elif "text" in msg and msg["text"] is not None:
                # Reserved for future control messages (e.g. {"flush":true}).
                continue
            # Emit one frame per SAMPLES_PER_FRAME of buffered PCM.
            while len(buf) >= bytes_per_frame:
                chunk = bytes(buf[:bytes_per_frame])
                del buf[:bytes_per_frame]
                pcm = np.frombuffer(chunk, dtype=np.int16)
                weights = runner.infer_frame(pcm)
                audio_offset_ms = (samples_consumed / PCM_SAMPLE_RATE_HZ) * 1000.0
                await ws.send_text(json.dumps({
                    "frame_idx": frame_idx,
                    "audio_offset_ms": audio_offset_ms,
                    "weights": weights.tolist(),
                }))
                frame_idx += 1
                samples_consumed += SAMPLES_PER_FRAME
    except WebSocketDisconnect:
        pass
    except Exception as e:  # noqa: BLE001
        log.exception("ws session error: %s", e)
    log.info("ws client disconnected: %d frames over %.2fs of audio",
             frame_idx, samples_consumed / PCM_SAMPLE_RATE_HZ)


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=9000)
