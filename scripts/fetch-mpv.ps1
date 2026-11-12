param([switch]$Force)
# Download and verify the pinned libmpv development build.
# Release: https://github.com/zhongfly/mpv-winbuild/releases/tag/2026-11-10-d075ec2a28
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$release = '2026-11-10-d075ec2a28'
$assetName = 'mpv-dev-x86_64-20261110-git-d075ec2a28.7z'
$archiveSha256 = '9342ed8dcbba81ab16c96c4155239524f563789b6a7261c748da9ea2adc874fe'
$librarySha256 = '0586211ed50e6c1b8eeb01952f7d812b205d81bc678f280810759d62e56bf545'
$baseUrl = "https://github.com/zhongfly/mpv-winbuild/releases/download/$release"

$cacheDirectory = Join-Path $repositoryRoot '.cache\mpv'
$targetDirectory = Join-Path $repositoryRoot 'third_party\mpv'
$libraryPath = Join-Path $targetDirectory 'libmpv-2.dll'
$archivePath = Join-Path $cacheDirectory $assetName
$checksumPath = Join-Path $cacheDirectory 'sha256.txt'

New-Item -ItemType Directory -Force -Path $cacheDirectory | Out-Null
New-Item -ItemType Directory -Force -Path $targetDirectory | Out-Null

if (-not $Force -and (Test-Path -LiteralPath $libraryPath)) {
    $existing = (Get-FileHash -Algorithm SHA256 -LiteralPath $libraryPath).Hash.ToLowerInvariant()
    if ($existing -eq $librarySha256) {
        Write-Host "libmpv already present: $libraryPath"
        exit 0
    }
    Write-Host "Existing libmpv does not match the pinned checksum; downloading again."
}

function Invoke-Download([string]$Uri, [string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Force
    }
    & curl.exe -L -A 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)' --fail --retry 3 --connect-timeout 30 -o $Path $Uri
    if ($LASTEXITCODE -ne 0) {
        throw "Download failed: $Uri"
    }
}

Invoke-Download "$baseUrl/$assetName" $archivePath
Invoke-Download "$baseUrl/sha256.txt" $checksumPath

$archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
if ($archiveHash -ne $archiveSha256) {
    throw "Archive checksum mismatch: $archiveHash"
}
$upstream = Get-Content -LiteralPath $checksumPath -Raw
if ($upstream -notmatch [regex]::Escape($archiveSha256)) {
    throw 'Upstream sha256.txt does not list the pinned archive checksum.'
}

$extractDirectory = Join-Path $cacheDirectory "dev-$release"
if (Test-Path -LiteralPath $extractDirectory) {
    Remove-Item -LiteralPath $extractDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $extractDirectory | Out-Null
& tar.exe -xf $archivePath -C $extractDirectory
if ($LASTEXITCODE -ne 0) {
    throw 'Extracting the libmpv archive failed (tar.exe must support 7z).'
}

Copy-Item -LiteralPath (Join-Path $extractDirectory 'libmpv-2.dll') -Destination $libraryPath -Force
$includeDirectory = Join-Path $targetDirectory 'include'
New-Item -ItemType Directory -Force -Path $includeDirectory | Out-Null
Copy-Item -Path (Join-Path $extractDirectory 'include\mpv\*.h') -Destination $includeDirectory -Force
Copy-Item -LiteralPath $checksumPath -Destination (Join-Path $targetDirectory 'sha256.txt') -Force

$libraryHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $libraryPath).Hash.ToLowerInvariant()
if ($libraryHash -ne $librarySha256) {
    throw "libmpv-2.dll checksum mismatch: $libraryHash"
}

Write-Host "libmpv ready."
Write-Host "  release : $release"
Write-Host "  archive : $archivePath"
Write-Host "  library : $libraryPath"
Write-Host "  sha256  : $libraryHash"
Write-Host "  license : mpv (GPLv2+), FFmpeg build bundled by zhongfly/mpv-winbuild"
Write-Host "Set DESKSHELL_MPV_DIR to override the runtime search directory."
