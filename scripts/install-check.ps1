param(
    [string]$InstallDir = '',
    [string]$SourceDir = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $InstallDir) { $InstallDir = Join-Path $root 'artifacts\install-test' }
if (-not $SourceDir) { $SourceDir = Join-Path $root 'target\release' }
$SourceDir = (Resolve-Path -LiteralPath $SourceDir).Path
$userData = Join-Path $root 'artifacts\install-test-userdata'
$succeeded = $false
$exe = Join-Path $InstallDir 'deskshell.exe'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runValueBefore = $null

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

try {
    if (Test-Path -LiteralPath $InstallDir) { Remove-Item -LiteralPath $InstallDir -Recurse -Force }
    if (Test-Path -LiteralPath $userData) { Remove-Item -LiteralPath $userData -Recurse -Force }
    $appDataDir = Join-Path $userData 'Roaming'
    $localAppDataDir = Join-Path $userData 'Local'
    $env:APPDATA = $appDataDir
    $env:LOCALAPPDATA = $localAppDataDir
    New-Item -ItemType Directory -Force -Path $appDataDir, $localAppDataDir | Out-Null

    # 隔离配置：跳过首次图标引导，避免无交互验收出现阻塞对话框。
    $configDir = Join-Path -Path $appDataDir -ChildPath 'DeskShell'
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    Set-Content -LiteralPath (Join-Path $configDir 'config.toml') -Encoding UTF8 -Value @"
version = 2
[onboarding]
desktop_icons_asked = true
desktop_icons_hidden_by_us = false
"@

    & (Join-Path $PSScriptRoot 'install.ps1') -SourceDir $SourceDir -InstallDir $InstallDir -NoShortcut -NoStart
    foreach ($name in @('deskshell.exe', 'wallpaper-host.exe', 'libmpv-2.dll')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $InstallDir $name)) "installed file missing: $name"
    }
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDir 'licenses\GPL-2.0.txt')) 'GPL license not installed'
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDir 'licenses\THIRD-PARTY-NOTICES.txt')) 'third-party notices not installed'
    Write-Host '[PASS] install copied binaries and license materials'

    # 覆盖安装等价于升级前置路径：二进制可替换，用户配置不动。
    & (Join-Path $PSScriptRoot 'install.ps1') -SourceDir $SourceDir -InstallDir $InstallDir -NoShortcut -NoStart -Force
    Assert-True (Test-Path -LiteralPath (Join-Path $configDir 'config.toml')) 'user config removed by reinstall'
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDir 'previous\deskshell.exe')) 'previous version not backed up'
    Write-Host '[PASS] reinstall keeps user config and backs up previous version'

    if (Test-Path -LiteralPath $runKey) {
        $runValueBefore = (Get-ItemProperty -Path $runKey -Name 'DeskShell' -ErrorAction SilentlyContinue).DeskShell
    }
    & $exe autostart --enable
    Assert-True ($LASTEXITCODE -eq 0) 'autostart --enable failed'
    $enabled = (Get-ItemProperty -Path $runKey -Name 'DeskShell').DeskShell
    $expected = '"{0}" app' -f $exe
    Assert-True ($enabled -eq $expected) "autostart value mismatch: $enabled"
    & $exe autostart --disable
    Assert-True ($LASTEXITCODE -eq 0) 'autostart --disable failed'
    $disabled = (Get-ItemProperty -Path $runKey -Name 'DeskShell' -ErrorAction SilentlyContinue).DeskShell
    Assert-True ($null -eq $disabled) 'autostart value still present after disable'
    Write-Host '[PASS] autostart enable/disable'

    $process = Start-Process -FilePath $exe -ArgumentList 'app', '--seconds', '3' -WorkingDirectory $InstallDir -PassThru
    $process | Wait-Process -Timeout 30
    Assert-True ($process.ExitCode -eq 0) "installed app exited with $($process.ExitCode)"
    Start-Sleep -Milliseconds 800
    $orphan = Get-Process -Name wallpaper-host -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
    Assert-True (-not $orphan) 'wallpaper host left after app exit'
    Write-Host '[PASS] installed app starts and exits without orphan host'

    & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $InstallDir
    Assert-True (-not (Test-Path -LiteralPath $InstallDir)) 'install directory still exists after uninstall'
    $leftover = Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
    Assert-True (-not $leftover) 'installed processes still running after uninstall'
    Write-Host '[PASS] uninstall removed files and processes'
    Write-Host '[PASS] install-check'
    $succeeded = $true
}
finally {
    if (Test-Path -LiteralPath $runKey) {
        if ($null -ne $runValueBefore) {
            Set-ItemProperty -Path $runKey -Name 'DeskShell' -Value $runValueBefore
        } else {
            Remove-ItemProperty -Path $runKey -Name 'DeskShell' -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path -LiteralPath $InstallDir) {
        Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) } |
            Stop-Process -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($succeeded -and (Test-Path -LiteralPath $userData)) {
        Remove-Item -LiteralPath $userData -Recurse -Force -ErrorAction SilentlyContinue
    }
}
