# Renderer PC runbook

Setup and operations notes for the UE5 MetaHuman renderer host. See
[`../ue5-metahuman-plan.md`](../ue5-metahuman-plan.md) for architecture and the
full multi-phase plan.

## This host

| | Plan target | Strix-4070 (this box) |
|---|---|---|
| GPU | RTX 3090 24 GB desktop | RTX 4070 **Laptop 8 GB** |
| Driver | ≥ 555.xx | 576.88 |
| CPU | Ryzen 5 7600X / i5-13600K | Ryzen AI 9 HX 370 |
| RAM | 32 GB | 31 GB |
| OS | Win 11 22H2+ | Win 11 Home 26200 |
| UE | 5.6 (pinned in plan) | **5.7** (deviation, see below) |
| WSL2 | required | installed (Ubuntu + docker-desktop distros, default v2) |
| Docker Desktop | required | 29.4.3 (client+server); nvidia runtime registered; GPU-PV verified |
| Visual Studio 2022 | required (C++ + game dev workload) | installed (workloads TBD) |
| Python | any 3.x | 3.12.4 |

This laptop is being used to drive the **full end-to-end** target — both NIM
inference and UE5 MetaHuman rendering co-resident on the 8 GB GPU. The plan
sized for 24 GB so VRAM is the dominant risk; the mitigations below make it
viable but tight. If the laptop proves inadequate at Phase 3 (sample-scene
smoke) the renderer migrates to a 24 GB+ desktop and this box reverts to
plugin/smoke-test development only.

## VRAM budget

Available on this box: **8 GB**. Co-resident workloads:

| Workload | Plan estimate | Measured (2026-05-18) |
|---|---|---|
| A2F-3D NIM, pre-built RTX 4090 bs=90 profile | 5–6 GB | **7.7 GB** (kills the budget) |
| A2F-3D NIM, local-build bs=1 (chosen) | ~0.9 GB | **1.6 GB** |
| UE5 ACE local inference (regressive v2.3 models) | 2.9–3.0 GiB | TBD (phase 3) |
| UE5 ACE local inference (diffusion v3.0 models) | 4.4+ GiB | n/a (rejected) |
| UE5 + MetaHuman scene (rendering) | 1–2 GB | TBD (phase 3) |
| Windows desktop + WDM | ~0.5 GB | n/a |

**Production config:** NIM with `NIM_DISABLE_MODEL_DOWNLOAD=true`, local-built
TRT engines at `min/optimal/maximum_shape: 1`, `stream_number: 1`,
**regressive** v2.3 models (not diffusion), MetaHuman with strands hair
disabled if needed. Budget: ~5 GB total, ~3 GB headroom.

**`stream_number` is not enough on its own** — verified 2026-05-18. The
pre-built RTX 4090 profile holds ~7.85 GB regardless of `stream_number`
because the engine's persistent workspace dominates, not runtime stream
buffers. The shape settings only apply during local engine generation, which
is what `NIM_DISABLE_MODEL_DOWNLOAD=true` triggers. See section 1.4b.

Sources:
[A2F-3D getting-started (fallback + local-build docs)](https://docs.nvidia.com/ace/audio2face-3d-microservice/2.0/text/getting-started/getting-started.html),
[A2F-3D support matrix](https://docs.nvidia.com/ace/audio2face-3d-microservice/latest/text/support-matrix.html).

## UE engine version

The plan pins **UE 5.6** because that's what the ACE Unreal Plugin (latest
2.5.0, July 2025) officially supports — 2.5.0 added 5.6, dropped 5.4; 2.4.0
added 5.5. **UE 5.7 is not listed in any plugin release.**

This box has UE 5.7 already installed and we're trying the plugin against it
first. The fallback if Phase 3 (sample-scene smoke) fails to build/load:

1. Open the Epic Games Launcher → Library → Engine Versions → "+" → install
   UE 5.6 alongside 5.7. Both coexist.
2. Re-target the sample project to 5.6 in its `.uproject` `EngineAssociation`.

Source:
[ACE Unreal Plugin changelog](https://docs.nvidia.com/ace/ace-unreal-plugin/latest/ace-unreal-plugin-changelog.html).

## Phase 1: WSL2 + Docker Desktop + NIM

User actions, in order. Each step ends with a verifiable check.

### 1.1 Install WSL2 — done

```powershell
wsl --install
# reboot
wsl --status   # should report default version 2
```

Verified on 2026-05-18: `wsl --list --verbose` reports `Ubuntu` (Running, v2)
and `docker-desktop` (Running, v2) with default version 2.

### 1.2 Install Docker Desktop with WSL2 + GPU-PV — done

1. Download Docker Desktop for Windows from
   <https://www.docker.com/products/docker-desktop/>.
2. In the installer, leave "Use WSL 2 instead of Hyper-V" checked.
3. After install + reboot, in Docker Desktop Settings → Resources → WSL
   integration, enable for the default distro.
4. Verify GPU passthrough:

   ```powershell
   docker run --rm --gpus all nvidia/cuda:12.5.0-base-ubuntu22.04 nvidia-smi
   ```

   Should report the RTX 4070 Laptop GPU. If not, see "Docker loses GPU
   after Windows updates" under Known Issues below.

Verified on 2026-05-18:

- `docker version`: 29.4.3 client and server.
- `docker info` shows `nvidia` runtime registered alongside `runc` /
  `io.containerd.runc.v2` — `--gpus all` will route through it.
- `nvidia-smi` from the CUDA 12.5 base image reports RTX 4070 Laptop,
  driver 576.88, CUDA runtime 12.9, 8188 MiB VRAM, idle.

### 1.3 NGC account + image pull

1. Create or log into your NGC account at <https://ngc.nvidia.com>.
2. Generate an API key (Profile → Setup → Generate API Key).
3. From PowerShell:

   ```powershell
   docker login nvcr.io
   # username: $oauthtoken
   # password: <NGC API key>
   docker pull nvcr.io/nim/nvidia/audio2face-3d:2.0
   ```

### 1.4 List profiles for this GPU — done; blocker found

```powershell
docker run --rm --gpus all --entrypoint nim_list_model_profiles `
  nvcr.io/nim/nvidia/audio2face-3d:2.0
```

Verified on 2026-05-18 (release `2.0.0-rc8`): **`No compatible profiles found
using selection criteria`**. The NIM ships pre-built TRT engines only for an
explicit `gpu_device` PCI-ID list (L40S, L4, A10G, A30, B200, RTX 6000
Ampere/Blackwell-SV, RTX 4090, RTX 5080, RTX 5090). The RTX 4070 Laptop's
`2820:10de` is not in any profile, and no `nim_generate_model_profiles`
entrypoint exists in the container.

The plan's assumed "RTX 30/40 → A10G auto-mapping" does not hold for 2.0.

Closest architectural match is RTX 4090 (also Ada, sm_89). TRT engines are
device-specific, so cross-loading is unproven — see 1.4a.

### 1.4a Experiment: force RTX 4090 profile on the 4070 Laptop — done

Verified on 2026-05-18. Both Ada (sm_89), so cross-loading was plausible.

```powershell
$key = [Environment]::GetEnvironmentVariable("NGC_API_KEY", "User")
docker run -d --name a2f_test --gpus all -p 52000:52000 -p 8000:8000 `
  -v a2f_cache:/opt/nim/.cache `
  -e NGC_API_KEY=$key `
  -e NIM_MODEL_PROFILE=c021f3ca049d620f84393cc2e8b1748439a849f4e4813e80343b46f819042f7d `
  nvcr.io/nim/nvidia/audio2face-3d:2.0
```

Result: **the engine loads and serves**, with logs reporting
`RTX GPU detected: NVIDIA GeForce RTX 4070 Laptop GPU`. But the bs=90 RTX 4090
engine consumes **7855 / 8188 MiB** at idle — nothing left for UE5. The
`stream_number: 1` override in `deployment_config.yaml` does not change this:
the engine's persistent workspace dominates, not the runtime stream buffers.

So pre-built fallback profiles get us running but bust the VRAM budget.

### 1.4b Local TRT build at bs=1 — done

Source models ship in the image at
`/opt/models/a2f/nets_fullface/{claire_v2.3.1,james_v2.3.1,multi_v3.3-jamesZara}/`,
and `inference.py` runs `service/generate_trt_models.py` when
`NIM_DISABLE_MODEL_DOWNLOAD=true`. That build script reads
`advanced_config.yaml` `trt_model_generation.{a2f,a2e}.{min,optimal,maximum}_shape`,
which lets us produce small engines sized for the 4070 Laptop.

Result on 2026-05-18 with `min/optimal/maximum_shape: 1` for both a2e and
a2f: **VRAM dropped to 1609 / 8188 MiB** (a 6.2 GB reduction).
Build time was ~150 s for the a2f engine.

This is the production config for this host. See section 1.5.

### 1.5 Production launch — single-stream + local bs=1 build

Configs land in [`renderer/nim/`](nim/):

- `deployment_config.yaml`: `stream_number: 1` (single kiosk; no concurrent users)
- `advanced_config.yaml`: `trt_model_generation` min/opt/max shape = 1 for a2e+a2f

Launch:

```powershell
docker volume create a2f_cache
docker volume create a2f_trt_cache
$key = [Environment]::GetEnvironmentVariable("NGC_API_KEY", "User")
docker run -d --name a2f --gpus all -p 52000:52000 -p 8000:8000 `
  -v a2f_cache:/opt/nim/.cache `
  -v a2f_trt_cache:/tmp/a2x `
  -v "$PWD\renderer\nim\deployment_config.yaml:/apps/configs/deployment_config.yaml:ro" `
  -v "$PWD\renderer\nim\advanced_config.yaml:/apps/configs/advanced_config.yaml:ro" `
  -e NGC_API_KEY=$key `
  -e NIM_DISABLE_MODEL_DOWNLOAD=true `
  nvcr.io/nim/nvidia/audio2face-3d:2.0
```

The `a2f_trt_cache` volume persists the locally-built engines across container
restarts; without it, every restart pays the ~150 s build cost. **Delete this
volume any time `advanced_config.yaml` changes** so engines regenerate at the
new shape.

### 1.6 Health + smoke client — done

```powershell
curl http://localhost:8000/v1/health/ready
python renderer\tools\smoke_a2f.py
```

Verified on 2026-05-18 with the bs=1 engines and a synthesized 2 s sawtooth:

- Health: `{"object":"health.response","message":"ready","status":"ready"}`
- TTFB (first response): **31.7 ms** (plan target: <100 ms p99)
- Frame inter-arrival p50/p99: **11.3 / 11.8 ms** — very steady
- 62 animation frames for 2 s audio = 31 fps (matches A2F cadence)
- Header advertises 68 blendshapes (ARKit set + tongue)
- 1.45x realtime processing

### 1.6 Health + smoke client

```powershell
curl http://localhost:52000/v1/health/ready
python renderer/tools/smoke_a2f.py ..\sample_plea.wav
```

See [`tools/smoke_a2f.py`](tools/smoke_a2f.py) for what it does.

## Phase 2-5

Backfilled as each phase completes. See `../ue5-metahuman-plan.md` for the
full plan and verification criteria.

## Known issues

- **Docker Desktop loses GPU after Windows updates** — recovered via
  `wsl --shutdown` from PowerShell, then restart Docker Desktop. Re-run the
  `nvidia-smi` GPU-PV check after.
- **UE 5.7 + ACE plugin 2.5.0** — unproven combination. Fallback to 5.6 if
  Phase 3 fails.
- **UE 5.5 known issue** — the plugin docs note crashes when Sound Wave assets
  have `Loading Behavior Override = Force Inline`. Doesn't affect 5.7 but
  noted in case we fall back to 5.5.
