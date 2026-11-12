//! 灵动岛/应用库绘制：玻璃卡片、搜索框、分类胶囊、文件行与操作栏。

use windows::Win32::Graphics::Direct2D::{Common::D2D_RECT_F, D2D1_ROUNDED_RECT};

use super::{
    IconCache, draw_caption, fill_rounded, glyph, header_tile, item_icon, placeholder_tile,
    stroke_rounded,
};
use crate::{
    gpu::Gpu,
    panel::{
        ISLAND_HEADER, ISLAND_PADDING, ISLAND_ROW_HEIGHT, IslandTab, PanelView, action_rect,
        panel_size,
    },
    surface::{RenderError, Surface, draw_text},
    theme,
};

pub(super) fn draw(
    gpu: &Gpu,
    surface: &Surface,
    view: &PanelView,
    hover: Option<usize>,
    icons: &IconCache,
    search_focused: bool,
) -> Result<(), RenderError> {
    let palette = theme::current();
    let (width, _) = panel_size(view);
    draw_header(gpu, surface, view, width);
    if !view.expanded {
        return Ok(());
    }
    draw_tabs(gpu, surface, view);
    if view.island_tab == IslandTab::Tools {
        draw_tools(gpu, surface, view, hover);
        draw_status(gpu, surface, view);
        return Ok(());
    }
    if view.island_tab == IslandTab::Files {
        draw_search(gpu, surface, view, search_focused);
        draw_chips(gpu, surface, view, hover);
    }
    let rows_top = view.rows_top();
    let indices = view.visible_indices();
    if indices.is_empty() {
        let rect = D2D_RECT_F {
            left: ISLAND_PADDING,
            top: rows_top,
            right: width - ISLAND_PADDING,
            bottom: rows_top + ISLAND_ROW_HEIGHT,
        };
        draw_text(
            &surface.target,
            &gpu.action,
            if view.island_tab == IslandTab::Running {
                "没有检测到运行中的应用"
            } else {
                "没有匹配项目，试试调整搜索或分类"
            },
            rect,
            &surface.brushes.muted,
        );
    }
    for (row, index) in indices.iter().enumerate() {
        let Some(item) = view.items.get(*index) else {
            continue;
        };
        let top = rows_top + row as f32 * ISLAND_ROW_HEIGHT;
        let selected = view.selected == Some(*index);
        let hovered = hover == Some(*index);
        let row_rect = D2D_RECT_F {
            left: ISLAND_PADDING,
            top: top + 2.0,
            right: width - ISLAND_PADDING,
            bottom: top + ISLAND_ROW_HEIGHT - 2.0,
        };
        if selected {
            fill_rounded(surface, row_rect, 12.0, palette.selected);
            fill_rounded(
                surface,
                D2D_RECT_F {
                    left: ISLAND_PADDING + 8.0,
                    top: top + 18.0,
                    right: ISLAND_PADDING + 11.0,
                    bottom: top + 38.0,
                },
                1.5,
                palette.accent,
            );
        } else if hovered {
            fill_rounded(surface, row_rect, 12.0, palette.hover);
        }
        let icon_rect = D2D_RECT_F {
            left: ISLAND_PADDING + 10.0,
            top: top + 10.0,
            right: ISLAND_PADDING + 38.0,
            bottom: top + 38.0,
        };
        if item.icon.is_some() || !item.detail.is_empty() {
            item_icon(gpu, surface, item, icon_rect, icons);
        } else {
            placeholder_tile(gpu, surface, &item.label, icon_rect);
        }
        let text_left = ISLAND_PADDING + 48.0;
        if item.detail.is_empty() {
            let rect = D2D_RECT_F {
                left: text_left,
                top,
                right: width - ISLAND_PADDING - 12.0,
                bottom: top + ISLAND_ROW_HEIGHT,
            };
            draw_text(
                &surface.target,
                &gpu.row,
                &item.label,
                rect,
                &surface.brushes.text,
            );
        } else {
            let label_rect = D2D_RECT_F {
                left: text_left,
                top: top + 5.0,
                right: width - ISLAND_PADDING - 12.0,
                bottom: top + 26.0,
            };
            draw_text(
                &surface.target,
                &gpu.row,
                &item.label,
                label_rect,
                &surface.brushes.text,
            );
            let detail_rect = D2D_RECT_F {
                left: text_left,
                top: top + 24.0,
                right: width - ISLAND_PADDING - 12.0,
                bottom: top + 44.0,
            };
            draw_caption(
                &surface.target,
                gpu,
                &item.detail,
                detail_rect,
                &surface.brushes.muted,
            );
        }
    }
    if view.island_tab == IslandTab::Files {
        draw_actions(gpu, surface, view, hover);
    }
    draw_status(gpu, surface, view);
    draw_scroll(surface, view);
    Ok(())
}

/// 页签：运行 / 工具 / 文件。
fn draw_tabs(gpu: &Gpu, surface: &Surface, view: &PanelView) {
    let palette = theme::current();
    for (index, rect) in view.tab_rects().into_iter().enumerate() {
        let Some(tab) = IslandTab::from_index(index) else {
            continue;
        };
        let active = tab == view.island_tab;
        let d2d = D2D_RECT_F {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        fill_rounded(
            surface,
            d2d,
            10.0,
            if active {
                palette.selected
            } else {
                palette.control
            },
        );
        if active {
            stroke_rounded(surface, d2d, 10.0, palette.accent, 1.0);
        }
        draw_text(
            &surface.target,
            &gpu.action,
            tab.label(),
            d2d,
            if active {
                &surface.brushes.text
            } else {
                &surface.brushes.muted
            },
        );
    }
}

/// 快捷工具网格。
fn draw_tools(gpu: &Gpu, surface: &Surface, view: &PanelView, hover: Option<usize>) {
    let palette = theme::current();
    for (index, rect) in view.tool_rects().into_iter().enumerate() {
        let Some(item) = view.items.get(index) else {
            continue;
        };
        let d2d = D2D_RECT_F {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        fill_rounded(surface, d2d, 14.0, palette.card);
        stroke_rounded(surface, d2d, 14.0, palette.separator, 1.0);
        if hover == Some(index) {
            fill_rounded(surface, d2d, 14.0, palette.hover);
        }
        let tile_size = 34.0;
        let tile = D2D_RECT_F {
            left: rect.left + (rect.width() - tile_size) / 2.0,
            top: rect.top + 10.0,
            right: rect.left + (rect.width() + tile_size) / 2.0,
            bottom: rect.top + 10.0 + tile_size,
        };
        let (glyph_text, color) = tool_visual(&item.id);
        crate::surface::draw_tile(
            &surface.target,
            &surface.brushes,
            D2D1_ROUNDED_RECT {
                rect: tile,
                radiusX: 10.0,
                radiusY: 10.0,
            },
            color,
        );
        glyph(
            &surface.target,
            gpu,
            glyph_text,
            tile,
            &surface.brushes.text,
            &gpu.symbol,
        );
        draw_text(
            &surface.target,
            &gpu.action,
            &item.label,
            D2D_RECT_F {
                left: rect.left + 4.0,
                top: rect.top + 48.0,
                right: rect.right - 4.0,
                bottom: rect.bottom - 4.0,
            },
            &surface.brushes.muted,
        );
    }
}

fn tool_visual(
    id: &str,
) -> (
    &'static str,
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
) {
    match id {
        "tool:settings" => ("\u{E713}", theme::color(130, 140, 255, 1.0)),
        "tool:explorer" => ("\u{E8B7}", theme::color(250, 190, 88, 1.0)),
        "tool:taskmgr" => ("\u{E7C4}", theme::color(255, 130, 150, 1.0)),
        "tool:sound" => ("\u{E767}", theme::color(120, 200, 255, 1.0)),
        "tool:bluetooth" => ("\u{E702}", theme::color(120, 220, 180, 1.0)),
        _ => ("\u{E80A}", theme::color(160, 170, 200, 1.0)),
    }
}

fn draw_header(gpu: &Gpu, surface: &Surface, view: &PanelView, width: f32) {
    let palette = theme::current();
    let tile_size = 26.0;
    let tile_top = (ISLAND_HEADER - tile_size) / 2.0;
    header_tile(
        gpu,
        surface,
        view.icon,
        D2D_RECT_F {
            left: ISLAND_PADDING,
            top: tile_top,
            right: ISLAND_PADDING + tile_size,
            bottom: tile_top + tile_size,
        },
    );
    let text_left = ISLAND_PADDING + 36.0;
    if view.expanded {
        draw_text(
            &surface.target,
            &gpu.title,
            &view.title,
            D2D_RECT_F {
                left: text_left,
                top: 0.0,
                right: width - 160.0,
                bottom: ISLAND_HEADER,
            },
            &surface.brushes.text,
        );
        let count = format!("{} 项", view.filtered_indices().len());
        let pill_width = 28.0 + count.chars().count() as f32 * 8.0;
        let pill = D2D_RECT_F {
            left: width - 52.0 - pill_width,
            top: (ISLAND_HEADER - 22.0) / 2.0,
            right: width - 52.0,
            bottom: (ISLAND_HEADER + 22.0) / 2.0,
        };
        fill_rounded(surface, pill, 11.0, palette.control);
        draw_text(
            &surface.target,
            &gpu.action,
            &count,
            pill,
            &surface.brushes.muted,
        );
        let close = D2D_RECT_F {
            left: width - 40.0,
            top: 6.0,
            right: width - 8.0,
            bottom: ISLAND_HEADER - 6.0,
        };
        glyph(
            &surface.target,
            gpu,
            "\u{E711}",
            close,
            &surface.brushes.muted,
            &gpu.symbol_small,
        );
    } else {
        draw_text(
            &surface.target,
            &gpu.strong,
            &view.summary,
            D2D_RECT_F {
                left: text_left,
                top: 0.0,
                right: width - 40.0,
                bottom: ISLAND_HEADER,
            },
            &surface.brushes.text,
        );
        glyph(
            &surface.target,
            gpu,
            "\u{E70D}",
            D2D_RECT_F {
                left: width - 36.0,
                top: 0.0,
                right: width - 14.0,
                bottom: ISLAND_HEADER,
            },
            &surface.brushes.faint,
            &gpu.symbol_small,
        );
    }
}

fn draw_search(gpu: &Gpu, surface: &Surface, view: &PanelView, focused: bool) {
    let palette = theme::current();
    let field = view.search_rect();
    let rect = D2D_RECT_F {
        left: field.left,
        top: field.top,
        right: field.right,
        bottom: field.bottom,
    };
    fill_rounded(surface, rect, 12.0, palette.input);
    stroke_rounded(
        surface,
        rect,
        12.0,
        if focused {
            palette.accent
        } else {
            palette.border
        },
        if focused { 1.4 } else { 1.0 },
    );
    glyph(
        &surface.target,
        gpu,
        "\u{E721}",
        D2D_RECT_F {
            left: field.left + 12.0,
            top: field.top,
            right: field.left + 34.0,
            bottom: field.bottom,
        },
        &surface.brushes.faint,
        &gpu.symbol_small,
    );
}

fn draw_chips(gpu: &Gpu, surface: &Surface, view: &PanelView, hover: Option<usize>) {
    let palette = theme::current();
    for (index, (label, rect)) in view.chip_rects().into_iter().enumerate() {
        let active = (index == 0 && view.category.is_empty())
            || (!view.category.is_empty() && label == view.category);
        let hovered = hover == Some(2000 + index);
        let shape = D2D_RECT_F {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        if active {
            surface.brushes.accent_points(shape.top, shape.bottom);
            // SAFETY: 画刷与矩形有效。
            unsafe {
                surface
                    .target
                    .FillRoundedRectangle(&rounded(shape, 15.0), &surface.brushes.accent_gradient);
            }
            draw_text(
                &surface.target,
                &gpu.action,
                &label,
                shape,
                &surface.brushes.accent_text,
            );
        } else {
            fill_rounded(
                surface,
                shape,
                15.0,
                if hovered {
                    palette.control_hover
                } else {
                    palette.control
                },
            );
            stroke_rounded(surface, shape, 15.0, palette.border, 1.0);
            draw_text(
                &surface.target,
                &gpu.action,
                &label,
                shape,
                if hovered {
                    &surface.brushes.text
                } else {
                    &surface.brushes.muted
                },
            );
        }
    }
}

fn draw_actions(gpu: &Gpu, surface: &Surface, view: &PanelView, hover: Option<usize>) {
    let palette = theme::current();
    let enabled = view.selected.is_some();
    for (index, action) in view.actions.iter().enumerate() {
        let Some(bounds) = action_rect(view, index) else {
            continue;
        };
        let rect = D2D_RECT_F {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        };
        let hovered = hover == Some(1000 + index) && enabled;
        if action.primary() && enabled {
            surface.brushes.accent_points(rect.top, rect.bottom);
            // SAFETY: 画刷与矩形有效。
            unsafe {
                surface
                    .target
                    .FillRoundedRectangle(&rounded(rect, 11.0), &surface.brushes.accent_gradient);
            }
        } else {
            let fill = if !enabled {
                palette.control
            } else if action.danger() {
                palette.danger_tint
            } else if hovered {
                palette.control_hover
            } else {
                palette.control
            };
            fill_rounded(surface, rect, 11.0, fill);
            stroke_rounded(
                surface,
                rect,
                11.0,
                if hovered {
                    palette.accent
                } else {
                    palette.separator
                },
                1.0,
            );
        }
        let brush = if !enabled {
            &surface.brushes.faint
        } else if action.primary() {
            &surface.brushes.accent_text
        } else if action.danger() {
            &surface.brushes.danger
        } else {
            &surface.brushes.text
        };
        glyph(
            &surface.target,
            gpu,
            action.glyph(),
            D2D_RECT_F {
                left: rect.left + 12.0,
                top: rect.top,
                right: rect.left + 28.0,
                bottom: rect.bottom,
            },
            brush,
            &gpu.symbol_small,
        );
        draw_text(
            &surface.target,
            &gpu.action,
            action.label(),
            D2D_RECT_F {
                left: rect.left + 26.0,
                top: rect.top,
                right: rect.right - 6.0,
                bottom: rect.bottom,
            },
            brush,
        );
    }
}

fn draw_status(gpu: &Gpu, surface: &Surface, view: &PanelView) {
    let Some(status) = &view.status else {
        return;
    };
    let palette = theme::current();
    let (width, _) = panel_size(view);
    let top = view.rows_top()
        + view.visible_rows() as f32 * ISLAND_ROW_HEIGHT
        + if view.actions.is_empty() {
            0.0
        } else {
            crate::panel::ISLAND_ACTION_BAR
        };
    fill_rounded(
        surface,
        D2D_RECT_F {
            left: ISLAND_PADDING + 2.0,
            top: top + 12.0,
            right: ISLAND_PADDING + 8.0,
            bottom: top + 18.0,
        },
        3.0,
        palette.success,
    );
    draw_caption(
        &surface.target,
        gpu,
        status,
        D2D_RECT_F {
            left: ISLAND_PADDING + 16.0,
            top,
            right: width - ISLAND_PADDING,
            bottom: top + crate::panel::ISLAND_STATUS_HEIGHT,
        },
        &surface.brushes.muted,
    );
}

fn draw_scroll(surface: &Surface, view: &PanelView) {
    let palette = theme::current();
    let total = view.filtered_indices().len();
    let visible = view.visible_rows();
    if total <= visible {
        return;
    }
    let (width, _) = panel_size(view);
    let top = view.rows_top() + 6.0;
    let bottom = view.rows_top() + visible as f32 * ISLAND_ROW_HEIGHT - 6.0;
    let track_height = (bottom - top).max(1.0);
    let thumb_height = (track_height * visible as f32 / total as f32).max(20.0);
    let max_offset = total.saturating_sub(visible) as f32;
    let progress = (view.offset as f32 / max_offset).clamp(0.0, 1.0);
    let thumb_top = top + (track_height - thumb_height) * progress;
    fill_rounded(
        surface,
        D2D_RECT_F {
            left: width - 9.0,
            top,
            right: width - 6.0,
            bottom,
        },
        1.5,
        palette.separator,
    );
    fill_rounded(
        surface,
        D2D_RECT_F {
            left: width - 9.0,
            top: thumb_top,
            right: width - 6.0,
            bottom: thumb_top + thumb_height,
        },
        1.5,
        palette.control_hover,
    );
}

fn rounded(rect: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    }
}
