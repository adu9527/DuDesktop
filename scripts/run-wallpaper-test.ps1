param(
    [string]$StaticDir = 'C:\Users\24911\Pictures\photo_8K\动漫',
    [string]$VideoDir,
    [string]$Exe,
    [string]$Config
)
# 一键测试：设为静态壁纸，并启用 Dock、灵动岛与小部件。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $VideoDir) { $VideoDir = Join-Path $root 'testdata\4k-samples\clips' }
if (-not $Exe) { $Exe = Join-Path $root 'target\debug\deskshell.exe' }
if (-not $Config) { $Config = Join-Path $root 'artifacts\wallpaper-test\config.toml' }
if (-not (Test-Path -LiteralPath $Exe)) { throw "找不到 DeskShell：$Exe（先运行 scripts/dev.ps1 build）" }
if (-not (Test-Path -LiteralPath $StaticDir)) { throw "静态壁纸目录不存在：$StaticDir" }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Config) | Out-Null
if (-not (Test-Path -LiteralPath $Config)) {
    # 新测试配置直接跳过首次引导，避免模态对话框阻塞控制通道；
    # 小部件使用真实应用/文件夹，验证图标提取与打开行为。
    Set-Content -LiteralPath $Config -Encoding UTF8 -Value @"
version = 6

[onboarding]
desktop_icons_asked = true

[dock]
style = "large"
show_running = true
show_system = true
max_items = 10

[widgets.controls]
enabled = true
x = 40
y = 420
width = 360.0

[widgets.clock]
enabled = false
x = 40
y = 240
width = 300.0

[widgets.weather]
enabled = false
city = "上海"
x = 1600
y = 60
width = 300.0

[widgets.music]
enabled = false
x = 40
y = 780
width = 340.0

[widgets.organizer]
enabled = true
columns = 3
x = 24
y = 80
width = 340.0
items = [
    'path:C:\Windows\System32\notepad.exe',
    'folder:$StaticDir',
]
"@
}

function Test-AppRunning {
    & $Exe status --json *> $null
    return $LASTEXITCODE -eq 0
}

if (-not (Test-AppRunning)) {
    # 使用独立测试配置，避免覆盖用户的正式配置；该目录可写。
    Start-Process -FilePath $Exe -ArgumentList @('app', '--config', $Config) -WorkingDirectory $root
    $ready = $false
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        Start-Sleep -Milliseconds 500
        if (Test-AppRunning) { $ready = $true; break }
    }
    if (-not $ready) { throw 'DeskShell 启动超时；请检查 debug.log 或托盘图标' }
} else {
    Write-Host '检测到 DeskShell 已在运行：沿用当前小部件配置。'
}

$image = Get-ChildItem -LiteralPath $StaticDir -File |
    Where-Object { $_.Extension -match '^\.(png|jpg|jpeg|webp|bmp)$' } |
    Sort-Object Name |
    Select-Object -First 1
if (-not $image) { throw "静态目录中没有可用图片：$StaticDir" }

& $Exe set-wallpaper --file $image.FullName --kind image --desktop
if ($LASTEXITCODE -ne 0) { throw "应用静态壁纸失败（退出码 $LASTEXITCODE）" }

$videos = @()
if (Test-Path -LiteralPath $VideoDir) {
    $videos = @(Get-ChildItem -LiteralPath $VideoDir -File |
        Where-Object { $_.Extension -match '^\.(mp4|mkv|webm|mov)$' } |
        Sort-Object Name)
}
Write-Host ''
Write-Host "静态壁纸：$($image.Name)"
Write-Host "测试配置：$Config"
Write-Host "4K 视频：$($videos.Count) 个（$VideoDir）"
Write-Host '验证项：'
Write-Host '  1. 壁纸铺满屏幕（cover，无上下黑边）；托盘「壁纸缩放」可切换。'
Write-Host '  2. 顶部灵动岛：点击展开，运行/工具/文件三个页签可切换；双击条目打开。'
Write-Host '  3. 底部 Dock：固定、运行中与系统入口；最右设置图标打开 DeskShell 设置。'
Write-Host '  4. 快捷控制：音量滑杆/静音真实生效，WLAN/蓝牙显示真实状态。'
Write-Host '  5. 桌面收纳：拖动页头移动，拖右边缘调宽；其余小部件默认关闭。'
