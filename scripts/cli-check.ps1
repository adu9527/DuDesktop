param(
    [string]$AppPath = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $AppPath) { $AppPath = Join-Path $root 'target\debug\deskshell.exe' }
if (-not (Test-Path -LiteralPath $AppPath)) { throw "Missing deskshell.exe: $AppPath" }
$AppPath = (Resolve-Path -LiteralPath $AppPath).Path
$work = Join-Path $root 'artifacts\cli-check'
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Force -Path $work | Out-Null
$configPath = Join-Path $work 'config.toml'
$image = Join-Path $root 'testdata\4k-pattern.png'
if (-not (Test-Path -LiteralPath $image)) { throw 'Missing testdata; run scripts/fetch-test-media.ps1' }
$configText = @"
version = 3

[onboarding]
desktop_icons_asked = true

[wallpaper]
desktop = false
paused = false
"@
Set-Content -LiteralPath $configPath -Value $configText -Encoding UTF8

function Invoke-Cli {
    param([string[]]$Arguments)
    $output = & $AppPath @Arguments --json 2>&1
    $code = $LASTEXITCODE
    $text = ($output | Out-String).Trim()
    if ($code -ne 0) { throw "CLI $($Arguments -join ' ') failed exit=$code`: $text" }
    return ($text | ConvertFrom-Json)
}

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

$process = Start-Process -FilePath $AppPath -ArgumentList @('app', '--config', $configPath) -PassThru
try {
    Start-Sleep -Seconds 4
    if ($process.HasExited) { throw 'DeskShell exited during startup; another instance may be running' }

    $status = Invoke-Cli @('status')
    Assert-True ($status.ok -and $status.state.app_running) 'status did not report a running app'
    Assert-True ($status.state.protocol_version -eq 5) 'protocol version mismatch'
    Assert-True ($status.state.config_version -eq 3) 'config version mismatch'
    Write-Host 'PASS status'

    $paused = Invoke-Cli @('pause')
    Assert-True ($paused.state.paused) 'pause did not set paused'
    $resumed = Invoke-Cli @('resume')
    Assert-True (-not $resumed.state.paused) 'resume did not clear paused'
    Write-Host 'PASS pause/resume'

    $set = Invoke-Cli @('set-wallpaper', '--file', $image, '--kind', 'image', '--preview', '--effects', 'ultra')
    Assert-True ($set.ok) 'set-wallpaper failed'
    Assert-True ($set.state.effects_active -contains 'Bloom') 'effects not applied by set-wallpaper'
    Assert-True ($set.state.preset -like 'Custom*') 'set-wallpaper with effects should switch to Custom'
    Start-Sleep -Seconds 2
    $after = Invoke-Cli @('status')
    $instance = @($after.state.host.instances)[0]
    Assert-True ($instance.render_path -eq 'gpu_effects') "expected gpu_effects, got $($instance.render_path)"
    Assert-True ($instance.effect_passes -eq 9) "expected 9 passes, got $($instance.effect_passes)"
    Write-Host 'PASS set-wallpaper + D3D11 effects'

    $preset = Invoke-Cli @('preset', 'eco')
    Assert-True ($preset.state.preset -like 'Eco*') 'preset eco not applied'
    Assert-True ($preset.state.effects_active.Count -eq 0) 'eco should disable image effects'
    Write-Host 'PASS preset'

    $report = Join-Path $work 'diagnostics.json'
    $exported = Invoke-Cli @('diagnostics', 'export', '--output', $report)
    Assert-True ($exported.ok -and (Test-Path -LiteralPath $report)) 'diagnostics export failed'
    $bundle = Get-Content -LiteralPath $report -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-True ($bundle.app.app_running) 'diagnostics bundle lacks app state'
    Assert-True ($null -ne $bundle.compat) 'diagnostics bundle lacks compat report'
    Write-Host 'PASS diagnostics export'

    & $AppPath preset turbo --json | Out-Null
    Assert-True ($LASTEXITCODE -eq 2) "invalid preset should exit 2, got $LASTEXITCODE"
    Write-Host 'PASS invalid argument exit code'

    $quit = Invoke-Cli @('quit')
    Assert-True ($quit.ok) 'quit failed'
    $deadline = (Get-Date).AddSeconds(15)
    while (-not $process.HasExited -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    Assert-True ($process.HasExited) 'app did not exit after quit'
    Start-Sleep -Seconds 2
    $orphans = @(Get-CimInstance Win32_Process -Filter "Name='wallpaper-host.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ParentProcessId -eq $process.Id })
    Assert-True ($orphans.Count -eq 0) "orphan hosts after quit: $($orphans.Count)"
    Write-Host 'PASS quit and host cleanup'

    & $AppPath status --json | Out-Null
    Assert-True ($LASTEXITCODE -eq 3) "status after quit should exit 3, got $LASTEXITCODE"
    Write-Host 'PASS not-running exit code'
} finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
}
Write-Host "CLI check PASS; artifacts: $work"
exit 0
