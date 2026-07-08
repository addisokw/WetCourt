# The Wet Court of Appeals

An interactive courtroom booth. An operator starts a trial, an absurd charge is
drawn against the next visitor, the visitor pleads their case into a microphone,
an LLM "judge" deliberates aloud in character, and a verdict is rendered
theatrically — gavel bang, lights, and on a guilty finding a **computer-aimed
squirt gun** fires at the splash zone. The defendant walks away with a printed
keepsake of their trial.

Speech-to-text, the LLM, and text-to-speech all run **locally** on an NVIDIA DGX
Spark — no cloud round-trip, no per-call cost, and it works with the venue WiFi
down.

> Status: actively built. The trial loop, local inference, the operator console,
> the distributed hardware fleet (gavel + turret + squirt), vision-guided
> targeting with an eye-safety fire gate, and the thermal-printer keepsake are
> all working. See [§ Where things stand](#where-things-stand).

---

## The experience

```
Idle attractor ─▶ operator starts ─▶ charge drawn & read aloud ─▶ plea window (~20s, push-to-talk)
      ▲                                                                      │
      │                                                                      ▼
   cooldown ◀─ sentence executed ◀─ verdict pronounced ◀─ judge deliberates ◀─ plea transcribed
            (gavel; if guilty, the              (streamed aloud,        (optional cross-examination
             turret aims & squirt fires;         token by token)         question first)
             keepsake prints)
```

1. **Idle** — attractor mode on the big screen.
2. **Operator triggers** a trial (console button, keyboard, or a physical
   start button on the booth).
3. **Charge** — drawn from a curated, operator-managed list (operator-queued
   charges first, then a filtered random draw). Displayed and spoken via TTS.
4. **Plea** — the visitor has ~20 s to plead into a mic (push-to-talk).
5. **Transcription** — audio → text via local STT.
6. **Cross-examination** *(optional, operator-toggleable)* — the judge asks one
   pointed follow-up and records a short answer.
7. **Deliberation** — the LLM judge weighs the plea in persona, streaming its
   reasoning to the screen and speaking it as it goes. As it deliberates, the
   turret **arms and visibly locks onto the defendant** — the gun starts each
   trial static at idle and acquires its target before the verdict, for suspense
   (`[vision] trial_targeting`, on by default).
8. **Verdict** — guilty / not guilty + a closing remark. The screen also names
   the deciding factor ("what decided it").
9. **Sentence** — gavel bangs; if guilty, the turret **freezes on its lock and
   the squirt fires**; if acquitted, a celebration cue and the gun returns to
   idle. A keepsake receipt prints.
10. **Cooldown** → back to Idle (gun idle, disarmed, ready for the next
    defendant).

---

## High-level architecture

Five cooperating subsystems. The **orchestrator** is the brain; everything else
is a service or a device it talks to over a network boundary.

```
                       ┌───────────────────────────────────────────────┐
                       │  DGX Spark — local AI stack (docker compose)    │
   browser kiosk       │                                                 │
  ┌──────────────┐ ws  │   LiteLLM :4000  (one OpenAI-compatible API)    │
  │ operator     │◀───▶│      ├─ /v1/chat/completions → vLLM  (Qwen3.6)  │
  │ console      │     │      ├─ /v1/audio/transcriptions → Parakeet STT │
  │ + face/case  │     │      └─ /v1/audio/speech → Kokoro TTS           │
  │ monitors     │     └───────────────────────▲─────────────────────────┘
  │ + mic/audio  │                              │ HTTP (base_url is a config knob)
  └──────┬───────┘                              │
         │ ws  ┌─────────────────────────────────────────────┐
         └────▶│            orchestrator (Rust)               │
               │  deterministic trial state machine          │
               │  + axum HTTP/WebSocket server :8080         │
               │  + inference client, hardware router,        │
               │    persona/crime/calibration registries,     │
               │    casebook + thermal-printer service        │
               └───▲───────────────────────────┬─────────────┘
        TCP line   │ :8090 (devices dial in)    │ HTTP proxy /vision/*
        protocol   │                            ▼
   ┌───────────────┴────────────┐      ┌──────────────────────────────┐
   │ device fleet (M5 NanoC6 ×N) │      │ vision process (Python :8091)│
   │  • turret  AIM (pan/tilt)   │◀────▶│  webcam → MediaPipe pose →    │
   │  • squirt  FIRE (relay)     │ aim  │  target points, MJPEG feed,   │
   │  • gavel   GAVEL (servo)    │stream│  closed-loop aim + fire_ok    │
   │  • judge  PANEL + gaze AIM  │      │  eye-safety verdict           │
   └─────────────────────────────┘      └──────────────────────────────┘
```

- **AI stack** (`dgx-ai-stack/`) — STT, LLM, and TTS as containers behind a
  single LiteLLM gateway on `:4000`. The orchestrator is just an HTTP client.
- **Orchestrator** (`orchestrator/`) — a Rust async state machine + axum server.
  Owns trial flow, talks to inference over HTTP, routes hardware commands to the
  device fleet, serves the UI, and prints keepsakes.
- **Browser** (`orchestrator/frontend/`, SolidJS) — display, mic capture, and
  audio playback. Deliberately part of the architecture so the backend needs no
  native audio I/O.
- **Device fleet** (`firmware/`, `protocol/`) — independent microcontrollers
  that dial the orchestrator over WiFi and announce a role. One board per job.
- **Vision** (`vision/`, Python) — webcam + pose detection that drives the
  turret in a closed loop and computes the eye-safety fire verdict.

---

## Execution paradigms

The design decisions that shape how the whole thing runs — read this to
understand *why* the code is laid out the way it is.

**A deterministic state machine drives everything.** The trial is a pure
function `step(state, event) → (next_state, commands)`
(`orchestrator/src/state_machine/transitions.rs`). The LLM is consulted at
well-defined points but **never controls flow or hardware**. A 100 ms `Tick`
event drives all timeouts centrally rather than per-state timers. The pure core
is wrapped by an impure `Runtime` shell (`state_machine/mod.rs`) that owns
channel I/O, persona snapshots, the trial draft, and casebook/printer hand-off.

**Events in, Commands out, over channels.** The runtime is a hub of Tokio tasks
connected by `mpsc`/`broadcast` channels. Every subsystem is *dumb*: it receives
a `Command`, does the thing, and emits an `Event`. Only the state machine knows
the trial flow.

```
operator HTTP ─┐
device acks   ─┼─▶ Event channel ─▶ [ state machine ] ─▶ Command channels ─▶ inference worker
frontend ws   ─┘                          │                                 ─▶ hardware router
                                          └────────── display broadcast ───▶ all WebSocket clients
```

The HTTP layer is just *one* event source, coequal with hardware (a physical
button) and the browser (plea audio, "tts finished").

**Every external call has a fallback.** LLM unreachable → canned charge/verdict.
STT empty/failed → "[no defense offered]" → guilty. Hardware silent → show the
verdict on screen and move on. A device that owns a gating command but isn't
connected still gets a synthesized ack so the trial never stalls in front of a
visitor. Timeouts everywhere, watchdog on the sentence.

**Inference is a service, not a library.** STT/LLM/TTS sit behind one
OpenAI-compatible endpoint (LiteLLM). The orchestrator holds no model weights —
it's an HTTP client with one `base_url`. This is what makes the next point
possible.

**Location is a config knob, not an architectural assumption.** The same binary
runs co-located on the Spark (talking to LiteLLM over the docker network) or
remotely on a laptop/booth PC (talking to the Spark over LAN/Tailscale). Devices
dial the orchestrator over WiFi and the browser points at its `:8080`, so both
follow it to whichever host it runs on. Two checked-in configs are the starting
points: `config.toml` (co-located) and `config.dev.toml` (remote). See
[§ Deployment shapes](#deployment-shapes).

**Pipelined LLM → TTS so the judge talks almost immediately.** Rather than wait
for the full deliberation, the orchestrator splits the LLM token stream on
sentence boundaries and synthesizes each sentence as it lands, streaming PCM to
the browser. Time-to-first-word drops from 3+ s to ~1.3 s.

**Hardware is a distributed fleet, hot-swappable behind a trait.** Instead of
one all-knowing microcontroller, each prop is its own board that dials in and
announces a role (`HELLO turret`). The orchestrator routes commands by role over
a small TCP line protocol. The whole layer is a `HardwareDriver` trait with two
implementations: `tcp` (real fleet) and `mock` (acks everything in software, for
development with nothing plugged in). The state machine can't tell which is
behind it.

**Vision owns a closed targeting loop; the orchestrator owns the safety gate.**
The Python vision process runs the proportional aim loop and streams aim +
a `fire_ok` eye-safety verdict to the orchestrator at ~15 Hz. The orchestrator
relays aim to the turret **only while armed**, and suppresses a trial FIRE when
armed without a fresh `fire_ok` — failing safe, while still letting the trial
advance.

**The browser does the I/O the backend shouldn't.** Mic capture and gapless
audio playback live in the kiosk page; the operator console, the read-only
`/face` and `/case` monitor views, and the maintenance panels are all the same
SolidJS app over one WebSocket.

---

## Repo layout

```
.
├── orchestrator/        Rust state machine + axum server + SolidJS UI (the brain)
│   ├── src/             config, state_machine/, inference/, hardware/, display/,
│   │                    personas/, crimes/, calibration/, printer/, fallbacks/
│   ├── frontend/        SolidJS + TypeScript operator console & monitor views
│   ├── crates/          crimes-core (shared) + crimes-editor (helper bin) +
│   │                    thermal-printer (vendored, private)
│   ├── personas/        judge persona definitions (*.toml)
│   ├── crimes/          curated charge list (JSON)
│   ├── calibration/     per-device servo/gavel calibration (*.toml)
│   ├── config.toml      co-located (on-Spark) starting config
│   └── config.dev.toml  remote (laptop/booth-PC) starting config
├── dgx-ai-stack/        Local AI stack: LiteLLM + vLLM (NVFP4) + Parakeet + Kokoro,
│                        the `ai-stack` control script, and the pipeline benchmark
├── vision/              Python turret-vision process (MediaPipe pose, MJPEG feed,
│                        closed-loop aim, eye-safety fire_ok)  — `uv run vision.py`
├── firmware/            Arduino sketches, one per board (turret / squirt / gavel)
├── protocol/            Device⇄orchestrator wire protocol spec (language-neutral)
├── deploy/homelab/      Persistent remote deployment (compose + Tailscale + Cloudflare)
├── docs/                Design docs (see § Documentation map)
└── strix-halo-port-notes.md   Feasibility notes for an AMD Strix Halo port (not greenlit)
```

---

## Subsystem tour

### Orchestrator (`orchestrator/`) — Rust + SolidJS

The brain. A `booth` binary plus a `crimes-editor` helper, in a Cargo workspace.

- **State machine** (`src/state_machine/`) — pure `transitions.rs`
  (`(state, event) → (state, commands)`), the `State`/`Event`/`Command` enums,
  and the `Runtime` shell. States cover the happy path (Idle → GeneratingCharge →
  DisplayingCharge → AwaitingPlea → Transcribing → Deliberating →
  PronouncingVerdict → ExecutingSentence), the optional cross-examination branch,
  a Maintenance state, and an Error/fallback path. E-stop returns to Idle from
  anywhere.
- **Inference** (`src/inference/`) — one HTTP client over LiteLLM; charge,
  verdict (streamed, pipelined into TTS), cross-exam question, STT, and TTS, each
  with a `real` and a `mock` mode and canned fallbacks.
- **Hardware** (`src/hardware/`) — the `HardwareDriver` trait, the `mock` and
  `tcp` registry drivers, the role map, the maintenance direct-control plane, the
  per-device calibration edge (degrees→raw µs; bare `Gavel`→`GavelStrike` from
  `gavel.toml`), and the vision fire gate.
- **Display** (`src/display/`) — the axum router: `/ws` (single operator
  console, last-connection-wins), `/ws/view` (read-only monitors), `/operator/*`
  (trial + persona/crime control), `/maintenance/*` (hardware test plane, gated
  on the Maintenance state), and `/vision/*` (reverse-proxy + arm/aim).
- **Personas / crimes / calibration** — file-backed registries, all editable
  live from the console.
- **Printer** (`src/printer/`) — the casebook (append-only JSONL trial log) and
  the ESC/POS keepsake renderer + service task.

Run it: see [§ Quick start](#quick-start). Details, deployment topologies, and
failure injection in [`orchestrator/README.md`](orchestrator/README.md).

### AI stack (`dgx-ai-stack/`) — local inference on the Spark

Five containers on a Grace-Blackwell DGX Spark, fronted by one OpenAI-compatible
gateway:

| Service | Model | Route | Model id |
|---|---|---|---|
| `litellm` | gateway/router on `:4000` (LAN-exposed) | — | — |
| `vllm-nvfp4` | Qwen3.6-35B-A3B (NVFP4, MARLIN FP4 on Blackwell, ~70 tok/s) | `/v1/chat/completions` | `qwen3.6-35b-a3b` |
| `parakeet` | NVIDIA Parakeet TDT 0.6B v2 (STT) | `/v1/audio/transcriptions` | `whisper-1` |
| `kokoro` | Kokoro TTS (67 voices) | `/v1/audio/speech` | `kokoro-tts` |

LiteLLM injects `enable_thinking: false` for the chat model by default (reasoning
would blow the latency budget). Operate it with the `ai-stack` script (SSHes
`docker compose` to the Spark): `./ai-stack` (up), `down`, `status`, `logs [svc]`,
`restart <svc>`, `ssh`. Config lives in `.env` and `litellm/config.yaml`. See
[`dgx-ai-stack/README.md`](dgx-ai-stack/README.md).

### Frontend (`orchestrator/frontend/`) — SolidJS

One app, several views over the WebSocket: the **operator console** (`/`, the
tabbed `Shell` — operator, judge mind, vision, and gated hardware panels), and
the read-only **`/face`** (animated judge) and **`/case`** (charge/plea/verdict)
monitor pages for audience-facing displays. It captures the plea mic, plays the
streamed TTS gaplessly, and renders the live deliberation. Built with Vite to
`frontend/dist/`; release builds embed `dist/` via `rust-embed`, and dev can run
Vite with HMR against the running orchestrator.

### Vision (`vision/`) — Python

A standalone process (booth PC or a Spark container) that captures the turret
camera, runs MediaPipe pose detection, and computes target points (chest, head,
eyes). It serves an annotated **MJPEG `/feed`** and a **`/state`** JSON, and runs
the **closed-loop aim**: each frame it nudges the commanded pan/tilt to put the
target on a fixed *boresight* pixel and POSTs aim + `fire_ok` to the
orchestrator (relayed to the turret only while armed). The **eye-safety** layer
builds a conservative eye-exclusion zone and only sets `fire_ok` when the impact
point is locked and clear (head shots additionally require operator
confirmation). Runs on `:8091`; the orchestrator reverse-proxies it at
`/vision/*` so the console stays same-origin. `cd vision && uv run vision.py`.
Roadmap in [`docs/turret-vision-roadmap.md`](docs/turret-vision-roadmap.md).

### Firmware + protocol (`firmware/`, `protocol/`)

Each prop is an independent M5Stack NanoC6 that joins the booth WiFi, dials the
orchestrator's `:8090`, and announces itself with `HELLO <role>` → `WELCOME`.
Commands are line-based ASCII (`VERB args\n`), each answered with `OK <verb>` or
`ERR <verb> <reason>`.

| Board | Role | Verbs | Hardware |
|---|---|---|---|
| turret | `turret` | `AIM <pan_us> <tilt_us>`, `PING` | NanoC6 + 8-Servos (pan/tilt) |
| squirt | `squirt` | `FIRE <ms>`, `PING` | NanoC6 + 3A relay |
| gavel | `gavel` | `GAVEL [geometry]`, `GJOG <us>`, `PING` | NanoC6 + 8-Servos (arm) |
| judge-face | `judge-face` | `PANEL`, `PING` | Matrix Portal M4 + 64×32 HUB75 panel |
| judge-neck | `judge-neck` | `AIM <pan_us> <tilt_us>`, `PING` | NanoC6 + 8-Servos (pan/tilt gaze) |

The turret (aim) and squirt (fire) are split across two boards because the
NanoC6's only Grove pins are consumed by the servo board's I2C on the turret.
Firmware is intentionally stateless — all geometry/calibration comes from the
host. Spec: [`protocol/README.md`](protocol/README.md);
[`docs/hardware-architecture.md`](docs/hardware-architecture.md).

### Thermal-printer keepsake (`orchestrator/src/printer/`, `crates/thermal-printer/`)

Every completed trial is appended to the **casebook** (`transcripts_jsonl`) and
rendered to an ESC/POS **keepsake receipt** (court seal, docket, charge → plea →
verdict, QR footer) — printed on an 80 mm USB printer in production, or rendered
to a log in `mock` mode. The state machine harvests the trial into a draft and
finalizes it on `ExecutingSentence`.

> ⚠️ **The `thermal-printer` crate is vendored from a private repo and this
> GitHub origin is public.** A plain `git push` of a branch containing it would
> publish private source. See [`docs/thermal-printer.md`](docs/thermal-printer.md)
> before pushing.

---

## Quick start

### Dev loop (orchestrator on your machine, inference on the Spark, no hardware)

The everyday loop. Mock hardware, real (or mock) inference.

```sh
cd orchestrator
npm --prefix frontend install && npm --prefix frontend run build   # one-time / on UI change
cargo run -- --config config.dev.toml
# operator console:        http://localhost:8080
# audience monitor views:  http://localhost:8080/face  and  /case
```

`config.dev.toml` points inference at the Spark's LiteLLM and runs hardware in
`mock` mode. Set `[inference] mode = "mock"` to go fully offline. Point at a real
Spark with `BOOTH__INFERENCE__BASE_URL=http://<spark-ip>:4000`.

For UI work, run Vite with hot reload instead of rebuilding `dist/`:
`cd orchestrator/frontend && npm run dev`.

### Add the hardware fleet

Flash the boards (`firmware/`), put them on the booth WiFi pointed at the
orchestrator host, and set `[hardware] driver = "tcp"` (bind `0.0.0.0:8090`).
Each board dials in and appears in the console's maintenance/device view. Add the
turret camera + `cd vision && uv run vision.py` for vision-guided aiming.

### Bring up the AI stack (on the Spark)

```sh
cd dgx-ai-stack
cp .env.example .env        # fill in LITELLM_MASTER_KEY and the model dir
./ai-stack                  # builds + starts everything
# unified endpoint: http://<spark-ip>:4000  (OpenAI-compatible /v1/*)
```

### Production / persistent deployments

See [§ Deployment shapes](#deployment-shapes) and
[`deploy/homelab/`](deploy/homelab/) for the always-on remote shape (orchestrator
+ Tailscale sidecar behind a Cloudflare tunnel, inference on the remote Spark).

---

## Deployment shapes

One binary, three shapes, selected by config:

| Shape | Orchestrator | Inference URL | Hardware | Use |
|---|---|---|---|---|
| **Dev (local)** | `cargo run` on a laptop | Spark over LAN/Tailscale | `mock` (or `tcp` if a board's handy) | day-to-day development |
| **Homelab (persistent remote)** | container + Tailscale sidecar on an always-on box, published via Cloudflare | Spark over Tailscale | `mock` | team access to the console/personas/crimes over HTTPS |
| **Production (booth)** | container on the Spark (or a booth PC) | `http://litellm:4000` (docker net) or Spark LAN IP | `tcp` — boards dial in over WiFi | the live exhibit |

Config knobs that differ: `inference.base_url`, `inference.mode`,
`hardware.driver`, `display.listen_addr`. Figment loads `config*.toml` with
`BOOTH__SECTION__KEY` env overrides; `.env` (repo root or `dgx-ai-stack/.env`)
supplies `LITELLM_MASTER_KEY`. Knob-by-knob breakdown in
[`orchestrator/README.md`](orchestrator/README.md).

---

## Benchmarking the pipeline

```sh
python3 -m venv .venv && .venv/bin/pip install openai   # one-time
cd dgx-ai-stack
set -a; . .env; set +a
../.venv/bin/python sample-benchmark.py --runs 5 --no-think
```

Reports per-stage latency (STT, LLM TTFT, LLM total, TTS, and time-to-first-audio
in pipelined mode), GPU power/util/temp sampled over SSH, per-container memory,
and the guilty-verdict rate. Target is <8 s end-to-end; with `--no-think` it
lands around 4 s.

---

## Documentation map

| Doc | What it covers |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | The foundational design doc: state machine, protocols, boundaries, fallbacks. (Parts predate the multi-device fleet and current hardware — cross-check against the docs below.) |
| [`docs/hardware-architecture.md`](docs/hardware-architecture.md) | The distributed device fleet: board map, roles, `HELLO` handshake, migration plan. |
| [`protocol/README.md`](protocol/README.md) | The device⇄orchestrator wire protocol (transport, handshake, verbs, ack model). |
| [`docs/turret-vision-roadmap.md`](docs/turret-vision-roadmap.md) | Turret + vision: what's done (through firing, eye-safety-gated) and what's next. |
| [`docs/thermal-printer.md`](docs/thermal-printer.md) | The keepsake receipt + casebook, the vendored private crate, and the do-not-push caveat. |
| [`docs/judge-persona-notes.md`](docs/judge-persona-notes.md) | Judge character + TTS delivery notes. (Partly stale: intensity is now binary; guilt rate is a config/persona knob.) |
| [`orchestrator/README.md`](orchestrator/README.md) | Running the orchestrator, deployment topologies, failure injection. |
| [`dgx-ai-stack/README.md`](dgx-ai-stack/README.md) | Operating the AI stack, the endpoints, troubleshooting. |
| [`docs/ideas.md`](docs/ideas.md) · [`docs/creators.md`](docs/creators.md) | Future-feature scratchpad · creator list for themed charges. |

---

## Where things stand

- **Inference stack** — running on the Spark (vLLM NVFP4 + Parakeet + Kokoro via
  LiteLLM); benchmarked under the latency target.
- **Orchestrator + frontend** — full trial loop, streamed/pipelined deliberation,
  cross-examination, live persona/crime editing, maintenance plane.
- **Hardware fleet** — gavel, turret, and squirt boards on the TCP registry with
  live calibration from the console.
- **Vision** — closed-loop targeting and the eye-safety fire gate; trial FIRE is
  wired and gated on a fresh `fire_ok`.
- **Keepsake** — casebook log + ESC/POS receipt rendering.
- **Next** — vision "moment of justice" still for the receipt + an audience
  turret-cam (see the turret-vision and thermal-printer docs).
