//! 面板绘制：玻璃外壳、渐变图标、分类色块与文本。
//!
//! Dock 与灵动岛共享图标缓存与底色逻辑；具体布局分别在 `dock`、`island`。

mod dock;
mod island;

use std::collections::HashMap;

use windows::Win32::Graphics::{
    Direct2D::{
        Common::{D2D_RECT_F, D2D1_COLOR_F},
        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1Geometry,
    },
    DirectWrite::IDWriteTextFormat,
};

use super::{PanelIcon, PanelItem, PanelView, panel_size};
use crate::{
    gpu::Gpu,
    surface::{Brushes, RenderError, Surface, bitmap_fingerprint, create_bitmap, draw_text},
    theme,
};

pub(super) struct IconCache {
    items: HashMap<String, CachedIcon>,
}

struct CachedIcon {
    fingerprint: u64,
    bitmap: ID2D1Bitmap,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }

    pub fn sync(&mut self, surface: &Surface, view: &PanelView) -> Result<(), RenderError> {
        let visible: Vec<usize> = if view.kind == super::PanelKind::Dock {
            (0..view.items.len().min(super::DOCK_MAX_ITEMS)).collect()
        } else {
            view.visible_indices()
        };
        let mut known = std::collections::HashSet::new();
        for index in visible {
            let Some(item) = view.items.get(index) else {
                continue;
            };
            let Some(icon) = &item.icon else {
                continue;
            };
            if !icon.is_valid() {
                continue;
            }
            known.insert(item.id.clone());
            let fingerprint = bitmap_fingerprint(icon);
            if self
                .items
                .get(&item.id)
                .is_some_and(|cached| cached.fingerprint == fingerprint)
            {
                continue;
            }
            let bitmap = create_bitmap(&surface.target, icon)?;
            self.items.insert(
                item.id.clone(),
                CachedIcon {
                    fingerprint,
                    bitmap,
                },
            );
        }
        // 丢弃已不在视图中的图标，避免无界缓存。
        self.items.retain(|id, _| known.contains(id));
        Ok(())
    }

    fn get(&self, id: &str) -> Option<&ID2D1Bitmap> {
        self.items.get(id).map(|item| &item.bitmap)
    }
}

pub(super) fn draw(
    gpu: &Gpu,
    surface: &Surface,
    view: &PanelView,
    hover: Option<usize>,
    hover_t: f32,
    icons: &IconCache,
    search_focused: bool,
) -> Result<(), RenderError> {
    let (width, height) = panel_size(view);
    crate::surface::draw_shell(
        &surface.target,
        &surface.brushes,
        width,
        height,
        super::panel_radius(view),
    );
    match view.kind {
        super::PanelKind::Dock => dock::draw(gpu, surface, view, hover, hover_t, icons)?,
        super::PanelKind::Island => {
            island::draw(gpu, surface, view, hover, icons, search_focused)?;
        }
    }
    Ok(())
}

pub(crate) fn glyph(
    target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
    _gpu: &Gpu,
    text: &str,
    rect: D2D_RECT_F,
    brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    format: &IDWriteTextFormat,
) {
    let text: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: 文本缓冲区、格式与画刷在调用期间有效。
    unsafe {
        target.DrawText(
            &text,
            format,
            &rect,
            brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// 标题左侧的品牌渐变方块。
pub(super) fn header_tile(gpu: &Gpu, surface: &Surface, icon: PanelIcon, rect: D2D_RECT_F) {
    let radius = (rect.bottom - rect.top) * 0.32;
    let tile = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    surface.brushes.accent_points(rect.top, rect.bottom);
    // SAFETY: 画刷与矩形有效。
    unsafe {
        surface
            .target
            .FillRoundedRectangle(&tile, &surface.brushes.accent_gradient);
    }
    glyph(
        &surface.target,
        gpu,
        icon.glyph(),
        rect,
        &surface.brushes.accent_text,
        &gpu.symbol,
    );
}

/// 条目图标：优先真实 Shell 图标，其次分类色块/首字母。
pub(super) fn item_icon(
    gpu: &Gpu,
    surface: &Surface,
    item: &PanelItem,
    rect: D2D_RECT_F,
    icons: &IconCache,
) {
    if let Some(bitmap) = icons.get(&item.id) {
        // 统一圆角裁切：原始应用图标直接绘制会保留方形边缘，与参考图不一致。
        let radius = (rect.bottom - rect.top) * 0.22;
        let rounded = D2D1_ROUNDED_RECT {
            rect,
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: 工厂与矩形参数有效；返回的几何对象由本函数持有。
        let mask = unsafe { gpu.d2d.CreateRoundedRectangleGeometry(&rounded) }
            .ok()
            .and_then(|geometry| windows::core::Interface::cast::<ID2D1Geometry>(&geometry).ok());
        // SAFETY: 渲染目标有效；图层尺寸由内容边界决定。
        let layer = unsafe { surface.target.CreateLayer(None) }.ok();
        if let (Some(mask), Some(layer)) = (mask, layer) {
            let parameters = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                contentBounds: rect,
                geometricMask: std::mem::ManuallyDrop::new(Some(mask)),
                maskAntialiasMode:
                    windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS_NONE,
            };
            // SAFETY: 图层、几何遮罩与位图均属于当前渲染目标，成对 Push/Pop。
            unsafe {
                surface.target.PushLayer(&parameters, &layer);
                surface.target.DrawBitmap(
                    bitmap,
                    Some(&rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
                surface.target.PopLayer();
            }
            return;
        }
        // SAFETY: 位图与目标矩形有效；只做绘制。
        unsafe {
            surface.target.DrawBitmap(
                bitmap,
                Some(&rect),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        return;
    }
    let color = category_color(item);
    let radius = (rect.bottom - rect.top) * 0.28;
    let tile = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    crate::surface::draw_tile(&surface.target, &surface.brushes, tile, color);
    let text = title_glyph(item);
    set_fill(&surface.brushes, glyph_on(color));
    glyph(
        &surface.target,
        gpu,
        text,
        rect,
        &surface.brushes.fill,
        &gpu.symbol,
    );
}

/// 分类色块使用的符号；未知类型退回文档图标。
fn title_glyph(item: &PanelItem) -> &'static str {
    let category = item.detail.split('·').next().unwrap_or("").trim();
    match category {
        "文件夹" => "\u{E8B7}",
        "图片" => "\u{E8B9}",
        "文档" => "\u{E8A5}",
        "压缩包" => "\u{E7B8}",
        "安装包" => "\u{E896}",
        _ => "\u{E8A5}",
    }
}

fn category_color(item: &PanelItem) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(item.accent.0) / 255.0,
        g: f32::from(item.accent.1) / 255.0,
        b: f32::from(item.accent.2) / 255.0,
        a: 0.92,
    }
}

fn is_bright(color: D2D1_COLOR_F) -> bool {
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b > 0.62
}

/// 图标底板上固定的高对比前景色，不跟随明暗主题。
fn glyph_on(color: D2D1_COLOR_F) -> D2D1_COLOR_F {
    if is_bright(color) {
        theme::color(22, 26, 36, 1.0)
    } else {
        theme::color(255, 255, 255, 1.0)
    }
}

pub(super) fn placeholder_tile(gpu: &Gpu, surface: &Surface, label: &str, rect: D2D_RECT_F) {
    let color = theme::current().accent;
    let radius = (rect.bottom - rect.top) * 0.28;
    let tile = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    crate::surface::draw_tile(&surface.target, &surface.brushes, tile, color);
    let glyph_text: String = label
        .chars()
        .next()
        .map(|character| character.to_uppercase().collect())
        .unwrap_or_else(|| "?".into());
    set_fill(&surface.brushes, glyph_on(color));
    glyph(
        &surface.target,
        gpu,
        &glyph_text,
        rect,
        &surface.brushes.fill,
        &gpu.symbol,
    );
}

/// 系统入口：应用库、资源管理器、回收站使用品牌色渐变方块。
pub(super) fn special_tile(gpu: &Gpu, surface: &Surface, id: &str, rect: D2D_RECT_F) {
    let color = match id {
        "special:library" => D2D1_COLOR_F {
            r: 0.38,
            g: 0.52,
            b: 1.0,
            a: 1.0,
        },
        "special:explorer" => D2D1_COLOR_F {
            r: 0.98,
            g: 0.73,
            b: 0.34,
            a: 1.0,
        },
        "special:recycle" => D2D1_COLOR_F {
            r: 0.47,
            g: 0.57,
            b: 0.70,
            a: 1.0,
        },
        "special:settings" => D2D1_COLOR_F {
            r: 0.55,
            g: 0.60,
            b: 0.72,
            a: 1.0,
        },
        _ => theme::current().accent,
    };
    let radius = (rect.bottom - rect.top) * 0.30;
    let tile = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    crate::surface::draw_tile(&surface.target, &surface.brushes, tile, color);
    let symbol = match id {
        "special:library" => "\u{E80A}",
        "special:explorer" => "\u{E8B7}",
        "special:recycle" => "\u{E74D}",
        "special:settings" => "\u{E713}",
        _ => "\u{E8A5}",
    };
    set_fill(&surface.brushes, glyph_on(color));
    glyph(
        &surface.target,
        gpu,
        symbol,
        rect,
        &surface.brushes.fill,
        &gpu.symbol,
    );
}

pub(crate) fn draw_caption(
    target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
    gpu: &Gpu,
    text: &str,
    rect: D2D_RECT_F,
    brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
) {
    draw_text(target, &gpu.caption, text, rect, brush);
}

pub(crate) fn set_fill(brushes: &Brushes, color: D2D1_COLOR_F) {
    // SAFETY: 画刷属于当前渲染目标；SetColor 只写入颜色。
    unsafe {
        brushes.fill.SetColor(&color);
    }
}

pub(super) fn set_stroke(brushes: &Brushes, color: D2D1_COLOR_F) {
    // SAFETY: 画刷属于当前渲染目标；SetColor 只写入颜色。
    unsafe {
        brushes.stroke.SetColor(&color);
    }
}

pub(crate) fn fill_rounded(surface: &Surface, rect: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
    let shape = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    set_fill(&surface.brushes, color);
    // SAFETY: 画刷与矩形有效。
    unsafe {
        surface
            .target
            .FillRoundedRectangle(&shape, &surface.brushes.fill);
    }
}

pub(crate) fn stroke_rounded(
    surface: &Surface,
    rect: D2D_RECT_F,
    radius: f32,
    color: D2D1_COLOR_F,
    width: f32,
) {
    let shape = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    set_stroke(&surface.brushes, color);
    // SAFETY: 画刷与矩形有效。
    unsafe {
        surface
            .target
            .DrawRoundedRectangle(&shape, &surface.brushes.stroke, width, None);
    }
}
