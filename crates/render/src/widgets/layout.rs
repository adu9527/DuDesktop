//! 小部件布局：窗口尺寸、关闭/媒体按钮与收纳网格命中。

use super::{WidgetKind, WidgetView};

pub(super) const PADDING: f32 = 14.0;
pub(super) const HEADER: f32 = 40.0;
pub(super) const RADIUS: f32 = 20.0;
pub(super) const CELL_GAP: f32 = 8.0;
pub(super) const CELL_HEIGHT: f32 = 82.0;
pub(super) const MUSIC_BUTTON: f32 = 34.0;
pub(super) const CONTROLS_HEIGHT: f32 = 214.0;
pub(super) const CONTROLS_BIG_HEIGHT: f32 = 52.0;
pub(super) const CONTROLS_SMALL_HEIGHT: f32 = 40.0;

#[derive(Debug, Clone, Copy)]
pub(super) struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn width(self) -> f32 {
        self.right - self.left
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hit {
    Close,
    Previous,
    Play,
    Next,
    Slider,
    Mute,
    Wifi,
    Bluetooth,
    Button(usize),
    Item(usize),
}

const ZERO: Rect = Rect {
    left: 0.0,
    top: 0.0,
    right: 0.0,
    bottom: 0.0,
};

#[derive(Debug, Clone)]
pub(super) struct Layout {
    pub width: f32,
    pub height: f32,
    pub close: Rect,
    pub previous: Rect,
    pub play: Rect,
    pub next: Rect,
    pub slider: Rect,
    pub mute: Rect,
    pub wifi: Rect,
    pub bluetooth: Rect,
    pub buttons: Vec<Rect>,
    pub items: Vec<Rect>,
}

impl Layout {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if self.close.contains(x, y) {
            return Some(Hit::Close);
        }
        if self.previous.contains(x, y) {
            return Some(Hit::Previous);
        }
        if self.play.contains(x, y) {
            return Some(Hit::Play);
        }
        if self.next.contains(x, y) {
            return Some(Hit::Next);
        }
        if self.slider.contains(x, y) {
            return Some(Hit::Slider);
        }
        if self.mute.contains(x, y) {
            return Some(Hit::Mute);
        }
        if self.wifi.contains(x, y) {
            return Some(Hit::Wifi);
        }
        if self.bluetooth.contains(x, y) {
            return Some(Hit::Bluetooth);
        }
        if let Some(index) = self.buttons.iter().position(|rect| rect.contains(x, y)) {
            return Some(Hit::Button(index));
        }
        self.items
            .iter()
            .position(|rect| rect.contains(x, y))
            .map(Hit::Item)
    }

    /// 滑块位置换算为 0..=100 音量。
    pub fn volume_at(&self, x: f32) -> u8 {
        if self.slider.width() <= 0.0 {
            return 0;
        }
        let ratio = ((x - self.slider.left) / self.slider.width()).clamp(0.0, 1.0);
        (ratio * 100.0).round() as u8
    }
}

pub(super) fn width_of(view: &WidgetView, width: f32) -> f32 {
    width
        .clamp(220.0, 640.0)
        .max(if view.kind == WidgetKind::Clock {
            220.0
        } else {
            240.0
        })
}

pub(super) fn compute(view: &WidgetView, width: f32) -> Layout {
    let width = width_of(view, width);
    let close = Rect {
        left: width - 34.0,
        top: 8.0,
        right: width - 8.0,
        bottom: 34.0,
    };
    let mut items = Vec::new();
    let height = match view.kind {
        WidgetKind::Organizer => {
            let columns = view.columns.clamp(2, 6);
            let count = view.organizer.len().min(24);
            let rows = count.div_ceil(columns).max(1);
            let cell_width =
                (width - PADDING * 2.0 - CELL_GAP * (columns.saturating_sub(1)) as f32)
                    / columns as f32;
            let top = HEADER + 2.0;
            for index in 0..count {
                let row = index / columns;
                let column = index % columns;
                let left = PADDING + column as f32 * (cell_width + CELL_GAP);
                let cell_top = top + row as f32 * (CELL_HEIGHT + CELL_GAP);
                items.push(Rect {
                    left,
                    top: cell_top,
                    right: left + cell_width,
                    bottom: cell_top + CELL_HEIGHT,
                });
            }
            HEADER
                + PADDING
                + rows as f32 * CELL_HEIGHT
                + (rows.saturating_sub(1) as f32) * CELL_GAP
                + PADDING
        }
        WidgetKind::Clock => 136.0,
        WidgetKind::Weather => 132.0,
        WidgetKind::Music => 148.0,
        WidgetKind::Controls => CONTROLS_HEIGHT,
    };
    let (play, previous, next) = if view.kind == WidgetKind::Music {
        let play = Rect {
            left: width / 2.0 - MUSIC_BUTTON / 2.0,
            top: height - MUSIC_BUTTON - 14.0,
            right: width / 2.0 + MUSIC_BUTTON / 2.0,
            bottom: height - 14.0,
        };
        let previous = Rect {
            left: play.left - MUSIC_BUTTON - 18.0,
            top: play.top,
            right: play.left - 18.0,
            bottom: play.bottom,
        };
        let next = Rect {
            left: play.right + 18.0,
            top: play.top,
            right: play.right + 18.0 + MUSIC_BUTTON,
            bottom: play.bottom,
        };
        (play, previous, next)
    } else {
        (ZERO, ZERO, ZERO)
    };
    let (slider, mute, wifi, bluetooth, buttons) = if view.kind == WidgetKind::Controls {
        let mute = Rect {
            left: PADDING,
            top: 46.0,
            right: PADDING + 26.0,
            bottom: 72.0,
        };
        let slider = Rect {
            left: PADDING + 34.0,
            top: 48.0,
            right: width - PADDING - 52.0,
            bottom: 70.0,
        };
        let gap = 10.0;
        let half = (width - PADDING * 2.0 - gap) / 2.0;
        let big_top = 96.0;
        let wifi = Rect {
            left: PADDING,
            top: big_top,
            right: PADDING + half,
            bottom: big_top + CONTROLS_BIG_HEIGHT,
        };
        let bluetooth = Rect {
            left: PADDING + half + gap,
            top: big_top,
            right: width - PADDING,
            bottom: big_top + CONTROLS_BIG_HEIGHT,
        };
        let gap4 = 8.0;
        let small = (width - PADDING * 2.0 - gap4 * 3.0) / 4.0;
        let small_top = big_top + CONTROLS_BIG_HEIGHT + 10.0;
        let buttons = (0..4)
            .map(|index| Rect {
                left: PADDING + index as f32 * (small + gap4),
                top: small_top,
                right: PADDING + index as f32 * (small + gap4) + small,
                bottom: small_top + CONTROLS_SMALL_HEIGHT,
            })
            .collect();
        (slider, mute, wifi, bluetooth, buttons)
    } else {
        (ZERO, ZERO, ZERO, ZERO, Vec::new())
    };
    Layout {
        width,
        height,
        close,
        previous,
        play,
        next,
        slider,
        mute,
        wifi,
        bluetooth,
        buttons,
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::WidgetItem;

    #[test]
    fn organizer_height_grows_with_rows() {
        let empty = WidgetView::organizer(Vec::new(), 3);
        let items = (0..6)
            .map(|index| WidgetItem {
                id: format!("app:{index}"),
                label: format!("应用 {index}"),
                icon: None,
            })
            .collect();
        let full = WidgetView::organizer(items, 3);
        assert!(compute(&full, 340.0).height > compute(&empty, 340.0).height);
        assert_eq!(compute(&full, 340.0).items.len(), 6);
    }

    #[test]
    fn close_and_music_buttons_hit_without_overlap() {
        let view = WidgetView::music(Default::default());
        let layout = compute(&view, 340.0);
        let center = |rect: Rect| {
            (
                (rect.left + rect.right) / 2.0,
                (rect.top + rect.bottom) / 2.0,
            )
        };
        let (x, y) = center(layout.close);
        assert_eq!(layout.hit(x, y), Some(Hit::Close));
        let (x, y) = center(layout.play);
        assert_eq!(layout.hit(x, y), Some(Hit::Play));
        let (x, y) = center(layout.previous);
        assert_eq!(layout.hit(x, y), Some(Hit::Previous));
        let (x, y) = center(layout.next);
        assert_eq!(layout.hit(x, y), Some(Hit::Next));
    }
}
