# Wet Court of Appeals — DGX Spark AI Stack

A single OpenAI-compatible endpoint on an NVIDIA DGX Spark exposing **chat
completions, text-to-speech, and audio transcription** for the booth.

```
LAN ─► :4000  litellm  (the only thing exposed on the LAN)
              │  /v1/chat/completions       ─► vllm-nvfp4   :8000  (Qwen3.6-35B-A3B-NVFP4, vLLM)
              │  /v1/audio/speech           ─► kokoro       :8880  (Kokoro-FastAPI, 67 voices)
              │  /v1/audio/transcriptions   ─► parakeet     :8082  (NVIDIA Parakeet TDT 0.6B v2)
              │  /v1/models
              └──────────── ai-stack docker network (private) ────────────
```

Hardware: DGX Spark (`user@dgx-spark`) — Grace Blackwell GB10 (sm_121),
CUDA 13.0, driver 580.142, Ubuntu 24.04 aarch64, 121 GiB unified memory.

## Repo layout

```
dgx-ai-stack/
├── ai-stack                  Wrapper script — run from the Mac to control the stack
├── docker-compose.yml        Services (vllm-nvfp4, parakeet, kokoro, litellm, orchestrator)
├── .env                      Secrets + model paths (chmod 600, do not commit)
├── .env.example              Template
├── litellm/config.yaml       Routes /v1/* to the right backend
├── llama-cpp/Dockerfile      (legacy) old llama-server image — superseded by vLLM NVFP4
├── parakeet/
│   ├── Dockerfile            NeMo ASR on top of nvcr pytorch:25.11-py3
│   └── server.py             OpenAI /v1/audio/transcriptions wrapper
└── whisper-cpp/Dockerfile    whisper.cpp built for arm64 + CUDA 13 + sm_121
                              (kept as a fallback STT — not in the active compose)
```

## Daily operations — `./ai-stack`

All commands run from this directory on the Mac. SSH key auth must be set up
to `user@dgx-spark`.

```sh
./ai-stack                    # bring everything up (detached)
./ai-stack down               # stop and remove containers
./ai-stack status             # show service health
./ai-stack logs               # tail combined logs (Ctrl-C to exit)
./ai-stack logs vllm-nvfp4    # tail one service
./ai-stack restart kokoro     # restart one service
./ai-stack pull               # pull updated images from registries
./ai-stack ssh                # drop into the ai-stack/ dir on the Spark
```

Override the host or directory with `AI_STACK_HOST=` and `AI_STACK_DIR=` env vars.

The compose file has `restart: unless-stopped` on every service, so after a
reboot of the Spark the stack comes back automatically — you only need
`./ai-stack up` after a deliberate `down` or after editing config.

## What's running, and how big

| Service | Image | Port (internal) | Resident memory | Role |
|---|---|---|---|---|
| `vllm-nvfp4` | `nvcr.io/nvidia/vllm:26.05.post1-py3` | 8000 | ~60 GiB (model ~20 GiB + KV cache, `--gpu-memory-utilization 0.5`) | Qwen3.6-35B-A3B-**NVFP4** — chat **and** Frigate vision |
| `parakeet` | `local/parakeet:latest` (NeMo on NGC pytorch:25.11) | 8082 | ~2 GiB | STT / `/v1/audio/transcriptions` |
| `kokoro` | `kokoro-tts-arm64:latest` | 8880 | ~2 GiB | TTS / `/v1/audio/speech` |
| `litellm` | `ghcr.io/berriai/litellm:main-stable` | 4000 (LAN-exposed) | ~1 GiB | OpenAI router |

Working set under live load: ~28 GiB out of 121 GiB. Power: ~10 W idle,
~36 W during inference, ~43 W peak.

## Calling the endpoint

Set base URL and key once:

```sh
export BASE=http://dgx-spark.local:4000
export KEY=$(grep ^LITELLM_MASTER_KEY .env | cut -d= -f2)
```

### Chat — Qwen3.6-35B-A3B (NVFP4 on vLLM)

The model name is unchanged (`qwen3.6-35b-a3b`), so **clients need no changes** — it
now routes to the vLLM NVFP4 backend instead of llama.cpp. litellm **injects
`enable_thinking: false` by default** (this model is a reasoning model; without it you
get empty/slow responses), so callers no longer need to send `chat_template_kwargs`.

```sh
curl -s $BASE/v1/chat/completions -H "Authorization: Bearer $KEY" \
  -H 'Content-Type: application/json' -d '{
    "model": "qwen3.6-35b-a3b",
    "messages": [{"role":"user","content":"Hello"}],
    "max_tokens": 200
  }' | jq '.choices[0].message.content'
```

Throughput: **~70 tok/s** single-stream decode (vs ~48 on the old llama.cpp Q4),
scaling to ~250 tok/s aggregate under batch (this matches NVIDIA's own DGX Spark
reference for this model). To re-enable reasoning for a specific call, pass
`"chat_template_kwargs": {"enable_thinking": true}`.

**Vision:** the NVFP4 checkpoint is multimodal (`image_url` content works on
`qwen3.6-35b-a3b`). **The Frigate camera pipeline uses the same model** via the
`qwen3.6-35b-a3b` alias (~0.6 s for a 720p doorstep image) — see
`homelab/services/frigate.md`. A 720p doorstep package describes in ~0.6 s.

**NVFP4 backend note:** vLLM auto-selects the **MARLIN** weight-only FP4 MoE backend on
the GB10 (sm_121) — the same backend NVIDIA's reference uses. True native FP4 tensor
cores need an `sm_121a` build (vLLM PR #40082 + patches), which only helps compute-bound
high-concurrency and isn't worth it here. See `docker-compose.yml` for the `flashinfer_b12x`
opt-in if ever needed.

### TTS — Kokoro

```sh
curl -s $BASE/v1/audio/speech -H "Authorization: Bearer $KEY" \
  -H 'Content-Type: application/json' -d '{
    "model": "kokoro-tts",
    "voice": "bm_george",
    "input": "Court is now in session.",
    "response_format": "wav"
  }' -o ruling.wav
```

Voices: 67 available — `am_*` American male, `af_*` American female,
`bm_*` British male (e.g. `bm_george` is the default judge voice),
`bf_*` British female, plus other locales. List them by hitting Kokoro's
`/v1/audio/voices` endpoint directly on `:8880` from inside the network.

### STT — Parakeet (model name `whisper-1` for OpenAI compatibility)

```sh
curl -s $BASE/v1/audio/transcriptions -H "Authorization: Bearer $KEY" \
  -F file=@plea.wav -F model=whisper-1 | jq .text
```

Latency: ~220 ms warm for a 25 s clip. Transcript quality is meaningfully
better than whisper.cpp on accents and partial words.

### List models

```sh
curl -s $BASE/v1/models -H "Authorization: Bearer $KEY" | jq '.data[].id'
# "qwen3.6-35b-a3b"
# "kokoro-tts"
# "whisper-1"
```

## Configuration files

`docker-compose.yml` reads from `.env`:

| Variable | Used by | Purpose |
|---|---|---|
| `LITELLM_MASTER_KEY` | litellm | Bearer token for the LAN endpoint |
| `LLAMA_MODEL_DIR` | vllm-nvfp4 | Host path mounted read-only at `/models` (holds the NVFP4 checkpoint dir) |
| `LLAMA_MODEL_FILE`, `LLAMA_CTX`, `LLAMA_NGL`, `LLAMA_BUILD_CONTEXT` | (legacy) | Unused since the move to vLLM NVFP4 — only the archived llama.cpp service read them |
| `WHISPER_MODEL_DIR`, `WHISPER_MODEL_FILE` | (legacy whisper.cpp) | Unused by Parakeet but kept for the fallback whisper service |
| `LITELLM_PORT` | litellm | LAN-exposed port (default 4000) |

Files expected in `LLAMA_MODEL_DIR` (`/models`):

| Path | For | Source |
|---|---|---|
| `Qwen3.6-35B-A3B-NVFP4/` (dir, ~22 GB) | **active** — chat + Frigate vision (vLLM) | [nvidia/Qwen3.6-35B-A3B-NVFP4](https://huggingface.co/nvidia/Qwen3.6-35B-A3B-NVFP4) |
| `Qwen3.6-35B-A3B-UD-Q4_K_M.gguf` + `mmproj-…-BF16.gguf` | (legacy) old llama.cpp chat | [unsloth/Qwen3.6-35B-A3B-GGUF](https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF) |
| `Qwen3VL-30B-A3B-Instruct-Q4_K_M.gguf` + `mmproj-…-F16.gguf` | (legacy) old Frigate VLM | [unsloth/Qwen3-VL-30B-A3B-Instruct-GGUF](https://huggingface.co/unsloth/Qwen3-VL-30B-A3B-Instruct-GGUF) |

The legacy GGUFs are no longer loaded by the stack — keep them only as a llama.cpp
fallback, else delete to reclaim ~42 GB.

`litellm/config.yaml` declares the models and where to route them. Edit
this file then `./ai-stack restart litellm` to pick up changes (LiteLLM does
not hot-reload its config).

## Benchmarking

End-to-end benchmark is `sample-benchmark.py` in this directory.

```sh
set -a; . .env; set +a
../.venv/bin/python sample-benchmark.py --runs 5 --no-think
```

Reports per-stage timing (STT, LLM TTFT, LLM total, TTS), GPU power and
utilization (sampled over SSH at 250 ms cadence), and per-container resident
memory. With `--no-think` the booth pipeline lands at ~4 s end-to-end, well
inside the framework target of <8 s.

## Troubleshooting

**A service won't start.** `./ai-stack logs <service>` shows recent output.
Most failures here are CUDA/driver or HuggingFace-gating issues that surface
clearly in the first 50 lines.

**`/v1/chat/completions` returns empty content.** Qwen3.6 went into thinking mode
and burned the token budget on reasoning. litellm injects `enable_thinking: false`
by default, so this only happens if you explicitly re-enabled thinking — drop
`enable_thinking: true` or raise `max_tokens` to 4000+.

**Empty TTS response or hung TTS request.** Kokoro can be sensitive to
unusual unicode in `input`. Strip control characters; the pipeline already
strips the `VERDICT: GUILTY` line before sending the rest to TTS.

**Parakeet first request is slow.** First call after container start is
~1.2 s (model warm-up); subsequent calls are ~220 ms. If the booth has been
idle for hours, `curl $BASE/v1/models` once before opening — that's enough to
re-warm everything.

**vLLM reserves memory up front.** Unlike the old llama.cpp service, vLLM grabs its
full budget at start (~60 GiB at `--gpu-memory-utilization 0.5`): model ~20 GiB + KV
cache. Lower the util if you need more GPU headroom for other work.

**vLLM first start is slow (~2–3 min).** Weight load + CUDA-graph compile. The compile
is cached in the `vllm-cache` volume, so subsequent restarts are quicker.

## What didn't land (and why)

- **Orpheus 3B TTS** — better-quality theatrical voice with inline emotion
  tags, but vLLM v1 engine init hangs at NCCL `parallel_state` setup on this
  exact Blackwell sm_121 + arm64 + CUDA 13 + vllm 26.03 stack. The
  `orpheus_tts` library's sync-over-async wrapper deadlocks under FastAPI as
  a separate, downstream issue. Both code paths are documented in the git
  history if vLLM's Blackwell support stabilizes later.
- **Speaches / faster-whisper** — no working arm64+CUDA wheel for ctranslate2.
  Use Parakeet (faster *and* more accurate on English), or the bundled
  whisper.cpp fallback if multilingual is ever needed.
