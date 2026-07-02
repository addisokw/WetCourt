// Wet Court — judge-face firmware (LED-matrix face)
// Board: Adafruit Matrix Portal M4 (SAMD51 + onboard AirLift ESP32 for WiFi)
// Display: 64x32 HUB75 RGB matrix (4mm pitch), driven by Adafruit Protomatter
//
// This board is the judge's FACE only. The pan/tilt gaze that aims the head is a
// SEPARATE board (role `judge-neck`, firmware/judge-neck/) — the HUB75 panel +
// Protomatter's DMA/timer use fully occupy this micro, so the servos live
// elsewhere (mirrors the turret/squirt split).
//
// Speaks the Wet Court device line protocol (see ../../protocol/README.md):
// dials the orchestrator over TCP, identifies with `HELLO judge-face`, and
// handles PANEL / PING, replying `OK <verb>` or `ERR <verb> <reason>`.
//
//   PANEL <pattern>   pattern ∈ { idle, thinking, verdict }
//
// Networking is structured exactly like firmware/judge-neck (the turret recipe);
// only the WiFi backend differs: the Matrix Portal reaches the LAN through its
// AirLift ESP32 co-processor, so this uses the Adafruit WiFiNINA fork instead of
// the NanoC6's native <WiFi.h>. The WiFiClient API is identical.
//
// LIBRARIES (Arduino Library Manager):
//   - Adafruit Protomatter
//   - Adafruit WiFiNINA   (the Adafruit fork, for the AirLift co-processor)
//   - Adafruit GFX        (pulled in by Protomatter)
// BOARD: select "Adafruit Matrix Portal M4" — its variant defines the AirLift
//   pins (SPIWIFI_SS / SPIWIFI_ACK / ESP32_RESETN / ESP32_GPIO0 / SPIWIFI).
//
// ─── BEFORE FLASHING: copy secrets.example.h → secrets.h and fill in your WiFi
//     + orchestrator IP (secrets.h is gitignored). ───

#include <SPI.h>
#include <WiFiNINA.h>              // Adafruit fork → AirLift ESP32 co-processor
#include <Adafruit_Protomatter.h>
#include "secrets.h"              // WIFI_SSID / WIFI_PASS / ORCH_HOST / ORCH_PORT (gitignored)

// ───────────────────────── CONFIG ─────────────────────────
static const char* FW_VERSION = "0.1";

// HUB75 panel wiring for the Matrix Portal M4 (canonical Protomatter pin map).
// A 64x32 panel is 1/16-scan, so 4 address lines (A,B,C,D); a 64x64 panel would
// add the E line on pin 21.
static uint8_t rgbPins[]  = {7, 8, 9, 10, 11, 12};
static uint8_t addrPins[] = {17, 18, 19, 20};
static const uint8_t clockPin = 14;
static const uint8_t latchPin = 15;
static const uint8_t oePin    = 16;

// matrix(width, bitDepth, #chains, rgbPins, #addrPins, addrPins, clk, lat, oe, dblbuf)
// Height is implied: 2^(#addrPins) * 2 = 32. Double-buffered for tear-free anim.
Adafruit_Protomatter matrix(64, 4, 1, rgbPins, 4, addrPins,
                            clockPin, latchPin, oePin, true);
// ───────────────────────────────────────────────────────────

WiFiClient client;
static char lineBuf[96];
static uint8_t lineLen = 0;

// Face animation state (set by PANEL, rendered non-blocking in loop()).
enum Pattern { PAT_IDLE, PAT_THINKING, PAT_VERDICT };
static Pattern pattern = PAT_IDLE;
static unsigned long lastFrame = 0;

// ───────────────────────── face rendering ─────────────────────────
static uint16_t C(uint8_t r, uint8_t g, uint8_t b) { return matrix.color565(r, g, b); }

// Two eyes + a mouth. `blink` closes the eyes to a thin line; `eye` tints them.
static void drawFace(uint16_t eye, bool blink, uint16_t mouth) {
  if (blink) {
    matrix.fillRect(14, 15, 12, 2, eye);   // left eye, shut
    matrix.fillRect(38, 15, 12, 2, eye);   // right eye, shut
  } else {
    matrix.fillRect(14, 9, 12, 9, eye);    // left eye
    matrix.fillRect(38, 9, 12, 9, eye);    // right eye
    matrix.fillRect(18, 12, 4, 4, C(0, 0, 0));  // left pupil
    matrix.fillRect(42, 12, 4, 4, C(0, 0, 0));  // right pupil
  }
  matrix.drawFastHLine(22, 25, 20, mouth); // mouth
}

// Render one frame for the current pattern. `now` = millis().
static void renderFace(unsigned long now) {
  matrix.fillScreen(C(0, 0, 0));
  switch (pattern) {
    case PAT_IDLE: {
      // Calm: a slow blink roughly every 3 s.
      bool blink = (now % 3000) < 150;
      drawFace(C(0, 40, 70), blink, C(0, 40, 70));
      break;
    }
    case PAT_THINKING: {
      // Eyes steady; three dots march along the bottom.
      drawFace(C(60, 50, 0), false, C(40, 35, 0));
      int n = (now / 300) % 3;
      for (int i = 0; i <= n; i++) matrix.fillRect(26 + i * 5, 28, 3, 3, C(120, 100, 0));
      break;
    }
    case PAT_VERDICT: {
      // Stern pulse.
      uint8_t p = 40 + (uint8_t)(40.0 * (1.0 + sin(now / 150.0)));
      drawFace(C(p, 0, 0), false, C(p, 0, 0));
      break;
    }
  }
  matrix.show();
}

// ───────────────────────── command handlers ─────────────────────────
static void reply(const char* s) {
  client.print(s);
  client.print('\n');
}

// PANEL <pattern>: switch the face animation.
static void doPanel(char* args) {
  if (!args) { reply("ERR PANEL missing_args"); return; }
  char* tok = strtok(args, " ");
  if (!tok)                         { reply("ERR PANEL missing_args"); return; }
  if      (strcmp(tok, "idle") == 0)     pattern = PAT_IDLE;
  else if (strcmp(tok, "thinking") == 0) pattern = PAT_THINKING;
  else if (strcmp(tok, "verdict") == 0)  pattern = PAT_VERDICT;
  else { reply("ERR PANEL unknown_pattern"); return; }
  reply("OK PANEL");
}

static void dispatch(char* line) {
  char* verb = strtok(line, " ");
  if (!verb) return;
  char* rest = strtok(nullptr, "");   // remainder after the verb
  if (strcmp(verb, "PANEL") == 0)     doPanel(rest);
  else if (strcmp(verb, "PING") == 0) reply("OK PING");
  else {
    client.print("ERR ");
    client.print(verb);
    client.print(" unsupported\n");   // AIM lives on the judge-neck board
  }
}

// ───────────────────────── connection ─────────────────────────
static bool ensureWifi() {
  if (WiFi.status() == WL_CONNECTED) return true;
  WiFi.begin(WIFI_SSID, WIFI_PASS);
  unsigned long start = millis();
  // Non-blocking-ish: keep the face animating while the link comes up.
  while (WiFi.status() != WL_CONNECTED && millis() - start < 15000) {
    renderFace(millis());
    delay(50);
  }
  return WiFi.status() == WL_CONNECTED;
}

// Dial the orchestrator and complete the HELLO handshake. Returns true if the
// host replied WELCOME.
static bool connectOrchestrator() {
  if (!client.connect(ORCH_HOST, ORCH_PORT)) return false;
  client.setNoDelay(true);
  client.print("HELLO judge-face ");
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

  ProtomatterStatus st = matrix.begin();
  if (st != PROTOMATTER_OK) {
    Serial.print("Protomatter init failed: ");
    Serial.println((int)st);
    // Without the panel there's nothing to show; halt so the failure is obvious.
    while (true) delay(1000);
  }
  renderFace(millis());            // show the idle face immediately

  // Point WiFiNINA at the Matrix Portal's onboard AirLift ESP32. These constants
  // come from the "Adafruit Matrix Portal M4" board variant.
  WiFi.setPins(SPIWIFI_SS, SPIWIFI_ACK, ESP32_RESETN, ESP32_GPIO0, &SPIWIFI);
  if (WiFi.status() == WL_NO_MODULE) {
    Serial.println("AirLift ESP32 not found — check board selection / firmware");
  }
}

void loop() {
  if (!ensureWifi()) { renderFace(millis()); delay(200); return; }

  if (!client.connected()) {
    if (!connectOrchestrator()) {
      client.stop();
      renderFace(millis());
      delay(500);                  // backoff before retry (face keeps animating)
      return;
    }
    lineLen = 0;
    Serial.println("connected to orchestrator");
  }

  // Service the socket without blocking — dispatch each complete line.
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

  // Render at ~30 fps between socket service passes.
  unsigned long now = millis();
  if (now - lastFrame >= 33) {
    lastFrame = now;
    renderFace(now);
  }
}
