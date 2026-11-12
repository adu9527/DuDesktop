param(
    [double]$Hours = 1,
    [int]$IntervalSeconds = 30,
    [string]$AppPath = '',
    [string]$Report = '',
    [switch]$Desktop,
    [switch]$SkipCycle
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $AppPath) { $AppPath = Join-Path $root 'target\debug\deskshell.exe' }
if (-not (Test-Path -LiteralPath $AppPath)) { throw "Missing deskshell.exe: $AppPath" }
$AppPath = (Resolve-Path -LiteralPath $AppPath).Path
$work = Join-Path $root 'artifacts\soak'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$configPath = Join-Path $work 'config.toml'
if (-not $Report) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $Report = Join-Path $root "artifacts\soak-$stamp.json"
}
$image = Join-Path $root 'testdata\4k-pattern.png'
$video = Join-Path $root 'testdata\4k-h264-30fps.mp4'
if (-not (Test-Path -LiteralPath $image) -or -not (Test-Path -LiteralPath $video)) {
    throw 'Missing testdata; run scripts/fetch-test-media.ps1'
}
$mode = if ($Desktop) { '--desktop' } else { '--preview' }
$configText = @"
version = 3

[onboarding]
desktop_icons_asked = true

[wallpaper]
desktop = $($Desktop.IsPresent.ToString().ToLowerInvariant())
paused = false
"@
Set-Content -LiteralPath $configPath -Value $configText -Encoding UTF8

function Invoke-Cli {
    param([string[]]$Arguments)
    $output = & $AppPath @Arguments --json 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "CLI failed ($($Arguments -join ' ')) exit=$LASTEXITCODE`: $output"
    }
    return ($output | Out-String)
}

function Get-TreeProcesses {
    param([int]$RootPid)
    $all = Get-CimInstance Win32_Process -Filter "Name='deskshell.exe' OR Name='wallpaper-host.exe'" -ErrorAction SilentlyContinue
    $selected = @()
    foreach ($process in $all) {
        if ($process.ProcessId -eq $RootPid -or $process.ParentProcessId -eq $RootPid) {
            $selected += $process
        }
    }
    return $selected
}

$duration = [TimeSpan]::FromHours($Hours)
if ($duration.TotalSeconds -lt 20) { throw 'Hours must be at least 0.006 (about 20 seconds)' }
Write-Host "Soak: $($duration.TotalSeconds) seconds, interval $IntervalSeconds s, mode $mode"
$process = Start-Process -FilePath $AppPath -ArgumentList @('app', '--config', $configPath) -PassThru
$samples = @()
$cycles = @()
$errors = @()
$hostKills = 0
$cycle = 0
try {
    Start-Sleep -Seconds 4
    if ($process.HasExited) { throw "DeskShell exited during startup (code $($process.ExitCode)); another instance may be running" }
    $null = Invoke-Cli @('status')
    $deadline = (Get-Date) + $duration
    $previousCpu = 0.0
    $previousStamp = Get-Date
    while ((Get-Date) -lt $deadline) {
        $tree = Get-TreeProcesses -RootPid $process.Id
        $appItem = $tree | Where-Object { $_.ProcessId -eq $process.Id } | Select-Object -First 1
        if (-not $appItem) { throw 'DeskShell process exited during soak' }
        $hosts = @()
        foreach ($item in $tree) {
            if ($item.ProcessId -eq $process.Id) { continue }
            $hosts += [ordered]@{
                pid = $item.ProcessId
                working_set_mb = [math]::Round($item.WorkingSetSize / 1MB, 2)
                private_mb = [math]::Round($item.PrivatePageCount / 1MB, 2)
                handles = $item.HandleCount
                threads = $item.ThreadCount
                cpu_seconds = [math]::Round(($item.KernelModeTime + $item.UserModeTime) / 1e7, 2)
            }
        }
        $now = Get-Date
        $appCpu = ($appItem.KernelModeTime + $appItem.UserModeTime) / 1e7
        $cpuDelta = [math]::Max(0, $appCpu - $previousCpu)
        $wallDelta = ($now - $previousStamp).TotalSeconds
        $cpuPercent = if ($wallDelta -gt 0) { [math]::Round($cpuDelta / $wallDelta * 100, 3) } else { 0 }
        $previousCpu = $appCpu
        $previousStamp = $now
        $sample = [ordered]@{
            time = $now.ToString('o')
            elapsed_s = [math]::Round(((Get-Date) - $process.StartTime).TotalSeconds, 1)
            app_working_set_mb = [math]::Round($appItem.WorkingSetSize / 1MB, 2)
            app_private_mb = [math]::Round($appItem.PrivatePageCount / 1MB, 2)
            app_handles = $appItem.HandleCount
            app_threads = $appItem.ThreadCount
            cpu_seconds = [math]::Round($appCpu, 2)
            cpu_percent_one_core_interval = $cpuPercent
            hosts = $hosts
            host_count = $hosts.Count
            vram = 'not measured (no per-process DXGI query)'
            gpu_time = 'not measured (AMD timestamp unavailable)'
        }
        $samples += $sample

        if (-not $SkipCycle) {
            $cycle++
            try {
                switch ($cycle % 6) {
                    1 { $null = Invoke-Cli @('set-wallpaper', '--file', $image, '--kind', 'image', $mode, '--effects', 'ultra') }
                    2 { $null = Invoke-Cli @('pause') }
                    3 { $null = Invoke-Cli @('resume'); $null = Invoke-Cli @('set-wallpaper', '--file', $video, '--kind', 'video', $mode) }
                    4 {
                        $targets = @($tree | Where-Object { $_.Name -eq 'wallpaper-host.exe' })
                        if ($targets.Count -gt 0) {
                            Stop-Process -Id $targets[0].ProcessId -Force
                            $hostKills++
                            $deadlineReconnect = (Get-Date).AddSeconds(25)
                            while ((Get-Date) -lt $deadlineReconnect) {
                                Start-Sleep -Milliseconds 500
                                try {
                                    $status = Invoke-Cli @('status')
                                    if ($status -match '"host_pid"') { break }
                                } catch { }
                            }
                        }
                    }
                    5 { $null = Invoke-Cli @('restore-desktop') }
                    0 { $null = Invoke-Cli @('resume'); $null = Invoke-Cli @('preset', 'balanced') }
                }
                $cycles += [ordered]@{ at = (Get-Date).ToString('o'); cycle = $cycle; ok = $true }
            } catch {
                $errors += $_.Exception.Message
                $cycles += [ordered]@{ at = (Get-Date).ToString('o'); cycle = $cycle; ok = $false; error = $_.Exception.Message }
            }
        }
        Start-Sleep -Seconds $IntervalSeconds
    }
} catch {
    $errors += $_.Exception.Message
} finally {
    try { $null = Invoke-Cli @('quit') } catch { }
    Start-Sleep -Seconds 2
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
}

$orphans = @(Get-CimInstance Win32_Process -Filter "Name='wallpaper-host.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.ParentProcessId -eq $process.Id })
$warm = @($samples | Select-Object -Skip 2)
$summary = [ordered]@{}
if ($warm.Count -gt 0) {
    $first = $warm[0]
    $last = $warm[$warm.Count - 1]
    $summary = [ordered]@{
        warm_samples = $warm.Count
        app_handles_first = $first.app_handles
        app_handles_last = $last.app_handles
        app_handles_delta = $last.app_handles - $first.app_handles
        app_private_mb_first = $first.app_private_mb
        app_private_mb_last = $last.app_private_mb
        app_private_mb_delta = [math]::Round($last.app_private_mb - $first.app_private_mb, 2)
        host_count_last = $last.host_count
    }
}
$reportData = [ordered]@{
    started = $process.StartTime.ToString('o')
    duration_hours = $Hours
    interval_seconds = $IntervalSeconds
    desktop = $Desktop.IsPresent
    host_kills = $hostKills
    errors = $errors
    orphans = $orphans.Count
    summary = $summary
    samples = $samples
    cycles = $cycles
    notes = @(
        'Memory/handles are process-tree samples; a plateau after warmup is expected, continuous growth is not.',
        'VRAM and GPU time are not measured; AMD timestamp queries return zero on the test machine.',
        '24/72 hour acceptance requires running this script with -Hours 24/72 and reviewing the trend, not a short smoke.'
    )
}
$reportData | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Report -Encoding UTF8
Write-Host "Soak report: $Report"
Write-Host "Samples: $($samples.Count), host kills: $hostKills, errors: $($errors.Count), orphans: $($orphans.Count)"
if ($summary.Count -gt 0) {
    Write-Host ("Handles delta: {0}, private MB delta: {1}" -f $summary.app_handles_delta, $summary.app_private_mb_delta)
}
if ($errors.Count -gt 0) { exit 1 }
if ($orphans.Count -gt 0) { exit 1 }
exit 0
