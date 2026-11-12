//! M0 胶囊原型的 GPU 合成与绘制。
//!
//! 路径：共享 D3D11 设备 → 每层一个 DXGI composition swap chain →
//! DirectComposition 视觉树；内容由 Direct2D/DirectWrite 绘制。
//! 该模块是隔离的原生后端原型，产品层不直接依赖这些类型。

use std::rc::Rc;

use windows::{
    Win32::{
        Foundation::{D2DERR_RECREATE_TARGET, HMODULE, HWND},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
                },
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
                D2D1CreateFactory, ID2D1Factory1, ID2D1RenderTarget, ID2D1SolidColorBrush,
            },
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext,
            },
            DirectComposition::{
                DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
                IDCompositionVisual,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
                DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
                IDWriteTextFormat,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
                    DXGI_SAMPLE_DESC,
                },
                DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT,
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
                DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice,
                IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
            },
        },
    },
    core::{Interface, PCWSTR},
};

use crate::capsule::{CAPSULE_HEIGHT, CAPSULE_WIDTH_MAX, CAPSULE_WIDTH_MIN, CompositionError};

const CAPSULE_TITLE: &str = "DeskShell";
const CAPSULE_STATUS: &str = "M0 · DComp";
const SHADOW_LAYERS: usize = 9;

/// 同一进程内共享的设备、工厂与文字格式。UI 线程独占使用。
pub struct GpuContext {
    _device: ID3D11Device,
    _context: ID3D11DeviceContext,
    dcomp: IDCompositionDevice,
    factory: IDXGIFactory2,
    d2d: ID2D1Factory1,
    _dwrite: IDWriteFactory,
    title_format: IDWriteTextFormat,
    status_format: IDWriteTextFormat,
    title: Vec<u16>,
    status: Vec<u16>,
}

impl GpuContext {
    pub fn new() -> Result<Rc<Self>, CompositionError> {
        // SAFETY: 只创建本线程拥有的设备与对象；失败返回 HRESULT 错误。
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        let levels = [
            D3D_FEATURE_LEVEL(0xb100),
            D3D_FEATURE_LEVEL(0xb000),
            D3D_FEATURE_LEVEL(0xa100),
            D3D_FEATURE_LEVEL(0xa000),
        ];
        // SAFETY: 不传入外部适配器或软件模块，输出参数由本函数初始化为空。
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
        .map_err(|error| CompositionError::Other(format!("创建 D3D11 设备失败：{error}")))?;
        let device = device.ok_or_else(|| CompositionError::Other("D3D11 设备为空".into()))?;
        let context = context.ok_or_else(|| CompositionError::Other("D3D11 上下文为空".into()))?;

        // SAFETY: 设备由当前线程持有，DComp 设备引用该 DXGI 设备。
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|error| CompositionError::Other(format!("获取 IDXGIDevice 失败：{error}")))?;
        // SAFETY: 同上。
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device) }
            .map_err(|error| {
                CompositionError::Other(format!("创建 DirectComposition 设备失败：{error}"))
            })?;
        // SAFETY: 适配器与工厂由 DXGI 设备持有。
        let adapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| CompositionError::Other(format!("获取适配器失败：{error}")))?;
        // SAFETY: 工厂查询只读取父对象。
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| CompositionError::Other(format!("获取 DXGI 工厂失败：{error}")))?;

        // SAFETY: 单线程 UI 使用单线程 D2D 工厂。
        let d2d: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }.map_err(
                |error| CompositionError::Other(format!("创建 Direct2D 工厂失败：{error}")),
            )?;
        // SAFETY: 共享 DirectWrite 工厂线程安全。
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|error| {
                CompositionError::Other(format!("创建 DirectWrite 工厂失败：{error}"))
            })?;
        let title_format = create_text_format(
            &dwrite,
            "Microsoft YaHei UI",
            14.5,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        let status_format = create_text_format(
            &dwrite,
            "Microsoft YaHei UI",
            11.0,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        )?;

        Ok(Rc::new(Self {
            _device: device,
            _context: context,
            dcomp,
            factory,
            d2d,
            _dwrite: dwrite,
            title_format,
            status_format,
            title: CAPSULE_TITLE.encode_utf16().collect(),
            status: CAPSULE_STATUS.encode_utf16().collect(),
        }))
    }
}

/// 胶囊主体或阴影层；两层共享设备，但各自拥有交换链与合成视觉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    Capsule,
    Shadow,
}

pub struct Composition {
    // 渲染目标必须先于交换链释放；字段声明顺序即释放顺序。
    surface: Option<RenderSurface>,
    _device: Rc<GpuContext>,
    swap_chain: IDXGISwapChain1,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    role: LayerRole,
    width_px: u32,
    height_px: u32,
    dpi: u32,
}

struct RenderSurface {
    target: ID2D1RenderTarget,
    body: ID2D1SolidColorBrush,
    rim: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    muted: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
}

impl Composition {
    pub fn new(
        gpu: Rc<GpuContext>,
        hwnd: HWND,
        width_px: u32,
        height_px: u32,
        dpi: u32,
        role: LayerRole,
    ) -> Result<Self, CompositionError> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width_px,
            Height: height_px,
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
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        // SAFETY: 交换链描述为固定结构；仅由本模块的视觉引用。
        let swap_chain = unsafe {
            gpu.factory
                .CreateSwapChainForComposition(&gpu._device, &desc, None)
        }
        .map_err(|error| CompositionError::Other(format!("创建合成交换链失败：{error}")))?;
        // SAFETY: HWND 由调用方保证在视觉树存活期间有效。
        let target = unsafe { gpu.dcomp.CreateTargetForHwnd(hwnd, true) }
            .map_err(|error| CompositionError::Other(format!("绑定合成目标失败：{error}")))?;
        // SAFETY: 合成设备已创建。
        let visual = unsafe { gpu.dcomp.CreateVisual() }
            .map_err(|error| CompositionError::Other(format!("创建合成视觉失败：{error}")))?;
        // SAFETY: 交换链与视觉均有效。
        unsafe { visual.SetContent(&swap_chain) }
            .map_err(|error| CompositionError::Other(format!("设置视觉内容失败：{error}")))?;
        // SAFETY: 目标与视觉均有效。
        unsafe { target.SetRoot(&visual) }
            .map_err(|error| CompositionError::Other(format!("设置根视觉失败：{error}")))?;
        // SAFETY: 提交当前视觉树。
        unsafe { gpu.dcomp.Commit() }
            .map_err(|error| CompositionError::Other(format!("提交视觉树失败：{error}")))?;

        let mut composition = Self {
            surface: None,
            _device: gpu,
            swap_chain,
            _target: target,
            _visual: visual,
            role,
            width_px,
            height_px,
            dpi,
        };
        composition.surface = Some(composition.create_surface()?);
        Ok(composition)
    }

    fn create_surface(&self) -> Result<RenderSurface, CompositionError> {
        let gpu = &self._device;
        // SAFETY: 交换链有效且缓冲区索引在 BufferCount 内。
        let buffer: IDXGISurface = unsafe { self.swap_chain.GetBuffer(0) }
            .map_err(|error| graphics_error("获取交换链缓冲区失败", error))?;
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        // SAFETY: 缓冲区为 BGRA 预乘 alpha 表面，属性与格式一致。
        let target = unsafe { gpu.d2d.CreateDxgiSurfaceRenderTarget(&buffer, &properties) }
            .map_err(|error| graphics_error("创建 D2D 渲染目标失败", error))?;
        // SAFETY: 画刷由同一渲染目标创建并在其生命周期内使用。
        unsafe {
            Ok(RenderSurface {
                body: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                rim: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                text: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                muted: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                accent: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                shadow: target
                    .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                    .map_err(brush_error)?,
                target,
            })
        }
    }

    pub fn resize(
        &mut self,
        width_px: u32,
        height_px: u32,
        dpi: u32,
    ) -> Result<(), CompositionError> {
        if self.width_px == width_px && self.height_px == height_px && self.dpi == dpi {
            return Ok(());
        }
        self.surface = None;
        // SAFETY: 已释放渲染目标对后缓冲区的引用；尺寸来自当前客户区。
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                width_px,
                height_px,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
        .map_err(|error| graphics_error("调整交换链失败", error))?;
        self.width_px = width_px;
        self.height_px = height_px;
        self.dpi = dpi;
        self.surface = Some(self.create_surface()?);
        Ok(())
    }

    pub fn recreate_surface(&mut self) -> Result<(), CompositionError> {
        self.surface = None;
        // D2D 的 RECREATE_TARGET 也可能源于底层 D3D 设备移除。
        // SAFETY: 设备由本 UI 线程拥有，仅查询其移除原因。
        unsafe { self._device._device.GetDeviceRemovedReason() }
            .map_err(|error| graphics_error("查询设备状态失败", error))?;
        self.surface = Some(self.create_surface()?);
        Ok(())
    }

    /// `hover` 范围 0..=1，驱动胶囊宽度、亮度与阴影范围。
    pub fn render(&self, hover: f32) -> Result<(), CompositionError> {
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| CompositionError::Other("渲染目标缺失".into()))?;
        let hover = hover.clamp(0.0, 1.0);
        // SAFETY: 绘制调用都作用于本渲染目标及其画刷；不跨线程共享。
        unsafe {
            surface.target.BeginDraw();
            surface.target.Clear(Some(&D2D1_COLOR_F::default()));
        }
        match self.role {
            LayerRole::Capsule => self.draw_capsule(surface, hover),
            LayerRole::Shadow => self.draw_shadow(surface, hover),
        }
        // SAFETY: 结束绘制并提交当前帧；Present 错误映射设备丢失。
        unsafe {
            surface.target.EndDraw(None, None).map_err(draw_error)?;
            self.swap_chain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .map_err(present_error)?;
        }
        Ok(())
    }

    fn draw_capsule(&self, surface: &RenderSurface, hover: f32) {
        let gpu = &self._device;
        let scale = self.dpi as f32 / 96.0;
        let window_width = self.width_px as f32 / scale;
        let height = self.height_px as f32 / scale;
        let capsule_width = CAPSULE_WIDTH_MIN + (CAPSULE_WIDTH_MAX - CAPSULE_WIDTH_MIN) * hover;
        let capsule_left = (window_width - capsule_width) / 2.0;
        let capsule_right = capsule_left + capsule_width;

        // SAFETY: 画刷与渲染目标有效，颜色为预乘 alpha 输出。
        unsafe {
            surface.body.SetColor(&D2D1_COLOR_F {
                r: 0.043 + 0.012 * hover,
                g: 0.051 + 0.014 * hover,
                b: 0.067 + 0.018 * hover,
                a: 0.90,
            });
            surface.rim.SetColor(&D2D1_COLOR_F {
                r: 0.75,
                g: 0.88,
                b: 1.0,
                a: 0.10 + 0.16 * hover,
            });
            surface.text.SetColor(&D2D1_COLOR_F {
                r: 0.94,
                g: 0.96,
                b: 0.99,
                a: 0.92,
            });
            surface.muted.SetColor(&D2D1_COLOR_F {
                r: 0.60,
                g: 0.70,
                b: 0.80,
                a: 0.62 + 0.16 * hover,
            });
            surface.accent.SetColor(&D2D1_COLOR_F {
                r: 0.33,
                g: 0.78,
                b: 0.98,
                a: 0.95,
            });

            let capsule = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: capsule_left + 0.5,
                    top: 0.5,
                    right: capsule_right - 0.5,
                    bottom: height - 0.5,
                },
                radiusX: (height - 1.0) / 2.0,
                radiusY: (height - 1.0) / 2.0,
            };
            surface.target.FillRoundedRectangle(&capsule, &surface.body);
            surface
                .target
                .DrawRoundedRectangle(&capsule, &surface.rim, 1.0, None);

            let dot = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: capsule_left + 18.0 - hover,
                    top: height / 2.0 - 3.6 - 0.8 * hover,
                    right: capsule_left + 26.0 + hover,
                    bottom: height / 2.0 + 3.6 + 0.8 * hover,
                },
                radiusX: 4.4,
                radiusY: 4.4,
            };
            surface.target.FillRoundedRectangle(&dot, &surface.accent);

            let title_rect = D2D_RECT_F {
                left: capsule_left + 38.0,
                top: 0.0,
                right: capsule_right - 96.0,
                bottom: height,
            };
            surface.target.DrawText(
                &gpu.title,
                &gpu.title_format,
                &title_rect,
                &surface.text,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            let status_rect = D2D_RECT_F {
                left: capsule_right - 96.0,
                top: 0.0,
                right: capsule_right - 18.0,
                bottom: height,
            };
            surface.target.DrawText(
                &gpu.status,
                &gpu.status_format,
                &status_rect,
                &surface.muted,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn draw_shadow(&self, surface: &RenderSurface, hover: f32) {
        let scale = self.dpi as f32 / 96.0;
        let window_width = self.width_px as f32 / scale;
        let window_height = self.height_px as f32 / scale;
        let capsule_width = CAPSULE_WIDTH_MIN + (CAPSULE_WIDTH_MAX - CAPSULE_WIDTH_MIN) * hover;
        let left = (window_width - capsule_width) / 2.0;
        let top = (window_height - CAPSULE_HEIGHT) / 2.0;

        // 用多层低透明度圆角矩形近似软阴影，避免额外引入 D2D 效果依赖。
        // SAFETY: 画刷与渲染目标有效；仅绘制矩形。
        unsafe {
            for index in (0..SHADOW_LAYERS).rev() {
                let t = index as f32 / (SHADOW_LAYERS - 1) as f32;
                let spread = 3.0 + 16.0 * t;
                let alpha = 0.055 * (1.0 - t).powf(1.8);
                let offset = 5.0 * (1.0 - 0.6 * t);
                surface.shadow.SetColor(&D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: alpha,
                });
                let shadow = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: left - spread,
                        top: top - spread + offset,
                        right: left + capsule_width + spread,
                        bottom: top + CAPSULE_HEIGHT + spread + offset,
                    },
                    radiusX: CAPSULE_HEIGHT / 2.0 + spread,
                    radiusY: CAPSULE_HEIGHT / 2.0 + spread,
                };
                surface
                    .target
                    .FillRoundedRectangle(&shadow, &surface.shadow);
            }
        }
    }
}

fn create_text_format(
    dwrite: &IDWriteFactory,
    family: &str,
    size: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat, CompositionError> {
    let family: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let locale: Vec<u16> = "zh-CN".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 字体名与区域字符串在调用期间保持有效。
    let format = unsafe {
        dwrite.CreateTextFormat(
            PCWSTR::from_raw(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR::from_raw(locale.as_ptr()),
        )
    }
    .map_err(|error| CompositionError::Other(format!("创建文本格式失败：{error}")))?;
    // SAFETY: 文本格式归本模块所有。
    unsafe {
        format.SetTextAlignment(alignment).map_err(draw_error)?;
        format
            .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
            .map_err(draw_error)?;
        format
            .SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
            .map_err(draw_error)?;
    }
    Ok(format)
}

fn brush_error(error: windows::core::Error) -> CompositionError {
    graphics_error("创建画刷失败", error)
}

fn draw_error(error: windows::core::Error) -> CompositionError {
    graphics_error("Direct2D 绘制失败", error)
}

fn present_error(error: windows::core::Error) -> CompositionError {
    graphics_error("Present 失败", error)
}

fn graphics_error(operation: &str, error: windows::core::Error) -> CompositionError {
    let code = error.code();
    if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
        CompositionError::DeviceLost
    } else if code == D2DERR_RECREATE_TARGET {
        CompositionError::TargetLost
    } else {
        CompositionError::Other(format!("{operation}：{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_failures_keep_recovery_category() {
        for code in [DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET] {
            assert!(matches!(
                graphics_error("ResizeBuffers", code.into()),
                CompositionError::DeviceLost
            ));
            assert!(matches!(
                draw_error(code.into()),
                CompositionError::DeviceLost
            ));
        }
        assert!(matches!(
            draw_error(D2DERR_RECREATE_TARGET.into()),
            CompositionError::TargetLost
        ));
        assert!(matches!(
            draw_error(windows::Win32::Foundation::E_FAIL.into()),
            CompositionError::Other(_)
        ));
    }
}
