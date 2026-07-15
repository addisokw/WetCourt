# Wet Court — judge-neck firmware (pan/tilt gaze), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Accessory: M5Stack 8-Servos board (I2C 0x25) — ch0 pan, ch1 tilt
#
# MicroPython reimplementation of the retired Arduino sketch (git history has
# it): same hardware, same AIM contract, same role — only the runtime changed.
# The role-agnostic protocol client lives in wetline.py; this file is the
# servo driver + the AIM handler.
#
# AIM values are servo pulse-width MICROSECONDS (the host applies calibration;
# judge_neck.toml holds the working ranges). The firmware stays "dumb" about
# calibration but NOT about motion:
#
# MOTION SAFETY. These servos have no position feedback, and a hobby servo
# slews to its commanded pulse at full speed. The head is heavy enough that a
# commanded jump snapped the tilt mount once (cold boot commanding center
# while the head sat at gravity droop). So the commanded pulse is never
# allowed to jump: handlers set *targets*, and a tick walks the output toward
# them at a bounded rate. The only estimate needed is where the head is when
# the servos first get a pulse:
#   - power-on reset: the servos were dark. Gravity has tilt at full droop —
#     on this rig's counter-spring geometry that is ~2167 µs, ABOVE the
#     working range (1500..1967; below 1500 the horn crashes into the
#     spring). Pan is wherever it was left; center is the best guess, and pan
#     bears no gravity load. A wrong droop guess fails DOWNWARD into the
#     spring, never upward through the mount.
#   - warm reset (OTA machine.reset(), soft reset): the 5 V rail never
#     dropped, so the STM32 kept outputting its last pulse — read it back and
#     start the ramp from the truth.
# Additionally, pan is locked (frozen, and on boot not even powered) while
# tilt is beyond TILT_SAFE: the drooped head only clears the booth with pan
# centered, so it must climb out of the droop zone before pan may move.
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), then
# ./deploy.sh — see README.md.

import struct
import time

import machine
from machine import Pin, I2C

import wetline

FW_VERSION = "0.5"           # 0.5 droop-zone pan lock + gentler rate; 0.4 slew/OC

# 8-Servos board (verified: I2C 0x25; STM32 sub-MCU).
SERVO_ADDR = 0x25
REG_MODE = 0x00              # 0x00+ch: 3 = servo (pulse) mode
REG_SERVO_PULSE = 0x60       # 0x60+ch*2: pulse width, u16 little-endian (µs)
REG_CURRENT = 0xA0           # total servo current, float32 little-endian (A)
CH_PAN = 0                   # channel 0 = pan
CH_TILT = 1                  # channel 1 = tilt

# Absolute safety clamp on pulse width (µs). The host already clamps to the
# per-axis calibration range; this is a hard backstop against a bad line.
PULSE_MIN = 500
PULSE_MAX = 2500
PULSE_CENTER = 1500

TILT_DROOP = 2167            # measured unpowered gravity droop (2026-07-14 rig)
TILT_SAFE = 1967             # top of the working range (judge_neck.toml tilt max).
                             # Beyond it the head is "deep" (toward droop), where
                             # it can only avoid the booth with pan centered — so
                             # while tilt is deep, pan is locked (not even powered).
BOOT_PAN = PULSE_CENTER      # boot pose: same as the old firmware, reached gently
BOOT_TILT = 1500             # = bottom of tilt's working range

HOME_RATE = 250              # µs/s — boot homing + protective moves (gentle)
AIM_RATE = 600               # µs/s — host AIM tracking (visibly smooth on the rig)
TICK_DT_CAP_MS = 100         # a stalled loop must not smuggle in a big step

# Overcurrent watchdog. REG_CURRENT is the board's TOTAL servo current, so
# this is stall detection (jammed head, broken linkage), not a per-axis
# limit: sustained draw above the limit eases tilt to TILT_SAFE at HOME_RATE
# and emits an unsolicited OVERCURRENT line (the host logs + skips it).
# Units believed amps but UNVERIFIED on hardware — tune on the rig; NaN or
# garbage reads are ignored. Set OC_LIMIT_A = None to disable.
#
# Polled ONLY while moving (and OC_TAIL_MS after): the STM32 renders its
# servo PWM in software, and servicing an I2C read can stretch an in-flight
# pulse — 0.4's always-on 4 Hz poll made the parked head twitch every few
# seconds. Idle keeps the bus silent; a stall only develops while the servo
# is being driven somewhere anyway.
OC_LIMIT_A = 1.5
OC_HOLD_MS = 700             # the spike must persist this long to trip
OC_POLL_MS = 250
OC_TAIL_MS = 1500            # keep watching this long after the last step

i2c = I2C(0, scl=Pin(1), sda=Pin(2), freq=100000)   # NanoC6 Grove: SCL=1, SDA=2

_cur = [float(BOOT_PAN), float(TILT_DROOP)]   # believed servo position (µs)
_tgt = [BOOT_PAN, BOOT_TILT]                  # where we're headed
_live = [False, False]                        # channel powered (mode 3 + pulse)
_rate = HOME_RATE                             # active slew limit (µs/s)
_i2c_ok = False                               # last pulse write reached the board

_last_ms = time.ticks_ms()
_move_ms = None              # when a pulse was last written, or None (parked)
_oc_poll_ms = _last_ms
_oc_since = None             # when current first exceeded the limit, or None
_oc_armed = True             # re-arms once current drops back under the limit


def servo_mode(ch, mode):
    i2c.writeto_mem(SERVO_ADDR, REG_MODE + ch, bytes([mode]))


def _clamp(us):
    return min(PULSE_MAX, max(PULSE_MIN, us))


def _write_pulse(ch, us):
    i2c.writeto_mem(SERVO_ADDR, REG_SERVO_PULSE + ch * 2,
                    int(us).to_bytes(2, "little"))


def _read_pulse(ch):
    """Pulse the STM32 is outputting right now — trusted only after a warm
    reset (5 V stayed up, the board held its last command). None if the read
    fails, is implausible, or the channel isn't in servo mode — a fresh STM32
    reads mode 0, so a brownout that also power-cycled the servo board (its
    registers back at defaults) can't spoof a held pulse."""
    try:
        if i2c.readfrom_mem(SERVO_ADDR, REG_MODE + ch, 1)[0] != 3:
            return None
        us = int.from_bytes(
            i2c.readfrom_mem(SERVO_ADDR, REG_SERVO_PULSE + ch * 2, 2), "little")
    except OSError:
        return None
    return us if PULSE_MIN <= us <= PULSE_MAX else None


def _power(ch):
    """Enable a channel where we believe it is. Pulse before mode: the first
    PWM the servo ever sees is our ramp start, never the STM32's default."""
    _write_pulse(ch, round(_cur[ch]))
    servo_mode(ch, 3)


def _step(dt_ms):
    """Walk each output toward its target at most _rate µs/s, pushing the new
    pulses. Channels power up lazily here (so an absent board just retries),
    and while tilt is deep (beyond TILT_SAFE, toward droop) pan is locked —
    not stepped, not even powered: swinging the drooped head can crash it
    into the booth, so the head must climb out of the droop zone first. On
    I2C failure the believed position is NOT advanced, so motion resumes
    from the right place when the board comes back."""
    global _i2c_ok, _move_ms
    max_move = _rate * dt_ms / 1000
    wrote = False
    try:
        if not _live[CH_TILT]:
            _power(CH_TILT)
            _live[CH_TILT] = True
            wrote = True
        tilt_deep = _cur[CH_TILT] > TILT_SAFE
        if not _live[CH_PAN] and not tilt_deep:
            # Pan's true position is unknowable without feedback, so its
            # power-up is the one unavoidable snap (to the assumed pose) —
            # taken only once the head is out of the droop zone.
            _power(CH_PAN)
            _live[CH_PAN] = True
            wrote = True
        for ch in (CH_PAN, CH_TILT):
            if not _live[ch] or (ch == CH_PAN and tilt_deep):
                continue
            d = _tgt[ch] - _cur[ch]
            if not d:
                continue
            move = max(-max_move, min(max_move, d))
            _write_pulse(ch, round(_cur[ch] + move))
            _cur[ch] += move
            wrote = True
        _i2c_ok = True
    except OSError:
        _i2c_ok = False
    if wrote:
        _move_ms = time.ticks_ms()


def handle_aim(args):
    """AIM <pan_us> <tilt_us> — already-calibrated pulse widths from the host.
    Sets slew-limited targets: OK acks the accepted target, not arrival."""
    global _rate
    if not args:
        return "missing_args"
    if len(args) < 2:
        return "need_pan_tilt"
    try:
        pan = int(args[0])
        tilt = int(args[1])
    except ValueError:
        return "bad_args"
    _tgt[CH_PAN] = _clamp(pan)
    _tgt[CH_TILT] = _clamp(tilt)
    _rate = AIM_RATE
    if not _i2c_ok:
        return "i2c_fail"    # board absent/unhappy; targets kept, tick retries
    return None


def _watch_current(now, send):
    """Stall guard: sustained overcurrent eases tilt into the counter-spring
    and reports it. One trip per excursion; re-arms when current recovers."""
    global _oc_poll_ms, _oc_since, _oc_armed, _rate
    if OC_LIMIT_A is None:
        return
    if _move_ms is None or time.ticks_diff(now, _move_ms) > OC_TAIL_MS:
        return                       # parked: keep the I2C bus silent
    if time.ticks_diff(now, _oc_poll_ms) < OC_POLL_MS:
        return
    _oc_poll_ms = now
    try:
        amps = struct.unpack("<f", i2c.readfrom_mem(SERVO_ADDR, REG_CURRENT, 4))[0]
    except OSError:
        return
    if not (0.0 <= amps < 50.0):     # NaN/garbage guard (NaN compares False)
        return
    if amps <= OC_LIMIT_A:
        _oc_since = None
        _oc_armed = True
        return
    if _oc_since is None:
        _oc_since = now
        return
    if _oc_armed and time.ticks_diff(now, _oc_since) >= OC_HOLD_MS:
        _oc_armed = False
        # Shed most of the gravity load without leaving the safe envelope:
        # full droop is only collision-safe with pan centered, so the guard
        # eases tilt to the top of the working range instead.
        _tgt[CH_TILT] = TILT_SAFE
        _rate = HOME_RATE
        print("overcurrent: %.2f A sustained, easing tilt to safe" % amps)
        if send:
            try:
                send("OVERCURRENT %.2f" % amps)
            except OSError:
                pass                 # link just died; wetline will notice


def tick(send):
    """wetline tick hook: one slew step + the overcurrent poll."""
    global _last_ms
    now = time.ticks_ms()
    dt = time.ticks_diff(now, _last_ms)
    _last_ms = now
    if dt <= 0:
        return
    _step(min(dt, TICK_DT_CAP_MS))
    _watch_current(now, send)


def init_servos():
    """Estimate where the head is, then soft-home. _step does the actual
    powering (tilt first; pan only once tilt is out of the droop zone)."""
    global _last_ms
    warm = machine.reset_cause() != machine.PWRON_RESET
    if warm:
        # Held pulses survive a warm reset; a failed/implausible readback
        # falls back to the same assumptions as a cold boot.
        _cur[CH_PAN] = float(_read_pulse(CH_PAN) or BOOT_PAN)
        _cur[CH_TILT] = float(_read_pulse(CH_TILT) or TILT_DROOP)
    # Blocking soft-home to the boot pose — the lift that used to be a snap.
    # Full droop → bottom-of-range tilt takes ~2.7 s at HOME_RATE; WiFi comes
    # up after. Bails if the board is absent (the tick keeps retrying, so
    # AIM acks ERR i2c_fail until the servo board shows up on the bus).
    _last_ms = time.ticks_ms()
    while _cur[CH_PAN] != _tgt[CH_PAN] or _cur[CH_TILT] != _tgt[CH_TILT]:
        now = time.ticks_ms()
        _step(min(time.ticks_diff(now, _last_ms), TICK_DT_CAP_MS))
        _last_ms = now
        if not _i2c_ok:
            print("servo board not responding on I2C 0x25")
            return
        _watch_current(now, None)    # the boot lift is the classic stall case
        time.sleep_ms(20)


init_servos()
wetline.run("judge-neck", FW_VERSION, {"AIM": handle_aim}, tick=tick)
