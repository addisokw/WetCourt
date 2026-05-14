# The Wet Court of Appeals — Software Architecture

A design document for an interactive courtroom exhibit booth. An LLM acts as judge, defendants plead their case, and the verdict is rendered theatrically — including, on guilty findings, a controlled burst from a computer-aimed squirt gun.

This document captures architectural decisions and is intended to inform an implementation session. It's prescriptive about boundaries and protocols; less prescriptive about implementation details inside each component.

---

## 1. Overview

### User experience flow

1. **Idle**: Booth runs an attractor mode on the display.
2. **Operator triggers** a new trial (button press / keyboard).
3. **Charge generation**: LLM produces an absurd charge against the next visitor.
4. **Charge displayed** on the big screen and read aloud via TTS.
5. **Plea window**: Visitor has ~20 seconds to plead their case into a microphone (push-to-talk).
6. **Transcription**: Audio → text via local STT.
7. **Deliberation**: LLM evaluates the plea in courtroom persona, streaming its reasoning to the screen.
8. **Verdict**: Structured output (guilty/not guilty + intensity + remarks).
9. **Sentence executed**: Gavel bangs. If guilty, squirt fires at the splash zone. If not guilty, celebration cue.
10. **Cooldown** → back to Idle.

### Design principles

- **Deterministic state machine drives everything.** The LLM is consulted at well-defined points; it never controls flow or hardware directly.
- **Every LLM call has a fallback.** Network down, model malformed output, timeout — the trial completes regardless.
- **Hardware actions are timing-critical and isolated.** A microcontroller owns the squirt valve, gavel servo, and lights. The Mac/Linux host never directly drives a solenoid.
- **The browser is part of the architecture.** It handles display, mic capture, and audio playback. This sidesteps native audio I/O integration in the backend.
- **Single deployable artifact preferred.** Rust static binary + ML model files + microcontroller firmware + frontend assets (embedded).

---

## 2. System Architecture

```
┌───────────────────────────────────────────────────────────────────┐
│                            HOST MACHINE                            │
│                                                                    │
│   ┌──────────────────────────────────────────────────┐            │
│   │             Rust Orchestrator (single binary)     │            │
│   │                                                   │            │
│   │   ┌─────────────┐    State Machine               │            │
│   │   │   Tokio     │      ▲    ▲    ▲               │            │
│   │   │   runtime   │      │    │    │               │            │
│   │   └─────────────┘      │    │    │               │            │
│   │                        │    │    │               │            │
│   │   ┌─────┬───────┬──────┴────┴────┴──────┬─────┐  │            │
│   │   │ STT │  TTS  │  LLM      Display     │ HW  │  │            │
│   │   │ task│  task │  task     server      │ task│  │            │
│   │   └──┬──┴───┬───┴────┬─────────┬────────┴──┬──┘  │            │
│   │      │      │        │         │           │     │            │
│   └──────┼──────┼────────┼─────────┼───────────┼─────┘            │
│          │      │        │         │           │                  │
│          │      │        │         │           │                  │
│          │      │        ▼         ▼           ▼                  │
│          │      │     ┌───────┐  ┌───────┐  ┌───────┐             │
│          │      │     │Ollama │  │Browser│  │Serial │             │
│          │      │     │daemon │  │kiosk  │  │ port  │             │
│          │      │     └───────┘  └───┬───┘  └───┬───┘             │
│          │      │                    │          │                 │
│          │ ┌────┘                    │          │                 │
│          │ │ Audio (PCM bytes        │          │                 │
│          │ │ via WebSocket binary)   │          │                 │
│          │ │                         │          │                 │
│   (in-process model inference)       │          │                 │
└──────────────────────────────────────┼──────────┼─────────────────┘
                                       │          │
                                       ▼          ▼
                                 ┌───────────┐ ┌──────────────┐
                                 │  Big LED  │ │Microcontroller│
                                 │  display  │ │ (ESP32/Arduino)│
                                 │ + speaker │ │      USB      │
                                 │  + mic    │ └──────┬───────┘
                                 │           │        │
                                 └───────────┘        ▼
                                                ┌───────────────┐
                                                │ Squirt valve  │
                                                │ Gavel servo   │
                                                │ Splash lights │
                                                │ Eye gimbal    │
                                                │ Thermal       │
                                                │ printer, etc. │
                                                └───────────────┘
```

### Process inventory

| Process | Role | Lifecycle |
|---|---|---|
| Rust orchestrator | State machine, ML inference, comms | Started by launch script |
| Ollama daemon | LLM inference | Started by launch script (or already running) |
| Browser (Chrome kiosk) | Display, mic, speakers | Started by launch script with `--kiosk --app=http://localhost:8080` |
| Microcontroller | Realtime hardware control | Always-on once powered |

Launch script (`run.sh`) starts these in order: Ollama → Rust binary → Chrome kiosk.

---

## 3. State Machine

### States

```rust
enum State {
    Idle,
    GeneratingCharge { started_at: Instant },
    DisplayingCharge { charge: String, until: Instant },
    AwaitingPlea { deadline: Instant },
    Transcribing { audio: Vec<f32>, started_at: Instant },
    Deliberating { plea: String, started_at: Instant },
    PronouncingVerdict { verdict: Verdict, audio_done: bool },
    ExecutingSentence { verdict: Verdict, hardware_done: bool },
    Cooldown { until: Instant },
    Error { message: String, until: Instant },
}
```

### Events

```rust
enum Event {
    // Operator inputs
    OperatorStart,
    OperatorEmergencyStop,

    // Subsystem completions
    ChargeReady(String),
    ChargeFailed(String),
    PleaAudioReceived(Vec<f32>),
    PleaTimeout,
    TranscriptReady(String),
    TranscriptFailed(String),
    VerdictReady(Verdict),
    VerdictFailed(String),
    TtsFinished,
    HardwareAck(String),
    HardwareError(String),

    // Time
    Tick,  // ~100ms heartbeat for timeout checks
}
```

### Transitions and timeouts

| From | Event | To | Notes |
|---|---|---|---|
| Idle | OperatorStart | GeneratingCharge | Begin LLM call |
| GeneratingCharge | ChargeReady | DisplayingCharge | TTS the charge, hold for ~5s |
| GeneratingCharge | ChargeFailed / timeout (10s) | DisplayingCharge | Use canned charge from list |
| DisplayingCharge | timeout reached | AwaitingPlea | Open mic, start 20s timer |
| AwaitingPlea | PleaAudioReceived | Transcribing | User pressed PTT and finished |
| AwaitingPlea | PleaTimeout | Transcribing | Use whatever was captured |
| Transcribing | TranscriptReady | Deliberating | Begin verdict LLM call |
| Transcribing | TranscriptFailed / timeout (5s) | Deliberating | Plea = "[no defense offered]" |
| Deliberating | VerdictReady | PronouncingVerdict | TTS verdict, gavel cue |
| Deliberating | VerdictFailed / timeout (15s) | PronouncingVerdict | Use coin-flip canned verdict |
| PronouncingVerdict | TtsFinished | ExecutingSentence | Fire hardware |
| ExecutingSentence | HardwareAck | Cooldown | 3s pause |
| ExecutingSentence | HardwareError / timeout (3s) | Cooldown | Show went on |
| Cooldown | timeout reached | Idle | Ready for next |
| any | OperatorEmergencyStop | Idle | Cancel everything |

### Implementation pattern

The state machine runs in its own task. A central `Tick` event fires from a `tokio::time::interval` every 100ms and drives all timeout checks centrally — much simpler than per-state timer tasks. The run loop is a `match (current_state, event)` that returns `(next_state, Vec<Command>)`. Commands are sent over channels to subsystem tasks.

---

## 4. Components

### 4.1 Rust Orchestrator

#### Module structure (suggested)

```
src/
├── main.rs              // bootstrap: config, tasks, channels
├── config.rs            // figment-based config loading
├── state_machine/
│   ├── mod.rs           // run loop
│   ├── states.rs        // State enum
│   ├── events.rs        // Event enum
│   └── transitions.rs   // (state, event) -> (state, commands)
├── llm/
│   ├── mod.rs           // task wrapper
│   ├── client.rs        // Ollama HTTP client
│   ├── charge.rs        // charge-generation prompt + schema
│   └── verdict.rs       // verdict prompt + schema
├── stt/
│   └── mod.rs           // whisper-rs integration
├── tts/
│   └── mod.rs           // ort + Kokoro ONNX
├── hardware/
│   ├── mod.rs           // task wrapper
│   └── protocol.rs      // serial command/response types
├── display/
│   ├── mod.rs           // axum WebSocket server
│   ├── events.rs        // typed events to frontend
│   └── assets.rs        // rust-embed for static files
├── operator.rs          // PTT button, e-stop, keyboard
└── fallbacks/
    ├── mod.rs
    ├── charges.rs       // canned charge list
    └── verdicts.rs      // canned verdict text
```

#### Cargo.toml dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
axum = { version = "0.8", features = ["ws"] }
tower-http = { version = "0.6", features = ["fs"] }
reqwest = { version = "0.12", features = ["stream", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
tokio-serial = "5"
whisper-rs = "0.13"
ort = "2"
ndarray = "0.16"
rust-embed = "8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
figment = { version = "0.10", features = ["toml", "env"] }
tokio-stream = "0.1"
futures-util = "0.3"
anyhow = "1"
thiserror = "1"
```

#### Concurrency model

One Tokio task per subsystem. Communication via `tokio::sync::mpsc` channels:

- `Sender<Event>` shared across all subsystems → state machine
- `Sender<Command>` per subsystem, owned by state machine

Subsystems are dumb: they receive a command, do the thing, emit completion events. The state machine is the only place that knows the trial flow.

### 4.2 Browser Frontend

#### Responsibilities

- Render attractor mode, charge display, plea timer, deliberation stream, verdict screen, cooldown
- Capture microphone audio when the backend says to
- Play TTS audio when the backend sends it
- Play sound cues (gavel, organ stings, etc.) — assets shipped with the frontend

#### Stack

Vanilla TypeScript or a small framework — Svelte, Solid, or Preact all work. Keep it simple; this is mostly text rendering and audio handling. Avoid SPA frameworks with heavy build tooling unless you're already comfortable with one.

Bundled into static files served by the Rust binary via `rust-embed`. Connect to `ws://localhost:8080/ws` on load.

#### Audio capture

```typescript
// On 'start_plea_recording' event:
const stream = await navigator.mediaDevices.getUserMedia({ audio: {
  channelCount: 1,
  sampleRate: 16000,
  echoCancellation: true,
  noiseSuppression: true,
}});
const recorder = new MediaRecorder(stream, { mimeType: 'audio/webm;codecs=opus' });
// or use AudioWorklet for raw PCM streaming if latency matters
```

Send audio over the WebSocket as binary frames. The backend converts to f32 PCM at 16kHz (Whisper's expected format).

#### Audio playback

TTS audio arrives as a binary blob (WAV or raw PCM). Decode via `AudioContext.decodeAudioData` and play via `BufferSource`. Notify backend with `{ type: 'tts_finished' }` on `onended`.

### 4.3 Ollama (LLM)

Runs as a separate daemon. The orchestrator talks to it via HTTP at `http://localhost:11434`.

#### Model selection (configurable)

Defaults by host RAM:
- 24GB: `qwen3:8b` or `qwen3:14b` at Q4_K_M
- 32GB: `qwen3.6:35b-a3b` (MoE) — sweet spot
- 48GB+: `qwen3.6:27b` dense at Q5/Q6

Model name read from config. Both calls (charge + verdict) use the same model.

#### API usage

Use the `/api/chat` endpoint with:
- `format` set to a JSON schema for structured output
- `stream: true` for the verdict deliberation (so frontend can render tokens live)
- `stream: false` for charge generation (just want the result)

### 4.4 Microcontroller

#### Hardware platform

ESP32 or Arduino-compatible board. ESP32 preferred — extra GPIO, faster, cheap. Communicates with host over USB-serial (CDC) at 115200 baud.

#### Responsibilities

| Channel | Hardware |
|---|---|
| Squirt valve | 12V solenoid via MOSFET, flyback diode |
| Gavel servo | Standard servo, ~180° travel |
| Splash zone lights | WS2812 strip, ~10 LEDs |
| HAL eye gimbal | 2× small servos (pan/tilt) |
| Status panel LEDs | WS2812 strip, ~10 LEDs |
| Thermal printer | Adafruit-style, separate UART |
| Thinking-reel motor | Small geared motor on N-channel MOSFET |
| E-stop button | Input pin with pull-up |
| Bell solenoid | Optional, on MOSFET |
| Confetti popper / fog | Optional, on MOSFET |

Firmware is intentionally simple: parse line-based serial commands, execute, ack. No state of its own beyond "is squirt currently firing."

---

## 5. Communication Protocols

### 5.1 WebSocket Protocol (Backend ↔ Browser)

URL: `ws://localhost:8080/ws`. JSON for control messages, binary frames for audio.

#### Backend → Frontend (DisplayEvent)

```typescript
type DisplayEvent =
  | { type: 'reset' }
  | { type: 'idle' }
  | { type: 'show_charge'; text: string }
  | { type: 'tts_audio'; format: 'wav' | 'pcm_f32_24000' }  // followed by binary frame
  | { type: 'start_plea_recording'; deadline_ms: number }
  | { type: 'stop_plea_recording' }
  | { type: 'transcribing' }
  | { type: 'transcript_ready'; text: string }
  | { type: 'deliberation_token'; text: string }
  | { type: 'deliberation_complete' }
  | { type: 'verdict'; guilty: boolean; intensity: number; remarks: string }
  | { type: 'execute_sentence'; guilty: boolean }
  | { type: 'play_cue'; name: 'gavel' | 'organ_guilty' | 'choir_acquittal' | 'court_session' }
  | { type: 'cooldown' }
  | { type: 'error'; message: string };
```

#### Frontend → Backend (ClientEvent)

```typescript
type ClientEvent =
  | { type: 'ready' }                     // sent on WS connect
  | { type: 'plea_audio_chunk' }          // followed by binary frame of PCM
  | { type: 'plea_audio_complete' }
  | { type: 'tts_finished' }
  | { type: 'cue_finished'; name: string };
```

#### Connection lifecycle

- Frontend opens WS on page load.
- Backend sends `idle` on connect to sync state.
- If WS drops, frontend retries with exponential backoff. Backend tolerates frontend disconnects — trials in progress complete with what hardware they have, frontend resyncs on reconnect.

### 5.2 Serial Protocol (Backend ↔ Microcontroller)

Line-based ASCII, `\n` terminated. 115200 baud.

#### Commands (Host → MCU)

```
FIRE <duration_ms>                  Fire squirt valve for N ms (e.g. FIRE 150)
GAVEL                                Bang gavel once
LIGHTS <state>                       splash_idle | splash_arming | guilty | not_guilty
EYE <pan_deg> <tilt_deg>             Move HAL eye gimbal
PRINT <line>                         Print one line on thermal printer
PRINTCUT                             Cut/feed receipt
REELS <on|off>                       Thinking-reel motor
PANEL <pattern>                      Status LED panel pattern: idle|thinking|verdict
BELL                                 Strike the bell once
CONFETTI                             Fire confetti popper
PING                                 Health check
```

#### Responses (MCU → Host)

```
OK <command>                         Action complete
ERR <command> <reason>               Action failed
ESTOP                                E-stop button pressed
PONG                                 Response to PING
```

The host should:
- Send `PING` on startup, fail loudly if no `PONG` within 2s.
- Treat `ESTOP` as an `OperatorEmergencyStop` event regardless of state.
- Time out commands at 3s and treat as `HardwareError`.

### 5.3 Ollama Prompts and Schemas

#### Charge generation

System prompt:
```
You are an absurdist court issuing comedic charges against visitors at an
interactive art exhibit. Generate ONE brief charge (1-2 sentences max) that
is absurd but harmless. Acceptable themes: technical pedantry, internet
culture, everyday foibles, anachronistic crimes against good taste,
violations of unspoken social norms.

Forbidden: real people, politics, religion, anything sexual, anything that
targets protected groups, anything genuinely accusatory. Keep it silly.

Examples:
- "You stand accused of pronouncing 'gif' with a hard G."
- "You are charged with reply-all on a company-wide email."
- "You stand accused of leaving a single dish in the sink for over 72 hours."
```

User message: `"Generate the next charge."`

Output schema (passed via `format` parameter):
```json
{
  "type": "object",
  "properties": {
    "charge": { "type": "string", "minLength": 10, "maxLength": 200 }
  },
  "required": ["charge"]
}
```

Timeout: 10s. On failure or timeout, pick a random charge from `fallbacks/charges.rs` (ship 30+ pre-written).

#### Verdict

System prompt:
```
You are the AI judge of the Wet Court of Appeals. You preside in the manner
of a pompous British barrister: formal, ornate, faintly disdainful, with
a taste for legal Latin used incorrectly. You take obvious comedic delight
in the proceedings while maintaining the appearance of grave seriousness.

You will be given a charge and the defendant's plea. Evaluate the plea on
its rhetorical merits, not on whether the charge is actually justified
(it never is). Render a verdict.

Issue a guilty verdict the majority of the time — the squirt is the joke.
Acquit only when the plea is genuinely clever, witty, or charming. If the
defendant offered no defense, find them guilty.

Output JSON only, no preamble.
```

User message:
```
Charge: {charge}
Defendant's plea: {plea_transcript}

Render your verdict.
```

Output schema:
```json
{
  "type": "object",
  "properties": {
    "deliberation": {
      "type": "string",
      "description": "1-3 sentences in pompous British legal voice, weighing the plea",
      "maxLength": 500
    },
    "verdict": { "enum": ["guilty", "not_guilty"] },
    "intensity": {
      "type": "integer",
      "minimum": 1,
      "maximum": 5,
      "description": "Squirt intensity if guilty; ignored if acquitted. 1=light spritz, 5=full blast"
    },
    "judges_remarks": {
      "type": "string",
      "description": "Brief closing line, ornate and witty",
      "maxLength": 200
    }
  },
  "required": ["deliberation", "verdict", "intensity", "judges_remarks"]
}
```

Stream the response. As tokens arrive, emit `deliberation_token` events to the frontend. When complete, parse the full JSON and emit `verdict` event.

Timeout: 15s for first token, 30s total. On failure: random verdict (70% guilty), canned remarks.

#### Intensity → squirt duration mapping

```
intensity 1 → FIRE 60   (light spritz)
intensity 2 → FIRE 100
intensity 3 → FIRE 150  (default)
intensity 4 → FIRE 200
intensity 5 → FIRE 280  (full blast)
```

These are tuning parameters in config.

---

## 6. Subsystem Implementation Notes

### 6.1 STT (whisper-rs)

- Model: `whisper-large-v3-turbo` GGML, quantized to Q5_0. ~1.6GB.
- Initialize once on startup, reuse across trials.
- Input: f32 PCM at 16kHz, mono. Frontend captures at 16kHz directly to avoid resampling.
- Run inference on a `tokio::task::spawn_blocking` since whisper-rs is sync.
- Whisper-rs handles Metal acceleration on Mac and CUDA on Linux automatically.

```rust
let ctx = WhisperContext::new_with_params(model_path, params)?;
let mut state = ctx.create_state()?;
state.full(params, &audio_pcm)?;
let n_segments = state.full_n_segments();
let text: String = (0..n_segments)
    .filter_map(|i| state.full_get_segment_text(i).ok())
    .collect::<Vec<_>>()
    .join(" ");
```

### 6.2 TTS (Kokoro via ort)

- Source the Kokoro ONNX model from one of the community ports on HuggingFace (e.g., `onnx-community/Kokoro-82M-ONNX`).
- Load via `ort::Session::builder()`. On Mac use CoreML EP; on Linux use CUDA EP if available, else CPU.
- Voice: select a clean, formal English voice (`af_bella`, `am_adam`, etc.). Voices are encoded as preloaded f32 reference embeddings.
- Phoneme conversion: Kokoro expects phoneme tokens, not raw text. Use `espeak-ng` (shell out, or `espeakng-sys`) to convert text → phonemes → token IDs.
- Output: f32 PCM at 24kHz. Send to frontend as binary frame after a JSON `tts_audio` event.

If Kokoro integration becomes a blocker, **fallback to a Python sidecar** running the reference Kokoro implementation, talking over a Unix socket. This is acceptable — keeps Rust as orchestrator.

### 6.3 Display Server

`axum` with `axum::extract::ws::WebSocketUpgrade`. Single `/ws` endpoint, single connected client at a time (extra connections rejected).

Static frontend assets bundled via `rust-embed` and served from `/`. Use `tower-http`'s `ServeDir` or a custom embedded handler.

The display task owns the WebSocket. It receives `DisplayCommand`s from the state machine and forwards as JSON. It receives client messages, parses, and forwards as `Event`s.

Audio frames are forwarded raw (binary) — no JSON wrapping for the bytes themselves. Header event tells the frontend what's coming next.

### 6.4 Hardware Task

Owns the serial port, full-duplex. Two halves:

- Writer: receives commands from state machine, writes line, awaits `OK`/`ERR` with timeout.
- Reader: continuously reads lines, parses `ESTOP` and unsolicited messages, forwards as events.

On startup, send `PING`, fail loudly if no `PONG`. Reconnect logic if serial port drops (USB unplugged): retry every 2s and notify state machine.

### 6.5 Operator Inputs

Three inputs:
- **Start trial**: arcade button (wired to MCU GPIO, sent as a serial event), or keyboard shortcut for testing.
- **Emergency stop**: physical button (MCU sends `ESTOP`), also keyboard shortcut.
- **Skip / reset**: keyboard shortcut for the operator to abort a wedged trial.

---

## 7. Reliability and Fallbacks

### Per-call fallbacks

| Failure | Fallback |
|---|---|
| Ollama unreachable | Use canned charges and canned verdicts (random, weighted guilty) |
| LLM returns malformed JSON | Constrained generation via `format` schema should prevent this; if it still happens, retry once, then canned |
| LLM timeout | Canned response, log it |
| STT returns empty | Treat as "no defense offered," proceed to guilty |
| STT fails | Same |
| TTS fails | Show text on screen only, skip audio |
| Hardware unresponsive | Show "verdict rendered" text on screen, skip physical sentence |
| Microcontroller disconnects | Soft-degrade: show continues without hardware effects, log it |
| WebSocket drops | Trial completes server-side, frontend reconnects to idle |

### Logging

`tracing` with structured fields. Log every:
- State transition with reason
- LLM call (prompt length, latency, success, content)
- Transcript
- Verdict
- Hardware command and ack/error

Sink to stdout (for dev) and a rolling file (for post-event review). Save full transcripts in JSONL — you'll want the funniest verdicts later.

### Operator monitoring

Small operator HUD on a separate browser page (or a terminal `tracing` sink) showing current state, last transcript, last verdict, hardware health. Useful for debugging during the event.

---

## 8. Configuration

`config.toml` next to the binary. Loaded via `figment` with env var overrides.

```toml
[ollama]
base_url = "http://localhost:11434"
model = "qwen3.6:35b-a3b"
charge_timeout_secs = 10
verdict_timeout_secs = 30

[stt]
model_path = "models/ggml-large-v3-turbo-q5_0.bin"
language = "en"

[tts]
model_path = "models/kokoro-v1.onnx"
voice_path = "models/voices/am_adam.bin"
sample_rate = 24000

[hardware]
serial_port = "/dev/cu.usbserial-XXXX"  # or COM3 on Windows, /dev/ttyUSB0 on Linux
baud = 115200
ack_timeout_ms = 3000

[squirt_intensity]
level_1 = 60
level_2 = 100
level_3 = 150
level_4 = 200
level_5 = 280

[trial]
plea_window_secs = 20
charge_display_secs = 5
cooldown_secs = 4
guilty_bias = 0.7  # for fallback verdicts

[display]
listen_addr = "127.0.0.1:8080"

[logging]
level = "info"
log_file = "logs/booth.log"
transcripts_jsonl = "logs/transcripts.jsonl"
```

---

## 9. Build and Deployment

### Build targets

Cross-compile from a dev machine:

```bash
# Mac (native)
cargo build --release

# Linux x86_64
cross build --release --target x86_64-unknown-linux-gnu

# Linux ARM64 (Pi 5, Jetson, Orin)
cross build --release --target aarch64-unknown-linux-gnu
```

Native deps (whisper.cpp, ONNX Runtime) pull in C/C++ libs — `cross` handles toolchains. CI matrix builds all targets on push.

### Distribution layout

```
booth/
├── booth                       (or booth.exe)
├── config.toml
├── models/
│   ├── ggml-large-v3-turbo-q5_0.bin
│   ├── kokoro-v1.onnx
│   └── voices/
│       └── am_adam.bin
├── logs/
└── run.sh
```

`run.sh`:
```bash
#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"

# Start Ollama if not running
if ! pgrep -x ollama > /dev/null; then
  ollama serve &
  sleep 2
fi

# Ensure model is pulled
ollama pull qwen3.6:35b-a3b || true

# Start booth
./booth &
BOOTH_PID=$!

# Wait for backend to come up
sleep 2

# Open kiosk
open -a "Google Chrome" --args --kiosk --app=http://localhost:8080
# (on Linux: google-chrome --kiosk --app=http://localhost:8080 &)

wait $BOOTH_PID
```

---

## 10. Implementation Roadmap

Suggested phasing — each phase produces a working artifact you can demo.

### Phase 1: Skeleton with mocks

- Set up Cargo project, modules, basic `tracing` setup.
- Implement state machine with all states and transitions, but every subsystem is mocked (returns hardcoded responses immediately).
- Implement axum WebSocket server with the full event protocol.
- Build minimal frontend that connects, logs events, and shows current state.
- Verify: a full trial cycles end-to-end with mocked subsystems. Operator hits "start", display shows charge, simulated 20s plea, mock transcript, mock verdict, mock hardware ack, return to idle.

### Phase 2: Real LLM

- Implement Ollama HTTP client with structured output and streaming.
- Implement charge and verdict prompts.
- Wire fallbacks (canned charges/verdicts).
- Verify: real charges generated, real verdicts rendered. Pull network cable mid-trial → fallback fires.

### Phase 3: Real audio

- Implement frontend mic capture and audio playback.
- Implement STT subsystem (whisper-rs).
- Implement TTS subsystem (Kokoro via ort, or Python sidecar fallback).
- Verify: full audio loop works. Speak a plea, get a transcribed plea, hear the verdict spoken.

### Phase 4: Real hardware

- Build microcontroller firmware implementing the serial protocol.
- Implement Rust hardware task with ack/timeout/reconnect.
- Wire up squirt + gavel + lights at minimum.
- Verify: end-to-end live trial with real water firing.

### Phase 5: Theater

- Frontend visual polish: typography, animations, deliberation streaming, verdict reveal.
- Sound cues bundled with frontend.
- Additional hardware props (eye gimbal, thermal printer, status panel, etc.) added incrementally.
- Operator HUD.

### Phase 6: Hardening

- 50-trial soak test on the actual booth. Random/adversarial inputs (silence, screaming, multilingual pleas, disconnect cables mid-trial). Fix every wedge until none remain.
- Cross-compile to all target platforms.
- Production logging and transcript archiving.

---

## 11. Open Questions / Decisions Deferred

- **TTS path**: pure Rust via `ort` + Kokoro ONNX, or Python sidecar. Decide after spiking the ONNX path; sidecar is the safety net.
- **Frontend framework**: vanilla TS vs. Svelte vs. Solid. Pick based on implementer comfort. Has no architectural impact.
- **PTT button location**: on the lectern (via MCU GPIO) vs. operator-side. Currently planned as defendant-facing on the lectern, illuminated arcade button.
- **Voice selection for Kokoro**: needs A/B testing for the pompous-British-barrister character. May need ElevenLabs-quality voice; if Kokoro can't carry the bit, swap to a different local TTS or use ElevenLabs API at the cost of internet dependency.
- **Hardware platform**: ESP32 default, but if a specific shield/HAT exists for the thermal printer or LED panels, that may dictate Arduino-flavor choice.
