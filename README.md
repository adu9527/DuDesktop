# DeskShell

Windows 动态壁纸与极简桌面增强软件：效果上限高、基础开销低、性能由用户掌控。

当前是 **M6 长期使用候选**：在 M0–M5 的透明面板、宿主监督、桌面附着、Dock/文件入口、配置/策略、D3D11 图片效果与视频互操作原型之上，已交付版本化 CLI、结构化滚动日志与诊断导出、受管理素材导入与缺失回退、安装升级备份/回滚、GPL 许可材料、可追溯本地候选包与兼容性报告。`cli-check`、72 秒 `soak-check`、隔离安装/升级/回滚/卸载与本机兼容报告通过。24/72 小时长稳、NVIDIA/Intel/混合显卡、多屏、100%/150% DPI、HDR 与真实设备失效仍未验收，因此不把本版本写成正式发布。

## 快速开始

在仓库根目录使用 Windows PowerShell。首次运行需要准备固定版本的 libmpv 与测试素材（均不提交到仓库）：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 fetch-mpv
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 fetch-test-media
```

随后：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 build
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 host-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 effects-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 interop-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 cli-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 soak-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 compat-report
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 package-release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 app-self-test
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 perf-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 doctor
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 probe
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 capsule
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 capsule-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 window
```

手动播放一个素材（独立预览窗口；`--seconds` 到时退出，省略则按 Enter 退出）：

```powershell
.\target\debug\deskshell.exe wallpaper --file testdata\4k-h264-30fps.mp4 --kind video --seconds 10
.\target\debug\deskshell.exe wallpaper --file testdata\4k-pattern.png --kind image --effects --particles --seconds 10
```

`host-check` 会真实启动宿主并验证：4K 硬解、暂停/停止、Eco 静态模式、图片效果 pass 裁剪与停帧、错误素材、静态回退、杀死宿主后的有限退避恢复、双实例独立控制、非法协议/超限消息/超时/断开故障注入，以及正常退出与强杀后的孤儿进程检查。`effects-check` 生成图片效果参考截图；`interop-check` 运行 mpv D3D11 视频互操作原型并输出一帧参考图。`perf-check` 默认运行 5 分钟真实产品进程树，正式测量请使用 release 构建。图形类检查（`capsule-check`、`host-check`、`effects-check`、`interop-check`、`app-self-test`、`perf-check`）应串行执行，避免窗口互相遮挡。

日常使用：

```powershell
.\target\release\deskshell.exe app
.\target\release\deskshell.exe status --json
.\target\release\deskshell.exe pause
.\target\release\deskshell.exe resume
.\target\release\deskshell.exe set-wallpaper --file testdata\4k-pattern.png --kind image --desktop --effects ultra --managed
.\target\release\deskshell.exe preset balanced
.\target\release\deskshell.exe diagnostics export --output artifacts\diagnostics.json
.\target\release\deskshell.exe assets export --output artifacts\assets-export
.\target\release\deskshell.exe restore-desktop
.\target\release\deskshell.exe quit
```

CLI 退出码：0 成功、2 参数错误、3 应用未运行、4 命令拒绝、5 超时/传输失败。

安装/升级/回滚（用户级，不需要管理员）：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install.ps1 -SourceDir target\release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rollback.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/uninstall.ps1
```

候选发布包：`scripts\package-release.ps1` 生成 `artifacts\DeskShell-<版本>.zip`，含 SHA-256 manifest、GPL 文本、第三方源码提供说明与安装/卸载/回滚脚本。当前只产出本地候选包，不伪造签名。当前候选：`DeskShell-0.2.0-m6.zip`（SHA-256 `E3052149…33130B8`，manifest 标记工作区 dirty）。

## 目录

| 路径 | 职责 |
|---|---|
| `crates/deskshell` | 主程序、版本化 CLI 控制通道、画质策略、配置、诊断、宿主监督器与验收命令 |
| `crates/wallpaper-host` | 独立壁纸宿主：IPC 服务循环、预览/桌面窗口、libmpv 播放、D3D11 图片效果与互操作原型 |
| `crates/protocol` | 版本化 IPC 数据契约、帧格式、配置/状态模型 |
| `crates/shell-win` | Win32 边界：命名管道、Job/进程、预览窗口、DPI |
| `crates/render` | DXGI 能力探测、透明合成/阴影层、面板、Render Graph 与 D3D11 图片效果管线 |
| `scripts/dev.ps1` | 本地构建、检查、验收入口 |
| `scripts/fetch-mpv.ps1` | 下载并校验固定 libmpv 构建到 `third_party/mpv/` |
| `scripts/fetch-test-media.ps1` | 下载固定 FFmpeg 工具并生成 `testdata/` 验收素材 |
| `scripts/install.ps1` / `scripts/uninstall.ps1` | 用户级安装与卸载；覆盖安装备份上一版本，卸载先恢复本程序拥有的 Shell 修改 |
| `scripts/rollback.ps1` | 从安装目录 `previous` 恢复上一版本 |
| `scripts/package-release.ps1` | 生成含 manifest/SHA256SUMS/GPL 材料的本地候选发布包 |
| `scripts/cli-check.ps1` / `scripts/soak-check.ps1` / `scripts/compat-report.ps1` | CLI 端到端、长稳资源采样、兼容性报告 |
| `licenses/` | GPLv2 文本与第三方源码提供说明（随包分发） |
| `scripts/package-trial.ps1` | 生成包含 release 二进制、libmpv 与安装脚本的内部试用 zip |
| `scripts/install-check.ps1` | 隔离用户数据目录的安装、自启、启动、卸载回归 |
| `third_party/mpv/` | 本地运行资源（不提交）；许可证与校验值见 ADR 0003 |
| `testdata/` | 本地测试素材（不提交） |

文档：[架构](ARCHITECTURE.md) · [开发环境](docs/DEVELOPMENT.md) · [分阶段计划](docs/DEVELOPMENT_PLAN.md) · [兼容矩阵](docs/COMPATIBILITY.md) · [协作约定](AGENTS.md) · [ADR 0001](docs/adr/0001-m0-composition-path.md) · [ADR 0002](docs/adr/0002-m1-ipc-and-host-supervision.md) · [ADR 0003](docs/adr/0003-m1-libmpv-integration.md) · [ADR 0004](docs/adr/0004-m2-desktop-attachment.md) · [ADR 0005](docs/adr/0005-m3-ui-and-shell-integration.md) · [ADR 0006](docs/adr/0006-m4-quality-policy-config-and-static-mode.md) · [ADR 0007](docs/adr/0007-m5-effects-backend-and-video-interop.md) · [ADR 0008](docs/adr/0008-m6-cli-diagnostics-assets-release.md)
