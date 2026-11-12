//! 自绘设置窗：与 Dock/灵动岛共享玻璃材质、主题与合成路径。
//!
//! 设置项只描述标签、命令与状态，业务命令仍由主程序执行；窗口不复制策略逻辑。

mod draw;
mod layout;
mod window;

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

pub use window::SettingsWindow;

/// 设置项交互类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItemKind {
    /// 一次性动作，例如打开文件选择器。
    Action,
    /// 开关；`checked` 表示当前状态。
    Toggle,
    /// 互斥选项，例如画质预设。
    Radio,
}

/// 一个设置项。
#[derive(Debug, Clone)]
pub struct SettingsItem {
    pub command: u32,
    pub label: String,
    pub note: Option<String>,
    pub kind: SettingsItemKind,
    pub checked: bool,
    pub enabled: bool,
}

impl SettingsItem {
    pub fn action(command: u32, label: impl Into<String>) -> Self {
        Self {
            command,
            label: label.into(),
            note: None,
            kind: SettingsItemKind::Action,
            checked: false,
            enabled: true,
        }
    }

    pub fn toggle(command: u32, label: impl Into<String>, checked: bool) -> Self {
        Self {
            command,
            label: label.into(),
            note: None,
            kind: SettingsItemKind::Toggle,
            checked,
            enabled: true,
        }
    }

    pub fn radio(command: u32, label: impl Into<String>, checked: bool) -> Self {
        Self {
            command,
            label: label.into(),
            note: None,
            kind: SettingsItemKind::Radio,
            checked,
            enabled: true,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// 一组设置卡片。
#[derive(Debug, Clone)]
pub struct SettingsGroup {
    pub title: String,
    pub items: Vec<SettingsItem>,
    /// 卡片内列数；宽卡片可放两列。
    pub columns: u8,
}

impl SettingsGroup {
    pub fn new(title: impl Into<String>, items: Vec<SettingsItem>) -> Self {
        Self {
            title: title.into(),
            items,
            columns: 1,
        }
    }

    pub fn with_columns(mut self, columns: u8) -> Self {
        self.columns = columns.clamp(1, 2);
        self
    }
}

/// 设置窗的完整快照。
#[derive(Debug, Clone)]
pub struct SettingsView {
    pub title: String,
    pub subtitle: String,
    pub groups: Vec<SettingsGroup>,
    pub footer: String,
}

impl SettingsView {
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        groups: Vec<SettingsGroup>,
        footer: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            groups,
            footer: footer.into(),
        }
    }
}

/// 设置窗产生的交互事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEvent {
    /// 请求执行主程序命令（与托盘菜单 ID 一致）。
    Command(u32),
    /// 关闭设置窗；由主程序决定窗口生命周期。
    Dismissed,
}

/// 绘制与命中共用的矩形。
#[derive(Debug, Clone, Copy)]
pub(crate) struct UiRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl UiRect {
    pub(crate) fn contains(self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub(crate) fn width(self) -> f32 {
        self.right - self.left
    }

    pub(crate) fn height(self) -> f32 {
        self.bottom - self.top
    }

    pub(crate) fn to_d2d(self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    pub(crate) fn offset(self, x: f32, y: f32) -> Self {
        Self {
            left: self.left + x,
            top: self.top + y,
            right: self.right + x,
            bottom: self.bottom + y,
        }
    }
}
