# The Wet Court of Appeals — Software Architecture

A design document for an interactive courtroom exhibit booth. An LLM acts as judge, defendants plead their case, and the verdict is rendered theatrically — including, on guilty findings, a controlled burst from a computer-aimed squirt gun.

This document captures architectural decisions and is intended to inform an implementation session. It's prescriptive about boundaries and protocols; less prescriptive about implementation details inside each component.

---

## 1. Overview

### User experience flow

1. **Idle**: Booth runs an attractor mode on the display.
2. **Operator triggers** a new trial (button press / keyboard).
3. **Charge selection**: an absurd charge is drawn from the curated list in
   `orchestrator/crimes/wet_court_crimes.json` — operator-queued charges first,
   then a random draw honoring an optional category filter. (Originally
   LLM-generated on the fly; `[crimes] source = "llm"` restores that.)
4. **Charge displayed** on the big screen and read aloud via TTS.
5. **Plea window**: Visitor has ~20 seconds to plead their case into a microphone (push-to-talk).
6. **Transcription**: Audio → text via local STT.
7. **Deliberation**: LLM evaluates the plea in courtroom persona, streaming its reasoning to the screen.
8. **Verdict**: Structured output (guilty/not guilty + intensity + remarks).
9. **Sentence executed**: Gavel bangs. If guilty, squirt fires at the splash zone. If not guilty, celebration cue.
10. **Cooldown** → back to Idle.

### Design principles

- **Deterministic state machine drives everything.** The LLM is consulted at well-defined points; it never controls flow or hardware directly.
- **Every LLM/STT/TTS call has a fallback.** Network down, model malformed output, timeout — the trial completes regardless.
- **Hardware actions are timing-critical and isolated.** A microcontroller owns the squirt valve, gavel servo, and lights. The host never directly drives a solenoid.
- **The browser is part of the architecture.** It handles display, mic capture, and audio playback. This sidesteps native audio I/O integration in the backend.
- **Inference is a service, not a library.** STT, LLM, and TTS run as containerized services behind a single OpenAI-compatible endpoint (LiteLLM). The orchestrator is just an HTTP client. This trades the "single static binary" ideal for a clean separation that matches what's already running on the DGX Spark.
- **The orchestrator's location is a config knob, not an architectural assumption.** Production: orchestrator runs as a fifth container on the Spark, talking to LiteLLM over the private docker network. Dev: orchestrator runs on the developer's laptop with `cargo run`, talking to the Spark's LiteLLM over LAN at `http://dgx-spark.local:4000`. Same binary, same code path, different `inference.base_url`. The microcontroller and kiosk follow the orchestrator (whichever machine has the USB cable plugged in is the one running the orchestrator).

---

## 2. System Architecture

The booth is built around a **DGX Spark** (Grace Blackwell, 121 GiB unified memory, Ubuntu 24.04 aarch64). The Spark is a small desktop-form-factor machine and lives at the booth itself. It runs the entire software stack as a single docker-compose deployment, has the microcontroller plugged in via USB, and drives the kiosk display directly.

```
┌──────────────────────────────────────────────────────────────────────────┐
│                    DGX SPARK (booth host) — docker compose               │
│                                                                          │
│   ┌────────────────────────────┐   ┌──────────────────────────────────┐ │
│   │  orchestrator (Rust)       │   │  litellm  (router :4000 LAN)     │ │
│   │                            │   │                                  │ │
│   │  ┌──────────────────────┐  │   │  /v1/chat/completions ──┐        │ │
│   │  │  State Machine       │  │   │  /v1/audio/speech     ──┼─┐      │ │
│   │  │  (tokio tasks)       │  │   │  /v1/audio/transcripts──┼─┼─┐    │ │
│   │  └──┬───────────┬───────┘  │◄─►│  /v1/models             │ │ │    │ │
│   │     │           │          │   └─────────────────────────┼─┼─┼────┘ │
│   │  display ws    hw task     │                             │ │ │      │
│   │  :8080         (serial)    │                             ▼ ▼ ▼      │
│   │     │           │          │   ┌──────────┐ ┌────────┐ ┌─────────┐  │
│   └─────┼───────────┼──────────┘   │llama-srv │ │kokoro  │ │parakeet │  │
│         │           │              │  :8000   │ │ :8880  │ │  :8082  │  │
│         │           │              │ Qwen3.6- │ │  TTS   │ │  STT    │  │
│         │           │              │ 35B-A3B  │ │        │ │         │  │
│         │           │              └──────────┘ └────────┘ └─────────┘  │
│         │           │                  (private "ai" docker network)    │
└─────────┼───────────┼────────────────────────────────────────────────────┘
          │           │
          │ WebSocket │ USB-serial (CDC, 115200)
          ▼           ▼
   ┌───────────┐  ┌────────────────┐
   │ Browser   │  │ Microcontroller│
   │ (Chrome   │  │  (ESP32)       │
   │  kiosk on │  └───────┬────────┘
   │  Spark or │          │
   │  attached │          ▼
   │  display) │   ┌──────────────┐
   │ + speaker │   │Squirt valve  │
   │ + mic     │   │Gavel servo   │
   └───────────┘   │Splash lights │
                   │Eye gimbal    │
                   │Thermal print │
                   └──────────────┘
```

### Process inventory

All processes run as containers on the Spark via the existing `dgx-ai-stack/docker-compose.yml`, with the orchestrator added as a fifth service.

| Service | Role | Lifecycle |
|---|---|---|
| `orchestrator` | Rust state machine, WS server, serial owner | `docker compose up -d` |
| `litellm` | OpenAI-compatible router on `:4000` (LAN-exposed) | `restart: unless-stopped` |
| `vllm-nvfp4` | Qwen3.6-35B-A3B-NVFP4 chat + vision (`/v1/chat/completions`) | `restart: unless-stopped` |
| `parakeet` | NeMo Parakeet TDT 0.6B v2 STT (`/v1/audio/transcriptions`) | `restart: unless-stopped` |
| `kokoro` | Kokoro-FastAPI TTS, 67 voices (`/v1/audio/speech`) | `restart: unless-stopped` |
| Browser (Chrome kiosk) | Display, mic, speakers | Launched outside compose by a small `kiosk.service` systemd unit on the Spark's display |
| Microcontroller | Realtime hardware control | Always-on once powered; appears at `/dev/ttyUSB0` and is passed into the orchestrator container via `devices:` |

The orchestrator does **no in-process model inference**. It speaks HTTP to LiteLLM at a configurable `inference.base_url`. In production that's `http://litellm:4000` over the private `ai` docker network; in dev it's `http://dgx-spark.local:4000` over the LAN, with the orchestrator running outside Docker (`cargo run`) on the developer's machine. Either way the orchestrator is the only thing besides litellm that the browser kiosk talks to (WebSocket on `:8080`).

### Deployment modes

| Mode | Where orchestrator runs | Inference URL | MCU plugged into | Kiosk points at |
|---|---|---|---|---|
| Production | Container on the Spark | `http://litellm:4000` | Spark USB | `http://localhost:8080` |
| Dev (laptop) | `cargo run` on dev machine | `http://dgx-spark.local:4000` | Dev machine USB (or mocked) | `http://localhost:8080` |
| Dev (no MCU) | `cargo run` on dev machine | `http://dgx-spark.local:4000` | none — `mock` driver in config | `http://localhost:8080` |

The `hardware` section of `config.toml` accepts a `driver` field — `serial` (real MCU) or `mock` (logs commands, fakes acks) — so a developer with no MCU on hand can still exercise the full state machine against the live AI stack.

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
├── inference/
│   ├── mod.rs           // task wrappers
│   ├── client.rs        // shared OpenAI-compatible HTTP client (LiteLLM)
│   ├── charge.rs        // charge-generation prompt + schema
│   ├── verdict.rs       // verdict prompt + schema
│   ├── stt.rs           // multipart upload to /v1/audio/transcriptions
│   └── tts.rs           // /v1/audio/speech, returns wav bytes
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
reqwest = { version = "0.12", features = ["stream", "json", "multipart"] }
eventsource-stream = "0.2"   # for SSE streaming from /v1/chat/completions
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
tokio-serial = "5"
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

**SolidJS + TypeScript**, built with Vite. Solid's fine-grained reactivity is a good match for a screen that's mostly streaming text (deliberation tokens) and swapping between a handful of view states (idle, charge, plea timer, verdict) — no virtual DOM diffing overhead, no SPA-router weight. Build output is bundled into static files served by the Rust binary via `rust-embed`. The frontend connects to `ws://localhost:8080/ws` on load.

A single top-level `createSignal<ViewState>()` mirrors the backend state machine; the WebSocket handler is the only writer. Components subscribe to the slices they care about.

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

### 4.3 Inference Stack (LiteLLM + vllm-nvfp4 + Parakeet + Kokoro)

All model inference runs as containers on the Spark behind a single OpenAI-compatible router. The orchestrator hits one base URL (`http://litellm:4000` from inside the docker network) and authenticates with a bearer token from `LITELLM_MASTER_KEY`.

| Endpoint | Backend container | Model |
|---|---|---|
| `/v1/chat/completions` | `vllm-nvfp4` (`nvcr.io/nvidia/vllm:26.05.post1-py3`) | Qwen3.6-35B-A3B-NVFP4 (vLLM) |
| `/v1/audio/speech` | `kokoro` (`kokoro-tts-arm64`) | Kokoro 82M, voice `bm_george` (default judge) |
| `/v1/audio/transcriptions` | `parakeet` (NeMo on NGC pytorch:25.11) | Parakeet TDT 0.6B v2, exposed as `whisper-1` |
| `/v1/models` | litellm | lists the three above |

This is exactly the stack documented in [`dgx-ai-stack/README.md`](dgx-ai-stack/README.md) — the orchestrator is just a client.

#### Chat API usage

`POST /v1/chat/completions` with:
- `model: "qwen3.6-35b-a3b"`
- **Thinking is off by default** — litellm injects `enable_thinking: false` for this model, so callers don't send it. With thinking on the model reasons silently for 30+ s; the booth needs <8 s end-to-end. Pass `chat_template_kwargs: {"enable_thinking": true}` only when you explicitly want reasoning.
- `response_format: {"type": "json_schema", "json_schema": …}` for structured charge/verdict output. vLLM honors JSON Schema via guided/structured-outputs decoding through the OpenAI-compat path.
- `stream: true` for the verdict deliberation (so the frontend can render tokens live via SSE).
- `stream: false` for charge generation.

Throughput on the Spark: **~70 tokens/sec** decode (NVFP4 on vLLM), ~2 s for a typical short reply.

#### Why not Ollama

Earlier drafts assumed Ollama. The Spark stack uses **vLLM (NVFP4)** directly because (a) it's the fastest path on Blackwell (FP4 weights + CUDA graphs, ~70 vs ~48 tok/s on the old llama.cpp Q4) and serves chat + vision from one model, (b) LiteLLM gives us OpenAI compatibility for free, and (c) putting STT and TTS behind the same router means one client, one auth model.

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

### 5.3 LLM Prompts and Schemas

Both calls go to `POST $LITELLM_BASE/v1/chat/completions` with `Authorization: Bearer $LITELLM_MASTER_KEY` and `model: "qwen3.6-35b-a3b"`. JSON Schema is passed via `response_format` (OpenAI-style); `enable_thinking: false` is set in `chat_template_kwargs` for both calls.

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

Output schema (passed via `response_format.json_schema`):
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

### 6.1 STT (Parakeet via LiteLLM)

- Backend: NVIDIA Parakeet TDT 0.6B v2, served by the `parakeet` container, routed as model id `whisper-1` for OpenAI compatibility.
- Orchestrator collects PCM/webm from the browser, repackages it as a multipart upload, and POSTs to `/v1/audio/transcriptions` with `model=whisper-1`. Parakeet accepts standard audio formats so no client-side conversion is required.
- Latency: ~220 ms warm for a 25 s clip. First call after container start is ~1.2 s (model warm-up); the orchestrator should warm the endpoint on its own startup with a one-shot transcription of a tiny silent clip.
- Choice of Parakeet over whisper.cpp: faster *and* meaningfully more accurate on accents and partial words. A whisper.cpp container is kept in the repo as a fallback for multilingual cases but is not in the active compose.

### 6.2 TTS (Kokoro-FastAPI via LiteLLM)

- Backend: Kokoro-FastAPI in the `kokoro` container, 67 voices preloaded.
- Default judge voice: `bm_george` (British male, formal — fits the pompous-barrister character better than the `am_*` American voices). Configurable.
- `POST /v1/audio/speech` with `{model: "kokoro-tts", voice: "bm_george", input: <text>, response_format: "pcm"}`. Use `response_format: "pcm"` (raw 24kHz s16le) so chunks are immediately playable without a container header — `wav` requires a length-prefixed header that forces the server to buffer the whole clip.
- Use the OpenAI SDK's streaming variant (`with_streaming_response.create`) so the orchestrator gets bytes back as Kokoro emits them, not after the whole clip is synthesized.
- Strip the `VERDICT: GUILTY` line and any non-printable characters before sending — Kokoro is sensitive to unusual unicode and can return empty or hang on it.

### 6.3 Pipelined LLM → TTS (the "judge starts talking immediately" path)

Naive flow waits for the LLM to finish, then waits for TTS to finish, then plays. Time-to-first-word ≈ LLM total + TTS total ≈ 3–5 s. Too slow for the booth.

Pipelined flow:

```
LLM stream ──► sentence buffer ──► TTS task per sentence ──► audio queue ──► browser playback
   (token at a time)   (split on . ! ?)        (HTTP /v1/audio/speech)        (in order)
```

The orchestrator runs three concurrent loops:

1. **Producer**: consumes the LLM SSE stream, appends tokens to a buffer, and on every sentence boundary (`[.!?]` followed by whitespace or stream end) pushes the completed sentence onto an `mpsc::channel<String>`. Stops emitting once a line beginning with `VERDICT:` is seen — that line goes only to the screen, never to TTS.
2. **TTS worker**: pops sentences in order, fires `/v1/audio/speech` with `response_format: "pcm"` and streaming response, forwards each chunk to the browser as it arrives via a binary WebSocket frame prefixed with the same `tts_audio` JSON event from §5.1.
3. **Frontend audio scheduler**: receives PCM chunks, decodes into `AudioBuffer`s, and queues `BufferSource` nodes back-to-back on a single `AudioContext` so playback is gapless across sentence boundaries.

Time-to-first-word becomes:

```
LLM TTFT  +  time-to-first-sentence-boundary  +  TTS TTFB for sentence 1
~300 ms       ~700 ms (decode at ~70 tok/s)        ~300 ms
≈ 1.3 s
```

A few constraints worth respecting:

- **Sentences must be played in order.** The TTS worker is single-task — do not parallelize across sentences, or the browser will need to reorder. Sequential TTS is fine because Kokoro is fast enough to stay ahead of speech playback.
- **Don't synthesize one-word sentences.** Buffer until the sentence is at least ~25 chars; otherwise Kokoro produces a clipped utterance with no prosody. If the LLM emits "Indeed.", hold it and prepend to the next sentence.
- **The verdict appears mid-stream sometimes.** Qwen3 occasionally emits `VERDICT: GUILTY` before the closing remarks. Treat the first `VERDICT:` line as a signal: emit the `verdict` event to the frontend immediately (so the screen flips), keep streaming the remaining tokens to the sentence buffer, but never include the verdict line itself in TTS.
- **Backpressure.** Cap the audio queue depth at ~3 sentences. If TTS is faster than playback (it usually is), the worker awaits queue space rather than buffering unbounded bytes.

### 6.4 Display Server

`axum` with `axum::extract::ws::WebSocketUpgrade`. Single `/ws` endpoint, single connected client at a time (extra connections rejected).

Static frontend assets bundled via `rust-embed` and served from `/`. Use `tower-http`'s `ServeDir` or a custom embedded handler.

The display task owns the WebSocket. It receives `DisplayCommand`s from the state machine and forwards as JSON. It receives client messages, parses, and forwards as `Event`s.

Audio frames are forwarded raw (binary) — no JSON wrapping for the bytes themselves. Header event tells the frontend what's coming next.

### 6.5 Hardware Task

The hardware task is selected at startup by `config.toml`'s `hardware.driver` field. Same `Command` / `Event` channels into and out of the state machine — only the implementation behind them changes. The state machine is unaware of which driver is in use.

```rust
trait HardwareDriver: Send {
    async fn send(&mut self, cmd: HardwareCommand) -> Result<()>;
    // events (HardwareAck, HardwareError, ESTOP) are pushed via the shared event channel
}
```

#### `serial` driver (production)

Owns the serial port, full-duplex. Two halves:

- Writer: receives commands from state machine, writes line, awaits `OK`/`ERR` with timeout.
- Reader: continuously reads lines, parses `ESTOP` and unsolicited messages, forwards as events.

On startup, send `PING`, fail loudly if no `PONG`. Reconnect logic if serial port drops (USB unplugged): retry every 2s and notify state machine.

#### `mock` driver (dev — no microcontroller required)

A pure-software stand-in that lets the entire state machine, frontend, and inference pipeline run on a developer's laptop with nothing plugged in. Behavior:

- Every `HardwareCommand` is logged at `info` level (`mock_hw: FIRE 150`, `mock_hw: GAVEL`, …).
- Each command emits a `HardwareAck` after a configurable simulated latency (default 50 ms) so timing-sensitive states (`ExecutingSentence`) advance like they would with real hardware.
- Optional `mock_hw.fail_rate = 0.05` in config to randomly emit `HardwareError` instead — useful for exercising the soft-degrade path without unplugging cables.
- Optional `mock_hw.simulate_estop_after_secs = N` to fire a synthetic `ESTOP` event N seconds after startup, for e-stop testing.
- The mock driver also forwards "FIRE" commands to the frontend as a `play_cue: 'mock_squirt'` event so the dev UI can render a debug overlay (a brief water-droplet animation) instead of actually getting wet.

The mock driver is the default in `config.dev.toml`. CI runs the orchestrator in mock mode against a recorded set of plea audio files for regression testing.

### 6.6 Operator Inputs

Three inputs, each with both a real-hardware path and a dev path:

| Input | Production | Dev |
|---|---|---|
| Start trial | Illuminated arcade button → MCU GPIO → serial event | Keyboard shortcut (`Space`), or POST to `/operator/start` |
| Emergency stop | Physical big-red-button → MCU emits `ESTOP` | Keyboard shortcut (`Esc`), or POST to `/operator/estop` |
| Skip / reset | Operator-side keyboard shortcut | Same |

The keyboard shortcuts are handled by the kiosk frontend (which has the focused window) and forwarded over the existing WebSocket as `ClientEvent`s — no separate transport. The `/operator/*` HTTP endpoints exist so a developer can `curl` from a script during automated testing without driving a browser.

---

## 7. Reliability and Fallbacks

### Per-call fallbacks

| Failure | Fallback |
|---|---|
| LiteLLM unreachable / 5xx | Use canned charges and canned verdicts (random, weighted guilty) |
| LLM returns malformed JSON | Constrained generation via `response_format` JSON schema should prevent this; if it still happens, retry once, then canned |
| LLM timeout (charge 10s, verdict 30s) | Canned response, log it |
| Qwen3 burned tokens on thinking | Should not happen — `enable_thinking: false` is set. If it does, retry with `max_tokens` raised; otherwise canned |
| STT returns empty | Treat as "no defense offered," proceed to guilty |
| STT fails / timeout | Same |
| TTS returns empty (Kokoro unicode bug) | Strip non-ASCII, retry once; otherwise show text on screen and skip audio |
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

`config.toml` mounted into the orchestrator container at `/app/config.toml`. Loaded via `figment` with env var overrides (`BOOTH__SECTION__KEY`). Secrets (LiteLLM key) come from env, not the toml.

```toml
[inference]
# Production (orchestrator container on the Spark): "http://litellm:4000"
# Dev (orchestrator on a laptop):                   "http://dgx-spark.local:4000"
base_url = "http://litellm:4000"
# api_key from env: BOOTH__INFERENCE__API_KEY → reuses LITELLM_MASTER_KEY
chat_model = "qwen3.6-35b-a3b"
stt_model = "whisper-1"
tts_model = "kokoro-tts"
tts_voice = "bm_george"
charge_timeout_secs = 10
verdict_first_token_timeout_secs = 15
verdict_total_timeout_secs = 30
stt_timeout_secs = 5
tts_timeout_secs = 10
enable_thinking = false                    # booth default — keep latency under 8s

[hardware]
driver = "serial"                          # "serial" for real MCU, "mock" for dev without hardware
serial_port = "/dev/ttyUSB0"               # ignored when driver = "mock"; on Windows use "COM3" etc.
baud = 115200
ack_timeout_ms = 3000

[mock_hw]                                  # only consulted when hardware.driver = "mock"
ack_latency_ms = 50                        # simulated time before HardwareAck is emitted
fail_rate = 0.0                            # 0.0–1.0; emit HardwareError this fraction of the time
simulate_estop_after_secs = 0              # >0 fires a synthetic ESTOP after orchestrator startup

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
guilty_bias = 0.7

[display]
listen_addr = "0.0.0.0:8080"               # exposed to LAN so the kiosk browser on the Spark display can connect

[logging]
level = "info"
log_file = "/var/log/booth/booth.log"
transcripts_jsonl = "/var/log/booth/transcripts.jsonl"
```

---

## 9. Build and Deployment

### Build target

The orchestrator is built and run as an arm64 docker image alongside the existing AI stack. No cross-compilation toolchain is needed on the dev machine — `docker buildx` handles it.

`dgx-ai-stack/orchestrator/Dockerfile` (sketch):

```dockerfile
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY frontend/dist ./frontend/dist          # bundled at build time, embedded via rust-embed
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/booth /usr/local/bin/booth
EXPOSE 8080
CMD ["booth", "--config", "/app/config.toml"]
```

### Adding to docker-compose

Append to `dgx-ai-stack/docker-compose.yml`:

```yaml
  orchestrator:
    image: local/booth-orchestrator:latest
    build:
      context: ../orchestrator
    container_name: orchestrator
    restart: unless-stopped
    networks: [ai]
    depends_on:
      - litellm
    ports:
      - "0.0.0.0:8080:8080"             # browser kiosk connects here
    devices:
      - "/dev/ttyUSB0:/dev/ttyUSB0"     # microcontroller USB-serial passthrough
    environment:
      BOOTH__INFERENCE__API_KEY: ${LITELLM_MASTER_KEY}
      RUST_LOG: info
    volumes:
      - ./orchestrator/config.toml:/app/config.toml:ro
      - booth-logs:/var/log/booth

volumes:
  booth-logs: {}
```

### Distribution layout (on the Spark)

```
~/dgx-ai-stack/
├── docker-compose.yml         # all five services
├── .env                       # LITELLM_MASTER_KEY, model paths, etc.
├── ai-stack                   # control script
├── litellm/config.yaml
├── llama-cpp/Dockerfile
├── parakeet/{Dockerfile,server.py}
├── kokoro/                    # (image is prebuilt; no Dockerfile here)
└── orchestrator/
    ├── Dockerfile
    ├── config.toml
    └── (Rust source pulled at build time from ../orchestrator/)
```

### Dev: running the orchestrator locally against the live Spark

For day-to-day development you don't want to rebuild a container on every change. Run the orchestrator natively against the already-running Spark stack:

```sh
# One-time: expose the Spark's LiteLLM port to the LAN (already true — :4000
# is published in docker-compose.yml). Confirm reachability:
curl -sf http://dgx-spark.local:4000/v1/models -H "Authorization: Bearer $LITELLM_MASTER_KEY"

# Point the orchestrator at the Spark and run it on your machine:
cd orchestrator
cp config.toml config.dev.toml
# Edit config.dev.toml:
#   inference.base_url = "http://dgx-spark.local:4000"
#   hardware.driver    = "mock"        (or "serial" + COM3 if you've got the MCU)
#   display.listen_addr = "127.0.0.1:8080"

BOOTH__INFERENCE__API_KEY=$LITELLM_MASTER_KEY \
  cargo run -- --config config.dev.toml

# In another shell, open the kiosk (or just a regular browser tab):
open http://localhost:8080
```

The frontend Vite dev server can run separately (`cd frontend && npm run dev`) and proxy `/ws` to `127.0.0.1:8080` for hot module reload during UI work — `rust-embed` only kicks in for release builds.

This is also the right mode for the benchmark script, which already accepts `--base-url http://dgx-spark.local:4000/v1`.

### Bringing it up (production, on the Spark)

From the dev machine, the existing wrapper still works:

```sh
cd dgx-ai-stack
./ai-stack                     # pulls + builds + starts all five services
```

The kiosk browser is launched on the Spark's attached display by a tiny systemd unit, **not** by docker (Chrome wants a display server, which complicates containerization for little gain):

```ini
# /etc/systemd/system/booth-kiosk.service
[Unit]
After=docker.service graphical.target
Requires=docker.service

[Service]
ExecStartPre=/usr/bin/sh -c 'until curl -sf http://localhost:8080/health; do sleep 1; done'
ExecStart=/usr/bin/google-chrome --kiosk --app=http://localhost:8080
Restart=on-failure
User=booth

[Install]
WantedBy=graphical.target
```

---

## 10. Implementation Roadmap

Suggested phasing — each phase produces a working artifact you can demo.

The DGX Spark inference stack (LiteLLM + vllm-nvfp4 + parakeet + kokoro) is **already running** — see `dgx-ai-stack/README.md` and the `dgx-ai-stack/sample-benchmark.py` end-to-end check. The roadmap below is for everything *above* that line: the orchestrator, frontend, microcontroller, and the booth itself.

### Phase 1: Skeleton with mocks

- Add `orchestrator/` to the repo. Cargo project, module layout per §4.1, basic `tracing` setup.
- Add the `orchestrator` service to `dgx-ai-stack/docker-compose.yml` per §9 (production path).
- Implement the state machine with all states and transitions; every external call is mocked (hardcoded responses, immediate completion).
- Implement the `mock` hardware driver (§6.5) so the entire trial loop runs with nothing plugged in.
- Implement axum WebSocket server with the full event protocol, plus the `/operator/start` and `/operator/estop` HTTP shims for scripted testing.
- Build a minimal SolidJS frontend that connects, logs events, and shows current state.
- Verify: `cargo run --config config.dev.toml` on the dev laptop with `hardware.driver = "mock"` cycles a full trial end-to-end. Operator hits "start" via keyboard shortcut, display shows mock charge, simulated 20s plea, mock transcript, mock verdict, mock hardware ack (logged as `mock_hw: FIRE 150`), return to idle. Then `docker compose up` on the Spark does the same with no code changes.

### Phase 2: Real LLM

- Implement the LiteLLM HTTP client (`inference/client.rs`) — bearer auth, retry, timeout, JSON-schema response parsing.
- Implement charge and verdict prompts (§5.3) with `enable_thinking: false`.
- Implement SSE streaming for the verdict and forward `deliberation_token` events to the frontend.
- Wire fallbacks (canned charges/verdicts).
- Verify: real charges generated, real verdicts rendered, deliberation streams to the screen. Stop the `litellm` container mid-trial → fallback fires within the configured timeout.

### Phase 3: Real audio

- Frontend: implement mic capture (16 kHz mono, sent as binary WebSocket frames) and gapless PCM playback via `AudioContext` + queued `BufferSource`s (§6.3).
- Backend: implement `inference/stt.rs` — multipart upload of captured audio to `/v1/audio/transcriptions`, model `whisper-1`. Warm Parakeet on orchestrator startup with a one-shot silent clip.
- Backend: implement `inference/tts.rs` — `/v1/audio/speech` with `response_format: "pcm"` and the SDK's streaming response variant.
- Backend: implement the **pipelined LLM → TTS path** from §6.3 — sentence buffer, ordered TTS worker, mid-stream `VERDICT:` handling.
- Verify: full audio loop works against the live Spark stack. Time-to-first-word from operator press is <2 s (roughly the TTFA the benchmark reports with `--pipeline-tts`). Mute the mic during the plea window → STT returns empty → "no defense offered" path fires.

### Phase 4: Real hardware

The state machine and mock hardware driver have been driving fake commands since Phase 1. This phase swaps the `mock` driver for the `serial` driver — same channel API, no state-machine changes.

- Build microcontroller firmware implementing the serial protocol (§5.2).
- Implement the `serial` hardware driver with ack/timeout/reconnect.
- Flip `hardware.driver = "serial"` in production config; add `devices: ["/dev/ttyUSB0:/dev/ttyUSB0"]` to the orchestrator service so the container sees the MCU.
- Wire up squirt + gavel + lights at minimum.
- Verify: end-to-end live trial with real water firing on the Spark; dev environment continues to work unchanged with `driver = "mock"`.

### Phase 5: Theater

- Frontend visual polish: typography, animations, deliberation streaming, verdict reveal.
- Sound cues bundled with frontend.
- Additional hardware props (eye gimbal, thermal printer, status panel, etc.) added incrementally.
- Operator HUD.

### Phase 6: Hardening

- 50-trial soak test on the actual booth. Random/adversarial inputs (silence, screaming, multilingual pleas, disconnect cables mid-trial, kill `kokoro` mid-verdict, unplug the MCU). Fix every wedge until none remain.
- Production logging and transcript archiving rotated under `booth-logs` volume; nightly rsync off the Spark.
- Image pinning: tag the orchestrator image and pin every other service in the compose file to a digest before the event so a `docker pull` doesn't surprise you.

---

## 11. Open Questions / Decisions Deferred

- **PTT button location**: on the lectern (via MCU GPIO) vs. operator-side. Currently planned as defendant-facing on the lectern, illuminated arcade button.
- **Voice selection for Kokoro**: `bm_george` is the current pick for the pompous-British-barrister character; A/B test against `bm_lewis` and `bf_isabella` during dress rehearsal. If no Kokoro voice carries the bit, ElevenLabs is the next step at the cost of internet dependency (and breaking the all-local property).
- **Hardware platform**: ESP32 default, but if a specific shield/HAT exists for the thermal printer or LED panels, that may dictate Arduino-flavor choice.
- **Kiosk display**: assumed to be driven directly by the Spark (HDMI out → big monitor). If the Spark can't drive both inference loads and a desktop session smoothly, fall back to a small attached machine (Pi 5 or NUC) running just Chrome and pointing at `http://dgx-spark.local:8080`.
- **Microcontroller location**: USB-serial passthrough into the orchestrator container assumes the MCU is plugged into the Spark itself. If the booth physical layout makes that awkward, options are (a) USB extender, (b) ser2net bridge over the booth LAN, (c) WebSerial from the kiosk browser relayed over the existing WebSocket. (a) is simplest; the others are escape hatches.
- **Operator HUD reachability**: with the orchestrator in a container, the HUD is a second route on `:8080` (e.g. `/operator`) rather than a separate process. Confirm during Phase 5.
