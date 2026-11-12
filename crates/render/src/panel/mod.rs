//! 产品面板：Dock、桌面文件灵动岛与应用库。
//!
//! 渲染层只消费完整视图快照，不依赖 Shell 或文件模型；玻璃材质、命中区域
//! 与键盘/鼠标事件都在本模块内闭环。设置窗与面板共享 `theme`/`surface`。

pub(crate) mod shadow;

mod composition;
mod controls;
pub(crate) mod paint;
mod tooltip;
mod window;

pub use window::PanelWindow;

/// 面板类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Dock,
    Island,
}

impl PanelKind {
    pub fn class_name(self) -> &'static str {
        match self {
            Self::Dock => "DeskShell.Panel.Dock",
            Self::Island => "DeskShell.Panel.Island",
        }
    }
}

/// 面板标题使用的图形语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelIcon {
    Files,
    Apps,
}

/// 灵动岛展开后的页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IslandTab {
    /// 运行中应用（任务栏职责）。
    Running,
    /// 快捷工具。
    Tools,
    /// 桌面文件。
    #[default]
    Files,
}

impl IslandTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行",
            Self::Tools => "工具",
            Self::Files => "文件",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Running => 0,
            Self::Tools => 1,
            Self::Files => 2,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Running),
            1 => Some(Self::Tools),
            2 => Some(Self::Files),
            _ => None,
        }
    }
}

impl PanelIcon {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Files => "\u{E8B7}",
            Self::Apps => "\u{E71D}",
        }
    }
}

/// 预乘 alpha 的 BGRA 图标像素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl IconPixels {
    /// D2D 从裸指针复制像素前必须验证完整缓冲区，不能只判断非空。
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= 4096
            && self.height <= 4096
            && self.width.checked_mul(4).is_some_and(|pitch| {
                (pitch as usize)
                    .checked_mul(self.height as usize)
                    .is_some_and(|length| self.bgra.len() == length)
            })
    }
}

/// 面板中的一个条目：Dock 应用或灵动岛文件行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelItem {
    /// 业务稳定身份（应用 ID 或文件完整路径）。
    pub id: String,
    pub label: String,
    /// 图标；文件行在没有缩略图时为空，由渲染层画分类色块。
    pub icon: Option<IconPixels>,
    /// 分类/状态等次要文本。
    pub detail: String,
    /// Dock 运行指示点。
    pub running: bool,
    /// 文件行分类色（预乘前的 RGB），用于色块。
    pub accent: (u8, u8, u8),
}

/// 灵动岛展开后的操作按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    Open,
    Reveal,
    CopyPath,
    Recycle,
}

impl PanelAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "打开",
            Self::Reveal => "定位",
            Self::CopyPath => "复制路径",
            Self::Recycle => "移入回收站",
        }
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Open => "\u{E8E5}",
            Self::Reveal => "\u{E8DA}",
            Self::CopyPath => "\u{E8C8}",
            Self::Recycle => "\u{E74D}",
        }
    }

    pub(crate) fn primary(self) -> bool {
        self == Self::Open
    }

    pub(crate) fn danger(self) -> bool {
        self == Self::Recycle
    }
}

/// 渲染层接收的完整视图状态；每次更新都是完整快照。
#[derive(Debug, Clone)]
pub struct PanelView {
    pub kind: PanelKind,
    pub icon: PanelIcon,
    pub items: Vec<PanelItem>,
    pub expanded: bool,
    pub selected: Option<usize>,
    /// 展开后的标题（灵动岛/应用库）。
    pub title: String,
    pub summary: String,
    pub status: Option<String>,
    pub actions: Vec<PanelAction>,
    /// 鱼眼动画开关；关闭时条目保持静态尺寸。
    pub fisheye: bool,
    /// 是否显示关闭/收起按钮。
    pub closable: bool,
    pub query: String,
    pub category: String,
    pub offset: usize,
    pub row_limit: usize,
    pub max_width: f32,
    /// Dock 图标样式；仅 Dock 使用。
    pub dock_style: DockStyle,
    /// 灵动岛当前页签。
    pub island_tab: IslandTab,
}

impl PanelView {
    pub fn dock(items: Vec<PanelItem>, fisheye: bool) -> Self {
        Self {
            kind: PanelKind::Dock,
            icon: PanelIcon::Apps,
            items,
            expanded: false,
            selected: None,
            title: "Dock".into(),
            summary: String::new(),
            status: None,
            actions: Vec::new(),
            fisheye,
            closable: false,
            query: String::new(),
            category: String::new(),
            offset: 0,
            row_limit: MAX_ISLAND_ROWS,
            max_width: ISLAND_EXPANDED_WIDTH,
            dock_style: DockStyle::Standard,
            island_tab: IslandTab::Files,
        }
    }

    pub fn island(items: Vec<PanelItem>, summary: String) -> Self {
        Self {
            kind: PanelKind::Island,
            icon: PanelIcon::Files,
            items,
            expanded: false,
            selected: None,
            title: "桌面文件".into(),
            summary,
            status: None,
            actions: Vec::new(),
            fisheye: false,
            closable: true,
            query: String::new(),
            category: String::new(),
            offset: 0,
            row_limit: MAX_ISLAND_ROWS,
            max_width: ISLAND_EXPANDED_WIDTH,
            dock_style: DockStyle::Standard,
            island_tab: IslandTab::Files,
        }
    }

    pub fn visible_rows(&self) -> usize {
        if !self.expanded || self.island_tab == IslandTab::Tools {
            return 0;
        }
        self.filtered_indices()
            .len()
            .clamp(1, self.row_limit.max(1))
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.detail.to_lowercase().contains(&query))
                    && (self.category.is_empty()
                        || item.detail.split('·').next().unwrap_or("").trim() == self.category)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        self.filtered_indices()
            .into_iter()
            .skip(self.offset)
            .take(self.visible_rows())
            .collect()
    }

    /// 分类胶囊：第一项始终是“全部类型”，其余按条目出现顺序去重。
    pub fn categories(&self) -> Vec<String> {
        let mut values: Vec<String> = vec!["全部类型".into()];
        for item in &self.items {
            let category = item.detail.split('·').next().unwrap_or("").trim();
            if !category.is_empty() && !values.iter().any(|value| value == category) {
                values.push(category.to_string());
            }
        }
        values
    }

    pub(crate) fn chips_visible(&self) -> bool {
        self.expanded && self.island_tab == IslandTab::Files && self.categories().len() > 1
    }

    pub(crate) fn content_top(&self) -> f32 {
        ISLAND_HEADER + if self.expanded { ISLAND_TAB_ROW } else { 0.0 }
    }

    pub(crate) fn tab_rects(&self) -> Vec<PanelRect> {
        if !self.expanded || self.kind != PanelKind::Island {
            return Vec::new();
        }
        let (width, _) = panel_size(self);
        let gap = 6.0;
        let cell = (width - ISLAND_PADDING * 2.0 - gap * 2.0) / 3.0;
        let top = ISLAND_HEADER + 4.0;
        (0..3)
            .map(|index| PanelRect {
                left: ISLAND_PADDING + index as f32 * (cell + gap),
                top,
                right: ISLAND_PADDING + index as f32 * (cell + gap) + cell,
                bottom: top + ISLAND_TAB_HEIGHT,
            })
            .collect()
    }

    pub(crate) fn tool_rects(&self) -> Vec<PanelRect> {
        if !self.expanded || self.kind != PanelKind::Island || self.island_tab != IslandTab::Tools {
            return Vec::new();
        }
        let (width, _) = panel_size(self);
        let gap = 8.0;
        let columns = ISLAND_TOOL_COLUMNS;
        let cell = (width - ISLAND_PADDING * 2.0 - gap * (columns - 1) as f32) / columns as f32;
        let top = self.content_top() + ISLAND_PADDING;
        (0..self.items.len().min(ISLAND_TOOL_COUNT))
            .map(|index| {
                let row = index / columns;
                let column = index % columns;
                let left = ISLAND_PADDING + column as f32 * (cell + gap);
                let cell_top = top + row as f32 * (ISLAND_TOOL_HEIGHT + gap);
                PanelRect {
                    left,
                    top: cell_top,
                    right: left + cell,
                    bottom: cell_top + ISLAND_TOOL_HEIGHT,
                }
            })
            .collect()
    }

    pub(crate) fn search_rect(&self) -> PanelRect {
        let (width, _) = panel_size(self);
        PanelRect {
            left: ISLAND_PADDING,
            top: self.content_top() + 8.0,
            right: width - ISLAND_PADDING,
            bottom: self.content_top() + 8.0 + ISLAND_SEARCH_FIELD,
        }
    }

    pub(crate) fn chip_rects(&self) -> Vec<(String, PanelRect)> {
        if !self.chips_visible() {
            return Vec::new();
        }
        let (width, _) = panel_size(self);
        let top = self.content_top() + ISLAND_SEARCH_ROW + 5.0;
        let mut left = ISLAND_PADDING;
        let mut rects = Vec::new();
        for category in self.categories() {
            let chip_width = 26.0 + category.chars().count() as f32 * 13.0;
            if left + chip_width > width - ISLAND_PADDING {
                break;
            }
            rects.push((
                category,
                PanelRect {
                    left,
                    top,
                    right: left + chip_width,
                    bottom: top + ISLAND_CHIP_HEIGHT,
                },
            ));
            left += chip_width + 8.0;
        }
        rects
    }

    pub(crate) fn rows_top(&self) -> f32 {
        match self.island_tab {
            IslandTab::Files => {
                self.content_top()
                    + ISLAND_SEARCH_ROW
                    + if self.chips_visible() {
                        ISLAND_CHIP_ROW
                    } else {
                        0.0
                    }
                    + ISLAND_PADDING
            }
            IslandTab::Running => self.content_top() + ISLAND_PADDING,
            IslandTab::Tools => self.content_top() + ISLAND_PADDING,
        }
    }
}

/// 面板内的命中目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelHit {
    /// 标题/摘要区域：点击切换展开。
    Header,
    Item(usize),
    Action(usize),
    /// 分类胶囊索引；0 表示全部。
    Category(usize),
    /// 灵动岛页签索引：0 运行、1 工具、2 文件。
    Tab(usize),
    /// 搜索框留白（实际输入由原生编辑控件处理）。
    Search,
    Close,
    Empty,
}

/// 面板产生的交互事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEvent {
    ItemActivated {
        id: String,
        index: usize,
    },
    SelectionChanged(Option<usize>),
    Action {
        action: PanelAction,
        id: String,
    },
    ExpandedChanged(bool),
    /// 灵动岛页签切换；索引 0 运行、1 工具、2 文件。
    TabChanged(usize),
    DismissRequested,
    HoverChanged(Option<usize>),
    /// 右键上下文菜单；id 对应条目身份。
    ContextMenu {
        id: String,
        index: usize,
    },
}

/// 面板创建选项。
#[derive(Debug, Clone, Copy)]
pub struct PanelOptions {
    /// 显示器索引；None 使用主显示器。
    pub monitor: Option<usize>,
    /// 创建后是否显示。
    pub visible: bool,
}

impl Default for PanelOptions {
    fn default() -> Self {
        Self {
            monitor: None,
            visible: true,
        }
    }
}

pub const DOCK_HEIGHT: f32 = 80.0;
pub const DOCK_ICON: f32 = 48.0;
pub const DOCK_SLOT: f32 = 66.0;
pub const DOCK_PADDING: f32 = 16.0;
pub const DOCK_MIN_SLOTS: usize = 3;
pub const DOCK_MAX_ITEMS: usize = 16;

/// Dock 图标尺寸/标签样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DockStyle {
    /// 参考图的大图标。
    Large,
    /// 现有标准大小。
    #[default]
    Standard,
    /// 图标下方显示名称。
    Labels,
}

impl DockStyle {
    pub fn icon_size(self) -> f32 {
        match self {
            Self::Large => 56.0,
            Self::Standard => 46.0,
            Self::Labels => 42.0,
        }
    }

    pub fn slot(self) -> f32 {
        match self {
            Self::Large => 70.0,
            Self::Standard => 60.0,
            Self::Labels => 70.0,
        }
    }

    pub fn height(self) -> f32 {
        match self {
            Self::Large => 92.0,
            Self::Standard => 80.0,
            Self::Labels => 92.0,
        }
    }

    pub fn icon_top(self) -> f32 {
        match self {
            Self::Large => 16.0,
            Self::Standard => 14.0,
            Self::Labels => 10.0,
        }
    }

    pub fn shows_labels(self) -> bool {
        self == Self::Labels
    }
}

pub const ISLAND_COLLAPSED_WIDTH: f32 = 260.0;
pub const ISLAND_EXPANDED_WIDTH: f32 = 440.0;
pub const ISLAND_HEADER: f32 = 48.0;
pub const ISLAND_TAB_ROW: f32 = 34.0;
pub const ISLAND_TAB_HEIGHT: f32 = 28.0;
pub const ISLAND_TOOL_HEIGHT: f32 = 64.0;
pub const ISLAND_SEARCH_ROW: f32 = 46.0;
pub const ISLAND_SEARCH_FIELD: f32 = 32.0;
pub const ISLAND_CHIP_ROW: f32 = 34.0;
pub const ISLAND_CHIP_HEIGHT: f32 = 26.0;
pub const ISLAND_ROW_HEIGHT: f32 = 48.0;
pub const ISLAND_ACTION_HEIGHT: f32 = 34.0;
pub const ISLAND_ACTION_BAR: f32 = 54.0;
pub const ISLAND_STATUS_HEIGHT: f32 = 26.0;
pub const ISLAND_PADDING: f32 = 14.0;
pub const ISLAND_TOOL_COLUMNS: usize = 2;
pub const ISLAND_TOOL_COUNT: usize = 4;
pub const MAX_ISLAND_ROWS: usize = 5;

/// Dock 逻辑尺寸（DIP）。
pub fn dock_size(item_count: usize, style: DockStyle) -> (f32, f32) {
    let slots = item_count.clamp(DOCK_MIN_SLOTS, DOCK_MAX_ITEMS);
    (
        DOCK_PADDING * 2.0 + slots as f32 * style.slot(),
        style.height(),
    )
}

/// 灵动岛逻辑尺寸（DIP）。
pub fn island_size(view: &PanelView) -> (f32, f32) {
    if !view.expanded {
        return (ISLAND_COLLAPSED_WIDTH.min(view.max_width), ISLAND_HEADER);
    }
    let status = if view.status.is_some() {
        ISLAND_STATUS_HEIGHT
    } else {
        0.0
    };
    let content = match view.island_tab {
        IslandTab::Files => {
            let rows = view.visible_rows() as f32;
            let actions = if view.actions.is_empty() {
                0.0
            } else {
                ISLAND_ACTION_BAR
            };
            let chips = if view.chips_visible() {
                ISLAND_CHIP_ROW
            } else {
                0.0
            };
            ISLAND_SEARCH_ROW + chips + ISLAND_PADDING + rows * ISLAND_ROW_HEIGHT + actions
        }
        IslandTab::Running => {
            let rows = view.visible_rows() as f32;
            ISLAND_PADDING + rows * ISLAND_ROW_HEIGHT
        }
        IslandTab::Tools => {
            let grid_rows = view
                .items
                .len()
                .min(ISLAND_TOOL_COUNT)
                .div_ceil(ISLAND_TOOL_COLUMNS)
                .max(1) as f32;
            ISLAND_PADDING + grid_rows * ISLAND_TOOL_HEIGHT + (grid_rows - 1.0) * 8.0
        }
    };
    (
        ISLAND_EXPANDED_WIDTH.min(view.max_width),
        ISLAND_HEADER + ISLAND_TAB_ROW + content + status + ISLAND_PADDING,
    )
}

pub fn panel_size(view: &PanelView) -> (f32, f32) {
    match view.kind {
        PanelKind::Dock => dock_size(view.items.len(), view.dock_style),
        PanelKind::Island => island_size(view),
    }
}

/// 绘制和命中共用同一组 DIP 矩形，按钮间隙不属于可点击区域。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl PanelRect {
    fn contains(self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub(crate) fn width(self) -> f32 {
        self.right - self.left
    }
}

pub(crate) fn action_rect(view: &PanelView, index: usize) -> Option<PanelRect> {
    if index >= view.actions.len() || !view.expanded {
        return None;
    }
    let (width, _) = panel_size(view);
    let gap = 8.0;
    let button_width =
        (width - ISLAND_PADDING * 2.0 - gap * view.actions.len().saturating_sub(1) as f32)
            / view.actions.len() as f32;
    let left = ISLAND_PADDING + index as f32 * (button_width + gap);
    let top = view.rows_top() + view.visible_rows() as f32 * ISLAND_ROW_HEIGHT + 12.0;
    Some(PanelRect {
        left,
        top,
        right: left + button_width,
        bottom: top + ISLAND_ACTION_HEIGHT,
    })
}

pub(crate) fn panel_radius(view: &PanelView) -> f32 {
    match view.kind {
        PanelKind::Dock => 26.0,
        PanelKind::Island if view.expanded => 24.0,
        PanelKind::Island => ISLAND_HEADER / 2.0,
    }
}

/// 逻辑坐标命中测试；窗口层负责把客户区像素换算为 DIP。
pub fn hit_test(view: &PanelView, x: f32, y: f32) -> PanelHit {
    let (width, height) = panel_size(view);
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return PanelHit::Empty;
    }
    if view.closable
        && view.expanded
        && x >= width - 44.0
        && x <= width - 8.0
        && (8.0..=ISLAND_HEADER - 8.0).contains(&y)
    {
        return PanelHit::Close;
    }
    match view.kind {
        PanelKind::Dock => {
            let style = view.dock_style;
            for index in 0..view.items.len().min(DOCK_MAX_ITEMS) {
                let left = DOCK_PADDING
                    + index as f32 * style.slot()
                    + (style.slot() - style.icon_size()) / 2.0;
                let top = style.icon_top();
                if x >= left
                    && x <= left + style.icon_size()
                    && y >= top
                    && y <= top + style.icon_size()
                {
                    return PanelHit::Item(index);
                }
            }
            PanelHit::Header
        }
        PanelKind::Island => {
            if !view.expanded {
                return PanelHit::Header;
            }
            if y <= ISLAND_HEADER {
                return PanelHit::Header;
            }
            for (index, rect) in view.tab_rects().into_iter().enumerate() {
                if rect.contains(x, y) {
                    return PanelHit::Tab(index);
                }
            }
            if view.island_tab == IslandTab::Tools {
                for (index, rect) in view.tool_rects().into_iter().enumerate() {
                    if rect.contains(x, y) {
                        return PanelHit::Item(index);
                    }
                }
                return PanelHit::Empty;
            }
            if view.island_tab == IslandTab::Files {
                if view.search_rect().contains(x, y) {
                    return PanelHit::Search;
                }
                for (index, (_, rect)) in view.chip_rects().into_iter().enumerate() {
                    if rect.contains(x, y) {
                        return PanelHit::Category(index);
                    }
                }
            }
            let rows_top = view.rows_top();
            for (row, index) in view.visible_indices().into_iter().enumerate() {
                let top = rows_top + row as f32 * ISLAND_ROW_HEIGHT;
                if x >= ISLAND_PADDING
                    && x < width - ISLAND_PADDING
                    && y >= top + 2.0
                    && y < top + ISLAND_ROW_HEIGHT - 2.0
                {
                    return PanelHit::Item(index);
                }
            }
            if view.island_tab == IslandTab::Files {
                for index in 0..view.actions.len() {
                    if action_rect(view, index).is_some_and(|rect| rect.contains(x, y)) {
                        return PanelHit::Action(index);
                    }
                }
            }
            PanelHit::Empty
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> PanelItem {
        PanelItem {
            id: id.into(),
            label: id.into(),
            icon: None,
            detail: String::new(),
            running: false,
            accent: (120, 160, 220),
        }
    }

    #[test]
    fn dock_hit_test_matches_item_slots() {
        let view = PanelView::dock(vec![item("a"), item("b"), item("c")], true);
        let style = view.dock_style;
        let (width, height) = dock_size(3, style);
        assert_eq!(hit_test(&view, -1.0, height / 2.0), PanelHit::Empty);
        assert_eq!(hit_test(&view, width + 1.0, height / 2.0), PanelHit::Empty);
        for index in 0..3 {
            let center = DOCK_PADDING + index as f32 * style.slot() + style.slot() / 2.0;
            assert_eq!(hit_test(&view, center, height / 2.0), PanelHit::Item(index));
        }
        // 条目之间的沟槽属于背景，不误触相邻应用。
        let gap = DOCK_PADDING + style.slot() - 1.0;
        assert_eq!(hit_test(&view, gap, height / 2.0), PanelHit::Header);
    }

    #[test]
    fn dock_styles_change_size_and_labels() {
        let mut view = PanelView::dock(vec![item("a"), item("b")], true);
        let standard = panel_size(&view);
        view.dock_style = DockStyle::Large;
        let large = panel_size(&view);
        assert!(large.0 > standard.0 && large.1 > standard.1);
        view.dock_style = DockStyle::Labels;
        let labels = panel_size(&view);
        assert!(labels.1 > standard.1);
        assert!(DockStyle::Labels.shows_labels());
        assert!(!DockStyle::Large.shows_labels());
    }

    #[test]
    fn island_expanded_rows_and_actions_hit() {
        let mut view = PanelView::island(
            (0..5).map(|index| item(&format!("file-{index}"))).collect(),
            "5 个文件".into(),
        );
        view.expanded = true;
        view.selected = Some(2);
        view.actions = vec![
            PanelAction::Open,
            PanelAction::Reveal,
            PanelAction::CopyPath,
            PanelAction::Recycle,
        ];
        let (width, height) = island_size(&view);
        assert_eq!(hit_test(&view, 10.0, 10.0), PanelHit::Header);
        assert_eq!(hit_test(&view, width - 24.0, 20.0), PanelHit::Close);
        let row_center = view.rows_top() + 2.0 * ISLAND_ROW_HEIGHT + 10.0;
        assert_eq!(hit_test(&view, width / 2.0, row_center), PanelHit::Item(2));
        let actions_top = view.rows_top() + 5.0 * ISLAND_ROW_HEIGHT + 16.0;
        assert_eq!(
            hit_test(&view, width - 20.0, actions_top),
            PanelHit::Action(3)
        );
        assert_eq!(hit_test(&view, width / 2.0, height - 1.0), PanelHit::Empty);
    }

    #[test]
    fn island_collapsed_clicks_anywhere_expand() {
        let view = PanelView::island(vec![item("a")], "1 个文件".into());
        let (width, height) = island_size(&view);
        assert_eq!(hit_test(&view, width / 2.0, height / 2.0), PanelHit::Header);
    }

    #[test]
    fn island_height_accounts_for_status_actions_and_chips() {
        let mut view = PanelView::island(
            (0..9)
                .map(|index| {
                    let mut value = item(&format!("{index}"));
                    value.detail = "图片 · 8 KB".into();
                    value
                })
                .collect(),
            String::new(),
        );
        view.expanded = true;
        view.actions = vec![PanelAction::Open];
        view.status = Some("失败".into());
        let (_, height) = island_size(&view);
        let base = ISLAND_HEADER
            + ISLAND_TAB_ROW
            + ISLAND_SEARCH_ROW
            + ISLAND_CHIP_ROW
            + ISLAND_PADDING
            + MAX_ISLAND_ROWS as f32 * ISLAND_ROW_HEIGHT
            + ISLAND_ACTION_BAR
            + ISLAND_STATUS_HEIGHT
            + ISLAND_PADDING;
        assert!((height - base).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_rows_and_action_gaps_do_not_activate() {
        let mut view = PanelView::island(Vec::new(), String::new());
        view.expanded = true;
        assert_eq!(
            hit_test(&view, 100.0, view.rows_top() + 10.0),
            PanelHit::Empty
        );
        view.actions = vec![PanelAction::Open, PanelAction::Recycle];
        let first = action_rect(&view, 0).unwrap();
        assert_eq!(
            hit_test(&view, first.left + 4.0, first.top + 4.0),
            PanelHit::Action(0)
        );
        assert_eq!(
            hit_test(&view, first.right + 2.0, first.top + 4.0),
            PanelHit::Empty
        );
        assert_eq!(
            hit_test(&view, first.left + 4.0, first.top - 2.0),
            PanelHit::Empty
        );
    }

    #[test]
    fn category_chips_filter_and_hit() {
        let mut view = PanelView::island(
            (0..4)
                .map(|index| {
                    let mut value = item(&format!("file-{index}"));
                    value.detail = if index % 2 == 0 {
                        "文档 · 2 KB".into()
                    } else {
                        "图片 · 8 KB".into()
                    };
                    value
                })
                .collect(),
            String::new(),
        );
        view.expanded = true;
        assert_eq!(view.categories(), vec!["全部类型", "文档", "图片"]);
        let chips = view.chip_rects();
        assert_eq!(chips.len(), 3);
        let chip = chips[2].1;
        assert_eq!(
            hit_test(&view, chip.left + 4.0, chip.top + 4.0),
            PanelHit::Category(2)
        );
        view.category = "图片".into();
        assert_eq!(view.filtered_indices(), vec![1, 3]);
        assert_eq!(
            hit_test(
                &view,
                ISLAND_PADDING + 4.0,
                ISLAND_HEADER + ISLAND_TAB_ROW + 12.0
            ),
            PanelHit::Search
        );
    }

    #[test]
    fn icon_pixels_require_complete_bounded_bgra_buffer() {
        let mut icon = IconPixels {
            width: 2,
            height: 2,
            bgra: vec![0; 16],
        };
        assert!(icon.is_valid());
        icon.bgra.pop();
        assert!(!icon.is_valid());
        icon.width = u32::MAX;
        assert!(!icon.is_valid());
        icon.width = 0;
        assert!(!icon.is_valid());
    }

    #[test]
    fn browsing_preserves_original_identity_after_search_filter_and_scroll() {
        let mut view = PanelView::island(
            (0..24)
                .map(|index| {
                    let mut value = item(&format!("文件 {index}"));
                    value.detail = if index % 2 == 0 {
                        "文档 · 2 KB".into()
                    } else {
                        "图片 · 8 KB".into()
                    };
                    value
                })
                .collect(),
            String::new(),
        );
        view.expanded = true;
        view.category = "图片".into();
        view.offset = 3;
        assert_eq!(view.visible_indices()[0], 7);
        assert_eq!(
            hit_test(&view, 100.0, view.rows_top() + 10.0),
            PanelHit::Item(7)
        );
        view.query = "文件 2".into();
        view.offset = 0;
        assert_eq!(view.filtered_indices(), vec![21, 23]);
        view.query = "不存在".into();
        assert!(view.visible_indices().is_empty());
    }
}
