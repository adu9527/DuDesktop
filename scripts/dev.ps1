param(
    [ValidateSet('check', 'build', 'doctor', 'host-check', 'window')]
    [string]$Task = 'check'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if ((Get-Location).Path -ne $root) { throw '请在仓库根目录执行 scripts/dev.ps1' }
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
function Invoke-Cargo {
    param([string[]]$CargoArgs)
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
}
switch ($Task) {
    'check' {
        Invoke-Cargo @('fmt', '--all', '--', '--check')
        Invoke-Cargo @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
        Invoke-Cargo @('test', '--workspace', '--locked')
    }
    'build' { Invoke-Cargo @('build', '--workspace', '--locked') }
    default {
        Invoke-Cargo @('build', '--workspace', '--locked')
        & "$root\target\debug\deskshell.exe" $Task
        if ($LASTEXITCODE -ne 0) { throw "deskshell 失败，退出码 $LASTEXITCODE" }
    }
}
