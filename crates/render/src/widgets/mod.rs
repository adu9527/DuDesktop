//! 桌面小部件：桌面收纳、时钟、天气、音乐。
//!
//! 渲染层只消费完整视图快照；每个小部件一个无边框玻璃窗口，位置/宽度由主程序
//! 配置驱动。拖动、右边缘调宽、条目点击与媒体按钮都返回事件，业务由主程序执行。

mod draw;
mod layout;
mod window;

pub use window::WidgetWindow;

use crate::IconPixels;

/// 小部件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    Organizer,
    Clock,
    Weather,
    Music,
    Controls,
}

impl WidgetKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Organizer => "桌面收纳",
            Self::Clock => "时钟",
            Self::Weather => "天气",
            Self::Music => "音乐",
            Self::Controls => "快捷控制",
        }
    }
}

/// 创建窗口所需的几何参数。
#[derive(Debug, Clone, Copy)]
pub struct WidgetSpec {
    pub kind: WidgetKind,
    /// 物理屏幕坐标。
    pub x: i32,
    pub y: i32,
    /// 逻辑宽度（DIP）。
    pub width: f32,
}

/// 收纳网格中的一个快捷方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetItem {
    pub id: String,
    pub label: String,
    pub icon: Option<IconPixels>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherView {
    pub city: String,
    pub temperature: String,
    pub text: String,
    /// Open-Meteo 天气代码。
    pub code: u8,
}

impl Default for WeatherView {
    fn default() -> Self {
        Self {
            city: "天气".into(),
            temperature: "--°".into(),
            text: "正在获取…".into(),
            code: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicView {
    pub title: String,
    pub artist: String,
    pub source: String,
    pub playing: bool,
}

impl Default for MusicView {
    fn default() -> Self {
        Self {
            title: "未检测到媒体会话".into(),
            artist: "打开 QQ 音乐或其他播放器".into(),
            source: String::new(),
            playing: false,
        }
    }
}

/// 快捷控制：系统音量、WLAN 与蓝牙状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlsView {
    pub volume: u8,
    pub muted: bool,
    pub device: String,
    /// 已连接的 WLAN 名称；未连接时为空。
    pub wifi: String,
    pub wifi_on: bool,
    /// None 表示设备不存在或不支持读取。
    pub bluetooth: Option<bool>,
}

impl Default for ControlsView {
    fn default() -> Self {
        Self {
            volume: 50,
            muted: false,
            device: "系统默认输出".into(),
            wifi: String::new(),
            wifi_on: false,
            bluetooth: Some(false),
        }
    }
}

/// 一个窗口的完整视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetView {
    pub kind: WidgetKind,
    pub organizer: Vec<WidgetItem>,
    pub columns: usize,
    pub clock: String,
    pub date: String,
    pub weather: WeatherView,
    pub music: MusicView,
    pub controls: ControlsView,
}

impl WidgetView {
    pub fn organizer(items: Vec<WidgetItem>, columns: usize) -> Self {
        Self {
            kind: WidgetKind::Organizer,
            organizer: items,
            columns: columns.clamp(2, 6),
            clock: String::new(),
            date: String::new(),
            weather: WeatherView::default(),
            music: MusicView::default(),
            controls: ControlsView::default(),
        }
    }

    pub fn clock(clock: String, date: String) -> Self {
        Self {
            kind: WidgetKind::Clock,
            organizer: Vec::new(),
            columns: 3,
            clock,
            date,
            weather: WeatherView::default(),
            music: MusicView::default(),
            controls: ControlsView::default(),
        }
    }

    pub fn weather(weather: WeatherView) -> Self {
        Self {
            kind: WidgetKind::Weather,
            organizer: Vec::new(),
            columns: 3,
            clock: String::new(),
            date: String::new(),
            weather,
            music: MusicView::default(),
            controls: ControlsView::default(),
        }
    }

    pub fn music(music: MusicView) -> Self {
        Self {
            kind: WidgetKind::Music,
            organizer: Vec::new(),
            columns: 3,
            clock: String::new(),
            date: String::new(),
            weather: WeatherView::default(),
            music,
            controls: ControlsView::default(),
        }
    }

    pub fn controls(controls: ControlsView) -> Self {
        Self {
            kind: WidgetKind::Controls,
            organizer: Vec::new(),
            columns: 3,
            clock: String::new(),
            date: String::new(),
            weather: WeatherView::default(),
            music: MusicView::default(),
            controls,
        }
    }
}

/// 小部件产生的交互事件。
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetEvent {
    Moved { x: i32, y: i32 },
    Resized { width: f32 },
    OpenItem(String),
    ItemMenu(String),
    ToggleMusic,
    NextMusic,
    PreviousMusic,
    VolumeChanged(u8),
    ToggleMute,
    Wifi,
    Bluetooth,
    OpenSettings,
    OpenDisplay,
    OpenSound,
    OpenNetwork,
    Dismissed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_carry_kind() {
        assert_eq!(
            WidgetView::clock("12:00".into(), "今天".into()).kind,
            WidgetKind::Clock
        );
        assert_eq!(
            WidgetView::organizer(Vec::new(), 9).columns,
            6,
            "列数必须收敛到 2..=6"
        );
    }
}
