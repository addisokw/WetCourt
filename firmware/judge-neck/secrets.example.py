# Per-deployment network config. Copy this file to `secrets.py` and fill in
# your values. `secrets.py` is gitignored, so WiFi credentials never reach
# the repo.
#
#   cp secrets.example.py secrets.py   # then edit secrets.py

WIFI_SSID = "your-ssid"
WIFI_PASS = "your-password"

# Where the orchestrator lives. Leave ORCH_HOST unset (or "") to DISCOVER it:
# the board listens for the orchestrator's UDP beacon (port 8091, see
# protocol README) and follows it across machines/addresses — the usual mode.
# Setting a host or IP here is a hard override that never listens for
# beacons: use it on show rigs, or when two orchestrators share a LAN.
# ORCH_HOST = "192.168.1.50"
# ORCH_PORT = 8090             # protocol default; only used with ORCH_HOST
# ORCH_BEACON_PORT = 8091      # only change if the orchestrator's differs

# Optional: WiFi OTA updates (see ../micropython/README.md). With a token set,
# the board listens on OTA_PORT and `../micropython/otapush.py <ip>` can push
# firmware with no cable. Unset/empty = OTA disabled.
# OTA_TOKEN = "a-long-random-string"
# OTA_PORT = 8266
