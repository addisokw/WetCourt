# Wet Court judge-face — entry point.
# Board: Adafruit Matrix Portal M4 · Panel: 64x32 HUB75, mounted PORTRAIT
# (rotation 90 → logical 32 wide x 64 tall).
#
# Init the display, build the EyeFace, and run the loop: poll the
# orchestrator link (demo mode while it's down), advance the animation,
# refresh. FPS is reported over serial every 5 s (brief §7 milestone 3).

import board
import displayio
import framebufferio
import rgbmatrix
from adafruit_ticks import ticks_ms, ticks_diff

import config
import ota
from eye_face import EyeFace
from inputs import DemoSource, OrchestratorLink

displayio.release_displays()

# Canonical Matrix Portal M4 HUB75 wiring, via the board's MTX_* pin names.
# A 64x32 panel is 1/16-scan → 4 address lines (a 64x64 adds MTX_ADDRE).
matrix = rgbmatrix.RGBMatrix(
    width=config.WIDTH, height=config.HEIGHT, bit_depth=config.BIT_DEPTH,
    rgb_pins=[board.MTX_R1, board.MTX_G1, board.MTX_B1,
              board.MTX_R2, board.MTX_G2, board.MTX_B2],
    addr_pins=[board.MTX_ADDRA, board.MTX_ADDRB, board.MTX_ADDRC, board.MTX_ADDRD],
    clock_pin=board.MTX_CLK, latch_pin=board.MTX_LAT, output_enable_pin=board.MTX_OE)
display = framebufferio.FramebufferDisplay(
    matrix, auto_refresh=False, rotation=config.ROTATION)

eye = EyeFace(display, persona=config.BOOT_PERSONA)
link = None if config.FORCE_DEMO else OrchestratorLink()
demo = DemoSource()
# WiFi firmware pushes (otapush.py). Rides the link's radio, so forced-demo
# mode (no link, no WiFi) has no OTA — deploy over USB there anyway.
ota_srv = ota.server_from_settings() if link else None

last = ticks_ms()
frames = 0
fps_t0 = last
was_connected = False

while True:
    now = ticks_ms()
    dt = ticks_diff(now, last) / 1000.0
    last = now
    # Clamp hitches (WiFi association, persona rebuild) so the eye doesn't leap.
    if dt > 0.25:
        dt = 0.25

    connected = link.poll(eye, now) if link else False
    if ota_srv:
        ota_srv.poll(link, now)    # self-rate-limits; near-free when idle
    if connected:
        if not was_connected:
            eye.set_phase("idle")     # host owns state now; drop demo leftovers
        demo.reset()
    else:
        demo.update(eye, dt)
    was_connected = connected

    eye.tick(dt)
    display.refresh(minimum_frames_per_second=0)

    frames += 1
    if ticks_diff(now, fps_t0) >= 5000:
        print("fps: %.1f%s" % (frames * 1000.0 / ticks_diff(now, fps_t0),
                               "" if connected else " (demo)"))
        frames = 0
        fps_t0 = now
