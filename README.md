# DeskShell

Windows 动态壁纸与极简桌面增强软件：效果上限高、基础开销低、性能由用户掌控。

当前是 **S0 开发框架**，可运行 CLI、原生 Win32 基础窗口与跨进程握手；尚未实现透明胶囊、GPU 渲染或视频播放。

## 快速开始

在仓库根目录使用 Windows PowerShell：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 build
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 doctor
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 host-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 window
```

`window` 打开空白原生窗口，关闭窗口即退出。`host-check` 启动同目录宿主，验证版本/PID 后退出，不常驻后台。脚本仅为本次进程指定执行策略，不修改系统执行策略。

## 目录

| 路径 | 职责 |
|---|---|
| `crates/deskshell` | 主程序、CLI 与宿主启动编排 |
| `crates/wallpaper-host` | 独立壁纸宿主入口，目前为开发握手 |
| `crates/protocol` | 版本化跨进程数据，不依赖 UI/GPU |
| `crates/shell-win` | Windows API 与 unsafe 边界、基础窗口 |
| `crates/render` | 渲染能力和事件驱动帧需求模型，实际后端待 M0 |
| `scripts/dev.ps1` | 本地构建与检查入口 |

文档：[架构](ARCHITECTURE.md) · [开发环境](docs/DEVELOPMENT.md) · [分阶段计划](docs/DEVELOPMENT_PLAN.md) · [协作约定](AGENTS.md)
