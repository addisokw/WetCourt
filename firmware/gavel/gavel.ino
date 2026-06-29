// Wet Court — gavel firmware (servo strike)
// Board: M5Stack NanoC6 (ESP32-C6)
// Accessory: M5Stack 8-Servos board (I2C 0x25) — ch0 drives the gavel servo
//
// Owns GAVEL + PING. A beefy servo swings a gavel for verdicts and "order in the
// court." One GAVEL = one rap: REST → RAISE → STRIKE → REST.
//
// Speaks the Wet Court device line protocol (see ../../protocol/README.md):
// dials the orchestrator over TCP, identifies with `HELLO gavel`, and handles
// GAVEL / PING, replying `OK <verb>` or `ERR <verb> <reason>`.
//
// Servo positions are pulse-width MICROSECONDS, same as the turret board (the
// 8-Servos board's verified path: mode 3, REG 0x60, 2 bytes LE). Geometry is
// firmware-side here — the host sends a bare `GAVEL` with no args.
//
// ─── BEFORE FLASHING: copy secrets.example.h → secrets.h and fill in your WiFi
//     + orchestrator IP (secrets.h is gitignored), and tune the strike µs. ───

#include <Wire.h>
#include <WiFi.h>
#include "secrets.h"   // WIFI_SSID / WIFI_PASS / ORCH_HOST / ORCH_PORT (gitignored)

// ───────────────────────── CONFIG ─────────────────────────
// Network + orchestrator address live in secrets.h (gitignored) — see
// secrets.example.h. Everything below is hardware config, safe to commit.
static const char* FW_VERSION = "0.1";

// 8-Servos board (verified: I2C 0x25; STM32 sub-MCU).
static const uint8_t SERVO_ADDR      = 0x25;
static const uint8_t REG_MODE        = 0x00;     // 0x00+ch: 3 = servo (pulse) mode
static const uint8_t REG_SERVO_PULSE = 0x60;     // 0x60+ch*2: pulse width, 2 bytes LE (µs)
static const uint8_t CH_GAVEL        = 0;        // channel 0 = gavel arm

// Strike geometry (servo pulse µs). Tune to your linkage: REST is idle, RAISE is
// the wind-up, STRIKE is where the head bangs the block. On most builds the head
// swings DOWN to strike (STRIKE < REST < RAISE); flip freely if mirrored — the
// rap just drives REST → RAISE → STRIKE → REST in order.
static const int GAVEL_REST   = 1500;   // ~center
static const int GAVEL_RAISE  = 2000;   // wound up
static const int GAVEL_STRIKE = 1100;   // head down (the bang)

// Per-move dwell (ms): let the servo physically arrive before the next move.
// Increase if your servo is slow and arrives late; decrease for a snappier bang.
static const uint16_t RAISE_DWELL_MS  = 180;
static const uint16_t STRIKE_DWELL_MS = 120;
static const uint16_t SETTLE_DWELL_MS = 160;
static const uint8_t  STRIKE_RAPS     = 1;      // GAVEL = one strike (per spec)

// Absolute safety clamp on pulse width (µs) — hard backstop against a bad value.
static const int PULSE_MIN = 1000;
static const int PULSE_MAX = 2000;
// ───────────────────────────────────────────────────────────

WiFiClient client;
static char lineBuf[96];
static uint8_t lineLen = 0;
static bool servoOk = false;        // 8-Servos board present at boot?

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

// Is the 8-Servos board acking on the bus? Probed at boot.
static bool servoPresent() {
  Wire.beginTransmission(SERVO_ADDR);
  return Wire.endTransmission() == 0;
}

// One full gavel rap with the given geometry (servo µs positions + dwell ms).
// Blocking through the swing is fine — the device does one thing and the host
// waits for the ack *after* the strike completes; WiFi runs on its own RTOS
// task.
static void gavelStrike(int rest, int raise, int strike,
                        int raiseDwell, int strikeDwell, int settleDwell) {
  for (uint8_t i = 0; i < STRIKE_RAPS; i++) {
    servoPulse(CH_GAVEL, raise);
    delay(raiseDwell);
    servoPulse(CH_GAVEL, strike);
    delay(strikeDwell);
    servoPulse(CH_GAVEL, rest);
    delay(settleDwell);
  }
}

// ───────────────────────── command handlers ─────────────────────────
static void reply(const char* s) {
  client.print(s);
  client.print('\n');
}

// GAVEL [<rest> <raise> <strike> <raise_dwell> <strike_dwell> <settle_dwell>]:
// one rap, then ack. The host normally sends all six (servo µs + dwell ms) from
// gavel.toml so the firmware stays stateless; a bare GAVEL falls back to the
// compiled defaults. Guards the no-servo-board case so a verdict can't silently
// no-op while replying OK.
static void doGavel(char* args) {
  if (!servoOk) { reply("ERR GAVEL no_servo_board"); return; }
  int rest = GAVEL_REST, raise = GAVEL_RAISE, strike = GAVEL_STRIKE;
  int rd = RAISE_DWELL_MS, sd = STRIKE_DWELL_MS, td = SETTLE_DWELL_MS;
  if (args) {
    char* t;
    if ((t = strtok(args, " ")))    rest = atoi(t);
    if ((t = strtok(nullptr, " "))) raise = atoi(t);
    if ((t = strtok(nullptr, " "))) strike = atoi(t);
    if ((t = strtok(nullptr, " "))) rd = atoi(t);
    if ((t = strtok(nullptr, " "))) sd = atoi(t);
    if ((t = strtok(nullptr, " "))) td = atoi(t);
  }
  gavelStrike(rest, raise, strike, rd, sd, td);
  reply("OK GAVEL");
}

// GJOG <us>: move the servo to a raw pulse-width and hold — the console's live
// position preview while tuning. servoPulse clamps to the safe µs window.
static void doGjog(char* args) {
  if (!servoOk) { reply("ERR GJOG no_servo_board"); return; }
  if (!args)    { reply("ERR GJOG missing_us"); return; }
  servoPulse(CH_GAVEL, atoi(args));
  reply("OK GJOG");
}

static void dispatch(char* line) {
  char* verb = strtok(line, " ");
  if (!verb) return;
  char* rest = strtok(nullptr, "");   // remainder after the verb
  if (strcmp(verb, "GAVEL") == 0)     doGavel(rest);
  else if (strcmp(verb, "GJOG") == 0) doGjog(rest);
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
  client.print("HELLO gavel ");
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
  Wire.begin(2, 1);               // NanoC6 Grove: SDA=2, SCL=1

  servoOk = servoPresent();
  if (servoOk) {
    servoMode(CH_GAVEL, 3);       // 3 = servo (pulse) mode
    servoPulse(CH_GAVEL, GAVEL_REST);
  } else {
    Serial.println("8-Servos board not found at 0x25");
  }
}

void loop() {
  if (!ensureWifi()) { delay(1000); return; }

  if (!client.connected()) {
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
