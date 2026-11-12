param(
    [string]$Version = '0.2.0-m6',
    [string]$SourceDir = '',
    [string]$OutputDir = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $SourceDir) { $SourceDir = Join-Path $root 'target\release' }
if (-not $OutputDir) { $OutputDir = Join-Path $root "artifacts\DeskShell-trial-$Version" }
$SourceDir = (Resolve-Path -LiteralPath $SourceDir).Path

foreach ($name in @('deskshell.exe', 'wallpaper-host.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $SourceDir $name))) {
        throw "Missing $name; run scripts/dev.ps1 build (release) first"
    }
}
$mpv = Join-Path $root 'third_party\mpv\libmpv-2.dll'
if (-not (Test-Path -LiteralPath $mpv)) { throw 'Missing third_party\mpv\libmpv-2.dll; run scripts/fetch-mpv.ps1' }

if (Test-Path -LiteralPath $OutputDir) { Remove-Item -LiteralPath $OutputDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
Copy-Item -LiteralPath (Join-Path $SourceDir 'deskshell.exe') -Destination $OutputDir
Copy-Item -LiteralPath (Join-Path $SourceDir 'wallpaper-host.exe') -Destination $OutputDir
Copy-Item -LiteralPath $mpv -Destination $OutputDir
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install.ps1') -Destination $OutputDir
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'uninstall.ps1') -Destination $OutputDir
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'rollback.ps1') -Destination $OutputDir
$licenses = Join-Path $root 'licenses'
if (-not (Test-Path -LiteralPath $licenses)) { throw 'Missing licenses directory; GPL compliance materials are required' }
Copy-Item -LiteralPath $licenses -Destination $OutputDir -Recurse

$installText = @"
DeskShell 内部试用包（$Version）
================================

运行环境：Windows 11 x64。无需管理员权限。
安装：powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1
卸载：powershell -NoProfile -ExecutionPolicy Bypass -File uninstall.ps1
回滚：powershell -NoProfile -ExecutionPolicy Bypass -File rollback.ps1
运行：安装目录 deskshell.exe app（托盘提供暂停、画质、图片效果、素材、状态、诊断、恢复桌面与退出）

配置：%APPDATA%\DeskShell\config.toml（v3）；保存前保留 config.toml.prev，迁移前保留 .bak。
缓存/安装/受管理素材：%LOCALAPPDATA%。

已交付：透明 Dock/桌面文件入口、WorkerW 附着、mpv 图片/视频、Eco 静态模式、
D3D11 图片效果、版本化 CLI 与诊断导出。
已知限制：
- 视频效果未接入日常播放；24/72 小时长稳、多屏/NVIDIA/Intel/100%/150% DPI、
  HDR 与真实设备失效恢复尚未验收。
- GPU 时间在本机 AMD 驱动上不可用，不能作为性能结论。
- 本包为内部试用，不伪造签名或兼容性结果。

许可：包内 libmpv 为 GPLv2+ 构建；licenses\GPL-2.0.txt 与
licenses\THIRD-PARTY-NOTICES.txt 说明源码获取与提供方式。
"@
Set-Content -LiteralPath (Join-Path $OutputDir 'INSTALL.txt') -Value $installText -Encoding UTF8

$zip = Join-Path $root "artifacts\DeskShell-trial-$Version.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $OutputDir '*') -DestinationPath $zip
Write-Host "Trial package: $zip"
