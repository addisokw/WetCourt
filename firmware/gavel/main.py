# Wet Court — gavel firmware (servo strike), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Servo: signal driven DIRECTLY on Grove G2 (yellow wire) via an adapter
# cable — 50 Hz PWM, pulse width in µs. There is no 8-Servos board here:
# both units are taken by the turret and judge-neck pan/tilts.
#
# One GAVEL = one strike sequence: REST → RAISE, then (STRIKE → RAISE) ×
# strikes, then → REST, acked after the swing completes. The role-agnostic
# protocol client is the shared ../micropython/wetline.py; this file is the
# servo driver + the GAVEL/GJOG handlers.
#
# The host normally sends the full strike geometry on every GAVEL (seven values
# from gavel.toml, tuned live in the console's Gavel tab) so the firmware
# stays stateless; a bare GAVEL falls back to the compiled defaults below.
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), then
# ./deploy.sh — see README.md.

import time

import wetline
from machine import PWM, Pin

FW_VERSION = "0.5"  # 0.5 = multi-rap strike sequence (strikes arg)

# Servo signal pin: Grove G2 (the yellow/SDA-position wire). Power the servo
# from an external 5 V supply, NOT the Grove 5 V pin — a beefy servo's stall
# current will brown out the NanoC6. Share grounds.
GAVEL_PIN = 2

# Bare-GAVEL fallback geometry (servo pulse µs + dwell ms). The live values
# come from the host (gavel.toml); these only matter stand-alone. Most builds
# swing the head DOWN to strike (STRIKE < REST < RAISE); flip if mirrored.
GAVEL_REST = 1500  # ~center
GAVEL_RAISE = 2000  # wound up
GAVEL_STRIKE = 1100  # head down (the bang)
RAISE_DWELL_MS = 180
STRIKE_DWELL_MS = 120
SETTLE_DWELL_MS = 160
GAVEL_STRIKES = 1  # raps per sequence

# Absolute safety clamps — hard backstops against a bad line. The dwell clamp
# also bounds how long one GAVEL can block the loop (the rap is synchronous;
# the ack goes out after the swing), and STRIKES_MAX caps how many raps that
# blocking swing can chain.
PULSE_MIN = 500
PULSE_MAX = 2500
DWELL_MAX_MS = 2000
STRIKES_MAX = 10

_pwm = PWM(Pin(GAVEL_PIN), freq=50)  # standard hobby-servo frame rate


def servo_pulse(us):
    """Hold the servo at a pulse width (µs), clamped to the safe window."""
    us = min(PULSE_MAX, max(PULSE_MIN, us))
    _pwm.duty_ns(us * 1000)


def _dwell(ms):
    time.sleep_ms(min(max(ms, 0), DWELL_MAX_MS))


def handle_gavel(args):
    """GAVEL [<rest> <raise> <strike> <raise_dwell> <strike_dwell> <settle_dwell> <strikes>]
    — one strike sequence, then ack: REST → RAISE, then (STRIKE → RAISE) ×
    strikes, then → REST. (Direct PWM has no presence detection: a missing or
    unpowered servo still acks OK — watch the mech, not just the ack.)"""
    rest, raise_, strike = GAVEL_REST, GAVEL_RAISE, GAVEL_STRIKE
    rd, sd, td = RAISE_DWELL_MS, STRIKE_DWELL_MS, SETTLE_DWELL_MS
    strikes = GAVEL_STRIKES
    if args:
        try:
            vals = [int(a) for a in args[:7]]
        except ValueError:
            return "bad_args"
        defaults = (rest, raise_, strike, rd, sd, td, strikes)
        vals += list(defaults[len(vals) :])
        rest, raise_, strike, rd, sd, td, strikes = vals
    strikes = min(max(strikes, 1), STRIKES_MAX)
    # servo_pulse(raise_)
    _dwell(rd)
    for _ in range(strikes):
        servo_pulse(strike)
        _dwell(sd)
        servo_pulse(raise_)
        _dwell(rd)
    servo_pulse(rest)
    _dwell(td)
    return None


def handle_gjog(args):
    """GJOG <us> — move to a raw pulse-width and hold (console live tuning)."""
    if not args:
        return "missing_us"
    try:
        us = int(args[0])
    except ValueError:
        return "bad_us"
    servo_pulse(us)
    return None


servo_pulse(GAVEL_REST)  # boot at rest
wetline.run("gavel", FW_VERSION, {"GAVEL": handle_gavel, "GJOG": handle_gjog})
