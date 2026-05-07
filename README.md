# Wet Court of Appeals

A courtroom-themed demo of a self-hosted, OpenAI-compatible AI stack running on
an NVIDIA DGX Spark. A defendant pleads their case (audio in), the Honorable
Justice Wettington deliberates (LLM), and the verdict is read aloud (audio
out) — all through one LAN endpoint, no cloud round-trip.

## Repo layout

```
.
├── dgx-ai-stack/          The stack itself: docker-compose + per-service Dockerfiles + ai-stack control script
│   └── README.md          Operate the stack, call the endpoints, troubleshoot
├── sample-benchmark.py    End-to-end pipeline benchmark (STT → LLM → TTS) with GPU telemetry
├── sample_plea.wav        Test fixture used by the benchmark
└── judges_ruling.wav      Example TTS output
```

The interesting documentation lives in [`dgx-ai-stack/README.md`](dgx-ai-stack/README.md):
architecture diagram, daily ops via `./ai-stack`, per-endpoint curl examples,
and what didn't land.

## Quick start

```sh
cd dgx-ai-stack
cp .env.example .env       # then fill in LITELLM_MASTER_KEY and model paths
./ai-stack                 # bring everything up on the Spark
```

Once the stack is up, the LAN endpoint is `http://dgx-spark.local:4000` and
speaks the OpenAI API: `/v1/chat/completions`, `/v1/audio/speech`,
`/v1/audio/transcriptions`, `/v1/models`.

## Benchmarking the pipeline

```sh
set -a; . dgx-ai-stack/.env; set +a
.venv/bin/python sample-benchmark.py --runs 5 --no-think
```

Reports per-stage latency (STT, LLM TTFT, LLM total, TTS), GPU power and
utilization sampled over SSH, and per-container resident memory. Target is
<8 s end-to-end; with `--no-think` it lands around 4 s.
