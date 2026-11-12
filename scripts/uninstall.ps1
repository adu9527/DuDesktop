param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\DeskShell",
    [switch]$RemoveUserData,
    [switch]$NoRestore
)
$ErrorActionPreference = 'Stop'

$running = Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
if ($running) {
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
if (Test-Path -LiteralPath $runKey) {
    $value = (Get-ItemProperty -Path $runKey -Name 'DeskShell' -ErrorAction SilentlyContinue).DeskShell
    if ($value -and $value -like "*$InstallDir*") {
        Remove-ItemProperty -Path $runKey -Name 'DeskShell' -ErrorAction SilentlyContinue
    }
}

# 让程序自己恢复它拥有的 Shell 修改（桌面图标、壁纸附着），再删除文件。
$exe = Join-Path $InstallDir 'deskshell.exe'
if (-not $NoRestore -and (Test-Path -LiteralPath $exe)) {
    try {
        $process = Start-Process -FilePath $exe -ArgumentList 'app', '--seconds', '3' -WorkingDirectory $InstallDir -PassThru
        $process | Wait-Process -Timeout 20 -ErrorAction SilentlyContinue
    } catch {
        Write-Warning "Restore-on-uninstall failed: $($_.Exception.Message)"
    }
}

# 恢复进程退出后，仍可能残留宿主；再次按安装目录清理，避免文件占用。
$leftovers = Get-Process -Name deskshell, wallpaper-host -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [System.StringComparison]::OrdinalIgnoreCase) }
if ($leftovers) {
    $leftovers | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'DeskShell.lnk'
if (Test-Path -LiteralPath $shortcut) {
    Remove-Item -LiteralPath $shortcut -Force
}

if (Test-Path -LiteralPath $InstallDir) {
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
}

if ($RemoveUserData) {
    $config = Join-Path $env:APPDATA 'DeskShell'
    $cache = Join-Path $env:LOCALAPPDATA 'DeskShell'
    if (Test-Path -LiteralPath $config) { Remove-Item -LiteralPath $config -Recurse -Force }
    if (Test-Path -LiteralPath $cache) { Remove-Item -LiteralPath $cache -Recurse -Force }
    Write-Host 'User config and cache removed'
} else {
    Write-Host 'User config preserved (pass -RemoveUserData to delete)'
}

Write-Host 'DeskShell uninstalled'
