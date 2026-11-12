param([switch]$Force)
# Prepare reproducible M1 test media. FFmpeg is only a development tool; it is not shipped.
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$ffmpegAsset = 'ffmpeg-2026-11-05-git-0893feba65-essentials_build.7z'
$ffmpegSha256 = '9d2c2ca426699e4e0f180341f001d1e4ed95ae3231b160853f24891c15706ccd'
$ffmpegUrl = "https://www.gyan.dev/ffmpeg/builds/packages/$ffmpegAsset"
$testData = Join-Path $repositoryRoot 'testdata'
$cacheDirectory = Join-Path $repositoryRoot '.cache\tools'
$archivePath = Join-Path $cacheDirectory $ffmpegAsset
$extractRoot = Join-Path $cacheDirectory 'ffmpeg'

New-Item -ItemType Directory -Force -Path $testData | Out-Null
New-Item -ItemType Directory -Force -Path $cacheDirectory | Out-Null
New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null

$imagePath = Join-Path $testData '4k-pattern.png'
$posterPath = Join-Path $testData 'poster.png'
$videoPath = Join-Path $testData '4k-h264-30fps.mp4'
$unsupportedPath = Join-Path $testData 'unsupported-mpeg4.mp4'
$badPath = Join-Path $testData 'not-a-video.mp4'

$allPresent = (Test-Path $imagePath) -and (Test-Path $posterPath) -and (Test-Path $videoPath) -and (Test-Path $unsupportedPath) -and (Test-Path $badPath)
if (-not $Force -and $allPresent) {
    Write-Host "Test media already present in $testData"
    exit 0
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

if (-not (Test-Path -LiteralPath (Join-Path $extractRoot $ffmpegAsset.Replace('.7z', '')))) {
    Invoke-Download $ffmpegUrl $archivePath
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($hash -ne $ffmpegSha256) {
        throw "FFmpeg archive checksum mismatch: $hash"
    }
    & tar.exe -xf $archivePath -C $extractRoot
    if ($LASTEXITCODE -ne 0) {
        throw 'Extracting FFmpeg failed.'
    }
}

$ffmpeg = Get-ChildItem -Path $extractRoot -Recurse -Filter 'ffmpeg.exe' | Select-Object -First 1
$ffprobe = Get-ChildItem -Path $extractRoot -Recurse -Filter 'ffprobe.exe' | Select-Object -First 1
if (-not $ffmpeg) {
    throw 'ffmpeg.exe not found after extraction.'
}

function New-Pattern([int]$Width, [int]$Height, [string]$Title, [string]$Path) {
    Add-Type -AssemblyName System.Drawing
    $bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = 'AntiAlias'
    $rectangle = New-Object System.Drawing.Rectangle 0, 0, $Width, $Height
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rectangle, ([System.Drawing.Color]::FromArgb(255, 12, 24, 48)), ([System.Drawing.Color]::FromArgb(255, 196, 88, 32)), 35.0
    $graphics.FillRectangle($brush, $rectangle)
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(70, 255, 255, 255)), ([float]($Width / 640))
    for ($x = 0; $x -le $Width; $x += [int]($Width / 16)) {
        $graphics.DrawLine($pen, $x, 0, $x, $Height)
    }
    for ($y = 0; $y -le $Height; $y += [int]($Height / 9)) {
        $graphics.DrawLine($pen, 0, $y, $Width, $y)
    }
    $font = New-Object System.Drawing.Font 'Segoe UI', ([float]($Height / 12)), ([System.Drawing.FontStyle]::Bold)
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = 'Center'
    $format.LineAlignment = 'Center'
    $graphics.DrawString($Title, $font, $white, (New-Object System.Drawing.RectangleF 0, 0, $Width, $Height), $format)
    $sub = New-Object System.Drawing.Font 'Consolas', ([float]($Height / 28)), ([System.Drawing.FontStyle]::Regular)
    $graphics.DrawString("$Width x $Height  DeskShell M1 test pattern", $sub, $white, (New-Object System.Drawing.RectangleF 0, ([float]($Height * 0.72)), $Width, ([float]($Height * 0.2))), $format)
    $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
}

New-Pattern 3840 2160 'DESKSHELL 4K' $imagePath
New-Pattern 1920 1080 'STATIC FALLBACK' $posterPath
Set-Content -LiteralPath $badPath -Value 'This is not a video file.' -Encoding ASCII

& $ffmpeg.FullName -hide_banner -loglevel error -y -f lavfi -i 'testsrc2=size=3840x2160:rate=30' -t 10 -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p -movflags +faststart $videoPath
if ($LASTEXITCODE -ne 0) {
    throw 'Encoding the 4K H.264 sample failed.'
}
& $ffmpeg.FullName -hide_banner -loglevel error -y -f lavfi -i 'testsrc2=size=1920x1080:rate=30' -t 5 -c:v mpeg4 -q:v 5 -pix_fmt yuv420p $unsupportedPath
if ($LASTEXITCODE -ne 0) {
    throw 'Encoding the MPEG-4 Part 2 sample failed.'
}

Write-Host 'Test media ready.'
Get-ChildItem -LiteralPath $testData | Select-Object Name, Length | Format-Table -AutoSize
if ($ffprobe) {
    Write-Host '4K sample:'
    & $ffprobe.FullName -v error -show_entries stream=codec_name,profile,width,height,pix_fmt,r_frame_rate,bit_rate -of default=noprint_wrappers=1 $videoPath
}
