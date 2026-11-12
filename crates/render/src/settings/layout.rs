//! 设置窗布局：两列卡片 + 底部通栏卡片，内容超出视口时纵向滚动。

use super::{SettingsGroup, SettingsView, UiRect};

pub(super) const WINDOW_WIDTH: f32 = 880.0;
pub(super) const DESIRED_HEIGHT: f32 = 730.0;
pub(super) const MIN_HEIGHT: f32 = 420.0;
pub(super) const HEADER_HEIGHT: f32 = 96.0;
pub(super) const FOOTER_HEIGHT: f32 = 44.0;
pub(super) const MARGIN: f32 = 24.0;
pub(super) const GAP: f32 = 14.0;
pub(super) const ITEM_GAP: f32 = 10.0;
pub(super) const CARD_PADDING: f32 = 12.0;
pub(super) const CARD_TITLE: f32 = 26.0;
pub(super) const ITEM_HEIGHT: f32 = 32.0;
pub(super) const RADIUS: f32 = 26.0;

#[derive(Debug, Clone, Copy)]
pub(super) struct CardLayout {
    pub group: usize,
    pub rect: UiRect,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ItemLayout {
    pub group: usize,
    pub item: usize,
    pub rect: UiRect,
}

#[derive(Debug, Clone)]
pub(super) struct Layout {
    pub width: f32,
    pub height: f32,
    pub logo: UiRect,
    pub close: UiRect,
    pub viewport: UiRect,
    pub footer: UiRect,
    pub cards: Vec<CardLayout>,
    pub items: Vec<ItemLayout>,
    pub content_height: f32,
}

impl Layout {
    pub fn scroll_max(&self) -> f32 {
        (self.content_height - self.viewport.height()).max(0.0)
    }
}

pub(super) fn compute(view: &SettingsView, width: f32, height: f32) -> Layout {
    let column_width = ((width - MARGIN * 2.0 - GAP) / 2.0).max(120.0);
    let full_width = width - MARGIN * 2.0;
    let mut cards = Vec::new();
    let mut items = Vec::new();
    let mut left_y = HEADER_HEIGHT;
    let mut right_y = HEADER_HEIGHT;
    let mut bottom_y = HEADER_HEIGHT;
    let mut stacked = false;
    for (group_index, group) in view.groups.iter().enumerate() {
        let columns = group.columns.clamp(1, 2) as usize;
        let rows = group.items.len().div_ceil(columns).max(1);
        let card_height = CARD_PADDING * 2.0 + CARD_TITLE + rows as f32 * ITEM_HEIGHT;
        let (left, top, card_width) = match group_index {
            0 | 1 => {
                let top = left_y;
                left_y += card_height + GAP;
                (MARGIN, top, column_width)
            }
            2 | 3 => {
                let top = right_y;
                right_y += card_height + GAP;
                (MARGIN + column_width + GAP, top, column_width)
            }
            _ => {
                if !stacked {
                    bottom_y = left_y.max(right_y);
                    stacked = true;
                }
                let top = bottom_y;
                bottom_y += card_height + GAP;
                (MARGIN, top, full_width)
            }
        };
        let card = UiRect {
            left,
            top,
            right: left + card_width,
            bottom: top + card_height,
        };
        cards.push(CardLayout {
            group: group_index,
            rect: card,
        });
        push_items(&mut items, group_index, group, card, columns);
    }
    let content_height = if stacked {
        bottom_y
    } else {
        left_y.max(right_y)
    } - GAP;
    Layout {
        width,
        height,
        logo: UiRect {
            left: MARGIN,
            top: 30.0,
            right: MARGIN + 36.0,
            bottom: 66.0,
        },
        close: UiRect {
            left: width - 56.0,
            top: 28.0,
            right: width - 24.0,
            bottom: 60.0,
        },
        viewport: UiRect {
            left: 0.0,
            top: HEADER_HEIGHT,
            right: width,
            bottom: (height - FOOTER_HEIGHT).max(HEADER_HEIGHT),
        },
        footer: UiRect {
            left: MARGIN,
            top: height - FOOTER_HEIGHT,
            right: width - MARGIN,
            bottom: height,
        },
        cards,
        items,
        content_height: content_height.max(HEADER_HEIGHT),
    }
}

fn push_items(
    target: &mut Vec<ItemLayout>,
    group_index: usize,
    group: &SettingsGroup,
    card: UiRect,
    columns: usize,
) {
    let inner = card.width() - CARD_PADDING * 2.0;
    let cell_width = (inner - ITEM_GAP * (columns.saturating_sub(1)) as f32) / columns as f32;
    for (item_index, _) in group.items.iter().enumerate() {
        let row = item_index / columns;
        let column = item_index % columns;
        let left = card.left + CARD_PADDING + column as f32 * (cell_width + ITEM_GAP);
        let top = card.top + CARD_PADDING + CARD_TITLE + row as f32 * ITEM_HEIGHT;
        target.push(ItemLayout {
            group: group_index,
            item: item_index,
            rect: UiRect {
                left,
                top: top + 2.0,
                right: left + cell_width,
                bottom: top + ITEM_HEIGHT - 2.0,
            },
        });
    }
}

/// `scroll` 是内容向上滚动的像素数。
pub(super) fn hit(layout: &Layout, x: f32, y: f32, scroll: f32) -> Option<usize> {
    if layout.close.contains(x, y) {
        return None;
    }
    if !layout.viewport.contains(x, y) {
        return None;
    }
    let content_y = y + scroll;
    layout
        .items
        .iter()
        .position(|item| item.rect.contains(x, content_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{SettingsGroup, SettingsItem};

    fn view() -> SettingsView {
        let groups = vec![
            SettingsGroup::new(
                "壁纸与素材",
                (0..5)
                    .map(|index| SettingsItem::action(index, format!("动作 {index}")))
                    .collect(),
            ),
            SettingsGroup::new(
                "性能预设",
                (10..14)
                    .map(|index| SettingsItem::radio(index, format!("预设 {index}"), index == 10))
                    .collect(),
            ),
            SettingsGroup::new(
                "外观与播放",
                (20..24)
                    .map(|index| SettingsItem::toggle(index, format!("外观 {index}"), true))
                    .collect(),
            ),
            SettingsGroup::new(
                "自动策略",
                (30..34)
                    .map(|index| SettingsItem::toggle(index, format!("规则 {index}"), false))
                    .collect(),
            ),
            SettingsGroup::new(
                "系统与恢复",
                (40..44)
                    .map(|index| SettingsItem::action(index, format!("系统 {index}")))
                    .collect(),
            )
            .with_columns(2),
        ];
        SettingsView::new("设置", "副标题", groups, "页脚")
    }

    #[test]
    fn five_groups_layout_in_two_columns_and_full_width_bottom() {
        let layout = compute(&view(), WINDOW_WIDTH, DESIRED_HEIGHT);
        assert_eq!(layout.cards.len(), 5);
        assert_eq!(layout.items.len(), 21);
        assert!((layout.cards[0].rect.left - MARGIN).abs() < f32::EPSILON);
        assert!(
            (layout.cards[2].rect.left - (MARGIN + layout.cards[0].rect.width() + GAP)).abs()
                < 0.01
        );
        assert!(
            (layout.cards[4].rect.width() - (WINDOW_WIDTH - MARGIN * 2.0)).abs() < f32::EPSILON
        );
        let first = &layout.items[0].rect;
        let last = &layout.items[20].rect;
        assert!(first.top >= HEADER_HEIGHT);
        assert!(last.bottom <= layout.content_height);
    }

    #[test]
    fn hit_test_respects_scroll_and_viewport() {
        let layout = compute(&view(), WINDOW_WIDTH, DESIRED_HEIGHT);
        let item = layout.items[3].rect;
        assert_eq!(
            hit(
                &layout,
                item.left + 4.0,
                item.top + 4.0,
                layout.viewport.top - layout.items[3].rect.top
            ),
            None
        );
        // 视口内点击加上滚动偏移后命中内容坐标。
        let scroll = 80.0;
        let screen_y = layout.viewport.top + 20.0;
        let content_y = screen_y + scroll;
        let target = layout
            .items
            .iter()
            .position(|entry| entry.rect.contains(entry.rect.left + 4.0, content_y));
        if let Some(index) = target {
            assert_eq!(
                hit(
                    &layout,
                    layout.items[index].rect.left + 4.0,
                    screen_y,
                    scroll
                ),
                Some(index)
            );
        }
        assert!(layout.scroll_max() >= 0.0);
    }
}
