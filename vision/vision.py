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
import requests
from flask import Flask, Response, jsonify, request

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

# Targeting state, set by the operator via the control endpoints and read by the
# servo loop. `_boresight` is the camera pixel the gun points at (fixed, because
# the camera moves with the gun); `_aim` is the commanded turret aim (degrees)
# the loop integrates toward putting the target on the boresight.
_tlock = threading.Lock()
_target_part = "none"          # "none" | "chest" | "head"
_boresight: list | None = None  # [x, y]; defaults to frame center
_aim = {"pan": 0.0, "tilt": 0.0}
# Operator must explicitly confirm head shots; chest never needs it.
_head_confirm = False
# Live-tunable servo gains (deg of aim per pixel of error; sign matters) and the
# lock tolerance (px). Seeded from the CLI flags in main(), then editable from
# the operator console via POST /gains.
_tune = {"gain_pan": 0.025, "gain_tilt": 0.025, "tolerance": 12}

# Cap the per-frame aim change so a too-high gain can't fling the gun across its
# whole range in one step — softens overshoot while tuning.
_MAX_STEP_DEG = 8.0


def _clamp(v, lo, hi):
    return lo if v < lo else hi if v > hi else v


def _eye_zone(eyes, pad_frac):
    """Conservative eye-exclusion box from the two eye points: their bounding box
    padded by a fraction of the inter-eye distance, more upward (toward the brow /
    forehead, the danger above) and less downward so a nose/chin shot can clear."""
    if not eyes or len(eyes) < 2:
        return None
    (lx, ly), (rx, ry) = eyes[0], eyes[1]
    sep = max(20.0, ((rx - lx) ** 2 + (ry - ly) ** 2) ** 0.5)
    pad = pad_frac * sep
    xs, ys = (lx, rx), (ly, ry)
    return [min(xs) - pad, min(ys) - pad * 1.3, max(xs) + pad, max(ys) + pad * 0.6]


def _circle_hits_box(cx, cy, r, box):
    """True if a circle (impact point + uncertainty radius) overlaps the box."""
    nx = _clamp(cx, box[0], box[2])
    ny = _clamp(cy, box[1], box[3])
    return (cx - nx) ** 2 + (cy - ny) ** 2 <= r * r


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

    # Throttled aim sender to the orchestrator (which relays to the turret only
    # when armed). Short timeout + swallow errors so the loop never stalls.
    session = requests.Session()
    last_send = [0.0]

    def send_aim(pan, tilt, fire_ok, locked):
        now = time.time()
        if now - last_send[0] < cfg.send_interval:
            return
        last_send[0] = now
        try:
            # fire_ok rides the aim stream so the orchestrator can gate the trial
            # FIRE on a *fresh* safety verdict (m4b). Because we only post while
            # actively tracking, a stale value at the orchestrator means no fire.
            session.post(
                f"{cfg.orchestrator.rstrip('/')}/vision/aim",
                json={"pan": pan, "tilt": tilt, "fire_ok": fire_ok, "locked": locked},
                timeout=0.3,
            )
        except Exception:
            pass

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

        # --- Targeting servo (vision-owned). Nudge the commanded aim so the
        # target body part moves onto the boresight pixel; the orchestrator
        # relays it to the turret only when armed. Gains/sign are hardware-tuned. ---
        with _tlock:
            part = _target_part
            bs = list(_boresight) if _boresight else [w // 2, h // 2]
            aim_pan, aim_tilt = _aim["pan"], _aim["tilt"]
            gp, gt, tol = _tune["gain_pan"], _tune["gain_tilt"], _tune["tolerance"]
            head_confirm = _head_confirm
        state["target_part"] = part
        state["boresight"] = bs
        state["gains"] = {"pan": gp, "tilt": gt, "tolerance": tol}
        locked = False
        tp = (state.get("targets") or {}).get(part) if part != "none" else None
        tracking = bool(tp and state.get("person"))
        if tracking:
            ex, ey = tp[0] - bs[0], tp[1] - bs[1]
            step_pan = _clamp(gp * ex, -_MAX_STEP_DEG, _MAX_STEP_DEG)
            step_tilt = _clamp(gt * ey, -_MAX_STEP_DEG, _MAX_STEP_DEG)
            aim_pan = _clamp(aim_pan + step_pan, -cfg.pan_limit, cfg.pan_limit)
            aim_tilt = _clamp(aim_tilt + step_tilt, -cfg.tilt_limit, cfg.tilt_limit)
            locked = abs(ex) <= tol and abs(ey) <= tol
            with _tlock:
                _aim["pan"], _aim["tilt"] = aim_pan, aim_tilt
        state["aim"] = {"pan": round(aim_pan, 1), "tilt": round(aim_tilt, 1)}
        state["locked"] = locked

        # --- Safety: eye-exclusion zone + fire_ok (this milestone computes and
        # shows it; it does not fire anything yet). The impact point is the
        # boresight; it must clear the eye zone (+ uncertainty) to be safe. ---
        zone = _eye_zone(state.get("eyes"), cfg.eye_pad)
        eye_clear = zone is None or not _circle_hits_box(bs[0], bs[1], cfg.impact_radius, zone)
        if part == "chest":
            fire_ok = locked  # torso is inherently clear of the eyes
        elif part == "head":
            # Head shots require a lock, operator confirmation, detected eyes,
            # and the impact clear of the eye zone.
            fire_ok = locked and head_confirm and zone is not None and eye_clear
        else:
            fire_ok = False
        state["eye_zone"] = zone
        state["eye_clear"] = eye_clear
        state["head_confirm"] = head_confirm
        state["fire_ok"] = fire_ok

        # Stream aim + the safety verdict to the orchestrator (relayed to the
        # turret only while armed; fire_ok gates the trial FIRE). Sent only while
        # actively tracking, so a stale value at the orchestrator ⇒ no fire.
        if tracking:
            send_aim(aim_pan, aim_tilt, fire_ok, locked)

        # Boresight marker (where the gun points) + the aim vector to the target.
        cv2.drawMarker(frame, (int(bs[0]), int(bs[1])), (0, 255, 255),
                       cv2.MARKER_TILTED_CROSS, 20, 2)
        if tp:
            col = (0, 255, 0) if locked else (0, 165, 255)
            cv2.arrowedLine(frame, (int(bs[0]), int(bs[1])),
                            (int(tp[0]), int(tp[1])), col, 2, tipLength=0.2)
            if locked:
                cv2.putText(frame, "LOCKED", (int(bs[0]) - 42, int(bs[1]) - 24),
                            cv2.FONT_HERSHEY_SIMPLEX, 0.7, (0, 255, 0), 2)

        # Eye-exclusion zone (red when the impact would hit it) + fire status.
        if zone is not None:
            zc = (40, 40, 255) if not eye_clear else (60, 90, 160)
            cv2.rectangle(frame, (int(zone[0]), int(zone[1])),
                          (int(zone[2]), int(zone[3])), zc, 2)
            cv2.putText(frame, "eyes", (int(zone[0]), int(zone[1]) - 4),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.4, zc, 1)
        if part != "none":
            txt = "FIRE OK" if fire_ok else "NO FIRE"
            fc = (0, 255, 0) if fire_ok else (40, 40, 255)
            cv2.putText(frame, txt, (int(bs[0]) - 40, int(bs[1]) + 32),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.6, fc, 2)

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


@app.route("/target", methods=["POST"])
def set_target():
    """Choose the body part to track ("none" | "chest" | "head")."""
    global _target_part
    data = request.get_json(force=True, silent=True) or {}
    part = data.get("part", "none")
    if part not in ("none", "chest", "head"):
        return ("bad part", 400)
    with _tlock:
        _target_part = part
        if part != "none":
            # Acquire from center each time targeting (re)starts, so the
            # commanded aim and the gun's actual position stay in sync.
            _aim["pan"] = 0.0
            _aim["tilt"] = 0.0
    return ("", 204)


@app.route("/confirm_head", methods=["POST"])
def confirm_head():
    """Operator gate for head shots: head never fires unless this is enabled."""
    global _head_confirm
    data = request.get_json(force=True, silent=True) or {}
    with _tlock:
        _head_confirm = bool(data.get("enabled", False))
    return ("", 204)


@app.route("/center", methods=["POST"])
def center():
    """Stop tracking and reset the aim integrator to center. Paired with the
    orchestrator commanding the turret back to 0,0 — a one-click recovery from a
    bad overshoot without entering maintenance."""
    global _target_part
    with _tlock:
        _target_part = "none"
        _aim["pan"] = 0.0
        _aim["tilt"] = 0.0
    return ("", 204)


@app.route("/gains", methods=["POST"])
def set_gains():
    """Live-tune the servo gains / lock tolerance. Any subset of
    {gain_pan, gain_tilt, tolerance}; gains may be negative to flip the sign."""
    data = request.get_json(force=True, silent=True) or {}
    with _tlock:
        try:
            if "gain_pan" in data:
                _tune["gain_pan"] = float(data["gain_pan"])
            if "gain_tilt" in data:
                _tune["gain_tilt"] = float(data["gain_tilt"])
            if "tolerance" in data:
                _tune["tolerance"] = max(1, int(data["tolerance"]))
        except (TypeError, ValueError):
            return ("bad number", 400)
    return ("", 204)


@app.route("/boresight", methods=["POST"])
def set_boresight():
    """Set the boresight pixel (where the gun points in the camera image)."""
    global _boresight
    data = request.get_json(force=True, silent=True) or {}
    try:
        x, y = int(data["x"]), int(data["y"])
    except Exception:
        return ("need integer x,y", 400)
    with _tlock:
        _boresight = [x, y]
    return ("", 204)


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
    # --- Targeting servo ---
    p.add_argument("--orchestrator", default=os.environ.get("BOOTH_VISION_ORCH", "http://localhost:8080"),
                   help="orchestrator base URL to stream aim to (/vision/aim)")
    p.add_argument("--gain-pan", type=float, default=float(os.environ.get("BOOTH_VISION_GAIN_PAN", 0.025)),
                   help="deg of pan per pixel of error (sign is hardware-dependent; tune live)")
    p.add_argument("--gain-tilt", type=float, default=float(os.environ.get("BOOTH_VISION_GAIN_TILT", 0.025)))
    p.add_argument("--tolerance", type=int, default=int(os.environ.get("BOOTH_VISION_TOL", 12)),
                   help="pixel error within which the target counts as LOCKED")
    p.add_argument("--pan-limit", type=float, default=90.0)
    p.add_argument("--tilt-limit", type=float, default=45.0)
    p.add_argument("--send-interval", type=float, default=0.066, help="min seconds between aim posts (~15 Hz)")
    # --- Eye-safety ---
    p.add_argument("--eye-pad", type=float, default=float(os.environ.get("BOOTH_VISION_EYE_PAD", 0.8)),
                   help="eye-exclusion padding as a fraction of inter-eye distance (bigger = safer)")
    p.add_argument("--impact-radius", type=int, default=int(os.environ.get("BOOTH_VISION_IMPACT_R", 25)),
                   help="impact uncertainty radius (px) added around the boresight for the no-fire test")
    cfg = p.parse_args()

    # Seed the live-tunable gains from the CLI flags.
    with _tlock:
        _tune["gain_pan"] = cfg.gain_pan
        _tune["gain_tilt"] = cfg.gain_tilt
        _tune["tolerance"] = cfg.tolerance

    t = threading.Thread(target=detect_loop, args=(cfg,), daemon=True)
    t.start()
    print(f"[vision] serving on http://{cfg.host}:{cfg.port}  (camera {cfg.camera})")
    app.run(host=cfg.host, port=cfg.port, threaded=True)


if __name__ == "__main__":
    main()
