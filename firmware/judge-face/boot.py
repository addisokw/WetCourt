# Wet Court judge-face — filesystem ownership arbitration (runs at reset,
# before USB enumerates; NOT re-run on auto-reload or supervisor.reload()).
#
# CircuitPython gives the CIRCUITPY drive to exactly one writer:
#   - OTA mode (default): code owns it — storage.remount makes it writable to
#     ota.py so WiFi pushes (otapush.py) can stage files; the USB drive is
#     still visible to a host, but read-only.
#   - USB deploy mode: HOLD THE UP BUTTON WHILE RESETTING — the drive stays
#     host-writable so ./deploy.sh works. This is also the recovery path:
#     boot.py itself is on the OTA forbidden list, so a bad push can never
#     take this escape hatch away.
#
# Keep this file tiny and defensive: if anything here raises, CircuitPython
# continues with the default (host-writable) filesystem — the safe direction.

import board
import digitalio
import storage

try:
    btn = digitalio.DigitalInOut(board.BUTTON_UP)
    btn.switch_to_input(pull=digitalio.Pull.UP)
    up_held = not btn.value          # active low
    btn.deinit()
except Exception as e:               # unexpected board variant — stay host-writable
    print("boot: button probe failed, USB deploy mode:", e)
    up_held = True

if up_held:
    print("boot: UP held - USB deploy mode (drive writable to host, OTA off)")
else:
    try:
        storage.remount("/", readonly=False)
        print("boot: OTA mode (drive writable to code; hold UP at reset for USB deploy)")
    except Exception as e:
        print("boot: remount failed, USB deploy mode:", e)
