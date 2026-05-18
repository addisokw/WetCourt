# Wet Court Runbook

Three components, three rebuild loops. Each can be rebuilt independently;
only the UE renderer requires the others to be running for an end-to-end test.

```
[Kokoro TTS / Qwen LLM]      <-- runs in dgx-ai-stack (Spark, separate host)
        ^
        | HTTP/WS via LiteLLM
        |
[ Orchestrator ]   booth.exe (Rust)    127.0.0.1:8080
        ^
        | WS (events + audio frames)   HTTP (operator endpoints)
        |
[ UE Renderer ]    BoothRenderer.exe   F1=Start  F2=Plea  F3=E-Stop
        ^
        | gRPC localhost:52000
        v
[ A2F-3D NIM ]     docker container "a2f_test"
```

## Production launch (everything at once)

```pwsh
powershell -File renderer\tools\launch_production.ps1
```

Boots NIM → orchestrator → packaged renderer in order. Bails loudly if any
piece is missing. Inject overrides with `-RendererExe`, `-OrchestratorConfig`,
or `-NimContainerName` if your install paths differ.

## Component rebuilds

### A. Orchestrator (Rust)

Touch this when changing: state machine, LLM prompt, TTS/STT plumbing,
event protocol, hardware driver.

```pwsh
# Stop the running instance first; cargo can't replace booth.exe while it's
# bound to :8080.
$pid = (Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue).OwningProcess
if ($pid) { Stop-Process -Id $pid -Force }

cargo build --manifest-path orchestrator/Cargo.toml          # debug
cargo build --release --manifest-path orchestrator/Cargo.toml # release
```

Tests:

```pwsh
cargo test --manifest-path orchestrator/Cargo.toml --bin booth
```

Boot manually (when you don't want the launcher to run renderer too):

```pwsh
$env:LITELLM_MASTER_KEY = [Environment]::GetEnvironmentVariable('LITELLM_MASTER_KEY','User')
Start-Process -WorkingDirectory .\orchestrator `
  -FilePath .\orchestrator\target\debug\booth.exe `
  -ArgumentList "--config","config.dev.toml" `
  -RedirectStandardOutput .\orchestrator\booth.out.log `
  -RedirectStandardError  .\orchestrator\booth.err.log
```

The boot-time health check (`LLM endpoint reachable`) is your green light;
if it fails the orchestrator refuses to start, which is intentional — we'd
rather know now than after a trial silently 401s through every inference call.

### B. A2F-3D NIM (Docker)

Touch this when changing: character model, blendshape rate limit, GPU
config, TRT engine shape. The configs are bind-mounted from the repo so the
container picks them up on restart — no rebuild required.

```pwsh
# edit renderer\nim\advanced_config.yaml or deployment_config.yaml, then:
docker restart a2f_test

# verify it came back:
docker logs --tail 30 a2f_test
```

If you change the character model itself (e.g. claire → james) the TRT
engines need to be rebuilt for that character. That's a one-time cold start
inside the container — watch the logs until you see `gRPC server: Running...`
(takes ~2-5 min depending on GPU).

Key tunables in `renderer/nim/advanced_config.yaml`:
- `blendshape_streaming_fps` - 500 currently. Bump higher (1000+) or flip
  `burst_mode: true` if A2F latency creeps back up.
- `trt_model_generation` - precision and batch shapes. bs=1 chosen to fit
  the 4070's 8 GB while sharing VRAM with UE.

### C. UE Renderer (Unreal Engine 5.6)

Touch this when changing: scene/level, MetaHuman model, Face_AnimBP,
operator hotkeys, ACE audio buffer settings, anything visual.

#### Editor workflow (iterating)

1. Open `BoothRenderer.uproject` (UE 5.6).
2. Edit assets as needed (Booth.umap, Face_AnimBP, BP_Stephane, etc.).
3. Save (Ctrl+S).
4. PIE (Play in Editor) to test. F1/F2/F3 hotkeys work in PIE thanks to
   the Slate input pre-processor on `ABoothOperatorActor`.

For C++ changes (BoothFaceActor, BoothOperatorActor, BoothWSClient,
BoothSubscriber.Build.cs):
- Small body edits → Live Coding (Ctrl+Alt+F11 from the editor).
- New files / new module deps / virtual functions → close the editor and
  run a full rebuild:
  ```pwsh
  & "C:\Program Files\Epic Games\UE_5.6\Engine\Build\BatchFiles\Build.bat" `
      BoothRendererEditor Win64 Development `
      -Project="C:\Users\Strix-4070\UnrealProjects\BoothRenderer\BoothRenderer.uproject" `
      -WaitMutex -FromMsBuild
  ```

#### Package for production

```pwsh
powershell -File renderer\tools\package_renderer.ps1
# or:
powershell -File renderer\tools\package_renderer.ps1 -Config Shipping -Output D:\Kiosk\Build
```

That produces `C:\WetCourtBooth\Windows\BoothRenderer.exe` plus content
paks — a self-contained install. Copy the `Windows\` directory to the
kiosk and double-click the .exe.

#### Common asset swaps

| What you want to change | Where | Re-cook? |
|---|---|---|
| Background scene / lighting | Booth.umap → Save | yes (package) |
| MetaHuman model (e.g. swap Stephane for new) | BP_Stephane → reparent or duplicate; update BoothFaceActor's TargetCharacter ref in the level | yes |
| Voice (Kokoro voice id) | `orchestrator/config.dev.toml` → `tts_voice` | no (orchestrator only) |
| LLM prompt (judge personality) | `orchestrator/src/inference/verdict.rs` → `SYSTEM_PROMPT` | no (orchestrator only) |
| Blendshape multipliers (face expression intensity) | Face_AnimBP → Apply ACE Face Animations node → BlendshapeMultipliers map | yes |
| Audio pre-buffer | BoothFaceActor instance in level → AudioBufferSeconds | yes (cooked into the actor instance via the level) |
| Hotkey assignments | BoothOperatorActor instance in level → StartKey / PleaKey / EStopKey | yes |
| A2F emotion strength | `orchestrator/src/inference/verdict.rs` → `LLM_EMOTION_SCALE`, `intensity_to_strength` | no (orchestrator only) |
| Blendshape FPS cap | `renderer/nim/advanced_config.yaml` → `blendshape_streaming_fps` | no (just `docker restart a2f_test`) |

## Project-settings recommendations for kiosk build

Set these once in Project Settings (they get baked into the packaged exe):

- **Maps & Modes → Default Maps → Game Default Map** = `/Game/Maps/Booth`
- **Description → Settings → Project Display Name** = "Wet Court of Appeals"
- **Engine → General Settings → Framerate → "Use Less CPU when in Background"** = unchecked (keeps audio running if focus drifts)
- **User Interface → Mouse Properties → "Show Mouse Cursor"** = unchecked for kiosk lockdown

If audio still cuts when the renderer window loses focus, launch it as
"Standalone Game" from the editor while iterating, or in the packaged
build it should already keep playing because there's no editor focus
fighting it.

## Where the bodies are buried

- `orchestrator/booth.out.log` / `booth.err.log` — orchestrator stdout/stderr
- `docker logs --tail 200 a2f_test` — NIM
- UE Output Log → filter `LogBoothFace` / `LogBoothOperator` / `LogBoothWS` / `LogACERuntime` / `LogACEAIMSDK`

Hot signals to look for after a trial:
- `LogBoothFace: tts_emotion: N entries, overall=X override=Y` — LLM emotion arriving
- `LogBoothFace: animation_started: from_session_end_ms=X` — A2F crunch time (low is good; >2s means something regressed in the NIM)
- `LogBoothOperator: operator start -> HTTP 204` — hotkey hit landed
