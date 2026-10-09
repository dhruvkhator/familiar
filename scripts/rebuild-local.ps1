# Rebuild the desktop app on this PC and reopen it, quickly.
#   .\scripts\rebuild-local.ps1          # quit Familiar, build the fast `local` profile, start it again
#   .\scripts\rebuild-local.ps1 -Release # the slower, fully optimised build the installer uses
# Familiar is asked to quit (it shuts its built-in database down cleanly), never killed.
param([switch]$Release)
$ErrorActionPreference = "Stop"
$native = Join-Path (Split-Path $PSScriptRoot -Parent) "apps\native"
$buildProfile = if ($Release) { "release" } else { "local" }
$exe = Join-Path $native "target\$buildProfile\familiar-native.exe"

# Quit whichever copy is running (from either profile).
$running = Get-Process -Name familiar-native -ErrorAction SilentlyContinue | Select-Object -First 1
if ($running -and $running.Path) {
    & $running.Path --quit
    for ($i = 0; $i -lt 30 -and (Get-Process -Name familiar-native -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 500 }
}

$started = Get-Date
Push-Location $native
try {
    cargo build --profile $buildProfile
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
} finally {
    Pop-Location
}
"Built in {0:N0} s" -f ((Get-Date) - $started).TotalSeconds

# Start it from a clean environment: a FAMILIAR_HOME left in this shell would point it at another data folder.
Remove-Item Env:FAMILIAR_HOME -ErrorAction SilentlyContinue
Start-Process $exe
"Familiar started: $exe"
