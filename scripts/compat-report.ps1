param(
    [string]$Output = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\debug\deskshell.exe'
if (-not (Test-Path -LiteralPath $exe)) { throw 'Missing target\debug\deskshell.exe; run scripts/dev.ps1 build' }
if (-not $Output) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $Output = Join-Path $root "artifacts\compat-$stamp.json"
} else {
    $Output = Join-Path $root $Output
}

function Decode-MonitorString {
    param($Values)
    if (-not $Values) { return '' }
    $chars = @($Values | Where-Object { $_ -ne 0 } | ForEach-Object { [char]$_ })
    return -join $chars
}

$os = Get-CimInstance Win32_OperatingSystem
$computer = Get-CimInstance Win32_ComputerSystem
$processor = Get-CimInstance Win32_Processor | Select-Object -First 1
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object {
    [ordered]@{
        name = $_.Name
        driver_version = $_.DriverVersion
        driver_date = if ($_.DriverDate) { $_.DriverDate.ToString('o') } else { '' }
        adapter_ram = $_.AdapterRAM
        video_processor = $_.VideoProcessor
        status = $_.Status
        pnp_device_id = $_.PNPDeviceID
    }
})
$monitors = @()
try {
    $monitors = @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction Stop | ForEach-Object {
        [ordered]@{
            manufacturer = Decode-MonitorString $_.ManufacturerName
            product = Decode-MonitorString $_.ProductCodeID
            serial = Decode-MonitorString $_.SerialNumberID
            name = Decode-MonitorString $_.UserFriendlyName
            year = $_.YearOfManufacture
        }
    })
} catch {
    $monitors = @()
}
$probe = (& $exe probe 2>&1 | Out-String).Trim()
$doctor = (& $exe doctor 2>&1 | Out-String).Trim()
$report = [ordered]@{
    generated = (Get-Date).ToString('o')
    machine = $env:COMPUTERNAME
    os = [ordered]@{
        caption = $os.Caption
        version = $os.Version
        build = $os.BuildNumber
        architecture = $os.OSArchitecture
    }
    cpu = [ordered]@{
        name = $processor.Name
        cores = $processor.NumberOfCores
        logical_processors = $processor.NumberOfLogicalProcessors
    }
    memory_gb = [math]::Round($computer.TotalPhysicalMemory / 1GB, 1)
    gpus = $gpus
    monitors = $monitors
    probe = $probe
    doctor = $doctor
    notes = @(
        'Driver versions come from Win32_VideoController; DXGI adapter/display details come from deskshell probe.',
        'A single AMD sample does not establish NVIDIA/Intel/hybrid compatibility.',
        'VRAM/GPU time and 24/72 hour stability are not part of this report.'
    )
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Output -Encoding UTF8
Write-Host "Compat report: $Output"
