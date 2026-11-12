# ADR 0003：M1 固定 libmpv 与窗口嵌入播放

- 日期：2026-11-11
- 状态：已接受；M1 播放路径验证通过，D3D11 render API 留给 M5

## 背景

M1 需要独立宿主播放 4K 视频，并满足：硬解状态可观察、失败有明确原因、不静默启用软件解码、暂停停止持续解码、不引入逐帧 CPU 回读。架构要求固定二进制、C API/绑定版本、下载来源、校验值与许可证。

## 决策

1. **固定构建**：采用 `zhongfly/mpv-winbuild` 的 Windows x64 开发包。
   - Release：`2026-11-10-d075ec2a28`
   - 资源：`mpv-dev-x86_64-20261110-git-d075ec2a28.7z`
   - 来源：<https://github.com/zhongfly/mpv-winbuild/releases/tag/2026-11-10-d075ec2a28>
   - 归档 SHA256：`9342ed8dcbba81ab16c96c4155239524f563789b6a7261c748da9ea2adc874fe`
   - `libmpv-2.dll` SHA256：`0586211ed50e6c1b8eeb01952f7d812b205d81bc678f280810759d62e56bf545`
   - 实测：mpv `v0.42.0-36-gd075ec2a2`，client API `2.6`。
   - 许可证：mpv 为 GPLv2+；该构建捆绑的 FFmpeg 及其他原生库按其各自许可证分发。对外分发前必须完成许可证合规审查与源码/许可提供方案。

2. **加载方式**：构建期不链接 libmpv，也不依赖 mpv SDK。
   - 运行时用 `LoadLibraryW` + `GetProcAddress` 动态绑定 C API，固定检查 client API 主版本为 2、次版本 ≥ 1。
   - 搜索顺序：`DESKSHELL_MPV_DIR`、宿主可执行文件目录、可执行文件目录下 `mpv\`、工作区 `third_party\mpv\`。
   - `scripts/fetch-mpv.ps1` 下载、校验并解压到 `third_party\mpv\`（被 `.gitignore` 排除）；头文件一并保留用于版本核对。
   - 模块句柄在进程生命周期内不卸载，避免 mpv 线程仍在使用时释放 DLL。

3. **播放路径**：每个壁纸实例一个独立 mpv 上下文，通过 `wid` 嵌入宿主预览窗口。
   - `vo=gpu`、`gpu-context=d3d11`、`gpu-api=d3d11`。
   - `hwdec=d3d11va`、`hwdec-codecs=all`、`hwdec-software-fallback=no`；不静默切换软件视频解码。
   - `audio=no`、`ao=null`：壁纸宿主不打开音频输出，避免音频泄漏。
   - `idle=yes`、`image-display-duration=inf`；静态图片由同一 mpv 上下文显示，不额外实现 WIC/D2D 图片路径。
   - 不使用 mpv `loop-file=inf`：该模式在解码失败时可能无限重试而不产生 `end-file`。循环由宿主在收到 EOF 后重新加载文件实现，保留错误可见性。
   - 不使用 D3D11 render API、纹理互操作或逐帧 CPU 回读；这些属于 M5 的独立互操作原型，不作为 M1 播放基础。

4. **状态与失败判定**：
   - 观察属性：`hwdec-current`、`video-codec`、`width`、`height`、`container-fps`、`video-params/pixelformat`、`pause`、`idle-active`。
   - 硬解确认：视频首帧后必须同时具备有效宽高与非 `no` 的 `hwdec-current`。5 秒内未满足则判定 `HardwareDecodeUnavailable`。
   - `end-file` 错误映射为 `UnsupportedMedia`、`HardwareDecodeUnavailable` 或 `DecodeFailed`；错误在协议状态中保留，后续正常素材可覆盖。
   - 配置了海报素材时，失败后在同一 mpv 上下文加载海报并进入 `Fallback`；未配置海报则进入 `Stopped` 并保留错误原因。
   - 播放/暂停/停止：暂停后停止持续解码；停止释放解码与 VO 播放资源，但保留宿主进程、预览窗口和 mpv 上下文以便再次加载。

5. **测试素材**：
   - `scripts/fetch-test-media.ps1` 固定 FFmpeg 开发包
     `ffmpeg-2026-11-05-git-0893feba65-essentials_build.7z`
     （SHA256 `9d2c2ca426699e4e0f180341f001d1e4ed95ae3231b160853f24891c15706ccd`），
     生成 3840×2160 H.264 High / yuv420p / 30 fps / 10 秒样本与 MPEG-4 Part 2 不支持编码样本。
   - 测试素材位于 `testdata/`（被 `.gitignore` 排除），不随仓库分发。

## 证据

`scripts/dev.ps1 host-check` 实测：

- 4K 样本：H.264 High，3840×2160，30.00 fps，`yuv420p` 8 bit，硬解后端 `d3d11va`，5 秒播放掉帧 0。
- 静态图片与暂停状态 CPU 采样 0.00% 单核；停止并等待释放后 2 秒采样 0.00% 单核。
- MPEG-4 Part 2 素材未产生有效画面：5 秒看门狗判定 `HardwareDecodeUnavailable`，配置海报后进入 `Fallback` 并显示海报。
- 非视频文件返回 `UnsupportedMedia`，随后正常图片/视频可播放。
- 杀死宿主后新宿主重新加载期望素材；双实例独立暂停/停止。

## 已知限制

- 只在 AMD Radeon 780M 上验证；NVIDIA、Intel、混合显卡和 HDR/10-bit 矩阵未测试。
- 真实驱动设备移除/重置未注入；M1 只验证进程级故障与合成错误注入（M0）。
- 未使用音频输出，因此“无音频泄漏”由 `audio=no`/`ao=null` 的结构性配置保证，未用音频端点测量。
- 预览窗口不是 WorkerW 桌面附着；M2 才验证图标层下的窗口层级与恢复。
- 多显示器物理矩阵未验证；实例模型支持每实例独立显示器索引与窗口位置。
- 该 GPL 构建在正式分发前需要法律与源码提供方案；当前仅为开发验证。
