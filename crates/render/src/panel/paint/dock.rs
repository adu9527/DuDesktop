//! Dock 绘制：玻璃托盘、悬停放大、彩色系统入口与运行指示点。

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use super::{IconCache, fill_rounded, item_icon, placeholder_tile, special_tile, stroke_rounded};
use crate::{
    gpu::Gpu,
    panel::{DOCK_ICON, DOCK_PADDING, PanelView},
    surface::{RenderError, Surface, draw_text},
    theme,
};

pub(super) fn draw(
    gpu: &Gpu,
    surface: &Surface,
    view: &PanelView,
    hover: Option<usize>,
    hover_t: f32,
    icons: &IconCache,
) -> Result<(), RenderError> {
    let palette = theme::current();
    let style = view.dock_style;
    let icon_size = style.icon_size();
    let slot = style.slot();
    let height = style.height();
    let first_special = view
        .items
        .iter()
        .take(crate::panel::DOCK_MAX_ITEMS)
        .position(|item| item.id.starts_with("special:"));
    for (index, item) in view
        .items
        .iter()
        .take(crate::panel::DOCK_MAX_ITEMS)
        .enumerate()
    {
        let slot_left = DOCK_PADDING + index as f32 * slot;
        let weight = match hover {
            Some(active) if active == index => 1.0,
            Some(active) if active.abs_diff(index) == 1 => 0.45,
            _ => 0.0,
        };
        let scale = icon_size / DOCK_ICON;
        let grow = 8.0 * scale * hover_t * weight;
        let rise = 5.0 * hover_t * weight;
        let size = icon_size + grow;
        let left = slot_left + (slot - size) / 2.0;
        let top = style.icon_top() - rise + (icon_size - size) / 2.0;
        let icon_rect = D2D_RECT_F {
            left,
            top,
            right: left + size,
            bottom: top + size,
        };
        let plate = D2D_RECT_F {
            left: slot_left + 4.0,
            top: 5.0,
            right: slot_left + slot - 4.0,
            bottom: height - 5.0,
        };
        if view.selected == Some(index) {
            fill_rounded(surface, plate, 18.0, palette.selected);
            stroke_rounded(surface, plate, 18.0, palette.accent, 1.2);
        } else if hover == Some(index) {
            fill_rounded(surface, plate, 18.0, palette.hover);
        }
        if item.id.starts_with("special:") {
            special_tile(gpu, surface, &item.id, icon_rect);
        } else if item.icon.is_some() {
            item_icon(gpu, surface, item, icon_rect, icons);
        } else {
            placeholder_tile(gpu, surface, &item.label, icon_rect);
        }
        if style.shows_labels() {
            draw_text(
                &surface.target,
                &gpu.caption,
                &item.label,
                D2D_RECT_F {
                    left: slot_left + 2.0,
                    top: style.icon_top() + icon_size + 4.0,
                    right: slot_left + slot - 2.0,
                    bottom: height - 6.0,
                },
                if hover == Some(index) {
                    &surface.brushes.text
                } else {
                    &surface.brushes.muted
                },
            );
        }
        if item.running {
            let center = slot_left + slot / 2.0;
            let bottom = height - 8.0;
            fill_rounded(
                surface,
                D2D_RECT_F {
                    left: center - 8.0,
                    top: bottom - 4.0,
                    right: center + 8.0,
                    bottom,
                },
                2.0,
                theme::color(138, 180, 255, 0.22),
            );
            fill_rounded(
                surface,
                D2D_RECT_F {
                    left: center - 4.0,
                    top: bottom - 3.0,
                    right: center + 4.0,
                    bottom: bottom - 1.0,
                },
                1.0,
                palette.accent,
            );
        }
    }
    if let Some(first) = first_special
        && first > 0
    {
        let x = DOCK_PADDING + first as f32 * slot - 5.0;
        fill_rounded(
            surface,
            D2D_RECT_F {
                left: x,
                top: 22.0,
                right: x + 1.0,
                bottom: height - 22.0,
            },
            0.5,
            palette.separator,
        );
    }
    Ok(())
}
