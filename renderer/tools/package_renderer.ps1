# Package BoothRenderer for Windows. Produces a self-contained install dir
# at -Output that just needs to be copied to the kiosk and double-clicked.
#
# Usage:
#   powershell -File renderer\tools\package_renderer.ps1
#   powershell -File renderer\tools\package_renderer.ps1 -Config Shipping
#   powershell -File renderer\tools\package_renderer.ps1 -Output D:\Kiosk\Build
#
# Config trade-off:
#   Development (default) - faster build, retains some debug symbols, easier
#                           to triage crashes. Roughly the right choice for
#                           every build that isn't the actual show install.
#   Shipping              - smaller, faster runtime, no debug. Use for the
#                           production-day install once everything is stable.

param(
    [ValidateSet('Development','Shipping')]
    [string]$Config = 'Development',
    [string]$Output = 'C:\WetCourtBooth',
    [string]$EnginePath = 'C:\Program Files\Epic Games\UE_5.6',
    [string]$Project    = 'C:\Users\Strix-4070\UnrealProjects\BoothRenderer\BoothRenderer.uproject'
)
$ErrorActionPreference = 'Stop'

$uat = Join-Path $EnginePath 'Engine\Build\BatchFiles\RunUAT.bat'
if (-not (Test-Path $Project)) { throw "Project not found: $Project" }
if (-not (Test-Path $uat))     { throw "RunUAT.bat not found: $uat" }

Write-Host "===== Packaging BoothRenderer =====" -ForegroundColor Cyan
Write-Host "  Project : $Project"
Write-Host "  Config  : $Config"
Write-Host "  Output  : $Output"
Write-Host ""

# Make sure the editor isn't holding the binaries.
$editor = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.ExecutablePath -like '*UnrealEditor.exe' } |
    Select-Object -First 1
if ($editor) {
    throw "UnrealEditor.exe is running (PID $($editor.ProcessId)). Close it, then rerun."
}

# -build (compile module if needed), -cook (bake assets), -stage + -pak +
# -archive (assemble the standalone install dir at -archivedirectory).
& $uat BuildCookRun `
    "-project=$Project" `
    "-platform=Win64" `
    "-targetplatform=Win64" `
    "-clientconfig=$Config" `
    "-cook" `
    "-allmaps" `
    "-build" `
    "-stage" `
    "-pak" `
    "-archive" `
    "-archivedirectory=$Output" `
    "-utf8output" `
    "-nodebuginfo"

if ($LASTEXITCODE -ne 0) {
    throw "Packaging FAILED with exit code $LASTEXITCODE"
}

$exe = Get-ChildItem -Path "$Output\Windows" -Recurse -Filter 'BoothRenderer.exe' -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($exe) {
    Write-Host ""
    Write-Host "===== Build OK =====" -ForegroundColor Green
    Write-Host "  Exe   : $($exe.FullName)"
    Write-Host "  Size  : $([math]::Round((Get-Item $exe.FullName).Length / 1MB, 1)) MB"
    Write-Host ""
    Write-Host "Launch via renderer\tools\launch_production.ps1 or by double-clicking the exe."
} else {
    Write-Warning "UAT reported success but BoothRenderer.exe was not found under $Output\Windows."
}
