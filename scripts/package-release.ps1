param(
    [string]$Version = '0.2.0-m6',
    [string]$SourceDir = '',
    [string]$OutputDir = '',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

if (-not $SourceDir) { $SourceDir = Join-Path $root 'target\release' }
if (-not (Test-Path -LiteralPath $SourceDir)) { throw "Missing release directory: $SourceDir" }
$SourceDir = (Resolve-Path -LiteralPath $SourceDir).Path
if (-not $OutputDir) { $OutputDir = Join-Path $root "artifacts\DeskShell-$Version" }

if (-not $SkipBuild) {
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}

$required = @{
    'deskshell.exe' = (Join-Path $SourceDir 'deskshell.exe')
    'wallpaper-host.exe' = (Join-Path $SourceDir 'wallpaper-host.exe')
    'libmpv-2.dll' = (Join-Path $root 'third_party\mpv\libmpv-2.dll')
}
foreach ($name in $required.Keys) {
    if (-not (Test-Path -LiteralPath $required[$name])) { throw "Missing $name; run release build and scripts/fetch-mpv.ps1" }
}
$expectedMpvHash = '0586211ed50e6c1b8eeb01952f7d812b205d81bc678f280810759d62e56bf545'
$mpvHash = (Get-FileHash -LiteralPath $required['libmpv-2.dll'] -Algorithm SHA256).Hash.ToLowerInvariant()
if ($mpvHash -ne $expectedMpvHash) {
    throw "libmpv-2.dll checksum mismatch: $mpvHash (expected $expectedMpvHash)"
}

if (Test-Path -LiteralPath $OutputDir) { Remove-Item -LiteralPath $OutputDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
foreach ($name in $required.Keys) {
    Copy-Item -LiteralPath $required[$name] -Destination (Join-Path $OutputDir $name)
}
foreach ($script in @('install.ps1', 'uninstall.ps1', 'rollback.ps1')) {
    $path = Join-Path $PSScriptRoot $script
    if (Test-Path -LiteralPath $path) { Copy-Item -LiteralPath $path -Destination $OutputDir }
}
$licenses = Join-Path $root 'licenses'
if (-not (Test-Path -LiteralPath $licenses)) { throw 'Missing licenses directory; GPL compliance materials are required' }
Copy-Item -LiteralPath $licenses -Destination $OutputDir -Recurse

$fileEntries = @()
Get-ChildItem -LiteralPath $OutputDir -Recurse -File | ForEach-Object {
    $relative = $_.FullName.Substring($OutputDir.Length).TrimStart('\')
    $fileEntries += [ordered]@{
        path = $relative
        bytes = $_.Length
        sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$rustc = (& rustc --version 2>&1 | Out-String).Trim()
$cargo = (& cargo --version 2>&1 | Out-String).Trim()
$gitCommit = ''
try { $gitCommit = (& git -C $root rev-parse HEAD 2>$null | Out-String).Trim() } catch { $gitCommit = 'unknown' }
$gitDirty = $false
try { $gitDirty = ((& git -C $root status --porcelain 2>$null | Out-String).Trim().Length -gt 0) } catch { $gitDirty = $true }
$manifest = [ordered]@{
    product = 'DeskShell'
    version = $Version
    built_at_utc = [DateTime]::UtcNow.ToString('o')
    git_commit = if ($gitCommit) { $gitCommit } else { 'unknown' }
    git_dirty = $gitDirty
    reproducible_note = '记录工具链、锁文件与哈希；构建时间与 zip 元数据可能不同，不声称比特级可复现'
    rustc = $rustc
    cargo = $cargo
    protocol_version = 5
    config_version = 3
    build_command = 'cargo build --workspace --release --locked'
    mpv = [ordered]@{
        version = 'mpv v0.42.0-36-gd075ec2a2'
        source = 'https://github.com/zhongfly/mpv-winbuild/releases/tag/2026-11-10-d075ec2a28'
        sha256 = $mpvHash
        license = 'GPL-2.0-or-later'
    }
    files = $fileEntries
}
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDir 'manifest.json') -Encoding UTF8
$sums = $fileEntries | ForEach-Object { '{0}  {1}' -f $_.sha256, $_.path }
$sums | Set-Content -LiteralPath (Join-Path $OutputDir 'SHA256SUMS') -Encoding UTF8

$installText = @"
DeskShell 候选发布包（$Version）
================================

运行环境：Windows 11 x64。默认用户级安装，无需管理员权限。
安装：powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1
卸载：powershell -NoProfile -ExecutionPolicy Bypass -File uninstall.ps1
回滚：powershell -NoProfile -ExecutionPolicy Bypass -File rollback.ps1
运行：开始菜单 DeskShell，或安装目录 deskshell.exe app

版本与校验：manifest.json 使用 SHA-256 记录全部文件；SHA256SUMS 可直接校验。
配置：%APPDATA%\DeskShell\config.toml（v3，迁移前保留 .bak 与 .prev）。
缓存/安装/受管理素材：%LOCALAPPDATA%。

已交付：透明 Dock/桌面文件入口、Wallpaper WorkerW 附着、mpv 图片/视频、Eco 静态模式、
D3D11 图片效果（调色/暗角/模糊/Bloom/粒子/渐变）、版本化 CLI 与诊断导出。
已知边界：视频效果未接入日常播放；24/72 小时长稳、NVIDIA/Intel/多屏/100%/150% DPI、
HDR 与真实设备失效恢复尚未验收；GPU 时间在本机 AMD 驱动上不可用。

许可：包内 libmpv 为 GPLv2+ 构建；licenses\GPL-2.0.txt 与
licenses\THIRD-PARTY-NOTICES.txt 说明源码获取与提供方式。
本包为本地候选发布，不伪造签名或兼容性结果。
"@
Set-Content -LiteralPath (Join-Path $OutputDir 'INSTALL.txt') -Value $installText -Encoding UTF8

$zip = Join-Path $root "artifacts\DeskShell-$Version.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $OutputDir '*') -DestinationPath $zip
Write-Host "Release candidate: $zip"
Write-Host "Manifest: $(Join-Path $OutputDir 'manifest.json')"
Write-Host "libmpv sha256: $mpvHash"
