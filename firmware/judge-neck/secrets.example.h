#pragma once
// Per-deployment network config. Copy this file to `secrets.h` and fill in your
// values. `secrets.h` is gitignored, so WiFi credentials never reach the repo.
//
//   cp secrets.example.h secrets.h   # then edit secrets.h

#define WIFI_SSID "YOUR_WIFI"
#define WIFI_PASS "YOUR_PASS"
#define ORCH_HOST "192.168.1.50"   // orchestrator LAN IP
#define ORCH_PORT 8090             // protocol default
