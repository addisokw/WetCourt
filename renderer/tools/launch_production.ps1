# Boot the full kiosk stack in order:
#   1. A2F-3D NIM (docker)
#   2. Orchestrator (booth.exe — pulls LITELLM_MASTER_KEY from User registry)
#   3. Packaged UE renderer
#
# Usage:
#   powershell -File renderer\tools\launch_production.ps1
#   powershell -File renderer\tools\launch_production.ps1 -RendererExe D:\Kiosk\Build\Windows\BoothRenderer.exe

param(
    [string]$RendererExe        = 'C:\WetCourtBooth\Windows\BoothRenderer.exe',
    [string]$OrchestratorRoot   = 'C:\Users\Strix-4070\Documents\WetCourt\orchestrator',
    [string]$OrchestratorConfig = 'config.dev.toml',
    [string]$NimContainerName   = 'a2f_test'
)
$ErrorActionPreference = 'Stop'

# --- 1. A2F NIM ---------------------------------------------------------
$nimStatus = docker inspect -f '{{.State.Status}}' $NimContainerName 2>$null
if (-not $nimStatus) {
    throw "Container $NimContainerName not found. Create it first (docker run ... --name $NimContainerName ...)."
}
if ($nimStatus -ne 'running') {
    Write-Host "Starting NIM container $NimContainerName..." -ForegroundColor Cyan
    docker start $NimContainerName | Out-Null
}
Write-Host "[ok] A2F NIM: $NimContainerName running" -ForegroundColor Green

# --- 2. Orchestrator ----------------------------------------------------
$boothExe = Join-Path $OrchestratorRoot 'target\debug\booth.exe'
if (-not (Test-Path $boothExe)) {
    throw "Orchestrator binary missing: $boothExe`nBuild with: cargo build --manifest-path orchestrator/Cargo.toml"
}
# Inject the User-scope LiteLLM key so the spawned process inherits it.
# (Cygwin / non-PowerShell shells don't see User-scope env until reopened.)
$key = [Environment]::GetEnvironmentVariable('LITELLM_MASTER_KEY','User')
if (-not $key) { throw "LITELLM_MASTER_KEY missing from User registry." }
$env:LITELLM_MASTER_KEY = $key

# Bail if 8080 is already held (a stale booth.exe will silently fail to bind).
$held = Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue
if ($held) {
    throw "Port 8080 already in use (PID $($held.OwningProcess)). Stop the existing process first."
}

$orchProc = Start-Process -FilePath $boothExe -ArgumentList "--config",$OrchestratorConfig `
    -WorkingDirectory $OrchestratorRoot `
    -RedirectStandardOutput (Join-Path $OrchestratorRoot 'booth.out.log') `
    -RedirectStandardError  (Join-Path $OrchestratorRoot 'booth.err.log') `
    -WindowStyle Hidden -PassThru
Write-Host "Orchestrator PID $($orchProc.Id). Waiting for :8080..."

$deadline = [DateTime]::UtcNow.AddSeconds(30)
while ([DateTime]::UtcNow -lt $deadline) {
    if (Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue) {
        Write-Host "[ok] Orchestrator bound to :8080" -ForegroundColor Green
        break
    }
    Start-Sleep -Milliseconds 500
}
if (-not (Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue)) {
    throw "Orchestrator did not bind to :8080 within 30s. Check $OrchestratorRoot\booth.out.log."
}

# --- 3. UE renderer -----------------------------------------------------
if (-not (Test-Path $RendererExe)) {
    throw "Renderer not packaged: $RendererExe`nRun: renderer\tools\package_renderer.ps1"
}
Start-Process -FilePath $RendererExe | Out-Null
Write-Host "[ok] Renderer launched: $RendererExe" -ForegroundColor Green

Write-Host ""
Write-Host "Kiosk up. Hotkeys in the renderer window: F1=Start  F2=Plea  F3=E-Stop" -ForegroundColor Cyan
