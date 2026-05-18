# UE5 renderer: BoothSubscriber plugin

A UE5 plugin that subscribes to the WetCourt orchestrator's `/ws` endpoint
and drives a MetaHuman face via NVIDIA ACE Audio2Face-3D. See
[`../../ue5-metahuman-plan.md`](../../ue5-metahuman-plan.md) for the
architecture and phase plan; this directory is Phase 5 of that plan.

## What this plugin gives you

- **`FBoothWSClient`** — `IWebSocket`-based subscriber for `ws://<spark>:8080/ws`.
  Parses the orchestrator's `DisplayEvent` JSON (snake_case `type` enum;
  see `orchestrator/src/display/events.rs`), routes binary frames between
  `tts_audio` and `tts_end` to a callback, and reconnects with the same
  500 ms → 8 s exponential backoff the operator browser frontend uses.
- **`FResample24To16`** — streaming polyphase 24 kHz → 16 kHz int16 mono
  resampler. Kokoro emits at 24 kHz; the A2F-3D NIM consumes 16 kHz.
- **`ABoothFaceActor`** — drop-in scene actor that owns a
  `USoundWaveProcedural` + `UAudioComponent` for booth audio output, plus
  the WS client and resampler. Configurable orchestrator URL and
  pre-roll delay (default 50 ms so blendshapes lead audio).

The ACE plugin calls (`AnimateFromAudioSamples` + the streaming session
open/close) are wrapped in `#if WITH_ACE_RUNTIME` and gated off by default.
The Build.cs has commented `PrivateDependencyModuleNames` entries for the
ACE modules to flip on once the NVIDIA ACE Unreal Plugin is installed.

## Dependencies

| | Required | This box |
|---|---|---|
| UE | 5.6 (per ACE Unreal Plugin 2.5.0 compat matrix) | 5.7 installed; treat as experimental, fall back to 5.6 if Phase 3 sample-scene smoke fails |
| Visual Studio 2022 | C++ workload + "Game development with C++" | installed (verify the C++ + game workloads are checked) |
| NVIDIA ACE Unreal Plugin | latest (2.5.0+) | install on this host alongside this plugin |
| WebSockets | built-in to UE | enabled in `.uplugin` |

## Install

1. Clone the WetCourt repo to wherever you keep UE projects (or symlink
   `renderer/ue5/BoothSubscriber` into your project's `Plugins/`).
2. Create a fresh UE 5.6 C++ project (or open
   [NVIDIA's Audio2Face-3D sample project](https://github.com/NVIDIA/Audio2Face-3D)
   and skip steps 3–5 — the sample already has a MetaHuman + ACE Face
   anim node hooked up).
3. Copy or symlink `renderer/ue5/BoothSubscriber/` into the project's
   `Plugins/` folder. Restart the editor; UE prompts to compile, accept.
4. Enable the NVIDIA ACE Unreal Plugin from Edit → Plugins.
   Restart again.
5. In `BoothSubscriber.Build.cs`, uncomment the
   `PrivateDependencyModuleNames` block that pulls in `ACERuntime` /
   `ACEAudio2Face`. Add `#define WITH_ACE_RUNTIME 1` in your project's
   precompiled header (or pass via Build.cs) so the gated ACE calls
   compile in.
6. In Project Settings → NVIDIA ACE → Default A2F-3D Server Config:
   - Dest URL: `localhost:52000` (NIM on this same box; see
     [`../README.md`](../README.md) for the NIM stand-up)
   - API Key: empty (local NIM, no auth)

## Wire the actor to a MetaHuman

1. Drag a `BoothFaceActor` into the booth scene.
2. Drag a MetaHuman skeletal-mesh actor as a child of `BoothFaceActor`,
   or place them as siblings if you'd rather keep them independent.
3. On the MetaHuman face's anim BP, add the **"Apply ACE Face
   Animations"** anim node (ships with the ACE plugin). Set its input
   to the ACE Face Animation Provider that `BoothFaceActor` writes to.
4. Fill in the actor's `OrchestratorWsUrl` (default
   `ws://10.10.1.221:8080/ws` — the Spark LAN address per the plan).
5. Keep `AudioPlaybackDelaySecs` at 0.05 unless smoke-test latency
   measurements suggest otherwise. The A2F-3D NIM p99 was 31.7 ms in
   our bs=1 smoke; 50 ms is a safe margin.

## What is stubbed

- **ACE wiring inside `BoothFaceActor`** — see the `#if WITH_ACE_RUNTIME`
  blocks. The planned calls per
  [`../../ue5-metahuman-plan.md`](../../ue5-metahuman-plan.md) are
  `FACERuntimeModule::Get().AnimateFromAudioSamples(samples, bEndOfSamples)`
  on each chunk + a session-end flush. Adjust to the actual ACE plugin
  symbol names once installed; this draft has no way to see the real
  headers.

## Validation

Each step has a green/yellow/red signal so you know whether to keep
going or fix the current step first.

1. **Plugin compiles standalone** — green: `BoothSubscriber.uplugin`
   compiles under the project's Build target without ACE enabled. Drop
   `ABoothFaceActor` in an empty level; no errors at PIE start. WS will
   fail to connect (no orchestrator yet), which logs but doesn't crash.
2. **Orchestrator subscribe** — start the orchestrator (see
   `../README.md` and `../../orchestrator/README.md`); the actor's WS
   client connects and you see `audio session start` /
   `audio session end` lines in the UE Output Log when you trigger a
   trial from a browser at the same orchestrator.
3. **Audio plays on the booth speaker** — first short trial: a charge
   utterance comes out the audio component. If the procedural wave
   underruns ("starvation" warnings), `AudioPlaybackDelaySecs` is too
   short; bump to 0.08–0.10.
4. **A2F lipsync** (requires ACE wiring in place) — MetaHuman's lips
   move in sync with the audio, within ~50 ms. If lipsync leads audio,
   your delay is too long; if it lags audio, NIM latency may have grown
   (re-run `../tools/smoke_a2f.py` to re-measure p99 and update the
   delay constant).
