//! 产品面板的合成与绘制入口。
//!
//! 交换链、Direct2D 目标与画笔由共享 `CompositionTarget` 管理；本模块只负责
//! 同步图标缓存并把视图交给 `paint`，避免绘制逻辑继续膨胀。

use std::rc::Rc;

use windows::Win32::Foundation::HWND;

use super::{
    PanelView,
    paint::{self, IconCache},
    panel_size,
};
use crate::{
    gpu::Gpu,
    surface::{CompositionTarget, RenderError},
};

/// 一个面板窗口的合成目标。
pub struct PanelComposition {
    target: CompositionTarget,
    icons: IconCache,
}

impl PanelComposition {
    pub fn new(
        gpu: Rc<Gpu>,
        hwnd: HWND,
        width_px: u32,
        height_px: u32,
        dpi: u32,
    ) -> Result<Self, RenderError> {
        Ok(Self {
            target: CompositionTarget::new(gpu, hwnd, width_px, height_px, dpi)?,
            icons: IconCache::new(),
        })
    }

    pub fn resize(&mut self, width_px: u32, height_px: u32, dpi: u32) -> Result<(), RenderError> {
        self.target.resize(width_px, height_px, dpi)
    }

    pub fn recreate_surface(&mut self) -> Result<(), RenderError> {
        self.target.recreate_surface()
    }

    /// 绘制一帧。`hover` 为当前悬停条目，`hover_t` 为 0..=1 的动画进度。
    pub fn render(
        &mut self,
        view: &PanelView,
        hover: Option<usize>,
        hover_t: f32,
        search_focused: bool,
    ) -> Result<(), RenderError> {
        let (logical_width, logical_height) = panel_size(view);
        let dpi = self.target.dpi();
        let scale = dpi as f32 / 96.0;
        let expected_width = (logical_width * scale).round().max(1.0) as u32;
        let expected_height = (logical_height * scale).round().max(1.0) as u32;
        self.target.resize(expected_width, expected_height, dpi)?;
        let icons = &mut self.icons;
        self.target.render(|gpu, surface| {
            icons.sync(surface, view)?;
            paint::draw(gpu, surface, view, hover, hover_t, icons, search_focused)
        })
    }
}
