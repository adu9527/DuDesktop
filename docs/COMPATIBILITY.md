# 兼容矩阵与环境记录

本文件记录实际验证过的 Windows/GPU/显示与媒体组合。没有样本的组合必须保持“未验证”，
不能用单一 AMD 机器推断 NVIDIA/Intel/混合显卡通过。

生成机器可读环境快照：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 compat-report
```

报告写入 `artifacts/compat-<时间>.json`，包含 OS build、GPU/驱动、显示器、`deskshell probe`
与 `doctor` 输出。复制报告摘要到下表，并注明验证日期与验收项。

## 已验证样本

| 日期 | Windows | GPU / 驱动 | 显示器 | DPI / 刷新率 | 后端 | 验证项 | 结果 |
|---|---|---|---|---|---|---|---|
| 2026-11-11 | Windows 11 build 26200（25H2） | AMD Radeon 780M / 32.0.12033.1030 | `\\.\DISPLAY2` 2560×1440 | 125% / 144 Hz | D3D11 + DComp + mpv v0.42.0-36-gd075ec2a2 | M0 透明合成/空闲停帧；M1 4K H.264 8-bit 硬解；M2 WorkerW 附着；M4 Eco 5 分钟；M5 图片效果与静态停帧 | PASS（已记录例外） |

同机还存在 `OrayIddDriver Device`（远程桌面虚拟显示驱动）。它未被用作渲染或性能样本；
不把虚拟显示驱动计入 GPU 兼容通过。

## 未验证 / 阻塞

| 项目 | 状态 | 需要的环境或方法 |
|---|---|---|
| NVIDIA 独显 / Optimus | 未验证 | 第二台 NVIDIA 机器；运行 `probe`、`host-check`、`effects-check`、`perf-check` |
| Intel 核显 / 混合显卡 | 未验证 | Intel 机器或混合输出；确认适配器 LUID、硬解与 D3D11 效果路径 |
| 多显示器（扩展/复制/负坐标） | 未验证 | 至少双屏；覆盖 HotPlug、主屏切换、跨屏 DPI |
| 100% 与 150% DPI | 未验证 | 修改显示比例后重跑窗口/命中/效果与桌面附着 |
| HDR / 10-bit / HEVC / AV1 | 未验收 | 独立 HDR 立项；准备对应编码素材并确认 mpv/驱动能力 |
| 真实设备移除/驱动重置 | 未验证 | 驱动重启或设备禁用注入；观察效果回退与 mpv 直接播放恢复 |
| 24 / 72 小时长稳 | 未完成 | `scripts/soak-check.ps1 -Hours 24/72`，并人工审查趋势；短测不能代替 |
| 签名与商店分发 | 未完成 | 需要代码签名证书与发布凭据；当前只产出本地候选包 |

## 记录规则

1. 只记录实际运行过的组合；模拟注入、合成错误与“代码路径存在”不算硬件通过。
2. GPU 时间无法读取时写“不可用”，不用 CPU 时间或 0 替代。
3. 性能数字必须注明构建类型（debug/release）、测量时长、进程树范围与归一方式。
4. 每次升级图形依赖（Rust windows crate、libmpv、驱动）后重跑相关样本；
   不默认跟随最新版本。
5. 兼容性结论以 `artifacts/` 中的报告与本文表格为准，不把单一厂商样本外推。
