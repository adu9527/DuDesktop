# 开发环境

## 本机准备结果

- 已有 Visual Studio Community 2026（18.10），已检测到 `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`。
- 已有 Windows SDK；实际 MSVC 编译、链接与 Win32 窗口运行已验证。
- 已有 Rust 1.96.0；项目通过 `rust-toolchain.toml` 固定版本，包含 rustfmt、Clippy 和 x64 MSVC target。
- 已将 `%USERPROFILE%\.cargo\bin` 补入用户 PATH；已有终端需重开，开发脚本会临时补齐当前进程 PATH。
- Cargo 依赖版本由根目录 `Cargo.lock` 固定，作为应用仓库文件保留。
- 尚未安装 mpv 开发包、RenderDoc/PIX，也未固定 wgpu：分别在 M1/M0 按原型结论引入；当前构建不需要它们、CMake 或额外 GPU SDK。

## 新机器准备

1. 安装 Visual Studio/Build Tools 的“使用 C++ 的桌面开发”，包含 x64 MSVC 和 Windows SDK。
2. 从 https://rustup.rs 安装 Rust，选择 `x86_64-pc-windows-msvc`。
3. 在仓库执行以下命令（rustup 会读取工具链文件）：

```powershell
rustup show
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run -p deskshell -- doctor
cargo run -p deskshell -- probe
cargo run -p deskshell -- capsule --self-test
cargo run -p deskshell -- host-check
cargo run -p deskshell -- window
```

首次执行 `host-check` 前需要准备运行资源：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/fetch-mpv.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/fetch-test-media.ps1
```

先构建整个 workspace，再执行 `host-check`，因为 `cargo run -p deskshell` 不会自动编译另一个二进制包。也可直接使用 README 中的开发脚本。

## M1 运行资源

- `scripts/fetch-mpv.ps1` 下载并校验固定 libmpv 构建（`zhongfly/mpv-winbuild` release `2026-11-10-d075ec2a28`，归档 SHA256 `9342ed8d…`，`libmpv-2.dll` SHA256 `0586211e…`），解压到 `third_party\mpv\`；该目录被 `.gitignore` 排除。
- `scripts/fetch-test-media.ps1` 下载固定 FFmpeg 开发包（SHA256 `9d2c2ca4…`）并生成 `testdata\` 下的 4K H.264、MPEG-4 Part 2、PNG 与错误文件；`testdata\` 同样不提交。
- 详见证 [ADR 0003](adr/0003-m1-libmpv-integration.md) 与 [ADR 0002](adr/0002-m1-ipc-and-host-supervision.md)。

## 已验证与边界

当前直接依赖：`windows 0.61` 用于原生 Win32、命名管道、Job 对象、进程信息、DXGI、Direct3D 11、DirectComposition、Direct2D、DirectWrite、Shell、托盘与剪贴板；`serde`/`serde_json` 用于版本化协议与 Shell 状态日志；`toml 1` 用于版本化用户配置。精确解析版本以 `Cargo.lock` 为准；尚未引入 wgpu、egui、notify 或 Rust mpv 绑定（文件监听直接使用 `ReadDirectoryChangesW`）。libmpv 通过运行时 `LoadLibraryW`/`GetProcAddress` 动态加载，不参与 Cargo 依赖图，也不要求构建期 SDK。

- workspace 编译、格式、Clippy（警告视为错误）、单元测试通过。
- 单元测试覆盖：不兼容协议拒绝、帧长度上界、信封/快照往返、管道同机交换、退避曲线、动画结束后无持续帧需求、96/120/144 DPI 胶囊尺寸与区域居中。
- 管道行为测试覆盖：空闲超时后连接可复用、部分帧超时使会话失效、协议错误后快速失败、并发读取不拆帧、无读者时写超时按截止时间返回、30 次建立/释放连接后句柄增长受限（协议 v3 起）。
- 原 stdout JSON 握手已被 M1 命名管道取代，不再作为正式 IPC。
- `host-check` 已覆盖：命名管道握手、4K 播放、暂停/停止、错误素材、静态回退、杀死宿主恢复、双实例独立控制、四类故障注入与孤儿进程检查；详见 `docs/DEVELOPMENT_PLAN.md` 的 M1 验收表。
- 协议单元测试覆盖配置校验、信封往返、未知版本、超限长度、空帧/截断帧与扁平化响应键冲突；管道单元测试覆盖同机帧交换、超限拒绝与超时换算。
- 基础窗口已验证 HWND 创建、关闭请求与正常进程退出；不代表完整产品窗口验收。
- `probe` 已在 Windows 11 build 26200、AMD Radeon 780M（驱动 32.0.12033.1030）上枚举 2560×1440@144、DPI 120（125%）；启动时设置 Per-Monitor V2 DPI 感知，避免报告虚拟化分辨率。
- `capsule-check` 已验证：DirectComposition 预乘 alpha 无黑底；独立阴影层不拦截鼠标；`WindowFromPoint` 五项（中心、窗口外、圆角外、未展开留白、阴影层）全部 PASS；动画结束后 600 ms 内 Present 增量 0。双层动画 CPU 平均 debug 0.73–0.81 ms / release 0.81–0.83 ms，峰值 debug 0.95–1.44 ms / release 1.23–1.44 ms（小样本）。
- `capsule --self-test --input-test` 已验证真实鼠标点击：阴影/留白不激活本程序，点击胶囊后前台窗口与键盘焦点均为胶囊；测试结束恢复鼠标位置。
- 96/120/144 DPI 的胶囊尺寸与窗口区域居中单元测试通过；`--monitor N` 越界返回明确错误。
- 设备/目标失效分类覆盖 `EndDraw`、`ResizeBuffers` 和表面创建；`capsule --recovery-test` 已通过合成错误注入验证表面重建、共享 GPU/双层交换链重建及后续呈现。真实驱动设备移除、真实 DPI 切换尚未验证。
- 窗口回调仅向队列写入事件，主循环处理业务状态；状态不由销毁回调释放。自检检查失败或未完成均返回非零退出码；`capsule-check` 包含正常、恢复和提前退出反向回归。图形自检应串行执行，避免同位置窗口互相遮挡。
- 帧耗时日志为绘制/提交墙钟时间，包含 API 等待，不能作为纯 CPU 执行时间或 GPU 帧时间；成功 Present 在各层分别计数，包含恢复前部分成功提交。
- M1 已用命名管道替换 stdout 握手；管道 DACL 仅当前用户与 SYSTEM，`PIPE_REJECT_REMOTE_CLIENTS`，帧上限 256 KiB；协议版本 3。
- 宿主资源语义：`Paused` 保留 mpv 上下文（`player_active=true`），`Stopped` 释放播放器（`player_active=false`）；停止仍以该实例 CPU 采样为辅助证据，未做显存/句柄归零测量。
- 宿主监督已实测：杀死宿主 0 ms 返回 `NotReady`、有限指数退避、重连重放期望状态、正常退出/强杀均无孤儿宿主。
- M1 播放实测：mpv `v0.42.0-36-gd075ec2a2`（client API 2.6），4K H.264 High / yuv420p 8 bit / 30 fps，硬解 `d3d11va`，5 秒掉帧 0；静态与暂停 CPU 采样 0.00% 单核。
- 不支持编码（MPEG-4 Part 2）在 5 秒内无有效画面时判定 `HardwareDecodeUnavailable` 并显示海报；错误视频文件返回 `UnsupportedMedia`。
- 未验证项：真实中文 IME 组合输入（无可编辑控件）、物理多显示器与 100%/150% DPI 矩阵、NVIDIA/Intel/混合显卡、HEVC/10-bit/AV1/HDR、真实驱动重置、24/72 小时资源增长、Explorer 重启恢复（M2）。

## M2 运行与验收

- `scripts/dev.ps1 desktop-check`：验证显示器稳定身份、WorkerW 探测、附着与父子校验、窗口覆盖 `rcMonitor` 完整边界、解除附着后的失效检测与重新附着、桌面图标状态查询与幂等；不修改桌面，不默认隐藏图标。
- `deskshell wallpaper --file PATH --desktop [--display \\.\DISPLAY2] [--seconds N]`：把静态图/视频附着到桌面壁纸层播放；退出时解除附着并销毁窗口。`--desktop` 与显式预览几何参数不能同时使用。
- `deskshell desktop-check --restore-icons [--journal PATH]`：仅在显式请求时把桌面图标恢复为可见；修改前先写 `ShellStateJournal`（默认 `%APPDATA%\DeskShell\shell-state.json`），损坏日志会改名保留。
- 桌面附着使用 `WS_CHILD` + 父子校验；Explorer 重建后窗口会被销毁，宿主会重建窗口与播放器再附着。附着期间有 2 秒低频恢复看门狗，不附着或退出时不启用。
- 本机实测（Windows 11 build 26200，AMD Radeon 780M，单屏 `\\.\DISPLAY2` 2560×1440@125%）：`desktop-check` 全部 PASS；`wallpaper --desktop` 静态图实际覆盖桌面并正常退出。
- 未验证：真实 Explorer 重启、锁屏/解锁、休眠恢复、热插拔、分辨率/DPI 变化、Show Desktop/任务视图/虚拟桌面。手工步骤见 `docs/DEVELOPMENT_PLAN.md` M2。

## M3 运行与验收

- `scripts/dev.ps1 app-self-test`：创建 Dock、灵动岛与应用库三个面板，验证可见性、圆角/透明区域命中、完整显示器边界、展开文件行、文件图标/缩略图和空闲 700 ms Present 增量 0；自检不弹首次引导、不写配置。
- `scripts/dev.ps1 app-check`：应用发现/身份、48px 图标、桌面索引、目录监听、Notepad 与计算器真实启动/关闭、复制路径与移入回收站探针。
- `deskshell app [--config PATH] [--seconds N]`：日常入口；单实例、托盘、全局热键 Ctrl+Alt+D（Dock）/ Ctrl+Alt+I（灵动岛）。GUI 模式隐藏控制台，`--seconds` 用于可控退出与截图。
- `app-check --probes` 会在用户桌面创建唯一临时文件；若安全策略拒绝未签名进程写入（Controlled Folder Access 等），会明确警告并改用临时根目录验证同一回收站实现，不静默跳过。
- 应用库：Dock 固定项与应用库分离；右键应用条目可固定/取消固定/左右移动；固定项上限 9 个；固定项为空时 Dock 仍保留应用库、资源管理器、回收站入口。
- 文件面板：折叠摘要显示项数与分类数；展开 8 行，支持打开、定位、复制路径、确认后移入回收站；失败保留条目并在状态行提示。
- 配置：`%APPDATA%\DeskShell\config.toml`（或用 `--config` 指定），版本 1；损坏文件改名保留，高于程序版本只读不覆盖。
- 本机实测（Windows 11 build 26200，AMD Radeon 780M，单屏 2560×1440@125%）：`check`、`app-check --launch --probes`、`app --self-test` 通过；应用 338（AppsFolder 305、开始菜单 33、AUMID 42），桌面 62（用户 35、公共 27），文件图标 8，Dock 3/6 项命中，岛体 8 行，空闲 Present 0。可视化证据在 `artifacts/`。
- 未验证：中文 IME 组合输入（尚无文本输入控件）、首次隐藏图标引导、边缘唤起（未实现）、OLE 拖拽（明确延期）、OneDrive/重定向桌面、监听溢出注入、慢 Shell 扩展挂起、托盘/右键菜单真实点选、100%/150% 与多屏、无障碍。手工步骤见 `docs/DEVELOPMENT_PLAN.md` M3。

## M4 运行与验收

- 配置：`%APPDATA%\DeskShell\config.toml` 版本 2；保存前旧文件复制为 `config.toml.prev`，损坏时优先从 `.prev` 恢复；v1 迁移前写 `config.v1-<时间>.bak`；未知字段保留；更高版本只读。`--config PATH` 仍可用于隔离验收。
- 画质：托盘 →“画质”切换 Eco/Balanced/Ultra/Custom；效果开关会把当前预设派生为 Custom。自动规则、逐显示器覆盖和素材选择均在托盘子菜单中；状态与故障对话框显示有效策略、原因、宿主、附着、硬解、掉帧与错误。
- Eco 语义：图片保持静态；视频停在当前帧，`player_active=true` 但播放位置不推进；用户暂停仍优先。协议 v4 新增 `render`/`video_position_ms` 供验收观察。
- 效果：UI 动画关闭后 Dock 不再悬停放大；面板阴影关闭会销毁独立分层阴影窗口与 DIB；模糊未交付，菜单显示置灰原因，不提供无效开关。
- 开机启动：托盘切换 HKCU `Software\Microsoft\Windows\CurrentVersion\Run\DeskShell`；值为当前 exe 加 `app`，不写 HKLM。`deskshell autostart --status|--enable|--disable` 可用于脚本化检查与恢复。
- 安装：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1 -InstallDir <目录> -SourceDir target\release [-Autostart] [-NoShortcut] [-NoStart]`。默认目录 `%LOCALAPPDATA%\Programs\DeskShell`；不修改用户配置。
- 卸载：`scripts/uninstall.ps1 -InstallDir <目录> [-RemoveUserData] [-NoRestore]`；先让安装目录内程序短跑以恢复它拥有的 Shell 修改，再删除文件、匹配的 Run 值与开始菜单快捷方式；用户配置默认保留。
- 试用包：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-trial.ps1 -Version 0.1.0-m4`，输出 `artifacts\DeskShell-trial-<版本>.zip`（release exe、宿主、libmpv、安装/卸载脚本、INSTALL.txt）。包内安装脚本从同目录复制二进制；zip 不包含用户配置。
- 安装验收：`scripts/dev.ps1 install-check`（或 `scripts/install-check.ps1`）在 `artifacts\install-test` 安装、检查文件、临时启用/关闭并恢复 HKCU Run、启动/退出安装目录程序、卸载并检查无残留进程；使用隔离的 APPDATA/LOCALAPPDATA，不改真实用户配置。
- 性能：`target\release\deskshell.exe perf-check [--seconds 300] [--warmup 20] [--max-cpu 0.5] [--no-desktop] [--report PATH]`。必须使用 release 构建做正式测量；默认 300 秒、桌面附着、Eco 静态图片，报告写入 `artifacts/`。`scripts/dev.ps1 perf-check` 使用默认 5 分钟参数。
- 性能口径：CPU 为 `GetProcessTimes` kernel+user 执行时间，报告同时给单核百分比与整机归一百分比（除以逻辑 CPU 数）；GPU 时间未采集；DWM 在本机受保护，读取失败会写入报告而不是伪造数值。测量结束后由验收工具终止被测进程，正常退出路径由 `host-check` 与安装/卸载验收覆盖；短测只用于冒烟，不能替代 5 分钟验收。
- 本机实测（Windows 11 build 26200，AMD Radeon 780M，单屏 2560×1440@125%，16 逻辑 CPU）：`check`、`host-check`（含 Eco 静态与恢复）、`app-self-test`（含阴影）通过；release 桌面附着 5 分钟整机归一 **0.0078%**（单核 0.125%，进程树 CPU 0.375 s），报告 `artifacts/m4-perf-300s-desktop.json`。安装/覆盖安装/启动退出/卸载与 HKCU Run 校验通过，试用包 `artifacts/DeskShell-trial-0.1.0-m4.zip` 可安装。
- 未验证：模糊（M5）、真实全屏/电池/RDP 规则链路、真实托盘菜单逐项点选与文件对话框、多屏与 100%/150% DPI、NVIDIA/Intel/混合显卡。手工步骤见 `docs/DEVELOPMENT_PLAN.md` M4。

## M5 运行与验收

- 配置：版本 3；`quality.effects` 保存调色/暗角/模糊/Bloom/粒子/渐变强度（0..=100）。v2 配置首次加载会写 `config.v2-<时间>.bak` 后迁移；未知字段与 `.prev` 回滚规则不变。
- 预设：Eco 关闭图片效果；Balanced 默认调色 35、暗角 25；Ultra 默认调色 55、暗角 40、模糊 20、Bloom 30、渐变 20，粒子默认 0；Custom 使用用户逐项值。托盘“画质 → 图片效果”提供关闭/低/中/高，首次修改自动切换 Custom。
- 预览验证：`target\debug\deskshell.exe wallpaper --file testdata\4k-pattern.png --kind image --effects [--particles] --seconds N`；状态行显示 `路径 D3D11 效果，pass N，Present N，GPU …`。
- 参考截图：`scripts/dev.ps1 effects-check` 调用 `wallpaper-host --effects-check`，生成 `artifacts/m5-effects-reference.png` 与 `artifacts/m5-effects-particles.png`，记录 SHA-256；不经过 IPC，不修改桌面。
- 视频互操作原型：`scripts/dev.ps1 interop-check` 调用 `wallpaper-host --interop-check --file testdata\4k-h264-30fps.mp4 --output artifacts\m5-video-interop.png --width 1280 --height 720 --seconds 3`。它验证固定 libmpv 的 `MPV_RENDER_API_TYPE_D3D11`、同设备纹理、硬解与单帧墙钟；正常路径每帧 0 次回读，结束时 1 次诊断回读。该原型不接入日常播放。
- 效果语义：静态效果只在参数/尺寸/素材变化时渲染一帧并停止 Present；粒子在 `Animate` 且壁纸可见时按 33 ms 期限唤醒，`Static`/暂停/停止后清除期限；关闭全部效果会释放 D3D11 管线并回到 mpv 图片路径。视频仍由 mpv 直接播放，不与图片效果叠加。
- 效果失败：记录 `effects_error` 并回退 mpv 图片路径；不因效果失败阻断图片显示。真实设备移除/重置未注入，不能把回退路径当成设备恢复验收。
- GPU 时间：已实现 timestamp disjoint/timestamp 查询；本机 AMD 780M 驱动返回 Frequency 100 MHz 但时间戳恒 0，状态显示“不可用”。host-check 的 CPU 百分比来自宿主进程 `GetProcessTimes`，视频互操作的平均/p95 是包含 mpv 帧时间等待的墙钟，不是 GPU 执行时间。
- 实测（Windows 11 build 26200，AMD Radeon 780M，单屏 2560×1440@125%）：`host-check` 新增效果场景 PASS（9→5→0 pass、静态 1.2 s Present 增量 0、粒子 21 次/秒、Static 后停帧、CPU 0.00% 单核）；`effects-check` 参考图与粒子图非空并人工检查；`interop-check` 多次 3 s 运行 85–87 帧、首帧 219–265 ms、平均 33.6–34.4 ms、p95 34.4–34.6 ms、`d3d11va`、每帧 0 拷贝。
- 未验证：真实托盘菜单逐项点选、真实设备失效、多屏/100%/150% DPI、NVIDIA/Intel/混合显卡、HDR、24/72 小时资源趋势、视频效果接入日常播放；GPU 时间需换驱动或外部工具另行测量。
- 背景模糊（窗口背板）不在本阶段图片效果范围；当前交付的是图片后处理模糊，面板阴影不能替代。

## M6 运行与验收

- CLI（连接已运行的 `deskshell app`，当前用户管道，输出 `--json`）：
  - `deskshell status|pause|resume|restore-desktop|quit [--json]`
  - `deskshell preset eco|balanced|ultra|custom [--json]`
  - `deskshell set-wallpaper --file PATH [--kind image|video] [--poster PATH] [--desktop|--preview] [--display ID] [--effects off|balanced|ultra] [--managed|--reference] [--json]`
  - `deskshell diagnostics export --output FILE [--json]`
  - `deskshell assets export --output DIR [--json]`
  - 退出码：0 成功、2 参数错误、3 应用未运行、4 命令拒绝、5 超时/传输失败。`rejected` 表示命令被业务拒绝（例如素材不存在）。
- 隔离 CLI 验收：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 cli-check`。脚本使用 `artifacts\cli-check` 独立配置，验证状态、暂停/恢复、设置壁纸与 9 pass 效果、预设、诊断导出、非法参数退出码、退出与无孤儿宿主、未运行退出码。
- 诊断：日志 `%LOCALAPPDATA%\DeskShell\logs\deskshell.log`，JSON 行，2 MiB 滚动、最多 5 个历史文件；导出包包含应用状态、宿主快照、Windows build、DXGI 适配器/显示器、最近 500 行日志。日志不作为控制流。
- 受管理素材：`--managed` 复制到 `%LOCALAPPDATA%\DeskShell\assets`，配置记录 `managed=true` 与 `original_source`；默认引用原文件。素材缺失时回退 `last_good_source`（已实测），配置 `.prev`/迁移 `.bak` 继续保留。`assets export` 导出配置快照、素材与 manifest；导入用 `set-wallpaper --managed`。
- 打包：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 package-release`。生成 `artifacts\DeskShell-0.2.0-m6` 与 zip，含 `manifest.json`、`SHA256SUMS`、GPL 文本、第三方说明、安装/卸载/回滚脚本。manifest 记录 rustc/cargo、git commit/dirty、协议 5、配置 3、全部文件哈希与 libmpv 校验值。构建记录可追溯，但不声称比特级可复现。
- 安装/回滚：`scripts/install.ps1` 默认在升级前备份旧 exe/宿主/libmpv 到 `InstallDir\previous`；`scripts/rollback.ps1 -InstallDir DIR` 恢复上一版本；`uninstall.ps1` 删除文件并恢复本程序拥有的 Shell 修改。程序运行中安装会被拒绝，`-Force` 才停止后替换。
- 兼容性：`scripts/dev.ps1 compat-report` 生成 `artifacts\compat-<时间>.json`；`docs/COMPATIBILITY.md` 记录 AMD 780M 实机样本与所有未验证项。
- 长稳：`scripts/dev.ps1 soak-check` 运行 72 秒冒烟（14 次采样、2 次宿主强杀、0 错误、0 孤儿、句柄差 -7、私有内存 +0.1 MiB）。正式 24/72 小时请运行 `scripts\soak-check.ps1 -Hours 24` / `-Hours 72 -Desktop`，并人工审查 JSON 趋势；短测不能替代长稳。
- 本机实测（Windows 11 build 26200，AMD Radeon 780M 驱动 32.0.12033.1030，单屏 2560×1440@125%）：`check`、`cli-check`、`soak-check`（72 s）、`compat-report`、隔离安装/升级/回滚/卸载通过；候选包 `artifacts/DeskShell-0.2.0-m6.zip`（49,982,912 字节，SHA-256 `E3052149…33130B8`，manifest 标记工作区 dirty）。回滚测试将安装目录 exe 换成 debug 后恢复为 release SHA-256 `66F4B9DD…712443`。
- 未完成/阻塞：24/72 小时真实长稳、NVIDIA/Intel/混合显卡、多屏、100%/150% DPI、HDR/10-bit/HEVC/AV1、真实驱动设备移除、在线更新通道与代码签名。无在线更新器时不提供“自动更新”假开关；稳定/实验边界仅由版本号与候选包说明表达。

## 排障

- `cargo` 不存在：重开终端或使用 `scripts/dev.ps1`，确认用户 PATH。
- 链接器/系统库缺失：用 VS Installer 补齐 C++ x64 工具和 Windows SDK。
- `host-check` 报 `libmpv 不可用`：执行 `scripts/fetch-mpv.ps1`；也可用 `DESKSHELL_MPV_DIR` 指向包含 `libmpv-2.dll` 的目录。
- `host-check` 报缺少测试素材：执行 `scripts/fetch-test-media.ps1`。
- 宿主报 `HardwareDecodeUnavailable`：确认显卡驱动与 D3D11VA 支持；宿主意在不静默回退软件解码，请改用静态图或受支持的编码。
- 窗口测试：当前是控制台程序创建 GUI 窗口，不能依赖 `WaitForInputIdle` 判断初始化；观察窗口/枚举 HWND 后发送关闭请求。
- PowerShell 5.1 如中文输出乱码，可使用 Windows Terminal 的 UTF-8 配置；不为此修改系统全局代码页。
- 原生库在确定版本后放入显式运行时目录并记录校验值，避免依赖随机系统 DLL。
