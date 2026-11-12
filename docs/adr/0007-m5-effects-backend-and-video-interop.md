# ADR 0007：M5 高画质效果后端与视频互操作边界

- 日期：2026-11-11

- 状态：已接受；图片效果管线已交付并通过 host-check/参考截图验收；视频互操作原型已实际跑通，但未接入日常播放；真实设备失效与长稳未验证

## 背景

M5 需要交付可见的画质提升，并保证关闭效果后真实停止 pass、定时器和资源占用。可选路线：

1. 原生 D3D11 + DXGI 交换链 + WIC 解码 + HLSL（运行时编译）；
2. wgpu（DX12/Vulkan 后端）+ egui/自绘；
3. 继续依赖 libmpv 的 `vo=gpu` 做效果。

约束：

- M0 已验证 D3D11 + DirectComposition 透明合成；M3 的 Dock/岛体使用 D3D11 + D2D + DComp；M1/M2 的壁纸窗口由宿主拥有，需要附着到 WorkerW。
- 壁纸是整屏层，不需要每帧 CPU 往返；图片效果只需要在参数/尺寸变化时重绘，静态时停帧。
- 不绑定 GPU 厂商；AMD/NVIDIA/Intel 都支持 D3D11 与 WIC。
- 不为了“更先进”引入新的设备/队列/同步模型，除非有可验证收益。

## 决策

### 1. 图片与程序化效果采用原生 D3D11 路径

- 渲染设备：宿主进程内单个 `ID3D11Device`（`D3D11_CREATE_DEVICE_BGRA_SUPPORT`）。
- 窗口呈现：`IDXGIFactory2::CreateSwapChainForHwnd`（`DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`、`DXGI_ALPHA_MODE_IGNORE`）。壁纸层不透明，不需要 DComp 透明度；Dock/胶囊继续使用已验证的 DComp 路径。
- 图片解码：WIC（`IWICImagingFactory` → `IWICFormatConverter` → `32bppBGRA`），仅在素材加载时执行一次；不逐帧 GPU→CPU→GPU 往返。
- 效果：内嵌 HLSL 源码，运行时用 `D3DCompile`（系统自带 `d3dcompiler_47.dll`）编译为像素着色器；全屏三角形，无顶点缓冲/输入布局。
- 效果节点：调色、暗角、Blur（可分离两 pass）、Bloom（阈值 + 两 pass 模糊 + 合成）、粒子、渐变；每个效果独立开关与强度。最后始终有一个输出/复制 pass，便于逐项验证关闭效果后 pass 数下降。
- 静态语义：所有效果关闭或只有静态效果时，只在参数/尺寸/素材变化时渲染一帧，然后停止 Present；粒子等时间相关效果只在 `RenderPlayback::Animate` 且壁纸可见时按 30 fps 调度，切换为 `Static` 立即停帧。
- GPU 时间：D3D11 timestamp disjoint/timestamp 查询，CPU 墙钟、CPU 执行时间与 GPU 时间分开记录；查询不可用时明确上报 `None`，不用 CPU 时间冒充 GPU 时间。
- 设备丢失：区分设备移除/重置与普通失败；重建一次，仍失败则回退到 mpv 图片路径并上报 `effects_error`，不拖垮桌面入口。

### 2. 不引入 wgpu

在本阶段 wgpu 不提供需要的额外能力：

- 目标窗口是已有的 Win32/WorkerW 子窗口，D3D11 交换链可直接绑定；wgpu 需要额外的 surface/queue 配置与 DComp/DXGI 互操作验证。
- 效果是本项目自有的少数全屏 pass，不需要跨平台着色器生态。
- wgpu 会引入更大的依赖树与新的设备丢失/同步路径；当前没有可量化的收益抵消维护成本。

保留复审条件：出现复杂 UI/3D 场景、需要跨平台后端，或视频互操作只能通过 wgpu 的 DX12 共享纹理完成时，重新发起隔离原型并写新 ADR；不直接改写已交付的 D3D11 路径。

### 3. 视频互操作是独立原型，不阻塞图片/程序化效果

固定 libmpv 构建导出 `mpv_render_context_create` 等 render API 符号，且 `render.h` 声明 `MPV_RENDER_API_TYPE_D3D11`；随附头文件未包含 `render_d3d11.h`，因此按官方 `render_d3d11.h` 的 ABI 做独立原型验证：

- `mpv_d3d11_init_params` 仅含 `ID3D11Device*`；mpv 自行增加引用，并可在解码线程使用同一设备。
- `mpv_d3d11_fbo` 仅含 `ID3D11Texture2D*`（`D3D11_BIND_RENDER_TARGET`、`DEFAULT`、非 MSAA/数组/mip）；mpv 在返回前把渲染提交到立即上下文，不保留纹理引用。
- 原型必须验证：同适配器设备、纹理格式、fence/同步语义、所有权、设备恢复、拷贝次数与延迟。
- 原型未通过前，视频仍走 M1 的 `wid` 直接播放；视频不叠加图片效果，托盘不提供假开关。

## 原型验证结果（2026-11-11）

- `wallpaper-host --interop-check` 使用固定 libmpv `mpv v0.42.0-36-gd075ec2a2`，以 `MPV_RENDER_API_TYPE_D3D11` 在调用方 `ID3D11Device` 上创建 render 上下文，并把 H.264 4K 视频渲染到调用方 BGRA8 纹理。
- 多次 3 秒运行 85–87 帧，硬解 `d3d11va`；首帧 219–265 ms，平均 33.6–34.4 ms、p95 34.4–34.6 ms、最大 150–201 ms（单帧墙钟，包含 mpv 目标帧时间等待，不是 GPU 执行时间）。
- 正常路径每帧 0 次 GPU→CPU→GPU 拷贝；结束时仅 1 次诊断回读保存 PNG（`artifacts/m5-video-interop.png`，已人工检查画面正确）。
- 尚未验证：真实设备移除/重置、暂停/停止/EOF/循环、显存与长稳、接入日常播放后的生命周期。因此不把视频效果写成已交付。

## 后果

- 图片效果可在 AMD/NVIDIA/Intel 上按 D3D11 能力运行；不支持的格式/设备明确回退 mpv 图片路径。
- 关闭全部效果后宿主释放效果管线（纹理、交换链、着色器）并回到 mpv 图片路径；静态效果只渲染一次。
- 视频画质增强暂不交付，互操作原型单独记录测量结果；不把它写成已支持。
- HDR 仍需独立验收，不因本 ADR 自动声明支持。
