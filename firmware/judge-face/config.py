# Wet Court judge-face — build/runtime configuration.
#
# Credentials and host addressing live in settings.toml (gitignored; copy
# settings.toml.example). Everything here reads that via os.getenv, with the
# display constants as plain code (they describe the hardware, not the site).

import os

FW_VERSION = "0.4"     # 0.4 = Matrix Portal S3 port (native WiFi + mDNS)

# Physical panel (as wired to the HUB75 header) — logical orientation comes
# from ROTATION. 90 = portrait (32 wide x 64 tall), 0 = landscape.
WIDTH = 64
HEIGHT = 32
ROTATION = 90
BIT_DEPTH = 5          # 4-6: iris gradient smoothness vs refresh cost (brief §2)

WIFI_SSID = os.getenv("WIFI_SSID")
WIFI_PASS = os.getenv("WIFI_PASS")
ORCH_HOST = os.getenv("ORCH_HOST")
ORCH_PORT = int(os.getenv("ORCH_PORT") or 8090)

# Network name: DHCP hostname (→ judge-face.lan on the booth router, like
# the NanoC6 fixtures) and mDNS (→ judge-face.local). Letters/digits/hyphen.
MDNS_NAME = str(os.getenv("EYE_MDNS") or "judge-face")

# Force demo mode (never dial the orchestrator); otherwise demo runs
# automatically whenever the link is down.
FORCE_DEMO = str(os.getenv("EYE_DEMO") or "0") == "1"
BOOT_PERSONA = str(os.getenv("EYE_PERSONA") or "honorable")
