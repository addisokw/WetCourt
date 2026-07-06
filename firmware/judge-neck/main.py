# Wet Court — judge-neck firmware (pan/tilt gaze), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Accessory: M5Stack 8-Servos board (I2C 0x25) — ch0 pan, ch1 tilt
#
# MicroPython reimplementation of the retired Arduino sketch (git history has
# it): same hardware, same AIM contract, same role — only the runtime changed.
# The role-agnostic protocol client lives in wetline.py; this file is just the
# servo driver + the AIM handler.
#
# AIM values are servo pulse-width MICROSECONDS (the host applies calibration;
# judge_neck.toml uses 1000..2000, center 1500). The firmware stays "dumb".
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), then
# ./deploy.sh — see README.md.

from machine import Pin, I2C

import wetline

FW_VERSION = "0.3"           # 0.3 adds OTA; 0.2 was the MicroPython rewrite

# 8-Servos board (verified: I2C 0x25; STM32 sub-MCU).
SERVO_ADDR = 0x25
REG_MODE = 0x00              # 0x00+ch: 3 = servo (pulse) mode
REG_SERVO_PULSE = 0x60       # 0x60+ch*2: pulse width, u16 little-endian (µs)
CH_PAN = 0                   # channel 0 = pan
CH_TILT = 1                  # channel 1 = tilt

# Absolute safety clamp on pulse width (µs). The host already clamps to the
# per-axis calibration range; this is a hard backstop against a bad line.
PULSE_MIN = 1000
PULSE_MAX = 2000
PULSE_CENTER = 1500

i2c = I2C(0, scl=Pin(1), sda=Pin(2), freq=100000)   # NanoC6 Grove: SCL=1, SDA=2


def servo_mode(ch, mode):
    i2c.writeto_mem(SERVO_ADDR, REG_MODE + ch, bytes([mode]))


def servo_pulse(ch, us):
    us = min(PULSE_MAX, max(PULSE_MIN, us))
    i2c.writeto_mem(SERVO_ADDR, REG_SERVO_PULSE + ch * 2, us.to_bytes(2, "little"))


def handle_aim(args):
    """AIM <pan_us> <tilt_us> — already-calibrated pulse widths from the host."""
    if not args:
        return "missing_args"
    if len(args) < 2:
        return "need_pan_tilt"
    try:
        pan = int(args[0])
        tilt = int(args[1])
    except ValueError:
        return "bad_args"
    try:
        servo_pulse(CH_PAN, pan)
        servo_pulse(CH_TILT, tilt)
    except OSError:
        return "i2c_fail"    # servo board absent / unhappy on the bus
    return None


def init_servos():
    try:
        servo_mode(CH_PAN, 3)            # 3 = servo (pulse) mode
        servo_mode(CH_TILT, 3)
        servo_pulse(CH_PAN, PULSE_CENTER)
        servo_pulse(CH_TILT, PULSE_CENTER)
    except OSError:
        # Keep serving the protocol anyway; AIM will ack ERR i2c_fail until
        # the servo board shows up on the bus.
        print("servo board not responding on I2C 0x25")


init_servos()
wetline.run("judge-neck", FW_VERSION, {"AIM": handle_aim})
