# Wet Court of Appeals

A courtroom-themed interactive booth. An operator triggers a trial, the LLM
invents an absurd charge against the next visitor, the visitor pleads their
case into a microphone, the judge deliberates aloud, and a verdict is rendered
theatrically — on guilty findings, a computer-aimed squirt gun fires. Audio,
LLM, and TTS all run locally on an NVIDIA DGX Spark; no cloud round-trip.

## Repo layout

```
.
├── docs/
│   ├── architecture.md         Design doc: state machine, protocols, boundaries
│   └── judge-persona-notes.md  Notes on judge characters and TTS delivery
├── dgx-ai-stack/               Self-hosted AI stack on the Spark
│                               (LiteLLM + vLLM NVFP4 + Kokoro TTS + Parakeet STT)
│                               plus the end-to-end pipeline benchmark
├── orchestrator/               Rust state machine, axum WS server, SolidJS kiosk UI
└── firmware/                   Rust firmware for M5Stack NanoC6 (squirt valve, gavel, button)
```

Start with [`docs/architecture.md`](docs/architecture.md) for the big picture.
Each subproject has its own README for ops details:

- [`dgx-ai-stack/README.md`](dgx-ai-stack/README.md) — operate the stack, call
  the endpoints, troubleshoot.
- [`orchestrator/README.md`](orchestrator/README.md) — run the state machine
  on a laptop against the Spark, drive the debug UI, inject failures.
- [`firmware/README.md`](firmware/README.md) — flash the NanoC6, pair it with
  the orchestrator over TCP.

## Quick start

Bring up the inference stack on the Spark:

```sh
cd dgx-ai-stack
cp .env.example .env       # then fill in LITELLM_MASTER_KEY and model paths
./ai-stack                 # builds + starts everything, orchestrator included
```

Once up, the LAN endpoint is `http://10.10.1.221:4000` (the Spark; mDNS
doesn't resolve reliably). It speaks the OpenAI API: `/v1/chat/completions`,
`/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/models`.

For laptop dev without rebuilding the Spark image:

```powershell
cd orchestrator
cargo run -- --config config.dev.toml
# kiosk UI: http://localhost:8080
```

This points the orchestrator at the Spark's LiteLLM over LAN and runs
hardware in mock mode. See `orchestrator/README.md` for failure injection
and pairing with real firmware over WiFi.

## Benchmarking the pipeline

```sh
cd dgx-ai-stack
set -a; . .env; set +a
../.venv/bin/python sample-benchmark.py --runs 5 --no-think
```

Reports per-stage latency (STT, LLM TTFT, LLM total, TTS), GPU power and
utilization sampled over SSH, and per-container resident memory. Target is
<8 s end-to-end; with `--no-think` it lands around 4 s.
