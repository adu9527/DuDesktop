//! 设置窗绘制：玻璃外壳、页头、卡片、开关/单选与滚动指示。

use windows::Win32::Graphics::Direct2D::{
    Common::D2D_RECT_F, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT,
};

use super::{
    SettingsItemKind, SettingsView, UiRect,
    layout::{CARD_PADDING, CARD_TITLE, Layout, RADIUS},
};
use crate::{
    gpu::Gpu,
    panel::paint,
    surface::{RenderError, Surface, draw_text},
    theme,
};

#[derive(Clone, Copy)]
pub(super) struct DrawState {
    pub scroll: f32,
    pub hover: Option<usize>,
    pub close_hover: bool,
    pub focus: Option<usize>,
}

pub(super) fn draw(
    gpu: &Gpu,
    surface: &Surface,
    view: &SettingsView,
    layout: &Layout,
    state: DrawState,
) -> Result<(), RenderError> {
    let scroll = state.scroll;
    let hover = state.hover;
    let close_hover = state.close_hover;
    let focus = state.focus;
    let palette = theme::current();
    crate::surface::draw_window_shell(
        &surface.target,
        &surface.brushes,
        layout.width,
        layout.height,
        RADIUS,
    );
    // 页头：品牌方块、标题与说明、关闭按钮。
    let logo = layout.logo.to_d2d();
    surface.brushes.accent_points(logo.top, logo.bottom);
    // SAFETY: 画刷与矩形有效。
    unsafe {
        surface
            .target
            .FillRoundedRectangle(&rounded(logo, 11.0), &surface.brushes.accent_gradient);
    }
    draw_text(
        &surface.target,
        &gpu.display,
        "D",
        D2D_RECT_F {
            left: logo.left + 9.0,
            top: logo.top,
            right: logo.right,
            bottom: logo.bottom,
        },
        &surface.brushes.accent_text,
    );
    draw_text(
        &surface.target,
        &gpu.display,
        &view.title,
        D2D_RECT_F {
            left: 74.0,
            top: 22.0,
            right: layout.width - 80.0,
            bottom: 58.0,
        },
        &surface.brushes.text,
    );
    draw_text(
        &surface.target,
        &gpu.caption,
        &view.subtitle,
        D2D_RECT_F {
            left: 74.0,
            top: 54.0,
            right: layout.width - 80.0,
            bottom: 82.0,
        },
        &surface.brushes.muted,
    );
    if close_hover {
        paint::fill_rounded(surface, layout.close.to_d2d(), 10.0, palette.close_hover);
    }
    paint::glyph(
        &surface.target,
        gpu,
        "\u{E711}",
        layout.close.to_d2d(),
        &surface.brushes.muted,
        &gpu.symbol_small,
    );
    // 内容区裁剪，避免滚动时压到页头/页脚。
    let clip = layout.viewport.to_d2d();
    // SAFETY: 裁剪矩形在目标范围内；与 Pop 配对。
    unsafe {
        surface
            .target
            .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    for card in &layout.cards {
        let group = &view.groups[card.group];
        let rect = card.rect.offset(0.0, -scroll);
        if rect.bottom < layout.viewport.top || rect.top > layout.viewport.bottom {
            continue;
        }
        paint::fill_rounded(surface, rect.to_d2d(), 18.0, palette.card);
        paint::stroke_rounded(surface, rect.to_d2d(), 18.0, palette.separator, 1.0);
        draw_text(
            &surface.target,
            &gpu.strong,
            &group.title,
            D2D_RECT_F {
                left: rect.left + CARD_PADDING,
                top: rect.top + CARD_PADDING - 2.0,
                right: rect.right - CARD_PADDING,
                bottom: rect.top + CARD_PADDING + CARD_TITLE - 2.0,
            },
            &surface.brushes.muted,
        );
    }
    for (index, entry) in layout.items.iter().enumerate() {
        let Some(group) = view.groups.get(entry.group) else {
            continue;
        };
        let Some(item) = group.items.get(entry.item) else {
            continue;
        };
        let rect = entry.rect.offset(0.0, -scroll);
        if rect.bottom < layout.viewport.top || rect.top > layout.viewport.bottom {
            continue;
        }
        let hovered = hover == Some(index);
        let focused = focus == Some(index);
        if hovered || focused {
            paint::fill_rounded(
                surface,
                rect.to_d2d(),
                9.0,
                if focused {
                    palette.selected
                } else {
                    palette.hover
                },
            );
            if focused {
                paint::stroke_rounded(surface, rect.to_d2d(), 9.0, palette.accent, 1.0);
            }
        }
        draw_item(gpu, surface, item, rect, hovered || focused);
    }
    // SAFETY: 与 PushAxisAlignedClip 配对。
    unsafe {
        surface.target.PopAxisAlignedClip();
    }
    draw_footer(gpu, surface, view, layout);
    draw_scroll(surface, layout, scroll);
    Ok(())
}

fn draw_item(
    gpu: &Gpu,
    surface: &Surface,
    item: &super::SettingsItem,
    rect: UiRect,
    _active: bool,
) {
    let palette = theme::current();
    let text_brush = if item.enabled {
        &surface.brushes.text
    } else {
        &surface.brushes.faint
    };
    match item.kind {
        SettingsItemKind::Action => {
            draw_text(
                &surface.target,
                &gpu.row,
                &item.label,
                D2D_RECT_F {
                    left: rect.left + 12.0,
                    top: rect.top,
                    right: rect.right - 28.0,
                    bottom: rect.bottom,
                },
                text_brush,
            );
            paint::glyph(
                &surface.target,
                gpu,
                "\u{E76C}",
                D2D_RECT_F {
                    left: rect.right - 26.0,
                    top: rect.top,
                    right: rect.right - 8.0,
                    bottom: rect.bottom,
                },
                &surface.brushes.faint,
                &gpu.symbol_small,
            );
        }
        SettingsItemKind::Toggle => {
            draw_text(
                &surface.target,
                &gpu.row,
                &item.label,
                D2D_RECT_F {
                    left: rect.left + 12.0,
                    top: rect.top,
                    right: rect.right - 62.0,
                    bottom: rect.bottom,
                },
                text_brush,
            );
            draw_switch(surface, item, rect);
        }
        SettingsItemKind::Radio => {
            let circle = UiRect {
                left: rect.left + 12.0,
                top: rect.top + (rect.height() - 16.0) / 2.0,
                right: rect.left + 28.0,
                bottom: rect.top + (rect.height() + 16.0) / 2.0,
            };
            if item.checked {
                paint::fill_rounded(surface, circle.to_d2d(), 8.0, palette.accent);
                paint::fill_rounded(
                    surface,
                    UiRect {
                        left: circle.left + 5.0,
                        top: circle.top + 5.0,
                        right: circle.right - 5.0,
                        bottom: circle.bottom - 5.0,
                    }
                    .to_d2d(),
                    3.0,
                    palette.accent_text,
                );
            } else {
                paint::stroke_rounded(surface, circle.to_d2d(), 8.0, palette.border, 1.4);
            }
            draw_text(
                &surface.target,
                &gpu.row,
                &item.label,
                D2D_RECT_F {
                    left: rect.left + 38.0,
                    top: rect.top,
                    right: rect.right - 8.0,
                    bottom: rect.bottom,
                },
                text_brush,
            );
        }
    }
}

fn draw_switch(surface: &Surface, item: &super::SettingsItem, rect: UiRect) {
    let palette = theme::current();
    let track = UiRect {
        left: rect.right - 54.0,
        top: rect.top + (rect.height() - 20.0) / 2.0,
        right: rect.right - 16.0,
        bottom: rect.top + (rect.height() + 20.0) / 2.0,
    };
    if item.checked {
        surface.brushes.accent_points(track.top, track.bottom);
        // SAFETY: 画刷与矩形有效。
        unsafe {
            surface.target.FillRoundedRectangle(
                &rounded(track.to_d2d(), 10.0),
                &surface.brushes.accent_gradient,
            );
        }
    } else {
        paint::fill_rounded(surface, track.to_d2d(), 10.0, palette.control_hover);
    }
    let knob_left = if item.checked {
        track.right - 18.0
    } else {
        track.left + 2.0
    };
    paint::fill_rounded(
        surface,
        UiRect {
            left: knob_left,
            top: track.top + 2.0,
            right: knob_left + 16.0,
            bottom: track.bottom - 2.0,
        }
        .to_d2d(),
        8.0,
        if item.checked {
            theme::color(255, 255, 255, 1.0)
        } else {
            palette.secondary
        },
    );
}

fn draw_footer(gpu: &Gpu, surface: &Surface, view: &SettingsView, layout: &Layout) {
    let palette = theme::current();
    paint::fill_rounded(
        surface,
        UiRect {
            left: 0.0,
            top: layout.footer.top,
            right: layout.width,
            bottom: layout.footer.top + 1.0,
        }
        .to_d2d(),
        0.5,
        palette.separator,
    );
    draw_text(
        &surface.target,
        &gpu.caption,
        &view.footer,
        D2D_RECT_F {
            left: layout.footer.left,
            top: layout.footer.top,
            right: layout.footer.right,
            bottom: layout.footer.bottom,
        },
        &surface.brushes.faint,
    );
}

fn draw_scroll(surface: &Surface, layout: &Layout, scroll: f32) {
    let max = layout.scroll_max();
    if max <= 0.0 {
        return;
    }
    let palette = theme::current();
    let top = layout.viewport.top + 6.0;
    let bottom = layout.viewport.bottom - 6.0;
    let track_height = (bottom - top).max(1.0);
    let thumb_height = (track_height * layout.viewport.height() / layout.content_height).max(28.0);
    let progress = (scroll / max).clamp(0.0, 1.0);
    let thumb_top = top + (track_height - thumb_height) * progress;
    paint::fill_rounded(
        surface,
        UiRect {
            left: layout.width - 12.0,
            top,
            right: layout.width - 8.0,
            bottom,
        }
        .to_d2d(),
        2.0,
        palette.separator,
    );
    paint::fill_rounded(
        surface,
        UiRect {
            left: layout.width - 12.0,
            top: thumb_top,
            right: layout.width - 8.0,
            bottom: thumb_top + thumb_height,
        }
        .to_d2d(),
        2.0,
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
