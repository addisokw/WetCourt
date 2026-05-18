# Verify a fresh PC has everything the kiosk stack needs. Read-only -- does
# not install or modify anything. Prints a checklist of OK / MISSING / WARN
# items with remediation pointers.
#
# Usage:
#   powershell -File renderer\tools\check_setup.ps1
#   powershell -File renderer\tools\check_setup.ps1 -RepoRoot D:\WetCourt -ProjectRoot D:\BoothRenderer

param(
    [string]$RepoRoot         = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$ProjectRoot      = 'C:\Users\Strix-4070\UnrealProjects\BoothRenderer',
    [string]$EnginePath       = 'C:\Program Files\Epic Games\UE_5.6',
    [string]$NimContainerName = 'a2f_test',
    [string]$RendererExe      = 'C:\WetCourtBooth\Windows\BoothRenderer.exe'
)

$results = New-Object System.Collections.ArrayList
function Check { param([string]$name, [bool]$ok, [string]$detail, [string]$fix='')
    $null = $results.Add([pscustomobject]@{ Name=$name; OK=$ok; Detail=$detail; Fix=$fix })
}

# ----- Hardware ------------------------------------------------------------
$gpu = (Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | Where-Object { $_.Name -match 'NVIDIA' } | Select-Object -First 1)
Check 'NVIDIA GPU' ([bool]$gpu) ($(if ($gpu) { $gpu.Name } else { 'no NVIDIA GPU detected' })) 'A2F-3D NIM requires an NVIDIA GPU (4070-class or better).'

# 32 GB laptops report ~31 GB usable after BIOS/iGPU reservation; allow 30.
$ram = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
Check 'RAM >= 30 GB' ($ram -ge 30) "$ram GB" ''

$diskFree = [math]::Round((Get-PSDrive C).Free / 1GB, 1)
Check 'C: free >= 50 GB' ($diskFree -ge 50) "$diskFree GB free" 'UE engine + cooked content + DDC each take 10-20 GB.'

# ----- Software ------------------------------------------------------------
# Rustup installs to %USERPROFILE%\.cargo\bin but doesn't always update the
# active shell's PATH on first install -- check the canonical path too.
$cargoPath = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargoPath) {
    $fallback = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $fallback) { $cargoPath = $fallback }
}
Check 'Rust toolchain' ([bool]$cargoPath) ($(if ($cargoPath) { (& $cargoPath --version) } else { 'cargo not found' })) 'Install: https://rustup.rs (then re-open shell)'

$docker = Get-Command docker -ErrorAction SilentlyContinue
Check 'Docker CLI' ([bool]$docker) ($(if ($docker) { (& docker version --format '{{.Client.Version}}' 2>$null) } else { 'docker not on PATH' })) 'Install Docker Desktop with WSL2 backend.'

if ($docker) {
    try { docker version --format '{{.Server.Version}}' | Out-Null; $daemonOk = $true }
    catch { $daemonOk = $false }
    Check 'Docker daemon running' $daemonOk '' 'Start Docker Desktop and wait for the whale icon to be steady.'
}

Check 'UE 5.6 install' (Test-Path (Join-Path $EnginePath 'Engine\Build\BatchFiles\Build.bat')) $EnginePath 'Install via Epic Games Launcher -> Unreal Engine 5.6.x.'

$vsBuild = Get-ChildItem 'C:\Program Files\Microsoft Visual Studio\2022' -ErrorAction SilentlyContinue | Select-Object -First 1
Check 'Visual Studio 2022' ([bool]$vsBuild) ($(if ($vsBuild) { $vsBuild.Name } else { 'not found' })) 'Install VS 2022 17.14+ with the "Game development with C++" workload AND the .NET Framework 4.8 SDK component (required by UE 5.6).'

# ----- Repo + project layout ----------------------------------------------
Check 'Repo present' (Test-Path (Join-Path $RepoRoot 'RUNBOOK.md')) $RepoRoot ''
Check 'UE project present' (Test-Path (Join-Path $ProjectRoot 'BoothRenderer.uproject')) $ProjectRoot 'Clone or extract BoothRenderer to the path passed via -ProjectRoot.'

$pluginLink = Join-Path $ProjectRoot 'Plugins\BoothSubscriber'
Check 'Plugin link -> repo' (Test-Path $pluginLink) $pluginLink ("Create: New-Item -ItemType SymbolicLink -Path `"$pluginLink`" -Target `"$(Join-Path $RepoRoot 'renderer\ue5\BoothSubscriber')`"")

# ----- Secrets / env -------------------------------------------------------
$ll = [Environment]::GetEnvironmentVariable('LITELLM_MASTER_KEY','User')
Check 'LITELLM_MASTER_KEY (User)' ([bool]$ll) ($(if ($ll) { "set (length $($ll.Length))" } else { 'unset' })) "[Environment]::SetEnvironmentVariable('LITELLM_MASTER_KEY','<key>','User')"

$ngc = [Environment]::GetEnvironmentVariable('NGC_API_KEY','User')
Check 'NGC_API_KEY (User)' ([bool]$ngc) ($(if ($ngc) { "set (length $($ngc.Length))" } else { 'unset' })) "Get from https://ngc.nvidia.com/setup/api-key, then [Environment]::SetEnvironmentVariable('NGC_API_KEY','<key>','User')"

# ----- NIM container -------------------------------------------------------
if ($docker -and $daemonOk) {
    $nimStatus = docker inspect -f '{{.State.Status}}' $NimContainerName 2>$null
    Check 'A2F NIM container exists' ([bool]$nimStatus) ($(if ($nimStatus) { $nimStatus } else { 'not created' })) 'See SETUP.md -> "Create the NIM container".'
}

# ----- Build artifacts -----------------------------------------------------
$boothExe = Join-Path $RepoRoot 'orchestrator\target\release\booth.exe'
if (-not (Test-Path $boothExe)) { $boothExe = Join-Path $RepoRoot 'orchestrator\target\debug\booth.exe' }
Check 'Orchestrator binary' (Test-Path $boothExe) $boothExe "cargo build --release --manifest-path $(Join-Path $RepoRoot 'orchestrator\Cargo.toml')"

Check 'Packaged renderer' (Test-Path $RendererExe) $RendererExe "powershell -File $(Join-Path $RepoRoot 'renderer\tools\package_renderer.ps1')"

# ----- Report --------------------------------------------------------------
Write-Host ""
Write-Host "Setup check -- $(Get-Date -Format 'yyyy-MM-dd HH:mm')" -ForegroundColor Cyan
Write-Host ""
foreach ($r in $results) {
    $mark = if ($r.OK) { '[OK]  ' } else { '[FAIL]' }
    $col  = if ($r.OK) { 'Green' } else { 'Red' }
    Write-Host -NoNewline $mark -ForegroundColor $col
    Write-Host (' {0,-30} {1}' -f $r.Name, $r.Detail)
    if (-not $r.OK -and $r.Fix) {
        Write-Host ('        -> ' + $r.Fix) -ForegroundColor Yellow
    }
}
Write-Host ""

$failed = ($results | Where-Object { -not $_.OK }).Count
if ($failed -eq 0) {
    Write-Host "All checks passed. Run launch_production.ps1 to start the kiosk." -ForegroundColor Green
    exit 0
} else {
    Write-Host "$failed issue(s) found. Fix the above, then rerun this script." -ForegroundColor Yellow
    exit 1
}
