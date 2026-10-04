# Start Familiar on this machine.
#   .\scripts\start-local.ps1            # the desktop app (runs its built-in database, the API and the daemon inside it)
#   .\scripts\start-local.ps1 -Web       # ...and the browser UI on http://localhost:47173
#   .\scripts\start-local.ps1 -Headless  # no window: Postgres (Docker) + standalone familiar-server + familiard
# Settings: ~/.familiar/config.toml (app) and ~/.familiar/local.env (headless database). Logs: ~/.familiar/logs.
param([switch]$Web, [switch]$Headless)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$Familiar = Join-Path $HOME ".familiar"
$oldHome = Join-Path $HOME ".zed"
if (-not (Test-Path $Familiar) -and (Test-Path $oldHome)) { Rename-Item $oldHome $Familiar }  # pre-rename folder
$logs = Join-Path $Familiar "logs"
New-Item -ItemType Directory -Force $logs | Out-Null

function Start-Bg($name, $file, $argList) {
    $p = @{ FilePath = $file; WorkingDirectory = $root; WindowStyle = "Hidden"; PassThru = $true
            RedirectStandardOutput = "$logs\$name.log"; RedirectStandardError = "$logs\$name.err.log" }
    if ($argList) { $p.ArgumentList = $argList }
    $proc = Start-Process @p
    Write-Host "  $name (pid $($proc.Id))"
}

Write-Host "Starting Familiar:"
if ($Headless) {
    # Headless needs a Postgres of its own; the desktop app's built-in database is not shared.
    Get-Content (Join-Path $Familiar "local.env") | ForEach-Object {
        if ($_ -match '^\s*([A-Z_]+)\s*=\s*(.*)$') { $key = $matches[1] -replace "^ZED_", "FAMILIAR_"  # accept the pre-rename ZED_ prefix too
            Set-Item "env:$key" $matches[2].Trim() }
    }
    if (-not (docker ps -a --filter "name=^zed-db$" --format "{{.Names}}")) {
        docker run -d --name zed-db --restart unless-stopped -e POSTGRES_PASSWORD=$env:FAMILIAR_DB_PASSWORD `
            -p "$($env:FAMILIAR_DB_PORT):5432" -v zed-db:/var/lib/postgresql/data postgres:16 | Out-Null
    } else {
        docker start zed-db | Out-Null
    }
    do {
        Start-Sleep -Milliseconds 500
        docker exec zed-db pg_isready -U postgres -q 2>$null
    } until ($LASTEXITCODE -eq 0)
    Start-Bg "server" "$root\target\debug\familiar-server.exe"
    Start-Bg "daemon" "$root\target\debug\familiard.exe"
} else {
    $exe = "$root\apps\native\target\release\familiar-native.exe"
    if (-not (Test-Path $exe)) { $exe = "$root\apps\native\target\debug\familiar-native.exe" }
    if (-not (Test-Path $exe)) { throw "Build the app first: cd apps/native; cargo build --release" }
    Start-Process $exe -WorkingDirectory $root | Out-Null
    Write-Host "  desktop app (window + tray icon; API on http://127.0.0.1:47080)"
}
if ($Web) {
    Start-Bg "web" "cmd.exe" "/c pnpm --filter web dev"
    Write-Host "  browser UI: http://localhost:47173"
}
Write-Host "Logs: $logs"
