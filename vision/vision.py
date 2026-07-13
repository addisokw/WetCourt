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
_MODEL_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "models", "pose_landmarker_lite.task"
)


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
_latest_clean_jpeg: bytes | None = None  # raw frame, no overlays (for captures)
_latest_state: dict = {"ts": 0.0, "person": False}

# Targeting state, set by the operator via the control endpoints and read by the
# servo loop. `_boresight` is the camera pixel the gun points at (fixed, because
# the camera moves with the gun); `_aim` is the commanded turret aim (degrees)
# the loop integrates toward putting the target on the boresight.
_tlock = threading.Lock()
_target_part = "head"  # "none" | "chest" | "head" — head is the default target
_boresight: list | None = None  # [x, y]; defaults to frame center
_aim = {"pan": 0.0, "tilt": 0.0}
# Live-tunable servo gains (deg of aim per pixel of error; sign matters) and the
# lock tolerance (px). Seeded from the CLI flags in main(), then editable from
# the operator console via POST /gains.
_tune = {"gain_pan": 0.01, "gain_tilt": 0.01, "tolerance": 12}
# Selected person-track id (click-to-track), or None. When a selection is set
# but its track is lost, the servo HOLDS rather than falling back to another
# person — the gun must never migrate to a bystander on its own.
_selected_id: int | None = None
# One-shot click-to-aim: stream the held aim until this wall-clock time so the
# orchestrator relays the nudge (it only trusts fresh values). fire_ok is never
# sent true on a manual hold.
_manual_until = 0.0
# Aim limits, seeded from CLI in main() (the servo clamps with cfg; the
# /aimpoint endpoint needs them outside the loop).
_limits = {"pan": 90.0, "tilt": 45.0}
# Last frame size, for endpoints that need the boresight default (frame center).
_frame_size = [640, 480]
# A single click may nudge at most this many degrees per axis — a deliberate
# act, but a wild gain must not fling the gun across the room.
_MAX_CLICK_DEG = 20.0

# Cap the per-frame aim change so a too-high gain can't fling the gun across its
# whole range in one step — softens overshoot while tuning.
_MAX_STEP_DEG = 8.0


def _clamp(v, lo, hi):
    return lo if v < lo else hi if v > hi else v


def _mid(a, b):
    return ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0)


def _px(lm, w, h):
    return (int(lm.x * w), int(lm.y * h))


class Tracker:
    """Greedy nearest-centroid tracker: turns per-frame pose detections into
    persistent tracks with stable integer ids, so the operator can click a
    person and the servo can follow *that* person across frames.

    Deliberately simple (≤ a handful of seated/queueing people, high frame
    rate): match each detection to the nearest live track within `max_jump`
    of frame width, closest pairs first; leftovers become new tracks; tracks
    unseen for `ttl` seconds expire. Identity is positional — someone who
    leaves and returns gets a new id (the operator re-clicks).
    """

    def __init__(self, max_jump=0.18, ttl=1.2):
        self._next_id = 1
        self._tracks = {}  # id -> {"center": (x,y), "last_seen": t, **points}
        self.max_jump = max_jump
        self.ttl = ttl

    def update(self, detections, now, frame_w):
        """detections: list of point-dicts from _pose_points. Returns the live
        track list [{"id", "center", "box", "targets", "eyes"}, ...]."""
        max_d = self.max_jump * frame_w
        pairs = []  # (dist, track_id, det_idx)
        for tid, tr in self._tracks.items():
            for di, det in enumerate(detections):
                dx = tr["center"][0] - det["center"][0]
                dy = tr["center"][1] - det["center"][1]
                d = (dx * dx + dy * dy) ** 0.5
                if d <= max_d:
                    pairs.append((d, tid, di))
        pairs.sort(key=lambda p: p[0])
        used_t, used_d = set(), set()
        for d, tid, di in pairs:
            if tid in used_t or di in used_d:
                continue
            used_t.add(tid)
            used_d.add(di)
            self._tracks[tid] = {**detections[di], "last_seen": now}
        for di, det in enumerate(detections):
            if di not in used_d:
                self._tracks[self._next_id] = {**det, "last_seen": now}
                self._next_id += 1
        for tid in [t for t, tr in self._tracks.items() if now - tr["last_seen"] > self.ttl]:
            del self._tracks[tid]
        return [
            {"id": tid, **{k: v for k, v in tr.items() if k != "last_seen"}}
            for tid, tr in sorted(self._tracks.items())
        ]


def _open_capture(cfg):
    """The camera, or a looping video file when `--video` is set.

    Video mode exercises the FULL pipeline — detection, tracking, selection,
    the aim servo — against recorded footage, so multi-person event scenarios
    (people milling behind the defendant) are testable at a desk with no
    camera, no booth, and no crowd. Returns (capture, frame_interval_secs):
    a file decodes as fast as read() is called, so the loop paces itself to
    the file's native FPS to mimic a live camera (the tracker's TTL and the
    aim stream cadence are wall-clock-based).
    """
    if cfg.video:
        cap = cv2.VideoCapture(cfg.video)
        if not cap.isOpened():
            # flush: these print from the capture thread, where block-buffered
            # stdout (piped logs) would otherwise swallow them for minutes.
            print(f"[vision] cannot open video {cfg.video!r}; falling back to test pattern", flush=True)
            return cap, 1 / 30.0
        fps = cap.get(cv2.CAP_PROP_FPS) or 0
        interval = 1.0 / fps if fps > 0 else 1 / 30.0
        print(f"[vision] video mode: {cfg.video} ({fps:.1f} fps, looping)", flush=True)
        return cap, interval
    cap = cv2.VideoCapture(cfg.camera, cv2.CAP_ANY)
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.width)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.height)
    return cap, None  # a live camera blocks at its own frame rate


def detect_loop(cfg):
    """Capture → detect → annotate → publish, forever."""
    global _latest_jpeg, _latest_clean_jpeg, _latest_state

    cap, frame_interval = _open_capture(cfg)

    landmarker = None
    if mp is not None:
        opts = PoseLandmarkerOptions(
            base_options=BaseOptions(model_asset_path=ensure_model(cfg.model)),
            running_mode=RunningMode.VIDEO,
            num_poses=cfg.max_poses,
        )
        landmarker = PoseLandmarker.create_from_options(opts)
    tracker = Tracker()

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
    next_frame_at = time.perf_counter()
    while True:
        ok, frame = cap.read()
        if (not ok or frame is None) and cfg.video:
            # End of file: loop back to the start (endless replay).
            cap.set(cv2.CAP_PROP_POS_FRAMES, 0)
            ok, frame = cap.read()
        if not ok or frame is None:
            frame = _test_pattern(cfg.width, cfg.height)
            time.sleep(0.05)
        if frame_interval is not None:
            # Pace file playback to its native FPS (decode is far faster than
            # realtime; the servo/tracker assume wall-clock frames).
            now = time.perf_counter()
            if now < next_frame_at:
                time.sleep(next_frame_at - now)
            next_frame_at = max(next_frame_at + frame_interval, now - frame_interval)
        h, w = frame.shape[:2]
        state = {"ts": time.time(), "frame": {"w": w, "h": h}, "person": False}

        # Encode a clean copy of the raw frame BEFORE any overlays are drawn, so
        # /clean can serve an un-annotated still (for the keepsake blast photo and
        # shareable content). Cheap: one extra JPEG encode per frame.
        ok_clean, clean_buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, cfg.quality])
        if ok_clean:
            with _lock:
                _latest_clean_jpeg = clean_buf.tobytes()

        _frame_size[0], _frame_size[1] = w, h

        tracks = []
        if landmarker is not None:
            rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            mp_img = mp.Image(image_format=mp.ImageFormat.SRGB, data=rgb)
            # VIDEO mode needs a strictly-increasing timestamp (ms).
            ts_ms = int((time.perf_counter() - t0) * 1000)
            if ts_ms <= last_ts:
                ts_ms = last_ts + 1
            last_ts = ts_ms
            result = landmarker.detect_for_video(mp_img, ts_ms)
            detections = []
            for lm in result.pose_landmarks or []:
                pts = _pose_points(lm, w, h)
                if pts:
                    detections.append(pts)
            tracks = tracker.update(detections, time.time(), w)

        # --- Targeting servo (vision-owned). Nudge the commanded aim so the
        # target body part moves onto the boresight pixel; the orchestrator
        # relays it to the turret only when armed. Gains/sign are hardware-tuned. ---
        with _tlock:
            part = _target_part
            bs = list(_boresight) if _boresight else [w // 2, h // 2]
            aim_pan, aim_tilt = _aim["pan"], _aim["tilt"]
            gp, gt, tol = _tune["gain_pan"], _tune["gain_tilt"], _tune["tolerance"]
            selected = _selected_id
            manual_until = _manual_until

        # The servo's subject: the selected track when visible; with no
        # selection, the track nearest the boresight (the "defendant" seat).
        # A selected-but-lost track means NO subject — the gun holds rather
        # than migrating to whoever else is in frame.
        sel_track = next((t for t in tracks if t["id"] == selected), None)
        if selected is not None:
            subject = sel_track
        else:
            subject = min(
                tracks,
                key=lambda t: (t["center"][0] - bs[0]) ** 2 + (t["center"][1] - bs[1]) ** 2,
            ) if tracks else None

        if subject:
            state["person"] = True
            state["targets"] = subject["targets"]
            state["eyes"] = subject["eyes"]
        state["tracks"] = [
            {"id": t["id"], "center": list(t["center"]), "box": list(t["box"])} for t in tracks
        ]
        state["selected"] = selected
        state["selected_visible"] = sel_track is not None
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

        # --- fire_ok: any acquired target fires once the aim is locked on it.
        # The eye-exclusion safety zone was retired when the softer nozzle made
        # the stream safe; firing is still gated on a lock here and on the
        # operator ARM at the orchestrator. ---
        fire_ok = locked if part in ("chest", "head") else False
        state["fire_ok"] = fire_ok

        # Stream aim + the safety verdict to the orchestrator (relayed to the
        # turret only while armed; fire_ok gates the trial FIRE). Sent only while
        # actively tracking, so a stale value at the orchestrator ⇒ no fire.
        # A click-to-aim nudge briefly streams the held aim the same way
        # (fire_ok always false — a manual point is not a verified person lock).
        if tracking:
            send_aim(aim_pan, aim_tilt, fire_ok, locked)
        elif time.time() < manual_until:
            send_aim(aim_pan, aim_tilt, False, False)

        _draw_tracks(frame, tracks, subject, selected, sel_track is not None)

        # Boresight marker (where the gun points) + the aim vector to the target.
        cv2.drawMarker(
            frame,
            (int(bs[0]), int(bs[1])),
            (0, 255, 255),
            cv2.MARKER_TILTED_CROSS,
            20,
            2,
        )
        if tp:
            col = (0, 255, 0) if locked else (0, 165, 255)
            cv2.arrowedLine(
                frame,
                (int(bs[0]), int(bs[1])),
                (int(tp[0]), int(tp[1])),
                col,
                2,
                tipLength=0.2,
            )
            if locked:
                cv2.putText(
                    frame,
                    "LOCKED",
                    (int(bs[0]) - 42, int(bs[1]) - 24),
                    cv2.FONT_HERSHEY_SIMPLEX,
                    0.7,
                    (0, 255, 0),
                    2,
                )

        # Fire status readout at the boresight.
        if part != "none":
            txt = "FIRE OK" if fire_ok else "NO FIRE"
            fc = (0, 255, 0) if fire_ok else (40, 40, 255)
            cv2.putText(
                frame,
                txt,
                (int(bs[0]) - 40, int(bs[1]) + 32),
                cv2.FONT_HERSHEY_SIMPLEX,
                0.6,
                fc,
                2,
            )

        ok, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, cfg.quality])
        if ok:
            with _lock:
                _latest_jpeg = buf.tobytes()
                _latest_state = state


def _pose_points(lm, w, h):
    """Target points + a rough body box for one pose. None if too little of a
    person is visible (matching the old shoulders-visible gate)."""

    def vis(i):
        return lm[i].visibility > 0.5

    if not (vis(L_SHOULDER) and vis(R_SHOULDER)):
        return None

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

    # Rough body box from every confidently-visible landmark, lightly padded —
    # the click-to-select hit target and the feed's per-person outline.
    xs = [int(p.x * w) for i, p in enumerate(lm) if vis(i)]
    ys = [int(p.y * h) for i, p in enumerate(lm) if vis(i)]
    pad = max(6, int(0.06 * w))
    box = [
        _clamp(min(xs) - pad, 0, w - 1),
        _clamp(min(ys) - pad, 0, h - 1),
        _clamp(max(xs) + pad, 0, w - 1),
        _clamp(max(ys) + pad, 0, h - 1),
    ]

    return {
        "center": (round(chest[0]), round(chest[1])),
        "box": box,
        "targets": {
            "chest": [round(chest[0]), round(chest[1])],
            "shoulders": [list(ls), list(rs)],
            "head": list(head) if head else None,
        },
        "eyes": [list(e) for e in eyes] if eyes else None,
    }


def _draw_tracks(frame, tracks, subject, selected, sel_visible):
    """Per-person outlines + ids; detailed target points on the subject only."""
    for t in tracks:
        x0, y0, x1, y1 = (int(v) for v in t["box"])
        is_sel = t["id"] == selected
        col = (0, 220, 120) if is_sel else (140, 140, 140)
        cv2.rectangle(frame, (x0, y0), (x1, y1), col, 2 if is_sel else 1)
        cv2.putText(
            frame,
            f"#{t['id']}" + (" SEL" if is_sel else ""),
            (x0 + 3, max(14, y0 - 6)),
            cv2.FONT_HERSHEY_SIMPLEX,
            0.5,
            col,
            1 if not is_sel else 2,
        )
    if selected is not None and not sel_visible:
        cv2.putText(
            frame, f"SELECTED #{selected} LOST", (10, 24),
            cv2.FONT_HERSHEY_SIMPLEX, 0.7, (40, 40, 255), 2,
        )
    if subject:
        tg = subject["targets"]
        ci = tuple(tg["chest"])
        cv2.circle(frame, ci, 10, (0, 200, 0), 2)  # chest target (green)
        cv2.drawMarker(frame, ci, (0, 200, 0), cv2.MARKER_CROSS, 22, 1)
        ls, rs = tg["shoulders"]
        cv2.line(frame, tuple(ls), tuple(rs), (0, 160, 0), 1)
        if tg["head"]:
            cv2.circle(frame, tuple(tg["head"]), 8, (0, 180, 220), 2)  # head (amber)
        for e in subject["eyes"] or []:
            cv2.circle(frame, tuple(e), 5, (60, 60, 255), -1)  # eyes (red)


def _test_pattern(w, h):
    import numpy as np

    img = np.zeros((h, w, 3), dtype="uint8")
    cv2.putText(
        img,
        "no camera",
        (w // 2 - 90, h // 2),
        cv2.FONT_HERSHEY_SIMPLEX,
        1.0,
        (60, 60, 200),
        2,
    )
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


@app.route("/snapshot")
def snapshot():
    """The latest single frame as a plain JPEG. Polled by clients that can't
    consume the endless `multipart/x-mixed-replace` /feed stream — notably an
    <img> proxied through the orchestrator (Safari errors on the keep-alive
    proxied stream). Cheap: hands back the frame the detect loop already encoded."""
    with _lock:
        jpeg = _latest_jpeg
    if not jpeg:
        return ("no frame yet", 503)
    return Response(jpeg, mimetype="image/jpeg",
                    headers={"Cache-Control": "no-store"})


@app.route("/clean")
def clean():
    """The latest frame WITHOUT the targeting overlay — the un-annotated blast
    photo the orchestrator captures for the keepsake receipt and stored content."""
    with _lock:
        jpeg = _latest_clean_jpeg
    if not jpeg:
        return ("no frame yet", 503)
    return Response(jpeg, mimetype="image/jpeg",
                    headers={"Cache-Control": "no-store"})


@app.route("/state")
def state():
    with _lock:
        return jsonify(_latest_state)


@app.route("/target", methods=["POST"])
def set_target():
    """Choose the body part to track ("none" | "chest" | "head")."""
    global _target_part, _selected_id
    data = request.get_json(force=True, silent=True) or {}
    part = data.get("part", "none")
    if part not in ("none", "chest", "head"):
        return ("bad part", 400)
    with _tlock:
        _target_part = part
        if part != "none":
            # Acquire from center each time targeting (re)starts, so the
            # commanded aim and the gun's actual position stay in sync. A
            # fresh acquisition also means a fresh subject: any operator
            # person-selection is stale by definition (the trial Acquire cue
            # lands here — it must never inherit a leftover selection).
            _aim["pan"] = 0.0
            _aim["tilt"] = 0.0
            _selected_id = None
    return ("", 204)


@app.route("/center", methods=["POST"])
def center():
    """Stop tracking and reset the aim integrator to center. Paired with the
    orchestrator commanding the turret back to 0,0 — a one-click recovery from a
    bad overshoot without entering maintenance."""
    global _target_part, _selected_id, _manual_until
    with _tlock:
        _target_part = "none"
        _aim["pan"] = 0.0
        _aim["tilt"] = 0.0
        _selected_id = None
        _manual_until = 0.0
    return ("", 204)


@app.route("/aimpoint", methods=["POST"])
def aimpoint():
    """Click-to-aim: nudge the commanded aim so the clicked pixel lands on the
    boresight. One-shot and open-loop — a static pixel can't feed the servo
    (the camera rides the gun, so a fixed frame coordinate never converges);
    instead the deg-per-pixel gains convert the click's boresight error into a
    single aim step, streamed briefly so the orchestrator relays it (armed
    only, fire_ok never true). Iterate by clicking again. Stops any body-part
    servo: a manual point is manual mode."""
    global _target_part, _selected_id, _manual_until
    data = request.get_json(force=True, silent=True) or {}
    try:
        x, y = float(data["x"]), float(data["y"])
    except Exception:
        return ("need numeric x,y", 400)
    with _tlock:
        bs = _boresight or [_frame_size[0] // 2, _frame_size[1] // 2]
        nudge_pan = _clamp(_tune["gain_pan"] * (x - bs[0]), -_MAX_CLICK_DEG, _MAX_CLICK_DEG)
        nudge_tilt = _clamp(_tune["gain_tilt"] * (y - bs[1]), -_MAX_CLICK_DEG, _MAX_CLICK_DEG)
        _aim["pan"] = _clamp(_aim["pan"] + nudge_pan, -_limits["pan"], _limits["pan"])
        _aim["tilt"] = _clamp(_aim["tilt"] + nudge_tilt, -_limits["tilt"], _limits["tilt"])
        _target_part = "none"
        _selected_id = None
        _manual_until = time.time() + 0.8
        aim = dict(_aim)
    return jsonify({"aim": aim})


@app.route("/select", methods=["POST"])
def select():
    """Click-to-track: pick the person whose track box contains (or whose
    center is nearest) the clicked pixel, then servo onto *them* until
    deselected — never falling back to someone else if the track is lost.
    `{"clear": true}` deselects. A miss keeps the current selection (an
    errant click must not silently retarget)."""
    global _selected_id, _target_part
    data = request.get_json(force=True, silent=True) or {}
    if data.get("clear"):
        with _tlock:
            _selected_id = None
        return jsonify({"selected": None, "hit": True})
    try:
        x, y = float(data["x"]), float(data["y"])
    except Exception:
        return ("need numeric x,y or clear", 400)
    with _lock:
        tracks = list(_latest_state.get("tracks") or [])
        frame_w = (_latest_state.get("frame") or {}).get("w", _frame_size[0])
    best = None
    best_d = 0.25 * frame_w  # generous: near-misses on a moving person still count
    for t in tracks:
        x0, y0, x1, y1 = t["box"]
        cx, cy = t["center"]
        d = ((cx - x) ** 2 + (cy - y) ** 2) ** 0.5
        if x0 <= x <= x1 and y0 <= y <= y1:
            d *= 0.25  # inside the box beats a nearer neighbouring center
        if d < best_d:
            best, best_d = t, d
    if best is None:
        with _tlock:
            current = _selected_id
        return jsonify({"selected": current, "hit": False})
    with _tlock:
        _selected_id = int(best["id"])
        # Selection means "track them": make sure a body-part servo is live
        # (without resetting the aim integrator — the loop is delta-driven
        # and converges from wherever the gun currently points).
        if _target_part == "none":
            _target_part = "chest"
    return jsonify({"selected": int(best["id"]), "hit": True})


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
    p.add_argument(
        "--camera", type=int, default=int(os.environ.get("BOOTH_VISION_CAMERA", 0))
    )
    p.add_argument(
        "--video",
        default=os.environ.get("BOOTH_VISION_VIDEO"),
        help="loop a video file through the full pipeline instead of the camera "
        "(test tracking/selection against recorded event footage)",
    )
    p.add_argument("--host", default=os.environ.get("BOOTH_VISION_HOST", "0.0.0.0"))
    p.add_argument(
        "--port", type=int, default=int(os.environ.get("BOOTH_VISION_PORT", 8091))
    )
    p.add_argument(
        "--width", type=int, default=int(os.environ.get("BOOTH_VISION_WIDTH", 640))
    )
    p.add_argument(
        "--height", type=int, default=int(os.environ.get("BOOTH_VISION_HEIGHT", 480))
    )
    p.add_argument(
        "--quality", type=int, default=int(os.environ.get("BOOTH_VISION_QUALITY", 80))
    )
    p.add_argument(
        "--model",
        default=os.environ.get("BOOTH_VISION_MODEL", _MODEL_PATH),
        help="pose .task model path (auto-downloaded if missing)",
    )
    p.add_argument(
        "--max-poses",
        type=int,
        default=int(os.environ.get("BOOTH_VISION_MAX_POSES", 4)),
        help="people detected per frame (tracker gives them stable ids)",
    )
    # --- Targeting servo ---
    p.add_argument(
        "--orchestrator",
        default=os.environ.get("BOOTH_VISION_ORCH", "http://localhost:8080"),
        help="orchestrator base URL to stream aim to (/vision/aim)",
    )
    p.add_argument(
        "--gain-pan",
        type=float,
        default=float(os.environ.get("BOOTH_VISION_GAIN_PAN", 0.025)),
        help="deg of pan per pixel of error (sign is hardware-dependent; tune live)",
    )
    p.add_argument(
        "--gain-tilt",
        type=float,
        default=float(os.environ.get("BOOTH_VISION_GAIN_TILT", 0.025)),
    )
    p.add_argument(
        "--tolerance",
        type=int,
        default=int(os.environ.get("BOOTH_VISION_TOL", 12)),
        help="pixel error within which the target counts as LOCKED",
    )
    p.add_argument("--pan-limit", type=float, default=90.0)
    p.add_argument("--tilt-limit", type=float, default=45.0)
    p.add_argument(
        "--send-interval",
        type=float,
        default=0.066,
        help="min seconds between aim posts (~15 Hz)",
    )
    cfg = p.parse_args()

    # Seed the live-tunable gains + aim limits from the CLI flags.
    with _tlock:
        _tune["gain_pan"] = cfg.gain_pan
        _tune["gain_tilt"] = cfg.gain_tilt
        _tune["tolerance"] = cfg.tolerance
        _limits["pan"] = cfg.pan_limit
        _limits["tilt"] = cfg.tilt_limit

    t = threading.Thread(target=detect_loop, args=(cfg,), daemon=True)
    t.start()
    print(f"[vision] serving on http://{cfg.host}:{cfg.port}  (camera {cfg.camera})")
    app.run(host=cfg.host, port=cfg.port, threaded=True)


if __name__ == "__main__":
    main()
