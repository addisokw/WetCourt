// Wet Court — squirt-gun FIRE firmware (relay)
// Board: M5Stack NanoC6 (ESP32-C6)
// Accessory: M5Stack 3A Relay (GPIO) on the NanoC6's Grove port
//
// Owns FIRE + PING. This is a SEPARATE board from the pan/tilt `turret` because
// the turret's NanoC6 has its only Grove I2C pins taken by the servo board,
// leaving no GPIO for the relay. This board's Grove port is free, so the relay
// signal drives a Grove GPIO directly.
//
// Speaks the Wet Court device line protocol (see ../../protocol/README.md):
// dials the orchestrator over TCP, identifies with `HELLO squirt`, and handles
// FIRE / PING, replying `OK <verb>` or `ERR <verb> <reason>`.
//
// ─── BEFORE FLASHING: copy secrets.example.h → secrets.h and fill in your WiFi
//     + orchestrator IP (secrets.h is gitignored), and confirm RELAY_PIN. ───

#include <WiFi.h>
#include "secrets.h"   // WIFI_SSID / WIFI_PASS / ORCH_HOST / ORCH_PORT (gitignored)

// ───────────────────────── CONFIG ─────────────────────────
// Network + orchestrator address live in secrets.h (gitignored). Below is
// hardware config, safe to commit.
static const char* FW_VERSION = "0.1";

// Relay signal pin. The 3A Relay's control wire lands on a NanoC6 Grove pin —
// GPIO2 (SDA position) or GPIO1 (SCL position). CONFIRM which by testing; if
// GPIO2 doesn't click the relay, try 1. HIGH = fire.
static const uint8_t RELAY_PIN = 2;

// FIRE safety clamp (ms) — refuse absurd durations even if the host asks.
static const uint32_t FIRE_MAX_MS = 1000;
// ───────────────────────────────────────────────────────────

WiFiClient client;
static char lineBuf[96];
static uint8_t lineLen = 0;

static void relaySet(bool on) {
  digitalWrite(RELAY_PIN, on ? HIGH : LOW);
}

// ───────────────────────── command handlers ─────────────────────────
static void reply(const char* s) {
  client.print(s);
  client.print('\n');
}

// FIRE <ms>: pulse the relay for <ms>, then ack. Durations are short (<1s) so a
// blocking wait is fine — the ESP32 WiFi stack runs on its own RTOS task.
static void doFire(const char* arg) {
  if (!arg) { reply("ERR FIRE missing_ms"); return; }
  long ms = atol(arg);
  if (ms <= 0) { reply("ERR FIRE bad_ms"); return; }
  if ((uint32_t)ms > FIRE_MAX_MS) ms = FIRE_MAX_MS;
  relaySet(true);
  delay(ms);
  relaySet(false);
  reply("OK FIRE");
}

static void dispatch(char* line) {
  char* verb = strtok(line, " ");
  if (!verb) return;
  char* rest = strtok(nullptr, "");   // remainder after the verb
  if (strcmp(verb, "FIRE") == 0)      doFire(rest);
  else if (strcmp(verb, "PING") == 0) reply("OK PING");
  else {
    client.print("ERR ");
    client.print(verb);
    client.print(" unsupported\n");   // AIM lives on the turret board
  }
}

// ───────────────────────── connection ─────────────────────────
static bool ensureWifi() {
  if (WiFi.status() == WL_CONNECTED) return true;
  WiFi.mode(WIFI_STA);
  WiFi.begin(WIFI_SSID, WIFI_PASS);
  unsigned long start = millis();
  while (WiFi.status() != WL_CONNECTED && millis() - start < 15000) delay(200);
  return WiFi.status() == WL_CONNECTED;
}

// Dial the orchestrator and complete the HELLO handshake. Returns true if the
// host replied WELCOME.
static bool connectOrchestrator() {
  if (!client.connect(ORCH_HOST, ORCH_PORT)) return false;
  client.setNoDelay(true);
  client.print("HELLO squirt ");
  client.print(FW_VERSION);
  client.print('\n');

  unsigned long start = millis();
  String first = "";
  while (millis() - start < 3000) {
    while (client.available()) {
      char c = client.read();
      if (c == '\n') {
        first.trim();
        return first == "WELCOME";
      }
      if (c != '\r' && first.length() < 64) first += c;
    }
    delay(5);
  }
  return false;
}

void setup() {
  Serial.begin(115200);
  pinMode(RELAY_PIN, OUTPUT);
  relaySet(false);                // fail safe: gun off
}

void loop() {
  if (!ensureWifi()) { delay(1000); return; }

  if (!client.connected()) {
    relaySet(false);              // never leave the gun on across a reconnect
    if (!connectOrchestrator()) {
      client.stop();
      delay(2000);                // backoff before retry
      return;
    }
    lineLen = 0;
    Serial.println("connected to orchestrator");
  }

  while (client.available()) {
    char c = client.read();
    if (c == '\n') {
      lineBuf[lineLen] = '\0';
      if (lineLen && lineBuf[lineLen - 1] == '\r') lineBuf[lineLen - 1] = '\0';
      if (lineLen) dispatch(lineBuf);
      lineLen = 0;
    } else if (lineLen < sizeof(lineBuf) - 1) {
      lineBuf[lineLen++] = c;
    } else {
      lineLen = 0;
    }
  }
}
