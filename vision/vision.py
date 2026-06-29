#!/usr/bin/env python3
"""Wet Court turret vision — sensing + feed (Phase B, milestone 1).

Captures the turret camera, runs MediaPipe pose detection to locate target body
parts (chest, shoulders, head) and rough eye points, and serves:

  GET /         a tiny HTML page embedding the live feed
  GET /feed     annotated MJPEG stream (multipart/x-mixed-replace)
  GET /state    JSON of the latest detection (target pixels, frame size, …)
  GET /health   "ok"

This is the sensor + feed foundation. Closed-loop targeting, the operator panel,
the eye-exclusion safety zone (precise FaceMesh eyes), and the firing-still /
terminator-cam reuse build on top of it.

Location-flexible (a deployment knob): runs on the booth PC for dev and as a
dgx-ai-stack container on the Spark for prod — the camera follows it and the
HTTP channel is reachable over the LAN. Config via flags or BOOTH_VISION_* env.
"""

import argparse
import os
import threading
import time

import cv2
from flask import Flask, Response, jsonify

# MediaPipe Tasks API (the legacy `solutions` API was removed in 0.10.x). The
# Tasks PoseLandmarker uses a downloadable `.task` model bundle; same 33-landmark
# layout as the old Pose, so the target math is unchanged.
mp = None
PoseLandmarker = PoseLandmarkerOptions = RunningMode = BaseOptions = None
try:
    import mediapipe as mp
    from mediapipe.tasks.python import vision as _mp_vision
    from mediapipe.tasks.python.core.base_options import BaseOptions
    PoseLandmarker = _mp_vision.PoseLandmarker
    PoseLandmarkerOptions = _mp_vision.PoseLandmarkerOptions
    RunningMode = _mp_vision.RunningMode
except Exception as _e:  # pragma: no cover - import guard
    print(f"[vision] mediapipe tasks unavailable ({_e}); streaming raw frames only")
    mp = None

# Pose landmarker model — auto-downloaded once to models/ (gitignored). "lite" is
# the fastest and fine for a single seated subject; swap for _full/_heavy if needed.
_MODEL_URL = (
    "https://storage.googleapis.com/mediapipe-models/pose_landmarker/"
    "pose_landmarker_lite/float16/latest/pose_landmarker_lite.task"
)
_MODEL_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           "models", "pose_landmarker_lite.task")


def ensure_model(path):
    """Return a local model path, downloading the default model if missing."""
    if os.path.exists(path):
        return path
    import urllib.request
    os.makedirs(os.path.dirname(path), exist_ok=True)
    print(f"[vision] downloading pose model -> {path}")
    urllib.request.urlretrieve(_MODEL_URL, path)
    return path

# MediaPipe Pose landmark indices we use.
NOSE, L_EYE, R_EYE = 0, 2, 5
L_SHOULDER, R_SHOULDER = 11, 12
L_HIP, R_HIP = 23, 24

app = Flask(__name__)

# Shared latest frame + detection, written by the capture thread.
_lock = threading.Lock()
_latest_jpeg: bytes | None = None
_latest_state: dict = {"ts": 0.0, "person": False}


def _mid(a, b):
    return ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0)


def _px(lm, w, h):
    return (int(lm.x * w), int(lm.y * h))


def detect_loop(cfg):
    """Capture → detect → annotate → publish, forever."""
    global _latest_jpeg, _latest_state

    cap = cv2.VideoCapture(cfg.camera, cv2.CAP_ANY)
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.width)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.height)

    landmarker = None
    if mp is not None:
        opts = PoseLandmarkerOptions(
            base_options=BaseOptions(model_asset_path=ensure_model(cfg.model)),
            running_mode=RunningMode.VIDEO,
            num_poses=1,
        )
        landmarker = PoseLandmarker.create_from_options(opts)

    t0 = time.perf_counter()
    last_ts = -1
    while True:
        ok, frame = cap.read()
        if not ok or frame is None:
            frame = _test_pattern(cfg.width, cfg.height)
            time.sleep(0.05)
        h, w = frame.shape[:2]
        state = {"ts": time.time(), "frame": {"w": w, "h": h}, "person": False}

        if landmarker is not None:
            rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            mp_img = mp.Image(image_format=mp.ImageFormat.SRGB, data=rgb)
            # VIDEO mode needs a strictly-increasing timestamp (ms).
            ts_ms = int((time.perf_counter() - t0) * 1000)
            if ts_ms <= last_ts:
                ts_ms = last_ts + 1
            last_ts = ts_ms
            result = landmarker.detect_for_video(mp_img, ts_ms)
            if result.pose_landmarks:
                _annotate(frame, result.pose_landmarks[0], w, h, state)

        # Center reticle (the camera's optical axis — boresight is calibrated later).
        cv2.drawMarker(frame, (w // 2, h // 2), (90, 90, 90), cv2.MARKER_CROSS, 18, 1)

        ok, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, cfg.quality])
        if ok:
            with _lock:
                _latest_jpeg = buf.tobytes()
                _latest_state = state


def _annotate(frame, lm, w, h, state):
    """Compute target points from pose landmarks and draw them."""
    def vis(i):
        return lm[i].visibility > 0.5

    if not (vis(L_SHOULDER) and vis(R_SHOULDER)):
        return
    state["person"] = True

    ls, rs = _px(lm[L_SHOULDER], w, h), _px(lm[R_SHOULDER], w, h)
    shoulder_mid = _mid(ls, rs)

    # Chest/sternum: a bit below the shoulder line toward the hips.
    if vis(L_HIP) and vis(R_HIP):
        hip_mid = _mid(_px(lm[L_HIP], w, h), _px(lm[R_HIP], w, h))
        chest = (
            shoulder_mid[0] + 0.30 * (hip_mid[0] - shoulder_mid[0]),
            shoulder_mid[1] + 0.30 * (hip_mid[1] - shoulder_mid[1]),
        )
    else:
        chest = (shoulder_mid[0], shoulder_mid[1] + 0.15 * (rs[0] - ls[0]))

    head = _px(lm[NOSE], w, h) if vis(NOSE) else None
    eyes = []
    if vis(L_EYE):
        eyes.append(_px(lm[L_EYE], w, h))
    if vis(R_EYE):
        eyes.append(_px(lm[R_EYE], w, h))

    state["targets"] = {
        "chest": [round(chest[0]), round(chest[1])],
        "shoulders": [list(ls), list(rs)],
        "head": list(head) if head else None,
    }
    state["eyes"] = [list(e) for e in eyes] if eyes else None

    # --- overlay ---
    ci = (round(chest[0]), round(chest[1]))
    cv2.circle(frame, ci, 10, (0, 200, 0), 2)            # chest target (green)
    cv2.drawMarker(frame, ci, (0, 200, 0), cv2.MARKER_CROSS, 22, 1)
    cv2.line(frame, ls, rs, (0, 160, 0), 1)
    if head:
        cv2.circle(frame, head, 8, (0, 180, 220), 2)     # head (amber) — gated later
    for e in eyes:
        cv2.circle(frame, e, 5, (60, 60, 255), -1)       # eyes (red) — exclusion zone later


def _test_pattern(w, h):
    import numpy as np

    img = np.zeros((h, w, 3), dtype="uint8")
    cv2.putText(img, "no camera", (w // 2 - 90, h // 2), cv2.FONT_HERSHEY_SIMPLEX,
                1.0, (60, 60, 200), 2)
    return img


_INDEX_HTML = """<!doctype html><meta charset=utf-8><title>Wet Court — turret cam</title>
<body style="margin:0;background:#0d1117;color:#c9d1d9;font-family:monospace;text-align:center">
<h3 style="color:#58a6ff">turret vision feed</h3>
<img src="/feed" style="max-width:100%;border:1px solid #30363d">
</body>"""


@app.route("/")
def index():
    return _INDEX_HTML


@app.route("/feed")
def feed():
    def gen():
        boundary = b"--frame\r\nContent-Type: image/jpeg\r\n\r\n"
        while True:
            with _lock:
                jpeg = _latest_jpeg
            if jpeg:
                yield boundary + jpeg + b"\r\n"
            time.sleep(1 / 30)

    return Response(gen(), mimetype="multipart/x-mixed-replace; boundary=frame")


@app.route("/state")
def state():
    with _lock:
        return jsonify(_latest_state)


@app.route("/health")
def health():
    return "ok"


def main():
    p = argparse.ArgumentParser(description="Wet Court turret vision")
    p.add_argument("--camera", type=int, default=int(os.environ.get("BOOTH_VISION_CAMERA", 0)))
    p.add_argument("--host", default=os.environ.get("BOOTH_VISION_HOST", "0.0.0.0"))
    p.add_argument("--port", type=int, default=int(os.environ.get("BOOTH_VISION_PORT", 8091)))
    p.add_argument("--width", type=int, default=int(os.environ.get("BOOTH_VISION_WIDTH", 640)))
    p.add_argument("--height", type=int, default=int(os.environ.get("BOOTH_VISION_HEIGHT", 480)))
    p.add_argument("--quality", type=int, default=int(os.environ.get("BOOTH_VISION_QUALITY", 80)))
    p.add_argument("--model", default=os.environ.get("BOOTH_VISION_MODEL", _MODEL_PATH),
                   help="pose .task model path (auto-downloaded if missing)")
    cfg = p.parse_args()

    t = threading.Thread(target=detect_loop, args=(cfg,), daemon=True)
    t.start()
    print(f"[vision] serving on http://{cfg.host}:{cfg.port}  (camera {cfg.camera})")
    app.run(host=cfg.host, port=cfg.port, threaded=True)


if __name__ == "__main__":
    main()
