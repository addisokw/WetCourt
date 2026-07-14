# Per-deployment network config. Copy this file to `secrets.py` and fill in
# your values. `secrets.py` is gitignored, so WiFi credentials never reach
# the repo.
#
#   cp secrets.example.py secrets.py   # then edit secrets.py

WIFI_SSID = "your-ssid"
WIFI_PASS = "your-password"
ORCH_HOST = "192.168.1.50"   # orchestrator LAN IP
ORCH_PORT = 8090             # protocol default

# Optional: WiFi OTA updates (see ../micropython/README.md). With a token set,
# the board listens on OTA_PORT and `../micropython/otapush.py <ip>` can push
# firmware with no cable. Unset/empty = OTA disabled.
# OTA_TOKEN = "a-long-random-string"
# OTA_PORT = 8266
