#!/usr/bin/env python3
"""Tests for click-to-aim / click-to-select — the Tracker and the control
endpoints, no camera or MediaPipe required (state is injected directly).

Run: cd vision && uv run test_click_modes.py
"""
import time

import vision


def _det(cx, cy, half=40):
    return {
        "center": (cx, cy),
        "box": [cx - half, cy - half * 2, cx + half, cy + half * 2],
        "targets": {"chest": [cx, cy], "shoulders": [[cx - 30, cy - 40], [cx + 30, cy - 40]], "head": [cx, cy - 60]},
        "eyes": None,
    }


def test_tracker_ids_are_stable_and_expire():
    tr = vision.Tracker(ttl=0.5)
    t0 = 100.0
    a = tr.update([_det(100, 200), _det(400, 200)], t0, 640)
    assert [t["id"] for t in a] == [1, 2]
    # Both drift a little: same ids, matched by proximity.
    b = tr.update([_det(410, 210), _det(95, 205)], t0 + 0.1, 640)
    by_id = {t["id"]: t["center"] for t in b}
    assert by_id[1] == (95, 205) and by_id[2] == (410, 210)
    # One vanishes: survives ttl, then expires; a far new person gets a NEW id.
    c = tr.update([_det(90, 210)], t0 + 0.3, 640)
    assert [t["id"] for t in c] == [1, 2]  # #2 remembered within ttl
    d = tr.update([_det(90, 210)], t0 + 1.0, 640)
    assert [t["id"] for t in d] == [1]
    e = tr.update([_det(90, 210), _det(500, 220)], t0 + 1.1, 640)
    assert [t["id"] for t in e] == [1, 3]
    print("ok: tracker ids stable, expiry + new-id behavior")


def test_tracker_does_not_teleport():
    tr = vision.Tracker(max_jump=0.18)
    tr.update([_det(100, 200)], 0.0, 640)
    # A detection across the frame is a different person, not a 400px jump.
    out = tr.update([_det(600, 200)], 0.1, 640)
    assert sorted(t["id"] for t in out) == [1, 2]
    print("ok: tracker refuses cross-frame teleports")


def _client():
    return vision.app.test_client()


def _inject_state(tracks, w=640, h=480):
    with vision._lock:
        vision._latest_state = {
            "ts": time.time(),
            "frame": {"w": w, "h": h},
            "tracks": tracks,
        }


def _reset_targeting():
    with vision._tlock:
        vision._target_part = "head"
        vision._selected_id = None
        vision._manual_until = 0.0
        vision._aim["pan"] = 0.0
        vision._aim["tilt"] = 0.0
        vision._boresight = None
        vision._tune["gain_pan"] = 0.025
        vision._tune["gain_tilt"] = 0.025


def test_aimpoint_nudges_and_holds():
    _reset_targeting()
    c = _client()
    vision._frame_size[0], vision._frame_size[1] = 640, 480
    # Click 200px right / 100px below the (default center) boresight.
    r = c.post("/aimpoint", json={"x": 320 + 200, "y": 240 + 100})
    assert r.status_code == 200
    aim = r.get_json()["aim"]
    assert abs(aim["pan"] - 200 * 0.025) < 1e-6
    assert abs(aim["tilt"] - 100 * 0.025) < 1e-6
    with vision._tlock:
        assert vision._target_part == "none"      # manual mode stops the servo
        assert vision._manual_until > time.time()  # hold-stream armed
    # Clicks accumulate (iterative refinement) and clamp per-click.
    r = c.post("/aimpoint", json={"x": 320 + 200, "y": 240 + 100})
    assert abs(r.get_json()["aim"]["pan"] - 2 * 200 * 0.025) < 1e-6
    r = c.post("/aimpoint", json={"x": "nope"})
    assert r.status_code == 400
    print("ok: aimpoint one-shot nudge, accumulation, manual hold, validation")


def test_aimpoint_click_clamp():
    _reset_targeting()
    with vision._tlock:
        vision._tune["gain_pan"] = 5.0  # absurd gain: click must not fling the gun
    c = _client()
    r = c.post("/aimpoint", json={"x": 320 + 300, "y": 240})
    assert r.get_json()["aim"]["pan"] == vision._MAX_CLICK_DEG
    print("ok: per-click nudge clamped")


def test_select_hit_miss_clear():
    _reset_targeting()
    c = _client()
    _inject_state([
        {"id": 1, "center": [100, 200], "box": [60, 120, 140, 280]},
        {"id": 2, "center": [400, 200], "box": [360, 120, 440, 280]},
    ])
    # Inside #2's box.
    r = c.post("/select", json={"x": 420, "y": 250})
    assert r.get_json() == {"selected": 2, "hit": True}
    # A far miss keeps the selection.
    r = c.post("/select", json={"x": 630, "y": 470})
    assert r.get_json() == {"selected": 2, "hit": False}
    # Near-miss just outside a box still selects the nearest person.
    r = c.post("/select", json={"x": 150, "y": 205})
    assert r.get_json() == {"selected": 1, "hit": True}
    r = c.post("/select", json={"clear": True})
    assert r.get_json() == {"selected": None, "hit": True}
    r = c.post("/select", json={})
    assert r.status_code == 400
    print("ok: select hit/near-miss/far-miss/clear")


def test_select_activates_tracking_part():
    _reset_targeting()
    with vision._tlock:
        vision._target_part = "none"
    c = _client()
    _inject_state([{"id": 7, "center": [300, 240], "box": [260, 160, 340, 320]}])
    c.post("/select", json={"x": 300, "y": 240})
    with vision._tlock:
        assert vision._target_part == "chest"  # selection implies tracking
        assert vision._selected_id == 7
    print("ok: selecting with part=none turns on chest tracking")


def test_target_and_center_clear_selection():
    _reset_targeting()
    c = _client()
    _inject_state([{"id": 3, "center": [300, 240], "box": [260, 160, 340, 320]}])
    c.post("/select", json={"x": 300, "y": 240})
    # A fresh acquisition (the trial Acquire cue lands here) must not inherit
    # a leftover operator selection.
    c.post("/target", json={"part": "head"})
    with vision._tlock:
        assert vision._selected_id is None
    c.post("/select", json={"x": 300, "y": 240})
    c.post("/center")
    with vision._tlock:
        assert vision._selected_id is None and vision._target_part == "none"
    print("ok: /target and /center clear the selection")


if __name__ == "__main__":
    test_tracker_ids_are_stable_and_expire()
    test_tracker_does_not_teleport()
    test_aimpoint_nudges_and_holds()
    test_aimpoint_click_clamp()
    test_select_hit_miss_clear()
    test_select_activates_tracking_part()
    test_target_and_center_clear_selection()
    print("\nALL CLICK-MODE TESTS PASSED")
