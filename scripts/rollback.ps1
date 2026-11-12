param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\DeskShell",
    [string]$PreviousDir = ''
)
$ErrorActionPreference = 'Stop'
if (-not $PreviousDir) { $PreviousDir = Join-Path $InstallDir 'previous' }
if (-not (Test-Path -LiteralPath $PreviousDir)) {
    throw "No previous version found: $PreviousDir"
}
foreach ($name in @('deskshell.exe', 'wallpaper-host.exe', 'libmpv-2.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $PreviousDir $name))) {
        throw "Previous version is incomplete: missing $name"
    }
}

$running = Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
if ($running) {
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

foreach ($name in @('deskshell.exe', 'wallpaper-host.exe', 'libmpv-2.dll')) {
    Copy-Item -LiteralPath (Join-Path $PreviousDir $name) -Destination (Join-Path $InstallDir $name) -Force
}
Write-Host "DeskShell rolled back from $PreviousDir"
Write-Host "User config was preserved: $env:APPDATA\DeskShell\config.toml"
