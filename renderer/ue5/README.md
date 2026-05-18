# UE5 renderer: BoothSubscriber plugin

A UE5 plugin that subscribes to the WetCourt orchestrator's `/ws` endpoint
and drives a MetaHuman face via NVIDIA ACE Audio2Face-3D. See
[`../../ue5-metahuman-plan.md`](../../ue5-metahuman-plan.md) for the
architecture and phase plan.

## What this plugin gives you

- **`FBoothWSClient`** — `IWebSocket`-based subscriber for `ws://<spark>:8080/ws`.
  Parses the orchestrator's `DisplayEvent` JSON (snake_case `type` enum;
  see `orchestrator/src/display/events.rs`), routes binary frames through
  a JSON-content filter so text frames don't pollute the audio queue, and
  reconnects with 500 ms → 8 s exponential backoff.
- **`FResample24To16`** — streaming polyphase 24 kHz → 16 kHz int16 mono
  resampler. Kokoro emits at 24 kHz; the A2F-3D NIM consumes 16 kHz.
- **`ABoothFaceActor`** — drop-in scene actor that owns a
  `UACEAudioCurveSourceComponent` as its root. Per audio session: opens
  an A2F-3D gRPC stream via `IA2FProvider::CreateA2FStream`, streams
  resampled 16 kHz int16 to the NIM, and pipes the original 24 kHz audio
  through `IA2FPassthroughProvider::EnqueueOriginalSamples` so ACE plays
  it synced with the blendshapes.

## Dependencies

| | Required | This box (verified 2026-05-18) |
|---|---|---|
| UE | 5.6 (per ACE Unreal Plugin 2.5.0 compat matrix) | UE 5.6.1 |
| Visual Studio 2022 | C++ desktop workload | 17.14 with MSVC 14.44.35207 |
| NVIDIA ACE Unreal Plugin | `NV_ACE_Reference` v2.5.0 (Win64) | 2.5.0-rc3 |
| .NET Framework SDK 4.6+ | for editor target build (SwarmInterface) | 4.8 |

## Install

1. Clone the WetCourt repo somewhere outside your UE project.
2. Create a fresh UE 5.6 C++ project (or use NVIDIA's Kairos sample if
   you want a pre-wired MetaHuman scene). Point its `EngineAssociation`
   at `5.6`, use `BuildSettingsVersion.V5` and
   `EngineIncludeOrderVersion.Unreal5_6` in the target.cs files (V6 /
   Unreal5_7 are 5.7-only and won't compile on 5.6).
3. Symlink `renderer/ue5/BoothSubscriber/` into the project's `Plugins/`
   folder (PowerShell: `cmd /c mklink /J <project>/Plugins/BoothSubscriber
   <repo>/renderer/ue5/BoothSubscriber`).
4. Download `NV_ACE_Reference-UE5.6-vX.Y.Z.zip` from
   <https://developer.nvidia.com/ace-for-games> (requires NVIDIA dev
   account login + EULA). Extract directly into `<project>/Plugins/`
   so you end up with `<project>/Plugins/NV_ACE_Reference/`.
5. **One source patch the plugin ships with**: UE 5.6 promotes
   `FInputGesture` deprecation to a hard error. Edit
   `Plugins/NV_ACE_Reference/Source/OmniverseLiveLink/Private/OmniverseLiveLinkCommands.cpp`:
   replace `FInputGesture()` → `FInputChord()` on the single line that
   uses it (around line 30).
6. Add the ACE plugin to your `.uproject`'s `Plugins[]` alongside
   BoothSubscriber:
   ```json
   { "Name": "BoothSubscriber",  "Enabled": true },
   { "Name": "NV_ACE_Reference", "Enabled": true }
   ```
7. Build from the command line (faster + better errors than the editor's
   compile-on-load):
   ```powershell
   & "C:\Program Files\Epic Games\UE_5.6\Engine\Build\BatchFiles\Build.bat" `
       BoothRendererEditor Win64 Development `
       -Project="C:\path\to\YourProject.uproject" -WaitMutex
   ```
8. Open the project. ACE should print
   `LogACECore: Loaded ACE plugin version 2.5.0-...` in the Output Log.

## Place the actor

1. Drop a `BoothFaceActor` (search "BoothFace" in the Place Actors panel)
   into your level.
2. In Details:
   - `Orchestrator Ws Url` → `ws://127.0.0.1:8080/ws` for local dev (the
     IPv4 form; `localhost` falls back to IPv6 first on Windows and the
     orchestrator's dev config only binds IPv4), or
     `ws://10.10.1.221:8080/ws` for the Spark.
   - `A2F Url` → `http://localhost:52000` (the NIM on this same box)
     — default is already this for the dev setup.
   - `A2F Provider Name` → `RemoteA2F` (the gRPC-via-NIM provider).
3. Save the level, Alt+P to PIE. Expected log lines at start:
   ```
   LogBoothFace: ACE: configured provider RemoteA2F -> http://localhost:52000 (pre-warmed)
   LogBoothWS: connected
   LogBoothFace: ws connected
   ```
4. Trigger a trial from a browser at `http://localhost:8080`. Expected:
   ```
   LogBoothFace: audio session start: format=pcm_s16le_24000
   LogACEA2FRemote: Connected to A2F-3D service at URL:"http://localhost:52000"
   LogACERuntime: [ACE SID 0 callback] received <N> animation samples, <M> audio samples ...
   LogACERuntime: start playing audio on BoothFaceActor ...
   ```

## Wire a MetaHuman to the curve source (Phase 11)

Not yet done in this repo's setup — the actor currently plays audio
through the curve source but no character consumes the curves.

To finish lipsync:

1. Add a MetaHuman to the scene (MetaHuman Creator + Quixel Bridge, or
   import an existing asset).
2. Either parent it under the `BoothFaceActor` or place it nearby —
   the curve source feeds the actor's components, not a specific
   character.
3. On the MetaHuman's face Anim BP, add the **Apply ACE Face Animation**
   anim node. Bind its input to the `BoothFaceActor`'s `AceCurveSource`.

## Known UE 5.7 vs 5.6 differences (relevant if you ever migrate)

| | UE 5.6 | UE 5.7 |
|---|---|---|
| `BuildSettingsVersion` enum | up to `V5` | adds `V6` |
| `EngineIncludeOrderVersion` | up to `Unreal5_6` | adds `Unreal5_7` |
| `FWebSocketsModule::CreateWebSocket(Url, Protocol)` | rejected by axum with HTTP 400 if `Protocol` is set | works either way |
| ACE plugin support | official | unofficial / build-from-source |

If you migrate to 5.7, also flip the WebSocket subprotocol back to
`TEXT("ws")` or some real subprotocol if your server negotiates one.

## Troubleshooting

- **`HTTP/1.1 400 Bad Request: Connection header did not include 'upgrade'`** —
  UE 5.6 libwebsockets sends a malformed upgrade request when a
  subprotocol is set. BoothWSClient calls `CreateWebSocket(Url)` with
  no subprotocol on purpose; don't add one back.
- **`LogACEAIMSDK: Error: Audio2Face inference instance not provided`
  on session 0 only** — gRPC connection establishment is racing with
  the first `tts_end`. BoothFaceActor calls
  `UACEBlueprintLibrary::AllocateA2F3DResources(A2FProviderName)` in
  BeginPlay to pre-warm; if you removed that call, restore it.
- **Doubled audio** — fixed in commit Phase 11 (this one): the manual
  `USoundWaveProcedural` path was removed in favour of ACE's curve
  source playback. If you re-introduce manual playback, mute the curve
  source via `AceCurveSource->Volume = 0.0f`.
- **`LogLiveCoding: Error: Cannot enable module ... nvinfer ...`** —
  Live Coding can't patch the NVIGI third-party DLLs (TensorRT, CUDA,
  gRPC). Benign — they're loaded lazily by AIM, not by UE itself.
