# Wet Court — gavel firmware (servo strike), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Accessory: M5Stack 8-Servos board (I2C 0x25) — ch0 drives the gavel servo
#
# MicroPython reimplementation of the retired Arduino sketch (git history has
# it): same hardware, same GAVEL/GJOG contract, same role. One GAVEL = one
# rap: REST → RAISE → STRIKE → REST, acked after the swing completes.
#
# The role-agnostic protocol client is the shared ../micropython/wetline.py;
# this file is the servo driver + the GAVEL/GJOG handlers.
#
# The host normally sends the full strike geometry on every GAVEL (six values
# from gavel.toml, tuned live in the console's Gavel tab) so the firmware
# stays stateless; a bare GAVEL falls back to the compiled defaults below.
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), then
# ./deploy.sh — see README.md.

import time

from machine import Pin, I2C

import wetline

FW_VERSION = "0.2"           # 0.2 = MicroPython rewrite (0.1 was Arduino)

# 8-Servos board (verified: I2C 0x25; STM32 sub-MCU).
SERVO_ADDR = 0x25
REG_MODE = 0x00              # 0x00+ch: 3 = servo (pulse) mode
REG_SERVO_PULSE = 0x60       # 0x60+ch*2: pulse width, u16 little-endian (µs)
CH_GAVEL = 0                 # channel 0 = gavel arm

# Bare-GAVEL fallback geometry (servo pulse µs + dwell ms). The live values
# come from the host (gavel.toml); these only matter stand-alone. Most builds
# swing the head DOWN to strike (STRIKE < REST < RAISE); flip if mirrored.
GAVEL_REST = 1500            # ~center
GAVEL_RAISE = 2000           # wound up
GAVEL_STRIKE = 1100          # head down (the bang)
RAISE_DWELL_MS = 180
STRIKE_DWELL_MS = 120
SETTLE_DWELL_MS = 160

# Absolute safety clamps — hard backstops against a bad line. The dwell clamp
# also bounds how long one GAVEL can block the loop (the rap is synchronous;
# the ack goes out after the swing).
PULSE_MIN = 1000
PULSE_MAX = 2000
DWELL_MAX_MS = 2000

i2c = I2C(0, scl=Pin(1), sda=Pin(2), freq=100000)   # NanoC6 Grove: SCL=1, SDA=2
servo_ok = False             # 8-Servos board present at boot?


def servo_mode(ch, mode):
    i2c.writeto_mem(SERVO_ADDR, REG_MODE + ch, bytes([mode]))


def servo_pulse(ch, us):
    us = min(PULSE_MAX, max(PULSE_MIN, us))
    i2c.writeto_mem(SERVO_ADDR, REG_SERVO_PULSE + ch * 2, us.to_bytes(2, "little"))


def _dwell(ms):
    time.sleep_ms(min(max(ms, 0), DWELL_MAX_MS))


def handle_gavel(args):
    """GAVEL [<rest> <raise> <strike> <raise_dwell> <strike_dwell> <settle_dwell>]
    — one rap, then ack. Guards the no-servo-board case so a verdict can't
    silently no-op while replying OK."""
    if not servo_ok:
        return "no_servo_board"
    rest, raise_, strike = GAVEL_REST, GAVEL_RAISE, GAVEL_STRIKE
    rd, sd, td = RAISE_DWELL_MS, STRIKE_DWELL_MS, SETTLE_DWELL_MS
    if args:
        try:
            vals = [int(a) for a in args[:6]]
        except ValueError:
            return "bad_args"
        defaults = (rest, raise_, strike, rd, sd, td)
        vals += list(defaults[len(vals):])
        rest, raise_, strike, rd, sd, td = vals
    try:
        servo_pulse(CH_GAVEL, raise_)
        _dwell(rd)
        servo_pulse(CH_GAVEL, strike)
        _dwell(sd)
        servo_pulse(CH_GAVEL, rest)
        _dwell(td)
    except OSError:
        return "i2c_fail"
    return None


def handle_gjog(args):
    """GJOG <us> — move to a raw pulse-width and hold (console live tuning)."""
    if not servo_ok:
        return "no_servo_board"
    if not args:
        return "missing_us"
    try:
        us = int(args[0])
    except ValueError:
        return "bad_us"
    try:
        servo_pulse(CH_GAVEL, us)
    except OSError:
        return "i2c_fail"
    return None


def init_servo():
    global servo_ok
    servo_ok = SERVO_ADDR in i2c.scan()
    if servo_ok:
        servo_mode(CH_GAVEL, 3)          # 3 = servo (pulse) mode
        servo_pulse(CH_GAVEL, GAVEL_REST)
    else:
        print("8-Servos board not found at 0x25")


init_servo()
wetline.run("gavel", FW_VERSION, {"GAVEL": handle_gavel, "GJOG": handle_gjog})
