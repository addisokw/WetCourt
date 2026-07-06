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

echo "deploying to $DEST ..."
mkdir -p "$DEST/lib"
cp -R "$SRC/lib/." "$DEST/lib/"
cp "$SRC/personas.py" "$SRC/eye_face.py" "$SRC/inputs.py" "$SRC/config.py" \
   "$SRC/settings.toml" "$DEST/"
cp "$SRC/code.py" "$DEST/code.py"    # last: triggers the auto-reload
# FAT volumes grow macOS ._* AppleDouble turds; they're harmless to CP but tidy up.
find "$DEST/lib" "$DEST" -maxdepth 1 -name "._*" -delete 2>/dev/null || true
sync
echo "done — board auto-reloads. Watch it with: screen /dev/cu.usbmodem* 115200"
