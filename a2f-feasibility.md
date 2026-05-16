# Audio2Face-3D on the Spark — feasibility study

Branch: `feat/a2f-3d-spike`. Decision document for whether NVIDIA Audio2Face-3D
can run alongside the booth's AI stack on the DGX Spark (GB10, sm_121, arm64),
and what to do about it.

## Bottom line

**A2F-3D on the Spark is not supported by NVIDIA, and we have now confirmed
empirically that it does not run.** Per the official
[Audio2Face-3D NIM getting-started](https://docs.nvidia.com/ace/audio2face-3d-microservice/latest/text/getting-started/getting-started.html):

- Platform: **x86_64 Linux only.** The Spark is aarch64.
- Pre-generated GPU profiles: A10G, A30, L4, L40S, RTX 4090, RTX 5080, RTX
  5090, RTX 6000 Ada, RTX PRO 6000 Blackwell, B200. **GB10 (sm_121) is not in
  the list.**

We pulled `nvcr.io/nim/nvidia/audio2face-3d:2.0` (~9.65 GB compressed,
~15.9 GB on disk) onto the Spark and tried to run it:

```
$ docker inspect nvcr.io/nim/nvidia/audio2face-3d:2.0 --format '{{.Architecture}}/{{.Os}}'
amd64/linux

$ docker run --rm --gpus all -e NGC_API_KEY=… nvcr.io/nim/nvidia/audio2face-3d:2.0
WARNING: The requested image's platform (linux/amd64) does not match the
  detected host platform (linux/arm64/v8) and no specific platform was requested
exec /bin/bash: exec format error
```

The image's entrypoint is an amd64 ELF binary; the arm64 kernel cannot
execute it. `--platform linux/amd64` + qemu-user-static emulation would
fail the GPU passthrough (CUDA can't tunnel through qemu), so emulation is
not a serviceable workaround.

The PyPI `nvidia-audio2face-3d` package (v1.3.0, py3-none-any, 13 KB) is a
gRPC client stub library only. It does not contain the inference engine.
Installing it from pip does not give us a local renderer; it gives us the
protobuf surface for talking to a backend that exists somewhere else.

The remaining theoretical path on the Spark is building
[NVIDIA/Audio2Face-3D-SDK](https://github.com/NVIDIA/Audio2Face-3D-SDK) from
source with TensorRT engine generation targeted at sm_121. That is a multi-day
effort with no documented success on GB10.

## What we built and verified anyway

The transport plumbing for any future face renderer (UE5 MetaHuman host,
browser glTF head, etc.) is wired and exercised end-to-end. The branch is
mergeable as-is; defaults leave the feature disabled and behavior unchanged
from `main`.

| Component | Status |
|---|---|
| `dgx-ai-stack/audio2face/Dockerfile` (+ `Dockerfile.slim` on the Spark) | Built, runs |
| `dgx-ai-stack/audio2face/server.py` (FastAPI WS `/v1/face/stream`, ARKit-52 weights @ 30 Hz, amplitude-stub fallback) | Running |
| `dgx-ai-stack/docker-compose.yml` (+ Spark-local `docker-compose.override.yml`) | Service `audio2face:9000` integrated into `ai` network |
| `dgx-ai-stack/bench-a2f.py` (standalone WS bench: frame rate, latency, GPU power/util, container memory) | Verified |
| `orchestrator/src/inference/a2f.rs` (Tokio WS client, bounded channels, drop-on-full) | Compiled |
| `A2fConfig` in `config.rs` (default `enabled=false`), `[a2f]` block in `config.toml` | Plumbed |
| `BlendshapeFrame { weights, audio_offset_ms }` variant on `DisplayEvent` | Added |
| `synth_into_display` and `tts::real` take `Option<&A2fSession>`; verdict path opens session + spawns forwarder | Wired |
| `cargo check` + 9 unit tests | Pass |

### Plumbing smoke test

Run against the slim amplitude-stub server (no real model) on the Spark via
`docker compose up -d audio2face`:

```
=== sample_plea.wav ===
  audio_s                 : 24.90
  bytes_sent              : 1195200
  frames_received         : 747
  frame_rate_hz           : 30.0
  first_frame_ms          : 8
  last_byte→last_frame_ms : -49   (negative = server emits frame for chunk N
                                   before bench paces chunk N+1, expected for
                                   the synthetic stub)
```

This proves the WebSocket protocol, framing, channel back-pressure, and bench
harness all work end-to-end. What it does not measure is the inference cost
of the real A2F-3D model on this hardware, because we have no way to run it.

## Options

### 1. Move the face renderer off the Spark
Recommended. The plan already enumerated the candidates:

| Host | Notes |
|---|---|
| **x86 Windows/Linux PC with a supported RTX or B200** | First-class. A2F-3D NIM + UE5 LiveLink plugin work as documented. Adds a second box. |
| **arm64 Mac (M3 Pro/Max+)** | UE5 + MetaHuman are native, but the **NVIDIA A2F-3D LiveLink plugin is x86 only**. The Mac would have to consume the blendshape stream over our own gRPC/WS path — feasible since the plugin's value is transport convenience, not the data path. Pixel Streaming from Apple Silicon lacks NVENC; drive the booth display via HDMI directly to sidestep. |
| **Browser (three.js + glTF head reading our `BlendshapeFrame` events)** | Lower fidelity. Cheapest. The transport on this branch already emits the events; only a renderer in `orchestrator/frontend/` is missing. |

The orchestrator-side work is the same regardless of which host actually
renders: it emits ARKit-52 weights + `audio_offset_ms` over the existing
display WebSocket. Renderers subscribe.

### 2. From-source SDK build for sm_121
[NVIDIA/Audio2Face-3D-SDK](https://github.com/NVIDIA/Audio2Face-3D-SDK).
C++/CUDA + TensorRT engine generation. Mirror the `whisper-cpp` pattern
(`dgx-ai-stack/whisper-cpp/Dockerfile`) which is documented as a from-source
GB10 build with `-DGGML_CUDA=1`. Expect days, not hours. Probably the right
move only if avoiding a second host is a hard requirement.

### 3. Lower-fidelity local lip-sync
Phoneme→viseme mapping is cheap and runs anywhere. Lower fidelity than
A2F-3D. The `BlendshapeFrame` event shape on this branch is renderer-agnostic
ARKit-52, so a viseme-driven backend can fill the same channel and the
frontend stays unchanged.

## Recommendation

Take option 1, browser variant, as the Phase 2 follow-up: a three.js +
glTF head in `orchestrator/frontend/` subscribing to `BlendshapeFrame` and
PCM. It is the only path that finishes the booth as a self-contained Spark
box and gets us a face in front of defendants quickly. Pair it with a viseme
backend (option 3) on the Spark to keep the high-fidelity path open — when
the second-host question is resolved, swap the backend without touching the
frontend.

## Branch state

- `feat/a2f-3d-spike`: all of the above, mergeable. Defaults disable the
  feature; no behavior change from `main`.
- Spark side: `audio2face` slim container running in amplitude-stub mode on
  port 9000 (LAN-private at `127.0.0.1:9000` only). Override at
  `/home/s65/ai-stack/docker-compose.override.yml`; remove the file and
  re-up to revert.
