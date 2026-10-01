# Stop everything start-local.ps1 started. The database keeps its data (Docker volume `zed-db`, kept under its old name so existing data survives).
#   .\scripts\stop-local.ps1          # stop app processes, leave Postgres running
#   .\scripts\stop-local.ps1 -All     # also stop the Postgres container
param([switch]$All)
foreach ($name in "familiar-desktop", "familiard", "familiar-server", "zed-desktop", "zedd", "zed-server") {  # old names too, for upgrades
    Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
}
Get-NetTCPConnection -LocalPort 47173 -State Listen -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
if ($All) { docker stop zed-db | Out-Null }
Write-Host "Familiar stopped."
