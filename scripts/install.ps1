param(
    [string]$SourceDir = '',
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\DeskShell",
    [switch]$Autostart,
    [switch]$NoShortcut,
    [switch]$NoStart,
    [switch]$NoBackup,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $SourceDir) {
    if (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'deskshell.exe')) {
        # 试用包布局：安装脚本与二进制同目录。
        $SourceDir = $PSScriptRoot
    } else {
        $release = Join-Path $root 'target\release'
        $debug = Join-Path $root 'target\debug'
        $SourceDir = if (Test-Path -LiteralPath (Join-Path $release 'deskshell.exe')) { $release } else { $debug }
    }
}
$SourceDir = (Resolve-Path -LiteralPath $SourceDir).Path
$files = @{}
foreach ($name in @('deskshell.exe', 'wallpaper-host.exe', 'libmpv-2.dll')) {
    $candidate = Join-Path $SourceDir $name
    if (-not (Test-Path -LiteralPath $candidate) -and $name -eq 'libmpv-2.dll') {
        # 开发目录通常只构建 exe；固定运行库位于 third_party\mpv。
        $candidate = Join-Path $root 'third_party\mpv\libmpv-2.dll'
    }
    if (-not (Test-Path -LiteralPath $candidate)) {
        throw "Missing $name in $SourceDir; run scripts/dev.ps1 build and scripts/fetch-mpv.ps1 first"
    }
    $files[$name] = $candidate
}

$running = Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
if ($running -and -not $Force) {
    throw "DeskShell is running from $InstallDir; exit it from the tray or pass -Force"
}
if ($running) {
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$previous = Join-Path $InstallDir 'previous'
if (-not $NoBackup -and (Test-Path -LiteralPath (Join-Path $InstallDir 'deskshell.exe'))) {
    New-Item -ItemType Directory -Force -Path $previous | Out-Null
    foreach ($name in @('deskshell.exe', 'wallpaper-host.exe', 'libmpv-2.dll')) {
        $existing = Join-Path $InstallDir $name
        if (Test-Path -LiteralPath $existing) {
            Copy-Item -LiteralPath $existing -Destination (Join-Path $previous $name) -Force
        }
    }
    Write-Host "Previous version backed up to $previous"
}
foreach ($name in $files.Keys) {
    Copy-Item -LiteralPath $files[$name] -Destination (Join-Path $InstallDir $name) -Force
}
$licenses = Join-Path $PSScriptRoot 'licenses'
if (-not (Test-Path -LiteralPath $licenses)) { $licenses = Join-Path $root 'licenses' }
if (Test-Path -LiteralPath $licenses) {
    Copy-Item -LiteralPath $licenses -Destination $InstallDir -Recurse -Force
}

if (-not $NoShortcut) {
    $startMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'DeskShell.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($startMenu)
    $shortcut.TargetPath = Join-Path $InstallDir 'deskshell.exe'
    $shortcut.Arguments = 'app'
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = 'DeskShell 桌面壁纸与 Dock'
    $shortcut.Save()
}

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$command = '"{0}" app' -f (Join-Path $InstallDir 'deskshell.exe')
if ($Autostart) {
    New-Item -Path $runKey -Force | Out-Null
    Set-ItemProperty -Path $runKey -Name 'DeskShell' -Value $command
} else {
    # 安装器不擅自开启自启；保留用户在托盘中的选择。
}

if (-not $NoStart) {
    Start-Process -FilePath (Join-Path $InstallDir 'deskshell.exe') -ArgumentList 'app' -WorkingDirectory $InstallDir
}

Write-Host "DeskShell installed to $InstallDir"
Write-Host "User config: $env:APPDATA\DeskShell\config.toml (preserved)"
if ($Autostart) { Write-Host 'Autostart enabled (HKCU Run)' }
