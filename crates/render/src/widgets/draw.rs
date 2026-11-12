//! 小部件绘制：玻璃外壳、桌面收纳网格、时钟、天气与媒体控制。

use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::{
    Common::D2D_RECT_F, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT, ID2D1Bitmap,
};

use super::{
    MusicView, WeatherView, WidgetKind, WidgetView,
    layout::{self, Layout, PADDING, RADIUS, Rect},
};
use crate::{
    gpu::Gpu,
    panel::paint,
    surface::{RenderError, Surface, create_bitmap, draw_text},
    theme,
};

pub(super) struct WidgetIcons {
    entries: HashMap<String, (u64, ID2D1Bitmap)>,
}

impl WidgetIcons {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn sync(&mut self, surface: &Surface, view: &WidgetView) -> Result<(), RenderError> {
        let mut known = std::collections::HashSet::new();
        for item in &view.organizer {
            let Some(icon) = &item.icon else {
                continue;
            };
            if !icon.is_valid() {
                continue;
            }
            known.insert(item.id.clone());
            let fingerprint = crate::surface::bitmap_fingerprint(icon);
            if self
                .entries
                .get(&item.id)
                .is_some_and(|(cached, _)| *cached == fingerprint)
            {
                continue;
            }
            let bitmap = create_bitmap(&surface.target, icon)?;
            self.entries.insert(item.id.clone(), (fingerprint, bitmap));
        }
        self.entries.retain(|id, _| known.contains(id));
        Ok(())
    }

    fn get(&self, id: &str) -> Option<&ID2D1Bitmap> {
        self.entries.get(id).map(|(_, bitmap)| bitmap)
    }
}

pub(super) fn draw(
    gpu: &Gpu,
    surface: &Surface,
    view: &WidgetView,
    layout: &Layout,
    icons: &WidgetIcons,
    hover: Option<layout::Hit>,
) -> Result<(), RenderError> {
    crate::surface::draw_shell(
        &surface.target,
        &surface.brushes,
        layout.width,
        layout.height,
        RADIUS,
    );
    match view.kind {
        WidgetKind::Organizer => draw_organizer(gpu, surface, view, layout, icons, hover),
        WidgetKind::Clock => draw_clock(gpu, surface, view, layout),
        WidgetKind::Weather => draw_weather(gpu, surface, view, layout),
        WidgetKind::Music => draw_music(gpu, surface, view, layout, hover),
        WidgetKind::Controls => draw_controls(gpu, surface, view, layout, hover),
    }
    draw_close(gpu, surface, layout);
    Ok(())
}

/// 快捷控制：音量滑杆、WLAN/蓝牙状态与四个系统入口。
fn draw_controls(
    gpu: &Gpu,
    surface: &Surface,
    view: &WidgetView,
    layout: &Layout,
    hover: Option<layout::Hit>,
) {
    let palette = theme::current();
    let controls = &view.controls;
    text(
        surface,
        &gpu.strong,
        "快捷控制",
        Rect {
            left: PADDING,
            top: 8.0,
            right: layout.width - 44.0,
            bottom: 34.0,
        },
        &surface.brushes.text,
    );
    // 静音按钮。
    let mute = to_d2d(layout.mute);
    paint::fill_rounded(
        surface,
        mute,
        8.0,
        if hover == Some(layout::Hit::Mute) {
            palette.control_hover
        } else {
            palette.control
        },
    );
    paint::glyph(
        &surface.target,
        gpu,
        if controls.muted {
            "\u{E74F}"
        } else {
            "\u{E767}"
        },
        mute,
        &surface.brushes.text,
        &gpu.symbol_small,
    );
    // 滑轨与滑块。
    let track = D2D_RECT_F {
        left: layout.slider.left,
        top: layout.slider.top + 7.0,
        right: layout.slider.right,
        bottom: layout.slider.top + 11.0,
    };
    paint::fill_rounded(surface, track, 2.0, palette.separator);
    let ratio = if controls.muted {
        0.0
    } else {
        f32::from(controls.volume) / 100.0
    };
    let knob_x = track.left + (track.right - track.left) * ratio;
    paint::fill_rounded(
        surface,
        D2D_RECT_F {
            left: track.left,
            top: track.top,
            right: knob_x.max(track.left),
            bottom: track.bottom,
        },
        2.0,
        palette.accent,
    );
    paint::fill_rounded(
        surface,
        D2D_RECT_F {
            left: knob_x - 7.0,
            top: layout.slider.top + 1.0,
            right: knob_x + 7.0,
            bottom: layout.slider.top + 15.0,
        },
        7.0,
        theme::color(255, 255, 255, 1.0),
    );
    text(
        surface,
        &gpu.caption,
        &format!("{}%", if controls.muted { 0 } else { controls.volume }),
        Rect {
            left: layout.width - PADDING - 44.0,
            top: layout.slider.top + 1.0,
            right: layout.width - PADDING,
            bottom: layout.slider.top + 21.0,
        },
        &surface.brushes.muted,
    );
    let device = truncate(&controls.device, 42);
    text(
        surface,
        &gpu.caption,
        &device,
        Rect {
            left: PADDING,
            top: 74.0,
            right: layout.width - PADDING,
            bottom: 92.0,
        },
        &surface.brushes.muted,
    );
    let wifi_detail = if controls.wifi.is_empty() {
        "未连接"
    } else {
        &controls.wifi
    };
    draw_big_button(
        gpu,
        surface,
        layout.wifi,
        BigButton {
            label: "WLAN",
            detail: wifi_detail,
            active: controls.wifi_on,
            glyph: "\u{E701}",
            hovered: hover == Some(layout::Hit::Wifi),
        },
    );
    let (bluetooth_detail, bluetooth_on) = match controls.bluetooth {
        Some(true) => ("已开启", true),
        Some(false) => ("已关闭", false),
        None => ("不可用", false),
    };
    draw_big_button(
        gpu,
        surface,
        layout.bluetooth,
        BigButton {
            label: "蓝牙",
            detail: bluetooth_detail,
            active: bluetooth_on,
            glyph: "\u{E702}",
            hovered: hover == Some(layout::Hit::Bluetooth),
        },
    );
    let labels = ["设置", "显示", "声音", "网络"];
    let glyphs = ["\u{E713}", "\u{E7F4}", "\u{E767}", "\u{E701}"];
    for (index, rect) in layout.buttons.iter().enumerate() {
        let d2d = to_d2d(*rect);
        paint::fill_rounded(
            surface,
            d2d,
            10.0,
            if hover == Some(layout::Hit::Button(index)) {
                palette.control_hover
            } else {
                palette.control
            },
        );
        paint::glyph(
            &surface.target,
            gpu,
            glyphs[index],
            D2D_RECT_F {
                left: d2d.left + 6.0,
                top: d2d.top + 8.0,
                right: d2d.left + 26.0,
                bottom: d2d.top + 28.0,
            },
            &surface.brushes.text,
            &gpu.symbol_small,
        );
        text(
            surface,
            &gpu.caption,
            labels[index],
            Rect {
                left: rect.left + 28.0,
                top: rect.top + 9.0,
                right: rect.right - 2.0,
                bottom: rect.bottom - 6.0,
            },
            &surface.brushes.muted,
        );
    }
}

/// 大按钮（WLAN / 蓝牙）内容。
struct BigButton<'a> {
    label: &'a str,
    detail: &'a str,
    active: bool,
    glyph: &'a str,
    hovered: bool,
}

fn draw_big_button(gpu: &Gpu, surface: &Surface, rect: Rect, button: BigButton<'_>) {
    let palette = theme::current();
    let d2d = to_d2d(rect);
    let background = if button.active {
        palette.accent
    } else if button.hovered {
        palette.control_hover
    } else {
        palette.control
    };
    paint::fill_rounded(surface, d2d, 14.0, background);
    let foreground = if button.active {
        paint::set_fill(&surface.brushes, theme::color(255, 255, 255, 1.0));
        &surface.brushes.fill
    } else {
        &surface.brushes.text
    };
    paint::glyph(
        &surface.target,
        gpu,
        button.glyph,
        D2D_RECT_F {
            left: d2d.left + 12.0,
            top: d2d.top + 13.0,
            right: d2d.left + 38.0,
            bottom: d2d.top + 39.0,
        },
        foreground,
        &gpu.symbol,
    );
    text(
        surface,
        &gpu.action,
        button.label,
        Rect {
            left: rect.left + 44.0,
            top: rect.top + 8.0,
            right: rect.right - 6.0,
            bottom: rect.top + 28.0,
        },
        foreground,
    );
    let detail_color = if button.active {
        foreground
    } else {
        &surface.brushes.muted
    };
    text(
        surface,
        &gpu.caption,
        button.detail,
        Rect {
            left: rect.left + 44.0,
            top: rect.top + 26.0,
            right: rect.right - 6.0,
            bottom: rect.bottom - 4.0,
        },
        detail_color,
    );
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut value: String = text.chars().take(limit.saturating_sub(1)).collect();
    value.push('…');
    value
}

fn draw_close(gpu: &Gpu, surface: &Surface, layout: &Layout) {
    let palette = theme::current();
    paint::fill_rounded(surface, to_d2d(layout.close), 8.0, palette.control);
    paint::glyph(
        &surface.target,
        gpu,
        "\u{E711}",
        to_d2d(layout.close),
        &surface.brushes.muted,
        &gpu.symbol_small,
    );
}

fn draw_organizer(
    gpu: &Gpu,
    surface: &Surface,
    view: &WidgetView,
    layout: &Layout,
    icons: &WidgetIcons,
    hover: Option<layout::Hit>,
) {
    let palette = theme::current();
    text(
        surface,
        &gpu.strong,
        WidgetKind::Organizer.title(),
        Rect {
            left: PADDING,
            top: 8.0,
            right: layout.width - 44.0,
            bottom: 36.0,
        },
        &surface.brushes.text,
    );
    if view.organizer.is_empty() {
        text(
            surface,
            &gpu.caption,
            "从应用库或托盘添加应用、文件夹或路径",
            Rect {
                left: PADDING,
                top: layout::HEADER + 16.0,
                right: layout.width - PADDING,
                bottom: layout::HEADER + 44.0,
            },
            &surface.brushes.muted,
        );
        return;
    }
    for (index, rect) in layout.items.iter().enumerate() {
        let Some(item) = view.organizer.get(index) else {
            continue;
        };
        if hover == Some(layout::Hit::Item(index)) {
            paint::fill_rounded(surface, to_d2d(*rect), 12.0, palette.hover);
        }
        let icon_size = 36.0;
        let icon_rect = D2D_RECT_F {
            left: rect.left + (rect.width() - icon_size) / 2.0,
            top: rect.top + 8.0,
            right: rect.left + (rect.width() + icon_size) / 2.0,
            bottom: rect.top + 8.0 + icon_size,
        };
        if let Some(bitmap) = icons.get(&item.id) {
            // SAFETY: 位图与目标矩形有效，只做绘制。
            unsafe {
                surface.target.DrawBitmap(
                    bitmap,
                    Some(&icon_rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        } else {
            let color = theme::current().accent;
            crate::surface::draw_tile(
                &surface.target,
                &surface.brushes,
                D2D1_ROUNDED_RECT {
                    rect: icon_rect,
                    radiusX: 10.0,
                    radiusY: 10.0,
                },
                color,
            );
            let initial: String = item
                .label
                .chars()
                .next()
                .map(|value| value.to_uppercase().collect())
                .unwrap_or_else(|| "?".into());
            text(
                surface,
                &gpu.action,
                &initial,
                Rect {
                    left: icon_rect.left,
                    top: icon_rect.top,
                    right: icon_rect.right,
                    bottom: icon_rect.bottom,
                },
                &surface.brushes.accent_text,
            );
        }
        text(
            surface,
            &gpu.action,
            &item.label,
            Rect {
                left: rect.left + 2.0,
                top: rect.top + 50.0,
                right: rect.right - 2.0,
                bottom: rect.bottom - 2.0,
            },
            &surface.brushes.muted,
        );
    }
}

fn draw_clock(gpu: &Gpu, surface: &Surface, view: &WidgetView, layout: &Layout) {
    text(
        surface,
        &gpu.clock,
        &view.clock,
        Rect {
            left: PADDING + 8.0,
            top: 26.0,
            right: layout.width - PADDING,
            bottom: 86.0,
        },
        &surface.brushes.text,
    );
    text(
        surface,
        &gpu.caption,
        &view.date,
        Rect {
            left: PADDING + 10.0,
            top: 92.0,
            right: layout.width - PADDING,
            bottom: 122.0,
        },
        &surface.brushes.muted,
    );
}

fn draw_weather(gpu: &Gpu, surface: &Surface, view: &WidgetView, layout: &Layout) {
    let weather = &view.weather;
    text(
        surface,
        &gpu.caption,
        &weather.city,
        Rect {
            left: PADDING,
            top: 10.0,
            right: layout.width - 44.0,
            bottom: 32.0,
        },
        &surface.brushes.muted,
    );
    text(
        surface,
        &gpu.temperature,
        &weather.temperature,
        Rect {
            left: PADDING + 2.0,
            top: 38.0,
            right: layout.width - 80.0,
            bottom: 86.0,
        },
        &surface.brushes.text,
    );
    text(
        surface,
        &gpu.row,
        &weather.text,
        Rect {
            left: PADDING + 4.0,
            top: 88.0,
            right: layout.width - 80.0,
            bottom: 118.0,
        },
        &surface.brushes.muted,
    );
    text(
        surface,
        &gpu.temperature,
        weather_symbol(weather),
        Rect {
            left: layout.width - 84.0,
            top: 46.0,
            right: layout.width - 20.0,
            bottom: 110.0,
        },
        &surface.brushes.accent,
    );
}

fn weather_symbol(weather: &WeatherView) -> &'static str {
    match weather.code {
        0 | 1 => "☀",
        2 | 3 | 45 | 48 => "☁",
        95 | 96 | 99 => "⚡",
        71 | 73 | 75 | 77 | 85 | 86 => "❄",
        _ => "☂",
    }
}

fn draw_music(
    gpu: &Gpu,
    surface: &Surface,
    view: &WidgetView,
    layout: &Layout,
    hover: Option<layout::Hit>,
) {
    let music: &MusicView = &view.music;
    if !music.source.is_empty() {
        text(
            surface,
            &gpu.caption,
            &music.source,
            Rect {
                left: PADDING,
                top: 10.0,
                right: layout.width - 44.0,
                bottom: 30.0,
            },
            &surface.brushes.faint,
        );
    }
    let tile = D2D_RECT_F {
        left: PADDING,
        top: 40.0,
        right: PADDING + 42.0,
        bottom: 82.0,
    };
    let palette = theme::current();
    crate::surface::draw_tile(
        &surface.target,
        &surface.brushes,
        D2D1_ROUNDED_RECT {
            rect: tile,
            radiusX: 10.0,
            radiusY: 10.0,
        },
        theme::color(132, 118, 246, 1.0),
    );
    paint::glyph(
        &surface.target,
        gpu,
        "\u{E8D6}",
        tile,
        &surface.brushes.text,
        &gpu.symbol,
    );
    text(
        surface,
        &gpu.strong,
        &music.title,
        Rect {
            left: PADDING + 56.0,
            top: 42.0,
            right: layout.width - PADDING,
            bottom: 66.0,
        },
        &surface.brushes.text,
    );
    text(
        surface,
        &gpu.caption,
        &music.artist,
        Rect {
            left: PADDING + 56.0,
            top: 66.0,
            right: layout.width - PADDING,
            bottom: 88.0,
        },
        &surface.brushes.muted,
    );
    for (rect, glyph, hit) in [
        (layout.previous, "\u{E100}", layout::Hit::Previous),
        (
            layout.play,
            if music.playing {
                "\u{E769}"
            } else {
                "\u{E768}"
            },
            layout::Hit::Play,
        ),
        (layout.next, "\u{E101}", layout::Hit::Next),
    ] {
        paint::fill_rounded(
            surface,
            to_d2d(rect),
            17.0,
            if hover == Some(hit) {
                palette.control_hover
            } else {
                palette.control
            },
        );
        paint::stroke_rounded(surface, to_d2d(rect), 17.0, palette.separator, 1.0);
        paint::glyph(
            &surface.target,
            gpu,
            glyph,
            to_d2d(rect),
            &surface.brushes.text,
            &gpu.symbol_small,
        );
    }
}

fn to_d2d(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn text(
    surface: &Surface,
    format: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
    value: &str,
    rect: Rect,
    brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
) {
    draw_text(&surface.target, format, value, to_d2d(rect), brush);
}
