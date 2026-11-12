//! 共享合成表面：DXGI flip 交换链 + DirectComposition 视觉 + Direct2D 目标。
//!
//! `CompositionTarget` 负责尺寸、设备失效恢复与呈现；画笔按主题版本重建，
//! 渐变只使用线性渐变，避免为静态面板引入额外依赖。

use std::rc::Rc;

use windows::Win32::{
    Foundation::{D2DERR_RECREATE_TARGET, HWND},
    Graphics::{
        Direct2D::{
            Common::{
                D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
                D2D1_PIXEL_FORMAT,
            },
            D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_EXTEND_MODE_CLAMP, D2D1_FEATURE_LEVEL_DEFAULT,
            D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
            D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
            ID2D1Bitmap, ID2D1GradientStopCollection, ID2D1LinearGradientBrush, ID2D1RenderTarget,
            ID2D1SolidColorBrush,
        },
        DirectComposition::{IDCompositionTarget, IDCompositionVisual},
        DirectWrite::IDWriteTextFormat,
        Dxgi::{
            Common::{
                DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
                DXGI_SAMPLE_DESC,
            },
            DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT, DXGI_SCALING_STRETCH,
            DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISurface, IDXGISwapChain1,
        },
    },
};
use windows_numerics::Vector2;

use crate::{gpu::Gpu, theme};

#[derive(Debug)]
pub(crate) enum RenderError {
    DeviceLost,
    TargetLost,
    Other(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLost => formatter.write_str("图形设备丢失"),
            Self::TargetLost => formatter.write_str("绘制目标失效"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl From<RenderError> for String {
    fn from(error: RenderError) -> Self {
        error.to_string()
    }
}

/// 一个顶层窗口的合成目标。
pub(crate) struct CompositionTarget {
    gpu: Rc<Gpu>,
    swap_chain: IDXGISwapChain1,
    // 目标与视觉必须在交换链之前释放。
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    surface: Option<Surface>,
    width_px: u32,
    height_px: u32,
    dpi: u32,
}

pub(crate) struct Surface {
    pub target: ID2D1RenderTarget,
    pub brushes: Brushes,
}

pub(crate) struct Brushes {
    pub text: ID2D1SolidColorBrush,
    pub muted: ID2D1SolidColorBrush,
    pub faint: ID2D1SolidColorBrush,
    pub accent: ID2D1SolidColorBrush,
    pub accent_text: ID2D1SolidColorBrush,
    pub danger: ID2D1SolidColorBrush,
    pub success: ID2D1SolidColorBrush,
    pub fill: ID2D1SolidColorBrush,
    pub stroke: ID2D1SolidColorBrush,
    pub input: ID2D1SolidColorBrush,
    pub material: ID2D1LinearGradientBrush,
    pub window: ID2D1LinearGradientBrush,
    pub rim: ID2D1LinearGradientBrush,
    pub sheen: ID2D1LinearGradientBrush,
    pub shade: ID2D1LinearGradientBrush,
    pub accent_gradient: ID2D1LinearGradientBrush,
    epoch: u32,
}

impl CompositionTarget {
    pub fn new(
        gpu: Rc<Gpu>,
        hwnd: HWND,
        width_px: u32,
        height_px: u32,
        dpi: u32,
    ) -> Result<Self, RenderError> {
        let width_px = width_px.max(1);
        let height_px = height_px.max(1);
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
        // SAFETY: 描述结构固定；交换链归本对象持有。
        let swap_chain = unsafe {
            gpu.factory
                .CreateSwapChainForComposition(gpu.device(), &desc, None)
        }
        .map_err(|error| RenderError::Other(format!("创建合成交换链失败：{error}")))?;
        // SAFETY: HWND 在视觉树存活期间有效，由窗口层保证。
        // 合成内容必须在原生子控件下方，topmost=true 会遮住子 HWND。
        let target = unsafe { gpu.dcomp.CreateTargetForHwnd(hwnd, false) }
            .map_err(|error| RenderError::Other(format!("绑定合成目标失败：{error}")))?;
        // SAFETY: 合成设备已创建。
        let visual = unsafe { gpu.dcomp.CreateVisual() }
            .map_err(|error| RenderError::Other(format!("创建合成视觉失败：{error}")))?;
        // SAFETY: 交换链与视觉均有效。
        unsafe { visual.SetContent(&swap_chain) }
            .map_err(|error| RenderError::Other(format!("设置视觉内容失败：{error}")))?;
        // SAFETY: 目标与视觉均有效。
        unsafe { target.SetRoot(&visual) }
            .map_err(|error| RenderError::Other(format!("设置根视觉失败：{error}")))?;
        // SAFETY: 提交视觉树。
        unsafe { gpu.dcomp.Commit() }
            .map_err(|error| RenderError::Other(format!("提交视觉树失败：{error}")))?;
        let mut composition = Self {
            gpu,
            swap_chain,
            _target: target,
            _visual: visual,
            surface: None,
            width_px,
            height_px,
            dpi,
        };
        composition.surface = Some(composition.create_surface()?);
        Ok(composition)
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    fn create_surface(&self) -> Result<Surface, RenderError> {
        let gpu = &self.gpu;
        // SAFETY: 交换链有效且索引在缓冲数量内。
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
        // SAFETY: 缓冲区格式与属性一致。
        let target = unsafe { gpu.d2d.CreateDxgiSurfaceRenderTarget(&buffer, &properties) }
            .map_err(|error| graphics_error("创建 D2D 渲染目标失败", error))?;
        let brushes = Brushes::new(&target, theme::current())?;
        Ok(Surface { target, brushes })
    }

    pub fn resize(&mut self, width_px: u32, height_px: u32, dpi: u32) -> Result<(), RenderError> {
        let width_px = width_px.max(1);
        let height_px = height_px.max(1);
        if self.width_px == width_px && self.height_px == height_px && self.dpi == dpi {
            return Ok(());
        }
        self.surface = None;
        // SAFETY: 渲染目标与位图已释放；新尺寸来自当前客户区。
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

    pub fn recreate_surface(&mut self) -> Result<(), RenderError> {
        self.surface = None;
        // SAFETY: 只查询设备移除原因。
        unsafe { self.gpu.device().GetDeviceRemovedReason() }
            .map_err(|error| graphics_error("查询设备状态失败", error))?;
        self.surface = Some(self.create_surface()?);
        Ok(())
    }

    /// 绘制一帧；主题变化时先重建渐变画笔。
    pub fn render<F>(&mut self, draw: F) -> Result<(), RenderError>
    where
        F: FnOnce(&Gpu, &Surface) -> Result<(), RenderError>,
    {
        let palette = theme::current();
        let epoch = theme::version();
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| RenderError::Other("渲染目标缺失".into()))?;
        if surface.brushes.epoch != epoch {
            surface.brushes = Brushes::new(&surface.target, palette)?;
        }
        sync_solid_colors(&surface.brushes, palette);
        // SAFETY: 所有绘制调用作用于本目标的画刷与位图，不跨线程共享。
        unsafe {
            surface.target.BeginDraw();
            surface.target.Clear(Some(&D2D1_COLOR_F::default()));
        }
        draw(&self.gpu, surface)?;
        // SAFETY: 结束绘制并提交当前帧。
        unsafe {
            surface.target.EndDraw(None, None).map_err(draw_error)?;
            self.swap_chain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .map_err(present_error)?;
        }
        Ok(())
    }
}

impl Brushes {
    fn new(target: &ID2D1RenderTarget, palette: theme::Palette) -> Result<Self, RenderError> {
        let solid = |color: D2D1_COLOR_F| {
            // SAFETY: 画刷由同一渲染目标创建并使用。
            unsafe { target.CreateSolidColorBrush(&color, None) }
                .map_err(|error| RenderError::Other(format!("创建画刷失败：{error}")))
        };
        let material = linear(
            target,
            &[(0.0, palette.glass_top), (1.0, palette.glass_bottom)],
        )?;
        let window = linear(
            target,
            &[(0.0, palette.window_top), (1.0, palette.window_bottom)],
        )?;
        let rim = linear(target, &[(0.0, palette.rim_top), (1.0, palette.rim_bottom)])?;
        let sheen = linear(
            target,
            &[
                (0.0, palette.sheen_top),
                (0.55, default_alpha(palette.sheen_top, 0.0)),
            ],
        )?;
        let shade = linear(
            target,
            &[
                (0.55, default_alpha(palette.shade_bottom, 0.0)),
                (1.0, palette.shade_bottom),
            ],
        )?;
        let accent_gradient = linear(
            target,
            &[(0.0, palette.accent_top), (1.0, palette.accent_bottom)],
        )?;
        Ok(Self {
            text: solid(palette.text)?,
            muted: solid(palette.secondary)?,
            faint: solid(palette.tertiary)?,
            accent: solid(palette.accent)?,
            accent_text: solid(palette.accent_text)?,
            danger: solid(palette.danger)?,
            success: solid(palette.success)?,
            fill: solid(palette.control)?,
            stroke: solid(palette.border)?,
            input: solid(palette.input)?,
            material,
            window,
            rim,
            sheen,
            shade,
            accent_gradient,
            epoch: theme::version(),
        })
    }

    pub fn material_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.material, top, bottom);
    }

    pub fn window_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.window, top, bottom);
    }

    pub fn rim_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.rim, top, bottom);
    }

    pub fn sheen_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.sheen, top, bottom);
    }

    pub fn shade_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.shade, top, bottom);
    }

    pub fn accent_points(&self, top: f32, bottom: f32) {
        set_vertical(&self.accent_gradient, top, bottom);
    }
}

fn default_alpha(color: D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: alpha, ..color }
}

fn linear(
    target: &ID2D1RenderTarget,
    stops: &[(f32, D2D1_COLOR_F)],
) -> Result<ID2D1LinearGradientBrush, RenderError> {
    let stops: Vec<D2D1_GRADIENT_STOP> = stops
        .iter()
        .map(|(position, color)| D2D1_GRADIENT_STOP {
            position: *position,
            color: *color,
        })
        .collect();
    // SAFETY: 渐变停止点数组在调用期间有效；集合由渲染目标持有。
    let collection: ID2D1GradientStopCollection = unsafe {
        target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
    }
    .map_err(|error| RenderError::Other(format!("创建渐变集合失败：{error}")))?;
    let properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
        startPoint: Vector2 { X: 0.0, Y: 0.0 },
        endPoint: Vector2 { X: 0.0, Y: 1.0 },
    };
    // SAFETY: 集合与属性有效；画刷由渲染目标持有。
    unsafe { target.CreateLinearGradientBrush(&properties, None, &collection) }
        .map_err(|error| RenderError::Other(format!("创建渐变画刷失败：{error}")))
}

fn set_vertical(brush: &ID2D1LinearGradientBrush, top: f32, bottom: f32) {
    // SAFETY: 渐变画刷属于当前渲染目标，设置坐标不跨线程。
    unsafe {
        brush.SetStartPoint(Vector2 { X: 0.0, Y: top });
        brush.SetEndPoint(Vector2 { X: 0.0, Y: bottom });
    }
}

fn sync_solid_colors(brushes: &Brushes, palette: theme::Palette) {
    // SAFETY: 画刷属于当前渲染目标；SetColor 只写入颜色。
    unsafe {
        brushes.text.SetColor(&palette.text);
        brushes.muted.SetColor(&palette.secondary);
        brushes.faint.SetColor(&palette.tertiary);
        brushes.accent.SetColor(&palette.accent);
        brushes.accent_text.SetColor(&palette.accent_text);
        brushes.danger.SetColor(&palette.danger);
        brushes.success.SetColor(&palette.success);
        brushes.fill.SetColor(&palette.control);
        brushes.stroke.SetColor(&palette.border);
        brushes.input.SetColor(&palette.input);
    }
}

/// 由绘制模块使用：绘制带 1px 渐变描边的玻璃面板。
pub(crate) fn draw_shell(
    target: &ID2D1RenderTarget,
    brushes: &Brushes,
    width: f32,
    height: f32,
    radius: f32,
) {
    let body = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: 0.5,
            top: 0.5,
            right: width - 0.5,
            bottom: height - 0.5,
        },
        radiusX: radius,
        radiusY: radius,
    };
    brushes.material_points(0.0, height);
    brushes.sheen_points(0.0, height);
    brushes.shade_points(0.0, height);
    brushes.rim_points(0.0, height);
    // SAFETY: 目标、画刷与矩形均有效。
    unsafe {
        target.FillRoundedRectangle(&body, &brushes.material);
        target.FillRoundedRectangle(&body, &brushes.sheen);
        target.FillRoundedRectangle(&body, &brushes.shade);
        target.DrawRoundedRectangle(&body, &brushes.rim, 1.0, None);
    }
}

/// 由绘制模块使用：设置窗使用接近不透明的玻璃，保证长期阅读清晰。
pub(crate) fn draw_window_shell(
    target: &ID2D1RenderTarget,
    brushes: &Brushes,
    width: f32,
    height: f32,
    radius: f32,
) {
    let body = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: 0.5,
            top: 0.5,
            right: width - 0.5,
            bottom: height - 0.5,
        },
        radiusX: radius,
        radiusY: radius,
    };
    brushes.window_points(0.0, height);
    brushes.sheen_points(0.0, height);
    brushes.shade_points(0.0, height);
    brushes.rim_points(0.0, height);
    // SAFETY: 目标、画刷与矩形均有效。
    unsafe {
        target.FillRoundedRectangle(&body, &brushes.window);
        target.FillRoundedRectangle(&body, &brushes.sheen);
        target.FillRoundedRectangle(&body, &brushes.shade);
        target.DrawRoundedRectangle(&body, &brushes.rim, 1.0, None);
    }
}

/// 由绘制模块使用：绘制图标底板并叠加顶部高光与底部压暗。
pub(crate) fn draw_tile(
    target: &ID2D1RenderTarget,
    brushes: &Brushes,
    rect: D2D1_ROUNDED_RECT,
    color: D2D1_COLOR_F,
) {
    brushes.sheen_points(rect.rect.top, rect.rect.bottom);
    brushes.shade_points(rect.rect.top, rect.rect.bottom);
    // SAFETY: 画刷与矩形有效；颜色只写入当前渲染目标的画刷。
    unsafe {
        brushes.fill.SetColor(&color);
        target.FillRoundedRectangle(&rect, &brushes.fill);
        target.FillRoundedRectangle(&rect, &brushes.sheen);
        target.FillRoundedRectangle(&rect, &brushes.shade);
    }
}

/// 文本绘制结果不影响布局；调用方负责矩形与裁剪。
pub(crate) fn draw_text(
    target: &ID2D1RenderTarget,
    format: &IDWriteTextFormat,
    text: &str,
    rect: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let text: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: 文本缓冲区、格式与画刷在调用期间有效。
    unsafe {
        target.DrawText(
            &text,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

pub(crate) fn bitmap_fingerprint(icon: &crate::IconPixels) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    icon.width.hash(&mut hasher);
    icon.height.hash(&mut hasher);
    icon.bgra.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn create_bitmap(
    target: &ID2D1RenderTarget,
    icon: &crate::IconPixels,
) -> Result<ID2D1Bitmap, RenderError> {
    let properties = windows::Win32::Graphics::Direct2D::D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    // SAFETY: 像素缓冲区在调用期间有效；D2D 复制数据后独立持有位图。
    unsafe {
        target.CreateBitmap(
            windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                width: icon.width,
                height: icon.height,
            },
            Some(icon.bgra.as_ptr() as *const core::ffi::c_void),
            icon.width * 4,
            &properties,
        )
    }
    .map_err(|error| RenderError::Other(format!("创建图标位图失败：{error}")))
}

fn draw_error(error: windows::core::Error) -> RenderError {
    graphics_error("Direct2D 绘制失败", error)
}

fn present_error(error: windows::core::Error) -> RenderError {
    graphics_error("Present 失败", error)
}

fn graphics_error(operation: &str, error: windows::core::Error) -> RenderError {
    let code = error.code();
    if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
        RenderError::DeviceLost
    } else if code == D2DERR_RECREATE_TARGET {
        RenderError::TargetLost
    } else {
        RenderError::Other(format!("{operation}：{error}"))
    }
}
