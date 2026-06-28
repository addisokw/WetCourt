// Wet Court — squirt-gun turret firmware
// Board: M5Stack NanoC6 (ESP32-C6)
// Accessories: M5Stack 8-Servos board (I2C 0x25) — ch0 pan, ch1 tilt
//              M5Stack single relay (fires the squirt gun)
//
// Speaks the Wet Court device line protocol (see ../../protocol/README.md):
// dials the orchestrator over TCP, identifies with `HELLO turret`, and handles
// FIRE / AIM / PING, replying `OK <verb>` or `ERR <verb> <reason>`.
//
// AIM values are servo pulse-width MICROSECONDS (the host applies calibration;
// turret.toml uses 1000..2000, center 1500). The firmware stays "dumb".
//
// ─── BEFORE FLASHING: copy secrets.example.h → secrets.h and fill in your WiFi
//     + orchestrator IP (secrets.h is gitignored), and confirm the RELAY wiring
//     (see RELAY CONFIG). ───

#include <Wire.h>
#include <WiFi.h>
#include "secrets.h"   // WIFI_SSID / WIFI_PASS / ORCH_HOST / ORCH_PORT (gitignored)

// ───────────────────────── CONFIG (edit me) ─────────────────────────
// Network + orchestrator address live in secrets.h (gitignored) — see
// secrets.example.h. Everything below is hardware config, safe to commit.
static const char* FW_VERSION = "0.1";

// 8-Servos board (verified: I2C 0x25; STM32 sub-MCU).
static const uint8_t SERVO_ADDR      = 0x25;
static const uint8_t REG_MODE        = 0x00;     // 0x00+ch: 3 = servo (pulse) mode
static const uint8_t REG_SERVO_PULSE = 0x60;     // 0x60+ch*2: pulse width, 2 bytes LE (µs)
static const uint8_t CH_PAN  = 0;                // channel 0 = pan
static const uint8_t CH_TILT = 1;                // channel 1 = tilt

// Absolute safety clamp on pulse width (µs). The host already clamps to the
// per-axis calibration range; this is a hard backstop against a bad line.
static const int PULSE_MIN = 1000;
static const int PULSE_MAX = 2000;
static const int PULSE_CENTER = 1500;

// FIRE safety clamp (ms) — refuse absurd durations even if the host asks.
static const uint32_t FIRE_MAX_MS = 1000;

// ── RELAY CONFIG — CONFIRM which relay you have, then set ONE path ──
// Path A (default): GPIO relay (e.g. M5Stack 3A Relay / 2-Ch Relay) — a single
//   signal pin, HIGH = fire. Set RELAY_PIN to the GPIO you wired it to.
// Path B: I2C relay unit on the same Grove bus — comment out RELAY_GPIO and set
//   the address/register below.
#define RELAY_GPIO
#ifdef RELAY_GPIO
static const uint8_t RELAY_PIN = 6;              // CONFIRM: GPIO wired to relay signal
#else
static const uint8_t RELAY_ADDR = 0x26;          // CONFIRM: I2C relay unit address
static const uint8_t RELAY_REG  = 0x00;          // CONFIRM: control register (1=on,0=off)
#endif
// ─────────────────────────────────────────────────────────────────────

WiFiClient client;
static char lineBuf[96];
static uint8_t lineLen = 0;

// ───────────────────────── hardware helpers ─────────────────────────
static void servoMode(uint8_t ch, uint8_t mode) {
  Wire.beginTransmission(SERVO_ADDR);
  Wire.write(REG_MODE + ch);
  Wire.write(mode);
  Wire.endTransmission();
}

static void servoPulse(uint8_t ch, int us) {
  if (us < PULSE_MIN) us = PULSE_MIN;
  if (us > PULSE_MAX) us = PULSE_MAX;
  Wire.beginTransmission(SERVO_ADDR);
  Wire.write(REG_SERVO_PULSE + ch * 2);
  Wire.write((uint8_t)(us & 0xFF));         // little-endian
  Wire.write((uint8_t)((us >> 8) & 0xFF));
  Wire.endTransmission();
}

static void relaySet(bool on) {
#ifdef RELAY_GPIO
  digitalWrite(RELAY_PIN, on ? HIGH : LOW);
#else
  Wire.beginTransmission(RELAY_ADDR);
  Wire.write(RELAY_REG);
  Wire.write(on ? 1 : 0);
  Wire.endTransmission();
#endif
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

// AIM <pan_us> <tilt_us>: set both servo pulse widths (already calibrated µs).
static void doAim(char* args) {
  if (!args) { reply("ERR AIM missing_args"); return; }
  char* panTok = strtok(args, " ");
  char* tiltTok = strtok(nullptr, " ");
  if (!panTok || !tiltTok) { reply("ERR AIM need_pan_tilt"); return; }
  servoPulse(CH_PAN, atoi(panTok));
  servoPulse(CH_TILT, atoi(tiltTok));
  reply("OK AIM");
}

static void dispatch(char* line) {
  char* verb = strtok(line, " ");
  if (!verb) return;
  char* rest = strtok(nullptr, "");   // remainder after the verb
  if (strcmp(verb, "FIRE") == 0)      doFire(rest);
  else if (strcmp(verb, "AIM") == 0)  doAim(rest);
  else if (strcmp(verb, "PING") == 0) reply("OK PING");
  else {
    client.print("ERR ");
    client.print(verb);
    client.print(" unsupported\n");
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
  client.print("HELLO turret ");
  client.print(FW_VERSION);
  client.print('\n');

  // Await the first line: WELCOME (accepted) or BYE <reason> (rejected).
  unsigned long start = millis();
  String first = "";
  while (millis() - start < 3000) {
    while (client.available()) {
      char c = client.read();
      if (c == '\n') {
        first.trim();
        if (first == "WELCOME") return true;
        return false;             // BYE or anything else
      }
      if (c != '\r' && first.length() < 64) first += c;
    }
    delay(5);
  }
  return false;
}

void setup() {
  Serial.begin(115200);
  Wire.begin(2, 1);               // NanoC6 Grove: SDA=2, SCL=1

  servoMode(CH_PAN, 3);           // 3 = servo (pulse) mode
  servoMode(CH_TILT, 3);
  servoPulse(CH_PAN, PULSE_CENTER);
  servoPulse(CH_TILT, PULSE_CENTER);

#ifdef RELAY_GPIO
  pinMode(RELAY_PIN, OUTPUT);
#endif
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

  // Read available bytes, dispatch on each complete line.
  while (client.available()) {
    char c = client.read();
    if (c == '\n') {
      lineBuf[lineLen] = '\0';
      // strip trailing CR
      if (lineLen && lineBuf[lineLen - 1] == '\r') lineBuf[lineLen - 1] = '\0';
      if (lineLen) dispatch(lineBuf);
      lineLen = 0;
    } else if (lineLen < sizeof(lineBuf) - 1) {
      lineBuf[lineLen++] = c;
    } else {
      lineLen = 0;                // overflow: drop the runaway line
    }
  }
}
