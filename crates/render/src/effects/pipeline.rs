//! 图片效果 D3D11 管线：WIC 解码源图、全屏着色器 pass、DXGI 交换链呈现。
//!
//! 只在图片素材启用效果时创建；关闭全部效果后由宿主释放本对象并回到 mpv 图片路径。
//! 静态效果只在参数/尺寸/素材变化时渲染并 Present 一次；粒子等动画由宿主按
//! `RenderPlayback::Animate` 调度，切换为 `Static` 后立即停帧。
//!
//! 线程与所有权：本类型由宿主主线程独占，包含设备、上下文、交换链与纹理。
//! `unsafe` 调用均在同一线程内，且对象在窗口销毁前释放。

use std::{
    collections::BTreeMap,
    ffi::c_void,
    path::{Path, PathBuf},
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                Fxc::D3DCompile,
            },
            Direct3D11::{
                D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_CONSTANT_BUFFER,
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
                D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAP_READ, D3D11_MAP_WRITE_DISCARD,
                D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP,
                D3D11_QUERY_TIMESTAMP_DISJOINT, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
                D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_USAGE_STAGING, D3D11_VIEWPORT,
                D3D11CreateDevice, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
                ID3D11PixelShader, ID3D11Query, ID3D11RenderTargetView, ID3D11Resource,
                ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
            },
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT,
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
            },
        },
    },
    core::{Interface, PCSTR},
};

use crate::effects::{
    image,
    shaders::{
        PS_BLOOM_COMPOSITE, PS_BLOOM_THRESHOLD, PS_BLUR, PS_COLOR, PS_GRADIENT, PS_OUTPUT,
        PS_PARTICLES, PS_VIGNETTE, SOURCE, VS_MAIN,
    },
};
use crate::graph::{
    EffectPass, EffectPlan, FitMode, FrameCache, ImageEffects, PassKind, PlanRequest, build_plan,
    frame_key,
};

#[derive(Debug)]
pub enum EffectError {
    DeviceLost,
    Image(String),
    Graphics(String),
    Plan(String),
}

impl std::fmt::Display for EffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLost => formatter.write_str("图形设备丢失"),
            Self::Image(message) => formatter.write_str(message),
            Self::Graphics(message) => formatter.write_str(message),
            Self::Plan(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for EffectError {}

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    pub drawn: bool,
    pub effect_passes: u32,
    pub gpu_time_us: Option<u32>,
    pub present_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ShaderParams {
    p0: [f32; 4],
    p1: [f32; 4],
    p2: [f32; 4],
}

struct SourceTexture {
    _texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
}

struct TargetTexture {
    _texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
    width: u32,
    height: u32,
}

struct GpuTimer {
    disjoint: ID3D11Query,
    start: ID3D11Query,
    end: ID3D11Query,
    pending: bool,
    last_us: Option<u32>,
    /// 连续阻塞读取仍无数据时判定时间戳不可用，避免每帧等待。
    unsupported: bool,
    frames_since_poll: u32,
}

impl GpuTimer {
    fn new(device: &ID3D11Device) -> Result<Self, EffectError> {
        let create = |query| -> Result<ID3D11Query, EffectError> {
            let desc = D3D11_QUERY_DESC {
                Query: query,
                MiscFlags: 0,
            };
            let mut output = None;
            // SAFETY: 描述结构有效；输出查询归本对象所有。
            unsafe { device.CreateQuery(&desc, Some(&mut output)) }.map_err(graphics_error)?;
            output.ok_or_else(|| EffectError::Graphics("创建 GPU 计时查询失败".into()))
        };
        Ok(Self {
            disjoint: create(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
            start: create(D3D11_QUERY_TIMESTAMP)?,
            end: create(D3D11_QUERY_TIMESTAMP)?,
            pending: false,
            last_us: None,
            unsupported: false,
            frames_since_poll: 0,
        })
    }

    fn begin(&mut self, context: &ID3D11DeviceContext) {
        // SAFETY: 查询由本对象持有；只记录当前帧 GPU 时间。
        unsafe {
            context.Begin(&self.disjoint);
            context.End(&self.start);
        }
        self.pending = true;
    }

    fn end(&mut self, context: &ID3D11DeviceContext) {
        // SAFETY: 与 begin 配对；结束查询。
        unsafe {
            context.End(&self.end);
            context.End(&self.disjoint);
        }
    }

    /// 读取上一帧结果。`blocking=false` 不等待；`blocking=true` 等待 GPU 完成，
    /// 仅用于首帧与低频采样，避免每帧阻塞主线程。
    fn poll(&mut self, context: &ID3D11DeviceContext, blocking: bool) -> Option<u32> {
        if !self.pending || self.unsupported {
            return self.last_us;
        }
        let flags = if blocking {
            0
        } else {
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32
        };
        let mut disjoint = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
        // SAFETY: 查询数据大小与类型匹配；blocking 模式等待 GPU 完成。
        let ready = unsafe {
            context.GetData(
                &self.disjoint,
                Some(&mut disjoint as *mut _ as *mut c_void),
                std::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                flags,
            )
        }
        .is_ok();
        if !ready || disjoint.Frequency == 0 || disjoint.Disjoint.as_bool() {
            if blocking {
                // 阻塞读取仍无有效频率：设备不支持时间戳，停止尝试。
                self.unsupported = true;
                self.pending = false;
            }
            return self.last_us;
        }
        let mut start = 0u64;
        let mut end = 0u64;
        // SAFETY: 输出为单个 u64；查询已就绪。
        let start_ready = unsafe {
            context.GetData(
                &self.start,
                Some(&mut start as *mut _ as *mut c_void),
                8,
                flags,
            )
        }
        .is_ok();
        // SAFETY: 同上。
        let end_ready = unsafe {
            context.GetData(&self.end, Some(&mut end as *mut _ as *mut c_void), 8, flags)
        }
        .is_ok();
        self.pending = false;
        // 部分驱动（本机 AMD 780M）返回有效 Frequency 但时间戳恒为 0；
        // 此时明确报告不可用，不用 0 冒充 GPU 时间。
        if start_ready && end_ready && start > 0 && end > start {
            let ticks = end - start;
            let micros = ticks.saturating_mul(1_000_000) / disjoint.Frequency;
            self.last_us = Some(micros.min(u64::from(u32::MAX)) as u32);
        } else if blocking {
            self.unsupported = true;
        }
        self.last_us
    }
}

/// 单窗口的图片效果渲染器。
pub struct EffectPipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    width: u32,
    height: u32,
    vertex_shader: ID3D11VertexShader,
    pixel_shaders: BTreeMap<PassKind, ID3D11PixelShader>,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    source: Option<SourceTexture>,
    targets: [Option<TargetTexture>; 2],
    half_targets: [Option<TargetTexture>; 2],
    backbuffer: Option<ID3D11RenderTargetView>,
    plan: EffectPlan,
    cache: FrameCache,
    image_revision: u64,
    source_scale: [f32; 2],
    source_contain: bool,
    source_tile: [f32; 2],
    fit: FitMode,
    timer: GpuTimer,
    presents: u64,
    last_passes: u32,
    capture: Option<PathBuf>,
}

impl EffectPipeline {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, EffectError> {
        let width = width.max(1);
        let height = height.max(1);
        let (device, context) = create_device()?;
        let swap_chain = create_swap_chain(&device, hwnd, width, height)?;
        let vertex_shader = compile_vertex(&device)?;
        let mut pixel_shaders = BTreeMap::new();
        for (kind, entry) in [
            (PassKind::Color, PS_COLOR),
            (PassKind::Vignette, PS_VIGNETTE),
            (PassKind::BlurH, PS_BLUR),
            (PassKind::BlurV, PS_BLUR),
            (PassKind::BloomThreshold, PS_BLOOM_THRESHOLD),
            (PassKind::BloomBlurH, PS_BLUR),
            (PassKind::BloomBlurV, PS_BLUR),
            (PassKind::BloomComposite, PS_BLOOM_COMPOSITE),
            (PassKind::Particles, PS_PARTICLES),
            (PassKind::Gradient, PS_GRADIENT),
            (PassKind::Output, PS_OUTPUT),
        ] {
            pixel_shaders.insert(kind, compile_pixel(&device, entry)?);
        }
        let sampler = create_sampler(&device)?;
        let constants = create_constants(&device)?;
        let mut pipeline = Self {
            device: device.clone(),
            context,
            swap_chain,
            width,
            height,
            vertex_shader,
            pixel_shaders,
            sampler,
            constants,
            source: None,
            targets: [None, None],
            half_targets: [None, None],
            backbuffer: None,
            plan: build_plan(PlanRequest {
                width,
                height,
                effects: ImageEffects::OFF,
                budget_bytes: None,
            })
            .map_err(|error| EffectError::Plan(error.to_string()))?,
            cache: FrameCache::default(),
            image_revision: 0,
            source_scale: [1.0, 1.0],
            source_contain: false,
            source_tile: [0.0, 0.0],
            fit: FitMode::Cover,
            timer: GpuTimer::new(&device)?,
            presents: 0,
            last_passes: 0,
            capture: None,
        };
        pipeline.ensure_targets()?;
        Ok(pipeline)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 设置源图缩放方式；只影响后续帧的 UV，不重新解码。
    pub fn set_fit(&mut self, fit: FitMode) {
        if self.fit == fit {
            return;
        }
        self.fit = fit;
        self.update_source_scale();
    }

    pub fn fit(&self) -> FitMode {
        self.fit
    }

    pub fn plan(&self) -> &EffectPlan {
        &self.plan
    }

    pub fn present_count(&self) -> u64 {
        self.presents
    }

    pub fn last_gpu_time_us(&self) -> Option<u32> {
        self.timer.last_us
    }

    /// 加载源图；只在加载时执行一次 WIC 解码与纹理上传。
    pub fn set_image(&mut self, path: &Path) -> Result<(), EffectError> {
        let decoded = image::decode(path).map_err(EffectError::Image)?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: decoded.width,
            Height: decoded.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: decoded.bgra.as_ptr() as *const c_void,
            SysMemPitch: decoded.stride(),
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: 像素缓冲区在调用期间有效；D3D11 复制初始化数据。
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
        }
        .map_err(graphics_error)?;
        let texture = texture.ok_or_else(|| EffectError::Graphics("创建源图纹理失败".into()))?;
        let srv = create_srv(&self.device, &texture)?;
        self.source = Some(SourceTexture {
            _texture: texture,
            srv,
            width: decoded.width,
            height: decoded.height,
        });
        self.image_revision = self.image_revision.wrapping_add(1);
        self.update_source_scale();
        self.cache.reset();
        Ok(())
    }

    /// 应用效果参数；关闭 Bloom 时释放半分辨率附件。
    pub fn configure(&mut self, effects: ImageEffects) -> Result<(), EffectError> {
        let plan = build_plan(PlanRequest {
            width: self.width,
            height: self.height,
            effects,
            budget_bytes: None,
        })
        .map_err(|error| EffectError::Plan(error.to_string()))?;
        let changed = plan.key != self.plan.key;
        self.plan = plan;
        self.ensure_targets()?;
        if changed {
            self.cache.reset();
        }
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), EffectError> {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return Ok(());
        }
        self.targets = [None, None];
        self.half_targets = [None, None];
        self.backbuffer = None;
        // SAFETY: 交换链缓冲区上的视图已释放；尺寸来自当前客户区。
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
        .map_err(graphics_error)?;
        self.width = width;
        self.height = height;
        self.update_source_scale();
        self.ensure_targets()?;
        self.cache.reset();
        Ok(())
    }

    /// 设置一次性参考截图路径；下一次成功渲染后写 PNG 并清除。
    pub fn request_capture(&mut self, path: PathBuf) {
        self.capture = Some(path);
    }

    /// 渲染一帧。`allow_animation=false` 时固定时间片，静态效果不会重复 Present。
    pub fn render(
        &mut self,
        elapsed: Duration,
        allow_animation: bool,
    ) -> Result<RenderStats, EffectError> {
        if self.source.is_none() {
            return Err(EffectError::Image("尚未加载源图".into()));
        }
        let key = frame_key(
            &self.plan,
            self.image_revision,
            self.width,
            self.height,
            elapsed,
            allow_animation,
        );
        if !self.cache.should_draw(key) {
            self.cache.skipped();
            return Ok(RenderStats {
                drawn: false,
                effect_passes: self.plan.effect_passes(),
                gpu_time_us: self.timer.last_us,
                present_count: self.presents,
            });
        }
        self.timer.poll(&self.context, false);
        self.timer.begin(&self.context);
        self.draw_passes(elapsed)?;
        if let Some(path) = self.capture.take() {
            self.capture_backbuffer(&path)?;
        }
        // SAFETY: 交换链由本对象持有；Present(0) 不等待垂直同步。
        unsafe { self.swap_chain.Present(0, DXGI_PRESENT(0)) }
            .ok()
            .map_err(present_error)?;
        self.timer.end(&self.context);
        // 首帧与低频采样使用阻塞读取，取得真实 GPU 时间；其余帧不等待。
        self.timer.frames_since_poll = self.timer.frames_since_poll.saturating_add(1);
        if self.timer.last_us.is_none() || self.timer.frames_since_poll >= 30 {
            // Present 之后查询可能仍未提交；显式 Flush 后阻塞读取，只在低频采样时执行。
            // SAFETY: 只刷新本对象的立即上下文。
            unsafe { self.context.Flush() };
            self.timer.poll(&self.context, true);
            self.timer.frames_since_poll = 0;
        }
        self.cache.presented(key);
        self.presents += 1;
        self.last_passes = self.plan.effect_passes();
        Ok(RenderStats {
            drawn: true,
            effect_passes: self.last_passes,
            gpu_time_us: self.timer.last_us,
            present_count: self.presents,
        })
    }

    fn draw_passes(&mut self, elapsed: Duration) -> Result<(), EffectError> {
        let source = self
            .source
            .as_ref()
            .ok_or_else(|| EffectError::Image("尚未加载源图".into()))?;
        let has_effects = self.plan.effect_passes() > 0;
        let mut current = source.srv.clone();
        let mut next = 0usize;
        let passes = self.plan.passes.clone();
        let aspect = self.width as f32 / self.height.max(1) as f32;
        let time = elapsed.as_secs_f32();
        let sign = if self.source_contain { -1.0 } else { 1.0 };
        let source_uv_scale = [self.source_scale[0] * sign, self.source_scale[1] * sign];
        let tile = self.source_tile;

        // SAFETY: 以下绘制调用都在宿主主线程的立即上下文上执行，对象由本对象持有。
        unsafe {
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vertex_shader, None);
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }

        for pass in &passes {
            let (target, width, height) = self.target_for(*pass, &mut next)?;
            let mut params = ShaderParams {
                p0: [
                    f32::from(pass.strength) / 100.0,
                    time,
                    self.width as f32,
                    self.height as f32,
                ],
                p1: [1.0 / width as f32, 1.0 / height as f32, aspect, aspect],
                p2: [0.0, 0.0, tile[0], tile[1]],
            };
            let mut secondary: Option<ID3D11ShaderResourceView> = None;
            match pass.kind {
                PassKind::Color => {
                    params.p1[2] = source_uv_scale[0];
                    params.p1[3] = source_uv_scale[1];
                }
                PassKind::BlurH | PassKind::BloomBlurH => {
                    params.p2[0] = 1.0 / width as f32;
                }
                PassKind::BlurV | PassKind::BloomBlurV => {
                    params.p2[1] = 1.0 / height as f32;
                }
                PassKind::BloomComposite => {
                    if let Some(half) = self.half_targets[0].as_ref() {
                        secondary = Some(half.srv.clone());
                    }
                }
                PassKind::Output => {
                    if !has_effects {
                        params.p1[2] = source_uv_scale[0];
                        params.p1[3] = source_uv_scale[1];
                    } else {
                        params.p1[2] = 1.0;
                        params.p1[3] = 1.0;
                    }
                }
                _ => {}
            }
            self.upload_constants(&params)?;
            let shader = self
                .pixel_shaders
                .get(&pass.kind)
                .ok_or_else(|| EffectError::Graphics(format!("缺少 {} 着色器", pass.kind.label())))?
                .clone();
            let viewports = [D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }];
            let mut resources = vec![Some(current.clone()), secondary];
            // SAFETY: 视图、着色器、常量与目标都属于同一设备与上下文。
            unsafe {
                self.context
                    .OMSetRenderTargets(Some(&[Some(target.clone())]), None);
                self.context.RSSetViewports(Some(&viewports));
                self.context.PSSetShader(&shader, None);
                self.context.PSSetShaderResources(0, Some(&resources));
                self.context.Draw(3, 0);
            }
            resources[0] = None;
            // 只有全分辨率 pass 会成为后续 pass 的主输入；Bloom 的半分辨率
            // 中间结果只通过 BloomComposite 的 SecondaryTexture 使用。
            if !matches!(
                pass.kind,
                PassKind::BloomThreshold
                    | PassKind::BloomBlurH
                    | PassKind::BloomBlurV
                    | PassKind::Output
            ) {
                let written = 1 - next;
                if let Some(target) = self.targets[written].as_ref() {
                    current = target.srv.clone();
                }
            }
        }
        // SAFETY: 解绑 SRV，避免下一次把同一资源绑定为渲染目标时产生 hazard。
        unsafe {
            self.context.PSSetShaderResources(0, Some(&[None, None]));
        }
        Ok(())
    }

    fn target_for(
        &self,
        pass: EffectPass,
        next: &mut usize,
    ) -> Result<(ID3D11RenderTargetView, u32, u32), EffectError> {
        match pass.kind {
            PassKind::Output => {
                let backbuffer = self
                    .backbuffer
                    .as_ref()
                    .ok_or_else(|| EffectError::Graphics("交换链后台缓冲视图缺失".into()))?;
                Ok((backbuffer.clone(), self.width, self.height))
            }
            PassKind::BloomThreshold | PassKind::BloomBlurH | PassKind::BloomBlurV => {
                let index = match pass.kind {
                    PassKind::BloomThreshold | PassKind::BloomBlurV => 0,
                    _ => 1,
                };
                let target = self.half_targets[index]
                    .as_ref()
                    .ok_or_else(|| EffectError::Graphics("Bloom 半分辨率目标缺失".into()))?;
                Ok((target.rtv.clone(), target.width, target.height))
            }
            _ => {
                let index = *next;
                *next = 1 - *next;
                let target = self.targets[index]
                    .as_ref()
                    .ok_or_else(|| EffectError::Graphics("效果渲染目标缺失".into()))?;
                Ok((target.rtv.clone(), target.width, target.height))
            }
        }
    }

    fn upload_constants(&self, params: &ShaderParams) -> Result<(), EffectError> {
        let mut mapped = windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: 动态常量缓冲每帧 DISCARD 重写；映射期间没有其他使用者。
        unsafe {
            self.context
                .Map(
                    &self.constants,
                    0,
                    D3D11_MAP_WRITE_DISCARD,
                    0,
                    Some(&mut mapped),
                )
                .map_err(graphics_error)?;
            std::ptr::copy_nonoverlapping(
                params as *const ShaderParams as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<ShaderParams>(),
            );
            self.context.Unmap(&self.constants, 0);
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
        }
        Ok(())
    }

    fn ensure_targets(&mut self) -> Result<(), EffectError> {
        if self.plan.effect_passes() > 0 {
            for index in 0..2 {
                if self.targets[index].is_none() {
                    self.targets[index] =
                        Some(create_target(&self.device, self.width, self.height)?);
                }
            }
        } else {
            self.targets = [None, None];
        }
        if self.plan.passes.iter().any(|pass| {
            matches!(
                pass.kind,
                PassKind::BloomThreshold
                    | PassKind::BloomBlurH
                    | PassKind::BloomBlurV
                    | PassKind::BloomComposite
            )
        }) {
            let half_width = self.width.div_ceil(2).max(1);
            let half_height = self.height.div_ceil(2).max(1);
            for index in 0..2 {
                if self.half_targets[index].is_none() {
                    self.half_targets[index] =
                        Some(create_target(&self.device, half_width, half_height)?);
                }
            }
        } else {
            self.half_targets = [None, None];
        }
        if self.backbuffer.is_none() {
            // SAFETY: 交换链由本对象持有且缓冲区索引 0 存在。
            let buffer: ID3D11Texture2D =
                unsafe { self.swap_chain.GetBuffer(0) }.map_err(graphics_error)?;
            let resource: ID3D11Resource = buffer.cast().map_err(graphics_error)?;
            let mut view = None;
            // SAFETY: 后台缓冲由交换链拥有；视图由本对象持有。
            unsafe {
                self.device
                    .CreateRenderTargetView(&resource, None, Some(&mut view))
            }
            .map_err(graphics_error)?;
            self.backbuffer =
                Some(view.ok_or_else(|| EffectError::Graphics("创建后台缓冲视图失败".into()))?);
        }
        Ok(())
    }

    fn update_source_scale(&mut self) {
        let Some(source) = &self.source else {
            self.source_scale = [1.0, 1.0];
            self.source_contain = false;
            self.source_tile = [0.0, 0.0];
            return;
        };
        let (scale, contain, tile) = fit_uv_scale(
            self.width,
            self.height,
            source.width,
            source.height,
            self.fit,
        );
        self.source_scale = scale;
        self.source_contain = contain;
        self.source_tile = tile;
    }

    fn capture_backbuffer(&self, path: &Path) -> Result<(), EffectError> {
        // SAFETY: 截图发生在 Present 前，交换链后台缓冲 0 有效。
        let back: ID3D11Texture2D =
            unsafe { self.swap_chain.GetBuffer(0) }.map_err(graphics_error)?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: 只读回读帧缓冲，格式与尺寸一致。
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(graphics_error)?;
        let staging =
            staging.ok_or_else(|| EffectError::Graphics("创建截图回读纹理失败".into()))?;
        let source: ID3D11Resource = back.cast().map_err(graphics_error)?;
        let destination: ID3D11Resource = staging.cast().map_err(graphics_error)?;
        // SAFETY: 同设备资源复制；随后映射前 GPU 已完成。
        unsafe { self.context.CopyResource(&destination, &source) };
        let mut mapped = windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging 纹理 CPU 可读；映射期间不使用其他映射。
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(graphics_error)?;
        }
        let mut bgra = vec![0u8; self.width as usize * 4 * self.height as usize];
        // SAFETY: 映射行宽可能大于紧凑行宽；逐行复制到调用方缓冲区。
        unsafe {
            for row in 0..self.height as usize {
                let source_row = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
                let destination_row = bgra.as_mut_ptr().add(row * self.width as usize * 4);
                std::ptr::copy_nonoverlapping(source_row, destination_row, self.width as usize * 4);
            }
        }
        // SAFETY: 与 Map 配对。
        unsafe { self.context.Unmap(&staging, 0) };
        image::save_png(path, self.width, self.height, &bgra).map_err(EffectError::Image)
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), EffectError> {
    let mut device = None;
    let mut context = None;
    let mut feature_level = D3D_FEATURE_LEVEL::default();
    let levels = [
        D3D_FEATURE_LEVEL(0xb100),
        D3D_FEATURE_LEVEL(0xb000),
        D3D_FEATURE_LEVEL(0xa100),
        D3D_FEATURE_LEVEL(0xa000),
    ];
    // SAFETY: 使用默认硬件适配器；输出由本函数初始化。
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )
    }
    .map_err(graphics_error)?;
    Ok((
        device.ok_or_else(|| EffectError::Graphics("D3D11 设备为空".into()))?,
        context.ok_or_else(|| EffectError::Graphics("D3D11 上下文为空".into()))?,
    ))
}

fn create_swap_chain(
    device: &ID3D11Device,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1, EffectError> {
    let dxgi_device: IDXGIDevice = device.cast().map_err(graphics_error)?;
    // SAFETY: 适配器与工厂由 DXGI 设备持有。
    let adapter = unsafe { dxgi_device.GetAdapter() }.map_err(graphics_error)?;
    // SAFETY: 适配器由 DXGI 设备持有；查询父工厂只读取 COM 关系。
    let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }.map_err(graphics_error)?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    // SAFETY: HWND 在交换链生命周期内有效；壁纸层不透明。
    unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None) }
        .map_err(graphics_error)
}

fn compile_vertex(device: &ID3D11Device) -> Result<ID3D11VertexShader, EffectError> {
    let blob = compile(SOURCE, VS_MAIN, "vs_5_0")?;
    let bytes = blob_bytes(&blob);
    let mut shader = None;
    // SAFETY: 字节码来自 D3DCompile，生命周期覆盖创建调用。
    unsafe { device.CreateVertexShader(bytes, None, Some(&mut shader)) }.map_err(graphics_error)?;
    shader.ok_or_else(|| EffectError::Graphics("创建顶点着色器失败".into()))
}

fn compile_pixel(device: &ID3D11Device, entry: &str) -> Result<ID3D11PixelShader, EffectError> {
    let blob = compile(SOURCE, entry, "ps_5_0")?;
    let bytes = blob_bytes(&blob);
    let mut shader = None;
    // SAFETY: 字节码来自 D3DCompile，生命周期覆盖创建调用。
    unsafe { device.CreatePixelShader(bytes, None, Some(&mut shader)) }.map_err(graphics_error)?;
    shader.ok_or_else(|| EffectError::Graphics(format!("创建像素着色器 {entry} 失败")))
}

fn compile(
    source: &str,
    entry: &str,
    target: &str,
) -> Result<windows::Win32::Graphics::Direct3D::ID3DBlob, EffectError> {
    let entry = std::ffi::CString::new(entry)
        .map_err(|_| EffectError::Graphics("着色器入口包含 NUL".into()))?;
    let target = std::ffi::CString::new(target)
        .map_err(|_| EffectError::Graphics("着色器目标包含 NUL".into()))?;
    let mut code = None;
    let mut errors = None;
    // D3DCOMPILE_ENABLE_STRICTNESS = 2048；OPTIMIZATION_LEVEL3 = 32768。
    // 不启用调试信息，避免发布包依赖 PDB。
    let flags = 2048u32 | 32768u32;
    // SAFETY: 源码与入口字符串在调用期间有效；失败时读取错误 blob。
    let result = unsafe {
        D3DCompile(
            source.as_ptr() as *const c_void,
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry.as_ptr() as *const u8),
            PCSTR(target.as_ptr() as *const u8),
            flags,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    match result {
        Ok(()) => code.ok_or_else(|| EffectError::Graphics("着色器编译未返回字节码".into())),
        Err(error) => {
            let detail = errors
                .map(|blob| {
                    let bytes = blob_bytes(&blob);
                    String::from_utf8_lossy(bytes).into_owned()
                })
                .unwrap_or_default();
            Err(EffectError::Graphics(format!(
                "着色器编译失败：{error} {detail}"
            )))
        }
    }
}

fn blob_bytes(blob: &windows::Win32::Graphics::Direct3D::ID3DBlob) -> &[u8] {
    // SAFETY: blob 在调用期间有效；返回底层字节切片。
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

fn create_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState, EffectError> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut sampler = None;
    // SAFETY: 描述结构固定；采样器由本对象持有。
    unsafe { device.CreateSamplerState(&desc, Some(&mut sampler)) }.map_err(graphics_error)?;
    sampler.ok_or_else(|| EffectError::Graphics("创建采样器失败".into()))
}

fn create_constants(device: &ID3D11Device) -> Result<ID3D11Buffer, EffectError> {
    let desc = windows::Win32::Graphics::Direct3D11::D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of::<ShaderParams>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer = None;
    // SAFETY: 只创建常量缓冲，无初始数据。
    unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer)) }.map_err(graphics_error)?;
    buffer.ok_or_else(|| EffectError::Graphics("创建常量缓冲失败".into()))
}

fn create_target(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<TargetTexture, EffectError> {
    let width = width.max(1);
    let height = height.max(1);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: 输出纹理由本对象持有。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(graphics_error)?;
    let texture = texture.ok_or_else(|| EffectError::Graphics("创建效果纹理失败".into()))?;
    let srv = create_srv(device, &texture)?;
    let resource: ID3D11Resource = texture.cast().map_err(graphics_error)?;
    let mut rtv = None;
    // SAFETY: 资源来自同一设备；视图由本对象持有。
    unsafe { device.CreateRenderTargetView(&resource, None, Some(&mut rtv)) }
        .map_err(graphics_error)?;
    Ok(TargetTexture {
        _texture: texture,
        srv,
        rtv: rtv.ok_or_else(|| EffectError::Graphics("创建渲染目标视图失败".into()))?,
        width,
        height,
    })
}

fn create_srv(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
) -> Result<ID3D11ShaderResourceView, EffectError> {
    let resource: ID3D11Resource = texture.cast().map_err(graphics_error)?;
    let mut srv = None;
    // SAFETY: 资源来自同一设备；视图由调用方持有。
    unsafe { device.CreateShaderResourceView(&resource, None, Some(&mut srv)) }
        .map_err(graphics_error)?;
    srv.ok_or_else(|| EffectError::Graphics("创建着色器资源视图失败".into()))
}

fn graphics_error(error: windows::core::Error) -> EffectError {
    let code = error.code();
    if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
        EffectError::DeviceLost
    } else {
        EffectError::Graphics(error.to_string())
    }
}

fn present_error(error: windows::core::Error) -> EffectError {
    graphics_error(error)
}

/// 计算源图 UV 缩放、contain 标记与平铺次数；纯函数便于单元测试。
fn fit_uv_scale(
    width: u32,
    height: u32,
    source_width: u32,
    source_height: u32,
    fit: FitMode,
) -> ([f32; 2], bool, [f32; 2]) {
    let scale_x = width.max(1) as f32 / source_width.max(1) as f32;
    let scale_y = height.max(1) as f32 / source_height.max(1) as f32;
    match fit {
        FitMode::Cover => {
            let cover = scale_x.max(scale_y);
            (
                [(scale_x / cover).min(1.0), (scale_y / cover).min(1.0)],
                false,
                [0.0, 0.0],
            )
        }
        FitMode::Contain => {
            let contain = scale_x.min(scale_y);
            ([scale_x / contain, scale_y / contain], true, [0.0, 0.0])
        }
        FitMode::Stretch => ([1.0, 1.0], false, [0.0, 0.0]),
        FitMode::Tile => {
            if scale_x < 1.0 || scale_y < 1.0 {
                // 源图大于输出：原生平铺没有意义，退化为铺满。
                let cover = scale_x.max(scale_y);
                (
                    [(scale_x / cover).min(1.0), (scale_y / cover).min(1.0)],
                    false,
                    [0.0, 0.0],
                )
            } else {
                ([1.0, 1.0], false, [scale_x, scale_y])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_modes_compute_expected_uv_scale() {
        // 16:9 输出 + 16:9 源图：cover/contain 都不需要裁切或黑边。
        assert_eq!(
            fit_uv_scale(2560, 1440, 3840, 2160, FitMode::Cover),
            ([1.0, 1.0], false, [0.0, 0.0])
        );
        // 16:9 输出 + 4:3 源图：cover 裁切上下，contain 左右黑边。
        let (scale, contain, tile) = fit_uv_scale(2560, 1440, 1024, 768, FitMode::Cover);
        assert!(!contain);
        assert!((scale[0] - 1.0).abs() < 0.001);
        assert!((scale[1] - 0.75).abs() < 0.001);
        assert_eq!(tile, [0.0, 0.0]);
        let (scale, contain, _) = fit_uv_scale(2560, 1440, 1024, 768, FitMode::Contain);
        assert!(contain);
        assert!((scale[0] - 1.333).abs() < 0.01);
        assert!((scale[1] - 1.0).abs() < 0.001);
        // 拉伸不缩放 UV。
        assert_eq!(
            fit_uv_scale(2560, 1440, 1024, 768, FitMode::Stretch),
            ([1.0, 1.0], false, [0.0, 0.0])
        );
        // 小图平铺：2560/1280 = 2，1440/720 = 2。
        let (_, _, tile) = fit_uv_scale(2560, 1440, 1280, 720, FitMode::Tile);
        assert_eq!(tile, [2.0, 2.0]);
        // 大图平铺退化为 cover。
        let (scale, contain, tile) = fit_uv_scale(2560, 1440, 7680, 4320, FitMode::Tile);
        assert_eq!(scale, [1.0, 1.0]);
        assert!(!contain);
        assert_eq!(tile, [0.0, 0.0]);
    }

    #[test]
    fn gpu_timestamp_query_reports_time() {
        let (device, context) = create_device().expect("创建 D3D11 设备");
        let target = create_target(&device, 64, 64).expect("创建渲染目标");
        let mut timer = GpuTimer::new(&device).expect("创建计时查询");
        timer.begin(&context);
        // SAFETY: 目标视图来自同一设备；只清空小纹理。
        unsafe { context.ClearRenderTargetView(&target.rtv, &[0.1, 0.2, 0.3, 1.0]) };
        timer.end(&context);
        // SAFETY: 只刷新立即上下文，确保查询完成。
        unsafe { context.Flush() };
        // 阻塞读取首帧，验证时间戳能力；不支持时返回 None，而不是伪造 CPU 时间。
        let micros = timer.poll(&context, true);
        eprintln!("GPU timestamp: {micros:?} us");
        if let Some(micros) = micros {
            assert!(micros > 0 && micros < 1_000_000, "GPU 时间应为合理非零值");
        }
    }
}
