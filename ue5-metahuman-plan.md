# Plan: UE5 MetaHuman renderer on a separate Windows + WSL2 box

## Context

`feat/kiosk-face` (browser TalkingHead + amplitude/speakAudio) shipped end-to-end but the
fidelity ceiling isn't enough — open/close jaw on a Ready Player Me head doesn't read as
photoreal speech, and amplitude-only never triggers TalkingHead's full speech-state head
and brow animation. The user has decided to commit it for the record and move to the
**Path B** option from the previous round's renderer-host evaluation: a separate
Windows 11 + WSL2 box running NVIDIA's A2F-3D NIM + native UE5 with MetaHuman, HDMI-direct
to the booth display. Operator stays on a tablet/laptop browser at the Spark's existing UI.

`feat/a2f-3d-spike` already established that A2F-3D doesn't run on the Spark (amd64-only NIM,
GB10 not in the GPU matrix), so the inference moves with the renderer. The Spark stays the
brain — LLM, Kokoro, Parakeet, LiteLLM, orchestrator. The new host is a dumb consumer of
the orchestrator's WS broadcast that owns A2F-3D + UE5 locally.

## Branch strategy

New branch `feat/ue5-metahuman` from `main`. Sibling to the parked `feat/a2f-3d-spike` and
`feat/kiosk-face` branches.

```
git checkout main
git checkout -b feat/ue5-metahuman
```

## Architecture

```
                   Spark (10.10.1.221, unchanged)
                   ┌──────────────────────────────┐
                   │ orchestrator :8080           │
   tablet/laptop ─→│   /ws  (multi-subscriber)   │
   browser        ←│   /operator/{start,estop}    │
   (mutes audio)   └──────────┬───────────────────┘
                              │ JSON events + binary PCM (24 kHz s16le mono)
                              │ over LAN
                              ▼
                   Renderer PC (Windows 11 + WSL2 + RTX 3090 24 GB)
                   ┌──────────────────────────────┐
                   │  UE5.6 booth.exe              │
                   │    WSSubscriber (C++)         │
                   │      ├→ resample 24→16 kHz ───┼→ NIM @ wsl-localhost:52000
                   │      │                        │     (Docker WSL2)
                   │      ├→ USoundWaveProcedural ←┼─ delayed by p99 NIM latency
                   │      └→ ACE plugin            │
                   │           └→ MetaHuman face   │
                   └──────────┬───────────────────┘
                              │ HDMI direct
                              ▼
                       Booth display (TV/monitor)
```

Operator UI stays unchanged in `orchestrator/frontend/`; production mode mutes
its audio output so only the renderer PC drives sound out the booth display.

## What lives in this branch

### Orchestrator (modify)

**`orchestrator/src/display/mod.rs:74-79`** — delete the single-client gate. The broadcast
channel `display_bcast` already supports multiple subscribers via `.subscribe()`; only the
`fetch_add/CONFLICT` check blocks it. Replace with: no check. Drop `ws_clients: Arc<AtomicUsize>`
from `AppState` entirely if nothing else reads it.

**`orchestrator/src/main.rs:77,86`** — adjust AppState init after removing ws_clients.
Confirm broadcast channel size (256) is plenty for the new traffic shape (renderer + 2-3
operator/debug tabs at ~50 ms/chunk = well within slack).

**`orchestrator/frontend/src/audio.ts`** — add a mute path. Insert a `GainNode` between
each BufferSource and `playCtx.destination`; gain reads from a query param `?mute=1` or
a localStorage key. The operator browser sets mute=1 in production; the renderer doesn't
use this codepath at all (it consumes the WS directly in UE5).

**Test:** `cargo test -p booth` still green. Live: open two browser tabs at the kiosk URL,
both receive events, no CONFLICT log line.

### New `renderer/` directory

```
renderer/
├── README.md                            Setup runbook for the renderer PC
├── tools/
│   └── smoke_a2f.py                     gRPC client: send sample wav → NIM → print frames
├── ue5/
│   ├── README.md                        UE5 project setup, ACE plugin install
│   ├── BoothSubscriber/                 Custom UE5 plugin (C++ module)
│   │   ├── BoothSubscriber.uplugin
│   │   ├── Source/BoothSubscriber/
│   │   │   ├── BoothSubscriber.Build.cs
│   │   │   ├── Public/BoothWSClient.h
│   │   │   ├── Public/BoothFaceActor.h
│   │   │   ├── Private/BoothWSClient.cpp   FWebSocketsModule client + JSON parse
│   │   │   ├── Private/BoothFaceActor.cpp  Glue: WS frames → ACE + procedural audio
│   │   │   └── Private/Resample24To16.cpp  Polyphase 24→16 kHz resampler
│   │   └── README.md
│   └── reference-scene.md               Notes on cloning NVIDIA's sample scene
```

**`renderer/README.md`** — Windows 11 + WSL2 + Docker Desktop + GPU-PV install steps,
NGC API key + `docker login nvcr.io` + `docker pull nvcr.io/nim/nvidia/audio2face-3d:2.0`,
NIM smoke test via `tools/smoke_a2f.py`, UE5.6 + sample project clone, ACE plugin Project
Settings, BoothSubscriber plugin install, end-to-end run instructions.

**`renderer/tools/smoke_a2f.py`** — Python gRPC client using the existing `nvidia-audio2face-3d`
PyPI protobuf stubs (the 13 KB pure-python wheel we already validated). Streams a 16 kHz
WAV at `localhost:52000`, prints frames-received count + p99 latency. This is the
hardware-acceptance test — confirms NIM is happy independent of any UE5 work.

**`renderer/ue5/BoothSubscriber/`** — the only non-trivial code. C++ UE5 plugin:
- `BoothWSClient` opens `ws://10.10.1.221:8080/ws` via Epic's `FWebSocketsModule`, parses
  JSON events, decodes `tts_audio`/binary PCM frames/`tts_end` like the existing frontend does.
- `Resample24To16` — polyphase resampler converting Kokoro's 24 kHz s16le → A2F-3D's
  16 kHz input rate. Single-channel mono, no fancy filtering needed.
- `BoothFaceActor` — actor in the scene that owns the MetaHuman + an `UAudioComponent`
  bound to a `USoundWaveProcedural`. On each WS PCM chunk:
  - Resample to 16 kHz and feed via `FACERuntimeModule::Get().AnimateFromAudioSamples(samples, bEndOfSamples=false)`
  - Queue original 24 kHz PCM into `USoundWaveProcedural::QueueAudio()`, **delayed by
    p99 NIM latency** (calibrated during smoke test, hard-coded; ~50-70 ms) so blendshapes
    arrive in time for the spoken audio.
- On `tts_end`: `AnimateFromAudioSamples(empty, bEndOfSamples=true)` flush + drain.

**`renderer/ue5/reference-scene.md`** — instructions to clone NVIDIA's
[Audio2Face-3D sample project](https://github.com/NVIDIA/Audio2Face-3D) (ships
`mh_arkit_mapping_pose_A2F` + the "Apply ACE Face Animations" anim node + stock MetaHuman),
add the BoothSubscriber plugin, set Project Settings → NVIDIA ACE → Default A2F-3D Server
Config (Dest URL `localhost:52000`, no API key for local NIM).

## Hardware target (informational)

| Component | Spec | Note |
|---|---|---|
| GPU | Used RTX 3090 24 GB | A2F-3D NIM auto-maps RTX 30 → A10G profile (~$700) |
| CPU | Ryzen 5 7600X or i5-13600K | |
| RAM | 32 GB DDR5 | UE5 + WSL2 + NIM all on one box |
| Storage | 1 TB NVMe | UE5 + MetaHuman asset libs are large |
| PSU | 850 W | |
| OS | Windows 11 Home 22H2+ | NVIDIA Game Ready ≥555.xx for CUDA 12.5+ in WSL2 |

## Phases

**Mac-side, no hardware needed (executable now):**

0. **Branch from main** (~5 min). `git checkout -b feat/ue5-metahuman`.
1. **Lift the WS gate** (~1 hr). Delete `if prev > 0` block, drop `ws_clients` from
   AppState, run `cargo check` + `cargo test`. Verify two tabs at the kiosk both work.
2. **Operator mute toggle** (~30 min). GainNode in audio.ts driven by `?mute=1`.
3. **Renderer runbook** (~2 hr). `renderer/README.md` with the full Windows + WSL2 + NIM
   + UE5 install sequence.
4. **NIM smoke client** (~2 hr). `renderer/tools/smoke_a2f.py` against a sample 16 kHz WAV.
   Testable on a cloud A10G even before procuring the box.
5. **UE5 plugin skeleton** (~1 day). `renderer/ue5/BoothSubscriber/` C++ source pinned to
   UE5.6. Won't compile on Mac; goal is to deliver a tree the renderer PC can drop into
   an UE5.6 project and `Build.bat` straight through.

**Hardware-side, after procurement:**

6. **Box build + Windows 11 install** (~half day).
7. **WSL2 + Docker Desktop + NIM** (~half day). Pull, run, hit `/v1/health/ready` and run
   `smoke_a2f.py` against localhost:52000.
8. **UE5.6 + sample project + ACE plugin** (~1 day). Stand up NVIDIA's sample scene
   standalone, confirm a canned wav drives the stock MetaHuman.
9. **BoothSubscriber plugin build + first compile pass** (~half day). Budget for
   Windows-specific build cleanup (TCHAR macros, FString::Printf formatters, Linux
   path assumptions that the Mac drafts won't catch).
10. **End-to-end** (~half day). Spark trial → WS → renderer → MetaHuman speaking.
11. **Booth installation** (~half day). HDMI cabling, fullscreen UE5 launch on boot,
    audio routing through HDMI to the booth speakers.

Total: ~3 Mac-side working days + ~3 hardware-side working days.

## Verification

- `cargo check -p booth` + `cargo test -p booth` clean on `feat/ue5-metahuman`.
- Two browser tabs at `http://10.10.1.221:8080/?mute=1` and `http://10.10.1.221:8080/`
  both receive WS events; only the non-mute tab plays audio. No CONFLICT log line.
- `python renderer/tools/smoke_a2f.py sample_plea.wav` (run on the renderer PC against
  WSL2 NIM at localhost:52000) returns ≥1 blendshape frame per 33 ms of input audio,
  p99 latency under 100 ms.
- NVIDIA's sample MetaHuman scene speaks a canned wav before any BoothSubscriber wiring.
- Live end-to-end: trigger trial at Spark UI → BoothSubscriber receives PCM →
  MetaHuman lipsyncs to the verdict TTS on the booth display, audio plays out HDMI in sync,
  no visible lead/lag > 100 ms.

## Risks and mitigations

- **PCM sample rate (Kokoro 24 kHz vs A2F-3D NIM 16 kHz)**. Most likely first-failure mode.
  Resampler is in the BoothSubscriber design; smoke-test it with a known waveform.
- **`AnimateFromAudioSamples` is animation-only**. A parallel `USoundWaveProcedural`
  drives the actual audio playback, delayed by calibrated NIM latency.
- **Audio/blendshape sync drift**. Hard-coded delay matching p99 NIM latency measured
  during Phase 7 smoke test. Re-measure if GPU is swapped.
- **Used RTX 3090 isn't enumerated in the A2F-3D support matrix**, but RTX 30-series is
  documented as auto-mapping to the A10G profile. Phase 7 smoke test is the cheap proof;
  if it fails, swap for a 4090.
- **Pin UE5.6, not 5.7**. ACE plugin docs cite 5.5/5.6 explicitly; 5.7 is bleeding edge
  and brought MetaHuman Linux support but the ACE plugin compatibility with 5.7 isn't
  proven yet.
- **MetaHuman EULA for paid public installation**. Stock MetaHuman in the spike is safe.
  Custom-baked Wettington likeness is a "later" item — re-read Epic's MetaHuman license
  before committing artist time.
- **Mac-drafted C++ UE5 won't compile first try on Windows**. TCHAR macros, FString::Printf
  format strings, and Linux path assumptions differ. Budget half a day on Phase 9 for
  build cleanup; nothing structural should change.
- **Docker Desktop loses GPU after Windows updates** — known issue, recovered via
  `wsl --shutdown` + Docker Desktop restart. Documented in `renderer/README.md`.
