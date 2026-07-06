#!/bin/sh
# Deploy the squirt MicroPython firmware to a NanoC6 via mpremote.
#
# Prereqs (see README.md):
#   - MicroPython ESP32_GENERIC_C6 flashed at offset 0x0 (one-time)
#   - pip3 install mpremote
#   - secrets.py created next to this script (copy secrets.example.py)
#
# Usage: ./deploy.sh [port]     (port auto-detected with one board attached)
set -eu

SRC="$(cd "$(dirname "$0")" && pwd)"
PORT="${1:-}"

[ -f "$SRC/secrets.py" ] || {
    echo "error: $SRC/secrets.py missing — copy secrets.example.py and fill it in" >&2
    exit 1
}

MP="mpremote${PORT:+ connect $PORT}"
echo "deploying via ${MP} ..."
$MP cp "$SRC/../micropython/wetline.py" :wetline.py
$MP cp "$SRC/secrets.py" :secrets.py
$MP cp "$SRC/main.py" :main.py
$MP reset
echo "done — board reboots into main.py."
echo "watch it: mpremote${PORT:+ connect $PORT} repl   (Ctrl-] exits)"
