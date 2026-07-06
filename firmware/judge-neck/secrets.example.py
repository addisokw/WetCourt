# Per-deployment network config. Copy this file to `secrets.py` and fill in
# your values. `secrets.py` is gitignored, so WiFi credentials never reach
# the repo.
#
#   cp secrets.example.py secrets.py   # then edit secrets.py

WIFI_SSID = "your-ssid"
WIFI_PASS = "your-password"
ORCH_HOST = "192.168.1.50"   # orchestrator LAN IP
ORCH_PORT = 8090             # protocol default
