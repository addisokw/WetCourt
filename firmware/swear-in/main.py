# Wet Court — swear-in firmware (defendant's arcade button), MicroPython.
# Board: M5Stack NanoC6 (ESP32-C6) · MicroPython v1.28 ESP32_GENERIC_C6 (flash @ 0x0)
# Hardware: arcade pushbutton with built-in LED, wired to the Grove pigtail:
#   - button switch between G1 (Grove SCL position, white wire) and GND
#   - button LED   between G2 (Grove SDA position, yellow wire) and GND
#
# The defendant's one physical control: pressing it emits an unsolicited
# `BUTTON` line (start a trial from Idle; the FSM decides what a press means
# in other states — see the protocol spec). The host drives the button's lamp
# with `LED <mode>` so the light itself is the cue for when pressing does
# something: blink = "press me" attract, pulse = gentle armed glow, on/off =
# steady states.
#
# The role-agnostic protocol client is the shared ../micropython/wetline.py;
# this file is the debounced button scan + the LED animator, run from
# wetline's tick hook. While the link is down the lamp is forced dark (a
# press would go nowhere) and presses are dropped — the light IS the
# "court is in session" indicator.
#
# BEFORE DEPLOYING: copy secrets.example.py → secrets.py (gitignored), then
# ./deploy.sh — see README.md.

import time

import wetline
from machine import PWM, Pin

FW_VERSION = "0.1"

BUTTON_PIN = 1  # arcade switch to GND, internal pull-up: pressed == 0
LED_PIN = 2  # arcade lamp to GND, PWM-driven, active high

DEBOUNCE_MS = 30  # stable-level time before a press/release is believed
REARM_MS = 250  # min gap between emitted presses (swallows mashing)
FLASH_MS = 150  # local full-bright flash on press (instant tactile ack)

BLINK_PERIOD_MS = 800  # attract blink: 400 on / 400 off
PULSE_PERIOD_MS = 2400  # breathing glow

_btn = Pin(BUTTON_PIN, Pin.IN, Pin.PULL_UP)
_pwm = PWM(Pin(LED_PIN), freq=1000)
_pwm.duty_u16(0)  # dark at boot; the host lights it once the link is up

_mode = "off"  # host-commanded LED mode: off | on | blink | pulse
_LED_MODES = ("off", "on", "blink", "pulse")

# Debounce state: raw edge timestamping + last believed level.
_raw_last = 1
_raw_since = time.ticks_ms()
_stable = 1
_last_emit = 0
_flash_until = 0


def handle_led(args):
    """LED <mode> — set the button lamp's animation (off|on|blink|pulse)."""
    global _mode
    if not args:
        return "missing_mode"
    mode = args[0]
    if mode not in _LED_MODES:
        return "bad_mode"
    _mode = mode
    return None


def _duty(now):
    """The lamp's PWM duty for this instant, from the commanded mode."""
    if time.ticks_diff(_flash_until, now) > 0:
        return 65535  # press feedback flash outranks the mode
    if _mode == "on":
        return 65535
    if _mode == "blink":
        return 65535 if (now % BLINK_PERIOD_MS) < (BLINK_PERIOD_MS // 2) else 0
    if _mode == "pulse":
        ph = now % PULSE_PERIOD_MS
        half = PULSE_PERIOD_MS // 2
        v = ph / half if ph < half else (PULSE_PERIOD_MS - ph) / half
        return int(65535 * v * v)  # squared ≈ gamma: reads as an even breathe
    return 0  # "off"


def _tick(send):
    """wetline tick: debounce the switch, emit BUTTON, animate the lamp."""
    global _raw_last, _raw_since, _stable, _last_emit, _flash_until
    now = time.ticks_ms()

    raw = _btn.value()
    if raw != _raw_last:
        _raw_last = raw
        _raw_since = now
    elif raw != _stable and time.ticks_diff(now, _raw_since) >= DEBOUNCE_MS:
        _stable = raw
        if raw == 0:  # debounced press edge
            _flash_until = time.ticks_add(now, FLASH_MS)
            if send and time.ticks_diff(now, _last_emit) >= REARM_MS:
                _last_emit = now
                send("BUTTON")

    # Lamp: dark whenever the link is down — the light doubles as the
    # "pressing this does something" indicator.
    _pwm.duty_u16(_duty(now) if send else 0)


wetline.run("swear-in", FW_VERSION, {"LED": handle_led}, tick=_tick)
