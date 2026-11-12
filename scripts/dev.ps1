param(
    [ValidateSet('check', 'build', 'doctor', 'probe', 'capsule', 'capsule-check', 'host-check', 'effects-check', 'interop-check', 'cli-check', 'desktop-check', 'app-check', 'app-self-test', 'perf-check', 'soak-check', 'install-check', 'package-release', 'compat-report', 'window', 'fetch-mpv', 'fetch-test-media')]
    [string]$Task = 'check'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if ((Get-Location).Path -ne $root) { throw 'Run scripts/dev.ps1 from the repository root' }
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
function Invoke-Cargo {
    param([string[]]$CargoArgs)
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo failed with exit code $LASTEXITCODE" }
}
switch ($Task) {
    'check' {
        Invoke-Cargo @('fmt', '--all', '--', '--check')
        Invoke-Cargo @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
        Invoke-Cargo @('test', '--workspace', '--locked')
    }
    'build' { Invoke-Cargo @('build', '--workspace', '--locked') }
    'capsule-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" capsule --self-test
        if ($LASTEXITCODE -ne 0) { throw "deskshell failed with exit code $LASTEXITCODE" }
        & "$root\target\debug\deskshell.exe" capsule --recovery-test
        if ($LASTEXITCODE -ne 0) { throw "recovery self-test failed with exit code $LASTEXITCODE" }
        & "$root\target\debug\deskshell.exe" capsule --self-test --seconds 1
        if ($LASTEXITCODE -ne 1) { throw 'An early-exiting self-test must return exit code 1' }
    }
    'host-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        $mpv = Join-Path $root 'third_party\mpv\libmpv-2.dll'
        if (-not (Test-Path -LiteralPath $mpv)) { throw 'Missing third_party\mpv\libmpv-2.dll; run scripts/fetch-mpv.ps1' }
        $video = Join-Path $root 'testdata\4k-h264-30fps.mp4'
        if (-not (Test-Path -LiteralPath $video)) { throw 'Missing testdata; run scripts/fetch-test-media.ps1' }
        & "$root\target\debug\deskshell.exe" host-check
        if ($LASTEXITCODE -ne 0) { throw "host-check failed with exit code $LASTEXITCODE" }
    }
    'effects-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        $image = Join-Path $root 'testdata\4k-pattern.png'
        if (-not (Test-Path -LiteralPath $image)) { throw 'Missing testdata; run scripts/fetch-test-media.ps1' }
        $artifacts = Join-Path $root 'artifacts'
        New-Item -ItemType Directory -Force -Path $artifacts | Out-Null
        $static = Join-Path $artifacts 'm5-effects-reference.png'
        $animated = Join-Path $artifacts 'm5-effects-particles.png'
        & "$root\target\debug\wallpaper-host.exe" --effects-check --file $image --output $static --width 2560 --height 1440
        if ($LASTEXITCODE -ne 0) { throw "effects-check static failed with exit code $LASTEXITCODE" }
        & "$root\target\debug\wallpaper-host.exe" --effects-check --file $image --output $animated --width 2560 --height 1440 --particles
        if ($LASTEXITCODE -ne 0) { throw "effects-check particles failed with exit code $LASTEXITCODE" }
        foreach ($path in @($static, $animated)) {
            if (-not (Test-Path -LiteralPath $path)) { throw "Missing effects reference: $path" }
            if ((Get-Item -LiteralPath $path).Length -le 0) { throw "Empty effects reference: $path" }
            $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            Write-Output "reference $path sha256 $hash"
        }
    }
    'interop-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        $video = Join-Path $root 'testdata\4k-h264-30fps.mp4'
        if (-not (Test-Path -LiteralPath $video)) { throw 'Missing testdata; run scripts/fetch-test-media.ps1' }
        $artifacts = Join-Path $root 'artifacts'
        New-Item -ItemType Directory -Force -Path $artifacts | Out-Null
        $output = Join-Path $artifacts 'm5-video-interop.png'
        & "$root\target\debug\wallpaper-host.exe" --interop-check --file $video --output $output --width 1280 --height 720 --seconds 3
        if ($LASTEXITCODE -ne 0) { throw "interop-check failed with exit code $LASTEXITCODE" }
        if (-not (Test-Path -LiteralPath $output)) { throw "Missing interop reference: $output" }
        if ((Get-Item -LiteralPath $output).Length -le 0) { throw "Empty interop reference: $output" }
        $hash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
        Write-Output "reference $output sha256 $hash"
    }
    'cli-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\scripts\cli-check.ps1"
        if ($LASTEXITCODE -ne 0) { throw "cli-check failed with exit code $LASTEXITCODE" }
    }
    'soak-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\scripts\soak-check.ps1" -Hours 0.02 -IntervalSeconds 5
        if ($LASTEXITCODE -ne 0) { throw "soak-check failed with exit code $LASTEXITCODE" }
    }
    'package-release' {
        & "$root\scripts\package-release.ps1" -Version '0.2.0-m6'
        if ($LASTEXITCODE -ne 0) { throw "package-release failed with exit code $LASTEXITCODE" }
    }
    'compat-report' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\scripts\compat-report.ps1"
        if ($LASTEXITCODE -ne 0) { throw "compat-report failed with exit code $LASTEXITCODE" }
    }
    'desktop-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" desktop-check
        if ($LASTEXITCODE -ne 0) { throw "desktop-check failed with exit code $LASTEXITCODE" }
    }
    'app-check' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" app-check --launch --probes
        if ($LASTEXITCODE -ne 0) { throw "app-check failed with exit code $LASTEXITCODE" }
    }
    'app-self-test' {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" app --self-test
        if ($LASTEXITCODE -ne 0) { throw "app self-test failed with exit code $LASTEXITCODE" }
    }
    'perf-check' {
        Invoke-Cargo @('build', '--workspace', '--release', '--locked')
        $image = Join-Path $root 'testdata\4k-pattern.png'
        if (-not (Test-Path -LiteralPath $image)) { throw 'Missing testdata; run scripts/fetch-test-media.ps1' }
        & "$root\target\release\deskshell.exe" perf-check
        if ($LASTEXITCODE -ne 0) { throw "perf-check failed with exit code $LASTEXITCODE" }
    }
    'install-check' {
        Invoke-Cargo @('build', '--workspace', '--release', '--locked')
        & "$root\scripts\install-check.ps1"
        if ($LASTEXITCODE -ne 0) { throw "install-check failed with exit code $LASTEXITCODE" }
    }
    'fetch-mpv' { & "$root\scripts\fetch-mpv.ps1" }
    'fetch-test-media' { & "$root\scripts\fetch-test-media.ps1" }
    default {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" $Task
        if ($LASTEXITCODE -ne 0) { throw "deskshell failed with exit code $LASTEXITCODE" }
    }
}
