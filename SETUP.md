# Wet Court — Fresh-PC Setup

End-to-end install on a clean Windows box. See [`RUNBOOK.md`](RUNBOOK.md)
once you're past first launch, for per-component rebuild loops.

For ongoing operation on the SAME machine you've already set up,
`renderer\tools\check_setup.ps1` reports what (if anything) is missing.

## Hardware

| | Minimum | Notes |
|---|---|---|
| GPU | NVIDIA RTX 4070 Laptop (8 GB VRAM) | A2F NIM + UE5 share VRAM. 8 GB is tight; 12 GB+ is comfortable. |
| RAM | 32 GB | UE cook spikes to ~16 GB by itself. |
| Disk | 50 GB free on C: | UE engine + DDC + cooked content + NIM image. |
| OS | Windows 10/11 64-bit | Tested on Windows 11. |

## 1. Install software dependencies

| Tool | Version | Where |
|---|---|---|
| Docker Desktop | 4.x (WSL2 backend on) | https://www.docker.com/products/docker-desktop/ |
| NVIDIA drivers + Container Toolkit | recent (550+) | bundled with the driver installer; verify `docker run --gpus all nvidia/cuda:12.2.0-base-ubuntu22.04 nvidia-smi` works |
| Unreal Engine 5.6.x | 5.6.1+ | Epic Games Launcher → Library → +Engine Version → 5.6 → install (~50 GB) |
| Visual Studio 2022 | 17.14 + | https://visualstudio.microsoft.com/ — install with the **"Game development with C++"** workload AND the individual component **".NET Framework 4.8 SDK"** (UE5.6's build chain requires both) |
| Rust | stable | https://rustup.rs — accept defaults, re-open shell after install |
| Git | recent | https://git-scm.com/ |
| NGC account | free | https://ngc.nvidia.com — needed to pull the A2F NIM image |

Optional but useful: `gh` (GitHub CLI) for PR/issue work.

## 2. Clone the repo

```pwsh
cd C:\Users\<you>\Documents     # or wherever you keep code
git clone https://github.com/addisokw/WetCourt.git
cd WetCourt
```

## 3. Set User-scope environment variables

Both keys live in the User registry so they survive shell restarts and
are inherited by child processes started from PowerShell.

```pwsh
# LiteLLM master key — get from the dgx-ai-stack admin (or whoever runs LiteLLM)
[Environment]::SetEnvironmentVariable('LITELLM_MASTER_KEY','<your-litellm-key>','User')

# NGC API key — generate at https://ngc.nvidia.com/setup/api-key
[Environment]::SetEnvironmentVariable('NGC_API_KEY','<your-ngc-key>','User')
```

Close and reopen PowerShell so child processes see them.

## 4. Build the orchestrator

```pwsh
cargo build --release --manifest-path orchestrator\Cargo.toml
```

First build takes 3-8 minutes (compiles tokio + axum + reqwest + a bunch
of small crates). Subsequent builds are incremental.

Verify:

```pwsh
.\orchestrator\target\release\booth.exe --config orchestrator\config.dev.toml
```

You should see `LLM endpoint reachable ... booting display server listening on 127.0.0.1:8080`.
If the health check fails — wrong key, wrong base_url, dgx-ai-stack
unreachable — fix that before continuing. Ctrl-C to stop.

The dev config points at the LiteLLM running on the Spark (`10.10.1.221:4000`).
If your LLM is somewhere else, edit `orchestrator\config.dev.toml` →
`[inference] base_url`.

## 5. Pull and create the A2F-3D NIM container

```pwsh
# Auth to NGC's container registry
docker login nvcr.io
# Username: $oauthtoken
# Password: <the NGC_API_KEY from step 3>

# Pull (~5 GB)
docker pull nvcr.io/nim/nvidia/audio2face-3d:2.0

# Create the long-lived container, bind-mounting the repo's configs
$repo = (Resolve-Path .).Path
docker run -d `
    --name a2f_test `
    --gpus all `
    -p 52000:52000 `
    --restart unless-stopped `
    -v "${repo}\renderer\nim\deployment_config.yaml:/apps/configs/deployment_config.yaml:ro" `
    -v "${repo}\renderer\nim\advanced_config.yaml:/apps/configs/advanced_config.yaml:ro" `
    -e NGC_API_KEY=$env:NGC_API_KEY `
    nvcr.io/nim/nvidia/audio2face-3d:2.0
```

First start takes ~2-5 minutes — the container builds TRT engines for
your specific GPU (engines are cached in a docker volume, subsequent
starts are fast).

Wait for `gRPC server: Running...` in the logs:

```pwsh
docker logs -f a2f_test
# Ctrl-C when you see "Running..."
```

## 6. Set up the UE project

Pick where the project lives (it ships separately from the repo because
it contains MetaHuman assets that aren't checked in). Open the
`BoothRenderer.uproject` in UE 5.6 — accept the "rebuild plugins?" prompt
when asked.

Then symlink the plugin from the repo into the project's Plugins dir so
edits in the repo are reflected in the project:

```pwsh
# Run as Administrator (mklink/junction needs elevation for symlinks)
$projectRoot = 'C:\Users\<you>\UnrealProjects\BoothRenderer'   # adjust
$repo = (Resolve-Path .).Path
Remove-Item "$projectRoot\Plugins\BoothSubscriber" -Recurse -ErrorAction SilentlyContinue
New-Item -ItemType SymbolicLink `
    -Path "$projectRoot\Plugins\BoothSubscriber" `
    -Target "$repo\renderer\ue5\BoothSubscriber"
```

In the editor:
1. **Project Settings → Maps & Modes → Default Maps → Game Default Map** = `/Game/Maps/Booth`
2. Open `Maps/Booth.umap` — confirm there's a **BoothFaceActor**, **BoothOperatorActor**, and **BP_Stephane** (or whatever MetaHuman you're using) in the Outliner. Drop any that are missing from the Place Actors panel.
3. On the BoothFaceActor instance → set **TargetCharacter** to the MetaHuman.
4. Save the level (Ctrl+S).

If you don't have a MetaHuman in the project yet, import one via Quixel
Bridge → Fab → drag any MetaHuman into the level, then point
BoothFaceActor's TargetCharacter at it.

## 7. First package and launch

```pwsh
# Verify everything's ready (read-only)
powershell -File renderer\tools\check_setup.ps1

# Package the renderer (~10-15 min the first time, ~2 min thereafter)
powershell -File renderer\tools\package_renderer.ps1 `
    -Project 'C:\Users\<you>\UnrealProjects\BoothRenderer\BoothRenderer.uproject'

# Launch the whole stack
powershell -File renderer\tools\launch_production.ps1
```

The renderer window should appear, connect to the orchestrator, and the
MetaHuman should be visible. Focus the window and press **F1** to run a
trial.

## 8. What's needed at runtime

Once everything is set up, the kiosk needs all three pieces running:

1. **A2F-3D NIM** (docker container `a2f_test`) — auto-restarts on host boot if you used `--restart unless-stopped` above.
2. **Orchestrator** (`booth.exe`) — started by `launch_production.ps1`, talks to LiteLLM on the Spark over the LAN.
3. **Packaged renderer** (`BoothRenderer.exe`) — also started by `launch_production.ps1`.

The browser frontend at http://127.0.0.1:8080/ is still needed for **plea
mic capture** — keep it open in the background (Chrome/Edge work; grant
microphone permission). F2 in the renderer controls the plea timing;
the browser is just the audio source.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `LLM endpoint health check FAILED` at orchestrator boot | wrong key, wrong URL, or LiteLLM down | check `LITELLM_MASTER_KEY` (User scope) and `orchestrator/config.dev.toml`'s `base_url` |
| `Docker daemon is not running` | Docker Desktop not started | open Docker Desktop, wait for whale icon to settle |
| A2F NIM container exits immediately | TRT engine build failed or GPU not visible | `docker logs a2f_test` — usually GPU passthrough; verify `nvidia-smi` runs inside `docker run --gpus all nvidia/cuda:12.2.0-base-ubuntu22.04 nvidia-smi` |
| Renderer launches but no face animation | curve source missing or A2F unreachable | UE Output Log filter on `LogBoothFace` and `LogACEAIMSDK` |
| Renderer launches into wrong/blank level | Game Default Map not set | Project Settings → Maps & Modes, then re-package |
| F1/F2/F3 don't do anything | BoothOperatorActor not in level | drop one from Place Actors panel, re-save level, re-package |
| Audio mutes when window loses focus | UE PIE focus behavior (only) | in packaged build this doesn't happen; for dev iteration use Standalone Game mode |

For per-component rebuild loops after setup is done, see
[`RUNBOOK.md`](RUNBOOK.md).
