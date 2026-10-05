# Real-network matrix for the channel spike, Windows client side (host -> peer).
# The server runs on the other machine (`channels server --bind 0.0.0.0:47000`).
#
# Usage: .\lan-matrix.ps1 -Server 192.168.100.4 [-Replicas 3] [-Secs 10] [-Label win-host]
param(
    [Parameter(Mandatory = $true)][string]$Server,
    [int]$Replicas = 3,
    [int]$Secs = 10,
    [string]$Label = "win-host"
)

$ErrorActionPreference = "Continue"
$bin = Join-Path $PSScriptRoot "channels.exe"
if (-not (Test-Path $bin)) {
    # Layout of the CI artifact zip.
    $bin = Join-Path $PSScriptRoot "target\release\channels.exe"
}
if (-not (Test-Path $bin)) { throw "channels.exe not found next to this script" }
$out = Join-Path $PSScriptRoot ("$Label-" + (Get-Date -Format "yyyyMMdd-HHmmss") + ".jsonl")
Write-Host "results: $out"

function Invoke-Run([int]$r, [string[]]$extra) {
    $line = & $bin client --connect "${Server}:47000" --secs $Secs --label "$Label-r$r" @extra
    if ($LASTEXITCODE -eq 0 -and $line) {
        Add-Content -Path $out -Value $line
    } else {
        Add-Content -Path $out -Value ('{"error":"r' + $r + ' ' + ($extra -join ' ') + '"}')
    }
}

for ($r = 1; $r -le $Replicas; $r++) {
    foreach ($t in @("tcp2", "quic")) {
        Invoke-Run $r @("--transport", $t, "--bulk", "none")
        foreach ($dir in @("saturate", "reverse")) {
            Invoke-Run $r @("--transport", $t, "--bulk", $dir, "--bulk-rate", "0")
            Invoke-Run $r @("--transport", $t, "--bulk", $dir, "--bulk-rate", "adaptive", "--target-ms", "4")
            Invoke-Run $r @("--transport", $t, "--bulk", $dir, "--bulk-rate", "adaptive", "--target-ms", "8")
        }
    }
    Write-Host "replica $r done"
}
Write-Host "done: $out"
