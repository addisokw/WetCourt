# Wet Court of Appeals

A courtroom-themed interactive booth. An operator triggers a trial, an absurd
charge is drawn against the next visitor from an operator-curated crime list,
the visitor pleads their case into a microphone, the judge deliberates aloud
(LLM), and a verdict is rendered theatrically — on guilty findings, a
computer-aimed squirt gun fires. Audio, LLM, and TTS all run locally on an
NVIDIA DGX Spark; no cloud round-trip.

## Repo layout

```
.
├── docs/
│   ├── architecture.md         Design doc: state machine, protocols, boundaries
│   ├── judge-persona-notes.md  Notes on judge characters and TTS delivery
│   └── ideas.md                Scratchpad of future exhibit ideas
├── dgx-ai-stack/               Self-hosted AI stack on the Spark
│                               (LiteLLM + vLLM NVFP4 + Kokoro TTS + Parakeet STT)
│                               plus the end-to-end pipeline benchmark
├── orchestrator/               Rust state machine, axum WS server, SolidJS UI
│                               (operator console, judge face, case view, personas)
├── firmware/                   Rust firmware for M5Stack NanoC6 (trial button works;
│                               valve/gavel actuation still stubbed)
└── strix-halo-port-notes.md    Feasibility notes for porting the AI stack to
                                Strix Halo (Vulkan/x86_64) — drafted, not greenlit
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

Once up, the LAN endpoint is `http://<spark-ip>:4000` (the Spark; mDNS
doesn't resolve reliably). It speaks the OpenAI API: `/v1/chat/completions`,
`/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/models`.

The orchestrator can also run on a different machine than the Spark — the
everyday dev loop, and a valid production shape too (the MCU dials the
orchestrator over WiFi, wherever it is):

```powershell
cd orchestrator
cargo run -- --config config.dev.toml
# operator console: http://localhost:8080
# judge face / case view monitors: /face and /case on the same port
```

This points the orchestrator at the Spark's LiteLLM over the network and
runs hardware in mock mode. The two shapes — everything on the Spark vs.
inference-only on the Spark — and exactly which knobs differ are laid out in
[`orchestrator/README.md` § Deployment topologies](orchestrator/README.md#deployment-topologies);
failure injection and firmware pairing are covered there too.

## Benchmarking the pipeline

```sh
# One-time: the benchmark needs a venv with the openai client
python3 -m venv .venv && .venv/bin/pip install openai

cd dgx-ai-stack
set -a; . .env; set +a
../.venv/bin/python sample-benchmark.py --runs 5 --no-think
```

Reports per-stage latency (STT, LLM TTFT, LLM total, TTS), GPU power and
utilization sampled over SSH, and per-container resident memory. Target is
<8 s end-to-end; with `--no-think` it lands around 4 s.
