# Wet Court — squirt-gun FIRE firmware (relay), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Accessory: M5Stack 3A Relay (GPIO) on the NanoC6's Grove port
#
# MicroPython reimplementation of the retired Arduino sketch (git history has
# it): same hardware, same FIRE contract, same role. This is a SEPARATE board
# from the pan/tilt `turret` because that NanoC6's only Grove pins are taken
# by the servo board's I2C, leaving no GPIO for the relay.
#
# The role-agnostic protocol client is the shared ../micropython/wetline.py;
# this file is just the relay pin + the FIRE handler.
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), confirm
# RELAY_PIN, then ./deploy.sh — see README.md.

import time

from machine import Pin

import wetline

FW_VERSION = "0.3"           # 0.3 adds OTA; 0.2 was the MicroPython rewrite

# Relay signal pin. The 3A Relay's control wire lands on a NanoC6 Grove pin —
# GPIO2 (SDA position) or GPIO1 (SCL position). CONFIRM which by testing; if
# GPIO2 doesn't click the relay, try 1. HIGH = fire.
RELAY_PIN = 2

# FIRE safety clamp (ms) — refuse absurd durations even if the host asks.
FIRE_MAX_MS = 1000

relay = Pin(RELAY_PIN, Pin.OUT)
relay.value(0)               # fail safe: gun off at boot


def handle_fire(args):
    """FIRE <ms> — pulse the relay for <ms>, then ack. Durations are short
    (<= 1 s) so blocking through the pulse is fine: the ack goes out after the
    water stops, which is what the host times."""
    if not args:
        return "missing_ms"
    try:
        ms = int(args[0])
    except ValueError:
        return "bad_ms"
    if ms <= 0:
        return "bad_ms"
    ms = min(ms, FIRE_MAX_MS)
    relay.value(1)
    try:
        time.sleep_ms(ms)
    finally:
        relay.value(0)       # the gun must never stay on past the pulse
    return None


wetline.run("squirt", FW_VERSION, {"FIRE": handle_fire})
