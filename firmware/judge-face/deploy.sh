#!/bin/sh
# Deploy the judge-face firmware to a mounted CIRCUITPY drive.
#
# Prereqs (see README.md):
#   - CircuitPython 10.2.1 for Matrix Portal M4 already flashed
#   - settings.toml created next to this script (copy settings.toml.example)
#
# Usage: ./deploy.sh [/Volumes/CIRCUITPY]
set -eu

DEST="${1:-/Volumes/CIRCUITPY}"
SRC="$(cd "$(dirname "$0")" && pwd)"

[ -d "$DEST" ] || { echo "error: $DEST not mounted (is the board plugged in?)" >&2; exit 1; }
[ -f "$SRC/settings.toml" ] || {
    echo "error: $SRC/settings.toml missing — copy settings.toml.example and fill it in" >&2
    exit 1
}
# boot.py gives the drive to on-device code by default (OTA mode), which makes
# it read-only to this Mac — probe before a half-finished copy.
if ! (touch "$DEST/.wc_write_probe" 2>/dev/null && rm -f "$DEST/.wc_write_probe"); then
    echo "error: $DEST is read-only (board is in OTA mode)." >&2
    echo "       Hold the UP button while pressing reset for USB deploy mode," >&2
    echo "       or push over WiFi instead: python3 ../micropython/otapush.py <board-ip>" >&2
    exit 1
fi

echo "deploying to $DEST ..."
mkdir -p "$DEST/lib"
cp -R "$SRC/lib/." "$DEST/lib/"
cp "$SRC/personas.py" "$SRC/eye_face.py" "$SRC/inputs.py" "$SRC/config.py" \
   "$SRC/ota.py" "$SRC/boot.py" "$SRC/settings.toml" "$DEST/"
cp "$SRC/code.py" "$DEST/code.py"    # last: triggers the auto-reload
# boot.py takes effect on the NEXT hard reset (auto-reload doesn't re-run it).
# FAT volumes grow macOS ._* AppleDouble turds; they're harmless to CP but tidy up.
find "$DEST/lib" "$DEST" -maxdepth 1 -name "._*" -delete 2>/dev/null || true
sync
echo "done — board auto-reloads. Watch it with: screen /dev/cu.usbmodem* 115200"
