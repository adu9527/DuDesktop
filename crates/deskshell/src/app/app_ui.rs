//! 主程序 UI 状态：Dock/灵动岛/应用库视图、托盘菜单与自检。

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use render::{
    DockStyle, IconPixels, IslandTab, PanelAction, PanelEvent, PanelIcon, PanelItem, PanelView,
};
use shell_win::{
    apps::AppEntry,
    deskfiles::{self, DesktopFile, FileCategory},
    shellui::MenuEntry,
};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    UI::WindowsAndMessaging::{
        GetClassNameW, GetWindowRect, KillTimer, PostQuitMessage, SetTimer, WindowFromPoint,
    },
};

use crate::{
    app::{
        AppState, RUNNING_FALLBACK_MS, TIMER_RUNNING_EVENT, TIMER_RUNNING_FALLBACK, WM_APP_NOTICE,
        WM_APP_RUNNING, WM_APP_RUNNING_ICONS, fit_label, launch_entry, yes_no,
    },
    policy::{EffectSettings, QualityPreset},
    shellstate::ShellStateJournal,
};

const CMD_TOGGLE_DOCK: u32 = 1;
const CMD_SHOW_ISLAND: u32 = 2;
const CMD_SHOW_LIBRARY: u32 = 3;
const CMD_TOGGLE_WALLPAPER: u32 = 4;
const CMD_RESTORE_DESKTOP: u32 = 5;
const CMD_RESTORE_ICONS: u32 = 6;
const CMD_EXIT: u32 = 7;
const CMD_PIN: u32 = 20;
const CMD_UNPIN: u32 = 21;
const CMD_MOVE_LEFT: u32 = 22;
const CMD_MOVE_RIGHT: u32 = 23;
const CMD_RUNNING_ACTIVATE: u32 = 30;
const CMD_RUNNING_CLOSE: u32 = 31;
const CMD_RUNNING_PIN: u32 = 32;
const CMD_ORGANIZER_APP: u32 = 33;

const CMD_PRESET_ECO: u32 = 100;
const CMD_PRESET_BALANCED: u32 = 101;
const CMD_PRESET_ULTRA: u32 = 102;
const CMD_PRESET_CUSTOM: u32 = 103;
const CMD_EFFECT_UI: u32 = 110;
const CMD_EFFECT_SHADOWS: u32 = 111;
const CMD_EFFECT_PLAYBACK: u32 = 112;
const CMD_AUTO_FULLSCREEN: u32 = 120;
const CMD_AUTO_OBSCURED: u32 = 121;
const CMD_AUTO_BATTERY: u32 = 122;
const CMD_AUTO_REMOTE: u32 = 123;
const CMD_AUTO_ADD_APP: u32 = 124;
const CMD_AUTO_CLEAR_APPS: u32 = 125;
const CMD_DISPLAY_BASE: u32 = 200;
const CMD_PICK_IMAGE: u32 = 300;
const CMD_PICK_VIDEO: u32 = 301;
const CMD_PICK_POSTER: u32 = 302;
const CMD_CLEAR_SOURCE: u32 = 303;
const CMD_STATUS: u32 = 400;
const CMD_AUTOSTART: u32 = 401;
const CMD_APPLY_WALLPAPER: u32 = 402;
/// 壁纸缩放方式。
const CMD_FIT_COVER: u32 = 410;
const CMD_FIT_CONTAIN: u32 = 411;
const CMD_FIT_STRETCH: u32 = 412;
const CMD_FIT_TILE: u32 = 413;
/// Dock 运行项与样式。
const CMD_DOCK_RUNNING: u32 = 420;
const CMD_DOCK_SYSTEM: u32 = 421;
const CMD_DOCK_STYLE_LARGE: u32 = 422;
const CMD_DOCK_STYLE_STANDARD: u32 = 423;
const CMD_DOCK_STYLE_LABELS: u32 = 424;
const CMD_DOCK_MAX_BASE: u32 = 430;
const CMD_DOCK_RUNNING_LIMIT_BASE: u32 = 440;
const CMD_WIDGET_ORGANIZER: u32 = 450;
const CMD_WIDGET_CLOCK: u32 = 451;
const CMD_WIDGET_WEATHER: u32 = 452;
const CMD_WIDGET_MUSIC: u32 = 453;
const CMD_WIDGET_ADD_FOLDER: u32 = 454;
const CMD_WIDGET_ADD_FILE: u32 = 455;
const CMD_WIDGET_CONTROLS: u32 = 456;
const DOCK_MAX_CHOICES: [usize; 5] = [6, 8, 10, 12, 16];
const RUNNING_LIMIT_CHOICES: [usize; 4] = [3, 4, 6, 8];

/// 图片效果菜单命令：每个效果预留 8 个命令，低/中/高使用 1..=3。
const CMD_IMAGE_EFFECT_BASE: u32 = 500;
const IMAGE_EFFECT_COMMAND_STRIDE: u32 = 8;

#[derive(Clone, Copy)]
enum EffectKind {
    UiAnimations,
    Shadows,
    WallpaperPlayback,
}

impl EffectKind {
    fn label(self) -> &'static str {
        match self {
            Self::UiAnimations => "UI 动画",
            Self::Shadows => "面板阴影",
            Self::WallpaperPlayback => "壁纸播放",
        }
    }

    fn get(self, settings: &EffectSettings) -> bool {
        match self {
            Self::UiAnimations => settings.ui_animations,
            Self::Shadows => settings.shadows,
            Self::WallpaperPlayback => settings.wallpaper_playback,
        }
    }
}

#[derive(Clone, Copy)]
enum AutoRule {
    Fullscreen,
    Obscured,
    Battery,
    RemoteSession,
}

/// 可独立调节的图片效果。
#[derive(Clone, Copy)]
enum ImageEffect {
    Color,
    Vignette,
    Blur,
    Bloom,
    Particles,
    Gradient,
}

impl ImageEffect {
    const ALL: [Self; 6] = [
        Self::Color,
        Self::Vignette,
        Self::Blur,
        Self::Bloom,
        Self::Particles,
        Self::Gradient,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Color => "调色",
            Self::Vignette => "暗角",
            Self::Blur => "模糊",
            Self::Bloom => "Bloom",
            Self::Particles => "粒子（动画）",
            Self::Gradient => "渐变",
        }
    }

    fn strength(self, effects: &protocol::ImageEffects) -> u8 {
        match self {
            Self::Color => effects.color,
            Self::Vignette => effects.vignette,
            Self::Blur => effects.blur,
            Self::Bloom => effects.bloom,
            Self::Particles => effects.particles,
            Self::Gradient => effects.gradient,
        }
    }

    fn set(self, effects: &mut protocol::ImageEffects, value: u8) {
        match self {
            Self::Color => effects.color = value,
            Self::Vignette => effects.vignette = value,
            Self::Blur => effects.blur = value,
            Self::Bloom => effects.bloom = value,
            Self::Particles => effects.particles = value,
            Self::Gradient => effects.gradient = value,
        }
    }
}

/// 菜单位置到强度：关闭 0，低 25，中 50，高 75。
fn effect_strength(level: u32) -> u8 {
    match level {
        0 => 0,
        1 => 25,
        2 => 50,
        _ => 75,
    }
}

impl AutoRule {
    fn label(self) -> &'static str {
        match self {
            Self::Fullscreen => "全屏",
            Self::Obscured => "遮挡",
            Self::Battery => "电池/节能",
            Self::RemoteSession => "远程会话",
        }
    }
}

impl AppState {
    pub(super) fn load_browser_icons(&mut self) {
        if self.icons_loading {
            return;
        }
        let panel = if self.library.is_visible() {
            &self.library
        } else {
            &self.island
        };
        if !panel.is_visible() || !panel.view().expanded {
            return;
        }
        let app = self.library.is_visible();
        let wanted: Vec<String> = panel
            .view()
            .visible_indices()
            .into_iter()
            .filter_map(|index| panel.view().items.get(index).map(|item| item.id.clone()))
            .collect();
        let mut requests = Vec::new();
        for id in wanted {
            if (if app {
                self.app_icons.contains_key(&id)
            } else {
                self.file_icons.contains_key(&id)
            }) || !self.icons_attempted.insert(id.clone())
            {
                continue;
            }
            let path = if app {
                // 使用 Shell 解析名提取，支持 AUMID 项；不在 UI 线程访问 Shell。
                self.apps
                    .iter()
                    .find(|entry| entry.id == id)
                    .cloned()
                    .map(|entry| (Some(entry), None))
            } else {
                self.files
                    .iter()
                    .find(|file| file.id == id && !file.cloud_placeholder)
                    .map(|file| (None, Some(file.path.clone())))
            };
            if let Some(path) = path {
                requests.push((id, path));
            }
        }
        if requests.is_empty() {
            return;
        }
        self.icons_loading = true;
        let store = Arc::clone(&self.icon_batch);
        let notify = self.main_hwnd.0 as isize;
        let spawn = thread::Builder::new()
            .name("browser-icons".into())
            .spawn(move || {
                let mut result = Vec::new();
                for (id, (entry, path)) in requests {
                    let icon = if let Some(entry) = entry {
                        crate::app_workers::app_icon_with_timeout(entry, 72)
                    } else if let Some(path) = path {
                        crate::app_workers::icon_with_timeout(path, 32, false)
                    } else {
                        continue;
                    };
                    if let Ok(icon) = icon {
                        result.push((
                            app,
                            id,
                            render::IconPixels {
                                width: icon.width,
                                height: icon.height,
                                bgra: icon.bgra,
                            },
                        ));
                    }
                }
                *store.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                shell_win::shellui::post_message_raw(notify, 0x8061);
            });
        if spawn.is_err() {
            self.icons_loading = false;
        }
    }

    pub fn refresh_dock(&mut self) {
        let animations = self
            .effective_policy
            .as_ref()
            .map(|policy| policy.settings.ui_animations)
            .unwrap_or(true);
        let mut view = PanelView::dock(self.dock_items(), self.config.dock.fisheye && animations);
        view.dock_style = dock_style(self.config.dock.style);
        if let Err(error) = self.dock.set_view(view) {
            eprintln!("更新 Dock 失败：{error}");
        }
    }

    pub fn refresh_island(&mut self) {
        let view = self.island_view();
        if let Err(error) = self.island.set_view(view) {
            eprintln!("更新灵动岛失败：{error}");
        }
    }

    /// 重新枚举运行中应用；由窗口事件防抖或低频回退触发。
    pub fn refresh_running_apps(&mut self) {
        if !self.config.dock.show_running {
            if !self.running_apps.is_empty() {
                self.running_apps.clear();
                self.refresh_dock();
            }
            return;
        }
        self.running_apps = shell_win::running::enumerate_running_apps();
        self.refresh_dock();
        if self.island_tab == IslandTab::Running {
            self.refresh_island();
        }
        self.load_running_icons();
    }

    /// 只在需要运行项时安装窗口事件钩子；失败时用 5 秒低频回退。
    pub fn sync_running_watch(&mut self) {
        if !self.config.dock.show_running {
            self.running_watch = None;
            self.running_fallback = false;
            // SAFETY: 只操作主窗口定时器。
            unsafe {
                let _ = KillTimer(Some(self.main_hwnd), TIMER_RUNNING_FALLBACK);
                let _ = KillTimer(Some(self.main_hwnd), TIMER_RUNNING_EVENT);
            }
            return;
        }
        if self.running_watch.is_some() || self.running_fallback {
            return;
        }
        match shell_win::running::RunningWatch::create(self.main_hwnd, WM_APP_RUNNING) {
            Ok(watch) => self.running_watch = Some(watch),
            Err(error) => {
                eprintln!("{error}");
                self.running_fallback = true;
                // SAFETY: 只操作主窗口定时器。
                unsafe {
                    let _ = SetTimer(
                        Some(self.main_hwnd),
                        TIMER_RUNNING_FALLBACK,
                        RUNNING_FALLBACK_MS,
                        None,
                    );
                }
            }
        }
    }

    fn load_running_icons(&mut self) {
        let mut requests = Vec::new();
        for app in &self.running_apps {
            let id = app.id();
            if self.running_icons.contains_key(&id)
                || !self.running_icons_attempted.insert(id.clone())
            {
                continue;
            }
            requests.push((id, PathBuf::from(&app.process_path)));
        }
        if requests.is_empty() {
            return;
        }
        let store = Arc::clone(&self.running_icon_batch);
        let notify = self.main_hwnd.0 as isize;
        let _ = thread::Builder::new()
            .name("running-icons".into())
            .spawn(move || {
                let mut batch = Vec::new();
                for (id, path) in requests {
                    if let Ok(icon) = crate::app_workers::icon_with_timeout(path, 96, false) {
                        batch.push((
                            id,
                            IconPixels {
                                width: icon.width,
                                height: icon.height,
                                bgra: icon.bgra,
                            },
                        ));
                    }
                }
                *store.lock().unwrap_or_else(|error| error.into_inner()) = Some(batch);
                shell_win::shellui::post_message_raw(notify, WM_APP_RUNNING_ICONS);
            });
    }

    pub fn refresh_library(&mut self) {
        let view = self.library_view();
        if let Err(error) = self.library.set_view(view) {
            eprintln!("更新应用库失败：{error}");
        }
    }

    fn dock_items(&self) -> Vec<PanelItem> {
        let mut items = Vec::new();
        let mut pinned_names = std::collections::HashSet::new();
        for id in &self.config.dock.pinned {
            if let Some(entry) = self.apps.iter().find(|entry| &entry.id == id) {
                pinned_names.insert(entry.name.to_ascii_lowercase());
                items.push(app_item(entry, &self.app_icons));
            }
        }
        if self.config.dock.show_running {
            let mut added = 0usize;
            let limit = self.config.dock.running_limit.clamp(1, 12);
            for app in &self.running_apps {
                if added >= limit {
                    break;
                }
                if pinned_names.contains(&app.name.to_ascii_lowercase()) {
                    continue;
                }
                let id = app.id();
                items.push(PanelItem {
                    icon: self.running_icons.get(&id).cloned(),
                    id,
                    label: app.name.clone(),
                    detail: app.title.clone(),
                    running: true,
                    accent: (120, 170, 220),
                });
                added += 1;
            }
        }
        if self.config.dock.show_system {
            // 系统入口固定在右侧；先截断应用区，保证设置入口不会被挤掉。
            let max_items = self.config.dock.max_items.clamp(5, 16);
            items.truncate(max_items.saturating_sub(4).max(1));
            items.push(special_item("special:library", "应用库", (96, 170, 235)));
            items.push(special_item(
                "special:explorer",
                "资源管理器",
                (240, 190, 100),
            ));
            items.push(special_item("special:recycle", "回收站", (150, 170, 190)));
            items.push(special_item("special:settings", "设置", (150, 160, 190)));
        } else {
            let max_items = self.config.dock.max_items.clamp(5, 16);
            items.truncate(max_items);
        }
        items
    }

    fn island_view(&self) -> PanelView {
        let mut view = match self.island_tab {
            IslandTab::Running => self.running_island_view(),
            IslandTab::Tools => self.tools_island_view(),
            IslandTab::Files => self.files_island_view(),
        };
        view.island_tab = self.island_tab;
        view
    }

    fn running_island_view(&self) -> PanelView {
        let items: Vec<PanelItem> = self
            .running_apps
            .iter()
            .map(|app| {
                let id = app.id();
                PanelItem {
                    icon: self.running_icons.get(&id).cloned(),
                    id,
                    label: app.name.clone(),
                    detail: app.title.clone(),
                    running: true,
                    accent: (120, 170, 220),
                }
            })
            .collect();
        let mut view =
            PanelView::island(items, format!("运行 · {} 个应用", self.running_apps.len()));
        view.title = "运行中的应用".into();
        view.icon = PanelIcon::Apps;
        view.expanded = self.island_expanded;
        view.status = Some("点击切换到窗口 · 右键可关闭".into());
        view
    }

    fn tools_island_view(&self) -> PanelView {
        let items: Vec<PanelItem> = [
            ("tool:library", "应用库"),
            ("tool:settings", "设置"),
            ("tool:explorer", "资源管理器"),
            ("tool:taskmgr", "任务管理器"),
        ]
        .into_iter()
        .map(|(id, label)| PanelItem {
            id: id.into(),
            label: label.into(),
            icon: None,
            detail: String::new(),
            running: false,
            accent: (120, 150, 220),
        })
        .collect();
        let mut view = PanelView::island(items, "快捷工具 · 4 个".into());
        view.title = "快捷工具".into();
        view.icon = PanelIcon::Apps;
        view.expanded = self.island_expanded;
        view
    }

    fn files_island_view(&self) -> PanelView {
        let items: Vec<PanelItem> = self
            .files
            .iter()
            .map(|file| file_item(file, &self.file_icons))
            .collect();
        let summary = if !self.files_ready {
            "正在加载桌面文件……".to_string()
        } else if self.files.is_empty() {
            "桌面暂无文件".to_string()
        } else {
            let categories: BTreeSet<&str> = self
                .files
                .iter()
                .map(|file| file.category.label())
                .collect();
            format!("桌面 · {} 项 · {} 类", self.files.len(), categories.len())
        };
        let mut view = PanelView::island(items, summary);
        view.title = "桌面文件".into();
        view.expanded = self.island_expanded;
        view.selected = self
            .island_selected
            .as_ref()
            .and_then(|id| self.files.iter().position(|file| &file.id == id));
        view.actions = vec![
            PanelAction::Open,
            PanelAction::Reveal,
            PanelAction::CopyPath,
            PanelAction::Recycle,
        ];
        view.status = self.island_status.clone().or_else(|| {
            self.file_warnings
                .first()
                .map(|warning| format!("索引警告：{warning}"))
        });
        view
    }

    fn library_view(&self) -> PanelView {
        let items: Vec<PanelItem> = self
            .apps
            .iter()
            .map(|entry| app_item(entry, &self.app_icons))
            .collect();
        let mut view = PanelView::island(items, format!("应用库 · {} 项", self.apps.len()));
        view.icon = PanelIcon::Apps;
        view.title = "应用库".into();
        view.expanded = true;
        view.status = Some("滚轮浏览 · F6 进入列表 · Enter 启动 · Esc 关闭".into());
        view
    }

    pub fn handle_dock_events(&mut self, events: Vec<PanelEvent>) {
        for event in events {
            match event {
                PanelEvent::ItemActivated { id, .. } => self.activate_dock_item(&id),
                PanelEvent::ContextMenu { id, .. } if id.starts_with("running:") => {
                    self.show_running_context_menu(&id);
                }
                PanelEvent::ContextMenu { id, .. } if !id.starts_with("special:") => {
                    self.show_app_context_menu(&id);
                }
                PanelEvent::DismissRequested => {
                    self.dock.hide();
                    self.dock_visible = false;
                }
                _ => {}
            }
        }
    }

    pub fn handle_island_events(&mut self, events: Vec<PanelEvent>) {
        for event in events {
            match event {
                PanelEvent::ItemActivated { id, .. } => {
                    if id.starts_with("running:") {
                        self.activate_running_app(&id);
                    } else if id.starts_with("tool:") {
                        self.run_tool(&id);
                    } else {
                        self.island_selected = Some(id.clone());
                        self.open_file(&id);
                    }
                }
                PanelEvent::TabChanged(index) => {
                    if let Some(tab) = IslandTab::from_index(index) {
                        self.island_tab = tab;
                        self.island_selected = None;
                        self.refresh_island();
                    }
                }
                PanelEvent::SelectionChanged(Some(index)) => {
                    self.island_selected = self.files.get(index).map(|file| file.id.clone());
                }
                PanelEvent::Action { action, id } => self.file_action(action, &id),
                PanelEvent::ExpandedChanged(expanded) => {
                    self.island_expanded = expanded;
                    if !expanded {
                        self.island.release_elevation();
                    }
                }
                PanelEvent::DismissRequested => {
                    self.island_expanded = false;
                    self.island.hide();
                    self.island_visible = false;
                }
                _ => {}
            }
        }
    }

    fn activate_running_app(&mut self, id: &str) {
        let target = self
            .running_apps
            .iter()
            .find(|app| app.id() == id)
            .map(|app| app.hwnd);
        if let Some(hwnd) = target
            && let Err(error) = shell_win::running::activate(hwnd)
        {
            self.push_notice(format!("切换到应用失败：{error}"));
        }
    }

    fn run_tool(&mut self, id: &str) {
        match id {
            "tool:library" => self.show_library(),
            "tool:settings" => self.show_settings(),
            "tool:explorer" => spawn_path_action(
                PathBuf::from(r"C:\Windows\explorer.exe"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            "tool:taskmgr" => spawn_path_action(
                PathBuf::from(r"C:\Windows\System32\Taskmgr.exe"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            "tool:sound" => spawn_path_action(
                PathBuf::from("ms-settings:sound"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            "tool:bluetooth" => spawn_path_action(
                PathBuf::from("ms-settings:bluetooth"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            _ => {}
        }
    }

    pub fn handle_library_events(&mut self, events: Vec<PanelEvent>) {
        for event in events {
            match event {
                PanelEvent::ItemActivated { id, .. } => {
                    self.launch_app(&id);
                    self.close_library();
                }
                PanelEvent::ContextMenu { id, .. } => self.show_app_context_menu(&id),
                PanelEvent::DismissRequested => self.close_library(),
                _ => {}
            }
        }
    }

    fn activate_dock_item(&mut self, id: &str) {
        if id.starts_with("running:") {
            let target = self
                .running_apps
                .iter()
                .find(|app| app.id() == id)
                .map(|app| app.hwnd);
            if let Some(hwnd) = target
                && let Err(error) = shell_win::running::activate(hwnd)
            {
                self.push_notice(format!("切换到应用失败：{error}"));
            }
            return;
        }
        match id {
            "special:library" => self.show_library(),
            "special:explorer" => spawn_path_action(
                PathBuf::from(r"C:\Windows\explorer.exe"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            "special:recycle" => spawn_path_action(
                PathBuf::from("shell:RecycleBinFolder"),
                PathAction::Open,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            "special:settings" => self.show_settings(),
            _ => self.launch_app(id),
        }
    }

    fn show_library(&mut self) {
        // 应用库与灵动岛共用顶部位置；打开库时隐藏岛，关闭后恢复常驻胶囊。
        self.island_expanded = false;
        self.island.hide();
        self.island_visible = false;
        self.refresh_library();
        if let Err(error) = self.library.summon(false) {
            self.push_notice(format!("显示应用库失败：{error}"));
        }
        self.library.focus_search();
        self.load_browser_icons();
    }

    fn close_library(&mut self) {
        // 灵动岛改为热键呼出的一次性面板；关闭应用库后回到桌面，不主动显示岛。
        self.library.hide();
        self.island_expanded = false;
        self.island_visible = false;
    }

    /// 运行中窗口的右键菜单：切换、关闭、固定。
    fn show_running_context_menu(&mut self, id: &str) {
        let Some(app) = self.running_apps.iter().find(|app| app.id() == id).cloned() else {
            return;
        };
        let pin_entry = self
            .apps
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&app.name))
            .map(|entry| entry.id.clone());
        let mut pin = MenuEntry::item(CMD_RUNNING_PIN, "固定到 Dock");
        pin.enabled = pin_entry
            .as_ref()
            .is_some_and(|id| !self.config.dock.pinned.contains(id));
        let entries = vec![
            MenuEntry::item(CMD_RUNNING_ACTIVATE, "切换到窗口"),
            MenuEntry::item(CMD_RUNNING_CLOSE, "关闭窗口"),
            MenuEntry::separator(),
            pin,
        ];
        let Some(command) = shell_win::shellui::popup_menu(self.main_hwnd, &entries) else {
            return;
        };
        match command {
            CMD_RUNNING_ACTIVATE => {
                if let Err(error) = shell_win::running::activate(app.hwnd) {
                    self.push_notice(format!("切换到应用失败：{error}"));
                }
            }
            CMD_RUNNING_CLOSE => {
                if let Err(error) = shell_win::running::close(app.hwnd) {
                    self.push_notice(error);
                }
            }
            CMD_RUNNING_PIN => {
                if let Some(id) = pin_entry
                    && !self.config.dock.pinned.contains(&id)
                {
                    self.config.dock.pinned.push(id);
                    self.save_config();
                    self.refresh_dock();
                }
            }
            _ => {}
        }
    }

    fn show_app_context_menu(&mut self, id: &str) {
        let Some(entry_name) = self
            .apps
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
        else {
            return;
        };
        let pinned = self.config.dock.pinned.iter().any(|pinned| pinned == id);
        let entries = vec![
            MenuEntry::checked(CMD_PIN, "固定到 Dock", pinned),
            MenuEntry::item(CMD_UNPIN, "从 Dock 移除"),
            MenuEntry::separator(),
            MenuEntry::item(CMD_ORGANIZER_APP, "添加到桌面收纳"),
            MenuEntry::separator(),
            MenuEntry::item(CMD_MOVE_LEFT, "在 Dock 中左移"),
            MenuEntry::item(CMD_MOVE_RIGHT, "在 Dock 中右移"),
        ];
        let Some(command) = shell_win::shellui::popup_menu(self.main_hwnd, &entries) else {
            return;
        };
        match command {
            CMD_PIN if !pinned => {
                if self.config.dock.pinned.len() >= 9 {
                    self.push_notice("Dock 固定项已达上限（9 个）".into());
                    return;
                }
                self.config.dock.pinned.push(id.to_owned());
                self.save_config();
                self.refresh_dock();
                self.push_notice(format!("已固定：{entry_name}"));
            }
            CMD_UNPIN if pinned => {
                self.config.dock.pinned.retain(|pinned| pinned != id);
                self.save_config();
                self.refresh_dock();
                self.push_notice(format!("已从 Dock 移除：{entry_name}"));
            }
            CMD_MOVE_LEFT | CMD_MOVE_RIGHT => {
                if !pinned {
                    return;
                }
                let Some(index) = self
                    .config
                    .dock
                    .pinned
                    .iter()
                    .position(|pinned| pinned == id)
                else {
                    return;
                };
                let target = if command == CMD_MOVE_LEFT {
                    index.checked_sub(1)
                } else if index + 1 < self.config.dock.pinned.len() {
                    Some(index + 1)
                } else {
                    None
                };
                if let Some(target) = target {
                    self.config.dock.pinned.swap(index, target);
                    self.save_config();
                    self.refresh_dock();
                }
            }
            CMD_ORGANIZER_APP => self.add_organizer_app(id),
            _ => {}
        }
    }

    pub(super) fn launch_app(&self, id: &str) {
        let Some(entry) = self.apps.iter().find(|entry| entry.id == id).cloned() else {
            return;
        };
        let notices = Arc::clone(&self.notices);
        let notify = self.main_hwnd.0 as isize;
        let _ = thread::Builder::new()
            .name("app-launch".into())
            .spawn(move || {
                if let Err(error) = launch_entry(&entry) {
                    push_notice(
                        &notices,
                        format!("启动 {} 失败：{error}", entry.name),
                        notify,
                    );
                }
            });
    }

    fn open_file(&mut self, id: &str) {
        let Some(file) = self.files.iter().find(|file| file.id == id).cloned() else {
            self.island_selection_lost();
            return;
        };
        spawn_path_action(
            file.path.clone(),
            PathAction::Open,
            Arc::clone(&self.notices),
            self.main_hwnd.0 as isize,
        );
    }

    fn file_action(&mut self, action: PanelAction, id: &str) {
        let Some(file) = self.files.iter().find(|file| file.id == id).cloned() else {
            self.island_selection_lost();
            return;
        };
        match action {
            PanelAction::Open => self.open_file(&file.id),
            PanelAction::Reveal => spawn_path_action(
                file.path.clone(),
                PathAction::Reveal,
                Arc::clone(&self.notices),
                self.main_hwnd.0 as isize,
            ),
            PanelAction::CopyPath => {
                self.island_status = Some(match deskfiles::copy_path(&file.path) {
                    Ok(()) => format!("已复制路径：{}", file.path.display()),
                    Err(error) => format!("复制路径失败：{error}"),
                });
                self.refresh_island();
            }
            PanelAction::Recycle => {
                if shell_win::shellui::confirm(
                    "移入回收站",
                    &format!("确定将“{}”移入回收站？", file.name),
                    self.island.hwnd(),
                ) {
                    self.island_status = Some(format!("正在移入回收站：{}", file.name));
                    self.refresh_island();
                    spawn_path_action(
                        file.path.clone(),
                        PathAction::Recycle,
                        Arc::clone(&self.notices),
                        self.main_hwnd.0 as isize,
                    );
                }
            }
        }
    }

    fn island_selection_lost(&mut self) {
        self.island_status = Some("条目已不存在，等待桌面刷新".into());
        self.refresh_island();
    }

    pub fn show_tray_menu(&mut self) {
        let entries = self.tray_entries();
        let Some(command) = shell_win::shellui::popup_menu(self.main_hwnd, &entries) else {
            return;
        };
        self.execute_ui_command(command);
    }

    pub(super) fn execute_ui_command(&mut self, command: u32) {
        match command {
            403 => {
                self.show_settings();
            }
            CMD_WIDGET_ORGANIZER => self.toggle_widget(render::WidgetKind::Organizer),
            CMD_WIDGET_CLOCK => self.toggle_widget(render::WidgetKind::Clock),
            CMD_WIDGET_WEATHER => self.toggle_widget(render::WidgetKind::Weather),
            CMD_WIDGET_MUSIC => self.toggle_widget(render::WidgetKind::Music),
            CMD_WIDGET_CONTROLS => self.toggle_widget(render::WidgetKind::Controls),
            CMD_WIDGET_ADD_FOLDER => {
                if let Some(path) =
                    shell_win::shellui::pick_folder("添加到桌面收纳", self.main_hwnd)
                {
                    self.add_organizer_path(&path, true);
                }
            }
            CMD_WIDGET_ADD_FILE => {
                if let Some(path) = shell_win::shellui::pick_file(
                    "添加到桌面收纳",
                    "所有文件",
                    "*.*",
                    self.main_hwnd,
                ) {
                    self.add_organizer_path(&path, false);
                }
            }
            CMD_TOGGLE_DOCK => {
                self.dock_visible = !self.dock_visible;
                if self.dock_visible {
                    self.refresh_dock();
                    self.dock.set_visible(true);
                } else {
                    self.dock.hide();
                }
            }
            CMD_SHOW_ISLAND => {
                self.library.hide();
                self.island_expanded = true;
                self.island_visible = true;
                self.refresh_island();
                if let Err(error) = self.island.summon(true) {
                    self.push_notice(format!("显示灵动岛失败：{error}"));
                }
            }
            CMD_SHOW_LIBRARY => self.show_library(),
            CMD_TOGGLE_WALLPAPER => self.toggle_wallpaper(),
            CMD_APPLY_WALLPAPER => self.apply_wallpaper_now(),
            CMD_FIT_COVER => self.apply_wallpaper_fit(protocol::FitMode::Cover),
            CMD_FIT_CONTAIN => self.apply_wallpaper_fit(protocol::FitMode::Contain),
            CMD_FIT_STRETCH => self.apply_wallpaper_fit(protocol::FitMode::Stretch),
            CMD_FIT_TILE => self.apply_wallpaper_fit(protocol::FitMode::Tile),
            CMD_DOCK_RUNNING => {
                self.config.dock.show_running = !self.config.dock.show_running;
                self.save_config();
                self.sync_running_watch();
                self.refresh_running_apps();
            }
            CMD_DOCK_SYSTEM => {
                self.config.dock.show_system = !self.config.dock.show_system;
                self.save_config();
                self.refresh_dock();
            }
            CMD_DOCK_STYLE_LARGE => self.apply_dock_style(crate::config::DockStyle::Large),
            CMD_DOCK_STYLE_STANDARD => self.apply_dock_style(crate::config::DockStyle::Standard),
            CMD_DOCK_STYLE_LABELS => self.apply_dock_style(crate::config::DockStyle::Labels),
            command if (CMD_DOCK_MAX_BASE..CMD_DOCK_MAX_BASE + 8).contains(&command) => {
                if let Some(value) = DOCK_MAX_CHOICES.get((command - CMD_DOCK_MAX_BASE) as usize) {
                    self.config.dock.max_items = *value;
                    self.save_config();
                    self.refresh_dock();
                }
            }
            command
                if (CMD_DOCK_RUNNING_LIMIT_BASE..CMD_DOCK_RUNNING_LIMIT_BASE + 8)
                    .contains(&command) =>
            {
                if let Some(value) =
                    RUNNING_LIMIT_CHOICES.get((command - CMD_DOCK_RUNNING_LIMIT_BASE) as usize)
                {
                    self.config.dock.running_limit = *value;
                    self.save_config();
                    self.refresh_dock();
                }
            }
            CMD_RESTORE_DESKTOP => self.restore_desktop_wallpaper(),
            CMD_RESTORE_ICONS => self.restore_icons_from_tray(),
            CMD_PRESET_ECO => self.apply_preset(QualityPreset::Eco),
            CMD_PRESET_BALANCED => self.apply_preset(QualityPreset::Balanced),
            CMD_PRESET_ULTRA => self.apply_preset(QualityPreset::Ultra),
            CMD_PRESET_CUSTOM => self.apply_preset(QualityPreset::Custom),
            CMD_EFFECT_UI => self.toggle_effect(EffectKind::UiAnimations),
            CMD_EFFECT_SHADOWS => self.toggle_effect(EffectKind::Shadows),
            CMD_EFFECT_PLAYBACK => self.toggle_effect(EffectKind::WallpaperPlayback),
            CMD_AUTO_FULLSCREEN => self.toggle_auto_rule(AutoRule::Fullscreen),
            CMD_AUTO_OBSCURED => self.toggle_auto_rule(AutoRule::Obscured),
            CMD_AUTO_BATTERY => self.toggle_auto_rule(AutoRule::Battery),
            CMD_AUTO_REMOTE => self.toggle_auto_rule(AutoRule::RemoteSession),
            CMD_AUTO_ADD_APP => self.add_foreground_app_rule(),
            CMD_AUTO_CLEAR_APPS => self.clear_app_rules(),
            CMD_PICK_IMAGE => self.choose_source(true),
            CMD_PICK_VIDEO => self.choose_source(false),
            CMD_PICK_POSTER => self.choose_poster(),
            CMD_CLEAR_SOURCE => self.clear_source(),
            CMD_STATUS => self.show_status(),
            CMD_AUTOSTART => self.toggle_autostart(),
            CMD_EXIT => {
                // SAFETY: 只结束本线程消息循环。
                unsafe {
                    PostQuitMessage(0);
                }
            }
            command if (CMD_DISPLAY_BASE..CMD_IMAGE_EFFECT_BASE).contains(&command) => {
                self.apply_display_override(command);
            }
            command
                if (CMD_IMAGE_EFFECT_BASE
                    ..CMD_IMAGE_EFFECT_BASE
                        + ImageEffect::ALL.len() as u32 * IMAGE_EFFECT_COMMAND_STRIDE)
                    .contains(&command) =>
            {
                self.apply_image_effect_command(command);
            }
            _ => {}
        }
    }

    fn tray_entries(&self) -> Vec<MenuEntry> {
        let preset = self.config.quality.preset;
        let settings = preset.settings(self.config.quality.custom);
        let effects = preset.effects(self.config.quality.effects);
        let auto = &self.config.quality.auto;
        let mut clear_apps = MenuEntry::item(
            CMD_AUTO_CLEAR_APPS,
            format!("清空应用规则（{}）", auto.apps.len()),
        );
        clear_apps.enabled = !auto.apps.is_empty();
        let mut auto_entries = vec![
            MenuEntry::checked(CMD_AUTO_FULLSCREEN, "全屏应用时暂停壁纸", auto.fullscreen),
            MenuEntry::checked(CMD_AUTO_OBSCURED, "壁纸被完全遮挡时暂停", auto.obscured),
            MenuEntry::checked(CMD_AUTO_BATTERY, "电池供电/节能模式时暂停", auto.battery),
            MenuEntry::checked(CMD_AUTO_REMOTE, "远程桌面会话时降档", auto.remote_session),
            MenuEntry::separator(),
            MenuEntry::item(CMD_AUTO_ADD_APP, "将当前前台应用加入暂停规则"),
            clear_apps,
        ];
        if !auto.apps.is_empty() {
            auto_entries.push(MenuEntry::note(format!("规则：{}", auto.apps.join("、"))));
        }
        let mut display_entries = Vec::new();
        for (index, display) in shell_win::desktop::displays().into_iter().enumerate() {
            let base = CMD_DISPLAY_BASE + index as u32 * 8;
            let current = self
                .config
                .quality
                .displays
                .get(&display.id)
                .and_then(|over| over.preset);
            let label = if display.primary {
                format!("{}（主显示器）", display.id)
            } else {
                format!("{}（{}×{}）", display.id, display.width, display.height)
            };
            display_entries.push(MenuEntry::submenu(
                label,
                vec![
                    MenuEntry::checked(base, "跟随全局", current.is_none()),
                    MenuEntry::checked(base + 1, "Eco", current == Some(QualityPreset::Eco)),
                    MenuEntry::checked(
                        base + 2,
                        "Balanced",
                        current == Some(QualityPreset::Balanced),
                    ),
                    MenuEntry::checked(base + 3, "Ultra", current == Some(QualityPreset::Ultra)),
                    MenuEntry::checked(base + 4, "Custom", current == Some(QualityPreset::Custom)),
                ],
            ));
        }
        if display_entries.is_empty() {
            display_entries.push(MenuEntry::note("未检测到显示器"));
        }
        let mut effect_entries = Vec::new();
        for (index, effect) in ImageEffect::ALL.iter().enumerate() {
            let base = CMD_IMAGE_EFFECT_BASE + index as u32 * IMAGE_EFFECT_COMMAND_STRIDE;
            let strength = effect.strength(&effects);
            let level = match strength {
                0 => 0,
                1..=32 => 1,
                33..=62 => 2,
                _ => 3,
            };
            effect_entries.push(MenuEntry::submenu(
                format!("{}：{}", effect.label(), strength_text(strength)),
                vec![
                    MenuEntry::checked(base, "关闭", level == 0),
                    MenuEntry::checked(base + 1, "低", level == 1),
                    MenuEntry::checked(base + 2, "中", level == 2),
                    MenuEntry::checked(base + 3, "高", level == 3),
                ],
            ));
        }
        effect_entries.push(MenuEntry::separator());
        effect_entries.push(MenuEntry::note(
            if self.config.wallpaper.kind.as_deref() == Some("video") {
                "当前为视频素材：使用 mpv 直接播放，不叠加图片效果"
            } else {
                "图片效果在静态时只渲染一帧；粒子动画会按 30 fps 唤醒"
            },
        ));
        vec![
            MenuEntry::checked(
                CMD_TOGGLE_DOCK,
                if self.dock_visible {
                    "隐藏 Dock"
                } else {
                    "显示 Dock"
                },
                self.dock_visible,
            ),
            MenuEntry::submenu(
                "Dock 样式",
                vec![
                    MenuEntry::checked(
                        CMD_DOCK_RUNNING,
                        "显示运行中应用",
                        self.config.dock.show_running,
                    ),
                    MenuEntry::checked(
                        CMD_DOCK_SYSTEM,
                        "显示系统入口",
                        self.config.dock.show_system,
                    ),
                    MenuEntry::separator(),
                    MenuEntry::submenu(
                        "图标大小",
                        vec![
                            MenuEntry::checked(
                                CMD_DOCK_STYLE_LARGE,
                                "大图标",
                                self.config.dock.style == crate::config::DockStyle::Large,
                            ),
                            MenuEntry::checked(
                                CMD_DOCK_STYLE_STANDARD,
                                "标准",
                                self.config.dock.style == crate::config::DockStyle::Standard,
                            ),
                            MenuEntry::checked(
                                CMD_DOCK_STYLE_LABELS,
                                "图标 + 名称",
                                self.config.dock.style == crate::config::DockStyle::Labels,
                            ),
                        ],
                    ),
                    MenuEntry::submenu(
                        "最大数量",
                        DOCK_MAX_CHOICES
                            .iter()
                            .enumerate()
                            .map(|(index, value)| {
                                MenuEntry::checked(
                                    CMD_DOCK_MAX_BASE + index as u32,
                                    format!("{value} 个"),
                                    self.config.dock.max_items == *value,
                                )
                            })
                            .collect(),
                    ),
                    MenuEntry::submenu(
                        "运行项上限",
                        RUNNING_LIMIT_CHOICES
                            .iter()
                            .enumerate()
                            .map(|(index, value)| {
                                MenuEntry::checked(
                                    CMD_DOCK_RUNNING_LIMIT_BASE + index as u32,
                                    format!("{value} 个"),
                                    self.config.dock.running_limit == *value,
                                )
                            })
                            .collect(),
                    ),
                ],
            ),
            MenuEntry::item(CMD_SHOW_ISLAND, "显示灵动岛"),
            MenuEntry::item(CMD_SHOW_LIBRARY, "应用库"),
            MenuEntry::separator(),
            MenuEntry::checked(
                CMD_TOGGLE_WALLPAPER,
                if self.config.wallpaper.paused || self.wallpaper_stopped {
                    "恢复壁纸"
                } else {
                    "暂停壁纸"
                },
                !self.config.wallpaper.paused && !self.wallpaper_stopped,
            ),
            MenuEntry::item(CMD_APPLY_WALLPAPER, "重新应用壁纸"),
            MenuEntry::submenu(
                "壁纸缩放",
                vec![
                    MenuEntry::checked(
                        CMD_FIT_COVER,
                        "铺满裁切（无黑边）",
                        self.config.wallpaper.fit == protocol::FitMode::Cover,
                    ),
                    MenuEntry::checked(
                        CMD_FIT_CONTAIN,
                        "适应（保留完整画面）",
                        self.config.wallpaper.fit == protocol::FitMode::Contain,
                    ),
                    MenuEntry::checked(
                        CMD_FIT_STRETCH,
                        "拉伸铺满",
                        self.config.wallpaper.fit == protocol::FitMode::Stretch,
                    ),
                    MenuEntry::checked(
                        CMD_FIT_TILE,
                        "平铺",
                        self.config.wallpaper.fit == protocol::FitMode::Tile,
                    ),
                ],
            ),
            MenuEntry::submenu(
                "画质",
                vec![
                    MenuEntry::checked(CMD_PRESET_ECO, "Eco（节能）", preset == QualityPreset::Eco),
                    MenuEntry::checked(
                        CMD_PRESET_BALANCED,
                        "Balanced（均衡）",
                        preset == QualityPreset::Balanced,
                    ),
                    MenuEntry::checked(
                        CMD_PRESET_ULTRA,
                        "Ultra（高画质）",
                        preset == QualityPreset::Ultra,
                    ),
                    MenuEntry::checked(
                        CMD_PRESET_CUSTOM,
                        "Custom（自定义）",
                        preset == QualityPreset::Custom,
                    ),
                    MenuEntry::separator(),
                    MenuEntry::checked(CMD_EFFECT_UI, "UI 动画", settings.ui_animations),
                    MenuEntry::checked(CMD_EFFECT_SHADOWS, "面板阴影", settings.shadows),
                    MenuEntry::checked(
                        CMD_EFFECT_PLAYBACK,
                        "壁纸播放",
                        settings.wallpaper_playback,
                    ),
                    MenuEntry::submenu("图片效果", effect_entries),
                ],
            ),
            MenuEntry::submenu("自动规则", auto_entries),
            MenuEntry::submenu("逐显示器覆盖", display_entries),
            MenuEntry::submenu(
                "素材",
                vec![
                    MenuEntry::item(CMD_PICK_IMAGE, "选择图片…"),
                    MenuEntry::item(CMD_PICK_VIDEO, "选择视频…"),
                    MenuEntry::item(CMD_PICK_POSTER, "选择视频静态回退图…"),
                    MenuEntry::separator(),
                    MenuEntry::item(CMD_CLEAR_SOURCE, "清除壁纸素材（恢复系统桌面）"),
                ],
            ),
            MenuEntry::submenu(
                "小部件",
                vec![
                    MenuEntry::checked(
                        CMD_WIDGET_CONTROLS,
                        "快捷控制",
                        self.widget_enabled(render::WidgetKind::Controls),
                    ),
                    MenuEntry::separator(),
                    MenuEntry::checked(
                        CMD_WIDGET_ORGANIZER,
                        "桌面收纳",
                        self.widget_enabled(render::WidgetKind::Organizer),
                    ),
                    MenuEntry::checked(
                        CMD_WIDGET_CLOCK,
                        "时钟",
                        self.widget_enabled(render::WidgetKind::Clock),
                    ),
                    MenuEntry::checked(
                        CMD_WIDGET_WEATHER,
                        "天气",
                        self.widget_enabled(render::WidgetKind::Weather),
                    ),
                    MenuEntry::checked(
                        CMD_WIDGET_MUSIC,
                        "音乐",
                        self.widget_enabled(render::WidgetKind::Music),
                    ),
                    MenuEntry::separator(),
                    MenuEntry::item(CMD_WIDGET_ADD_FOLDER, "收纳：添加文件夹…"),
                    MenuEntry::item(CMD_WIDGET_ADD_FILE, "收纳：添加文件…"),
                ],
            ),
            MenuEntry::item(CMD_STATUS, "状态与故障…"),
            MenuEntry::item(403, "设置…"),
            MenuEntry::checked(
                CMD_AUTOSTART,
                "开机启动",
                shell_win::autostart::is_enabled().unwrap_or(false),
            ),
            MenuEntry::separator(),
            MenuEntry::item(CMD_RESTORE_DESKTOP, "恢复桌面（停止壁纸）"),
            MenuEntry::item(CMD_RESTORE_ICONS, "恢复桌面图标"),
            MenuEntry::separator(),
            MenuEntry::item(CMD_EXIT, "退出 DeskShell"),
        ]
    }

    fn toggle_wallpaper(&mut self) {
        // “恢复”同时覆盖两种状态：用户暂停与托盘临时停止。
        let resume = self.wallpaper_stopped || self.config.wallpaper.paused;
        self.wallpaper_stopped = false;
        self.config.wallpaper.paused = !resume;
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(
            if resume {
                "壁纸已恢复"
            } else {
                "壁纸已暂停"
            }
            .into(),
        );
    }

    fn apply_wallpaper_now(&mut self) {
        if self.config.wallpaper.source.is_none() {
            self.push_notice("尚未选择壁纸素材".into());
            return;
        }
        self.wallpaper_stopped = false;
        self.config.wallpaper.paused = false;
        self.save_config();
        self.evaluate_policy(true);
    }

    fn apply_wallpaper_fit(&mut self, fit: protocol::FitMode) {
        if self.config.wallpaper.fit == fit {
            return;
        }
        self.config.wallpaper.fit = fit;
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("壁纸缩放：{}", fit_label(fit)));
    }

    fn apply_dock_style(&mut self, style: crate::config::DockStyle) {
        if self.config.dock.style == style {
            return;
        }
        self.config.dock.style = style;
        self.save_config();
        self.refresh_dock();
    }

    fn restore_desktop_wallpaper(&mut self) {
        self.wallpaper_stopped = true;
        let result = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.set_instances(Vec::new()));
        self.supervisor_instances.clear();
        match result {
            Some(Ok(_)) => self.push_notice("已恢复系统桌面；可从托盘重新应用壁纸".into()),
            Some(Err(error)) => self.push_notice(format!("恢复桌面失败：{}", error.message)),
            None => self.push_notice("已标记停止壁纸；当前没有运行中的宿主".into()),
        }
    }

    fn apply_preset(&mut self, preset: QualityPreset) {
        self.config.quality.preset = preset;
        self.config.quality.normalize();
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("画质预设：{}", preset.label()));
    }

    /// 图片效果菜单：等级 0..=3；首次修改时把当前预设基线写入 Custom。
    fn apply_image_effect_command(&mut self, command: u32) {
        let offset = command - CMD_IMAGE_EFFECT_BASE;
        let index = (offset / IMAGE_EFFECT_COMMAND_STRIDE) as usize;
        let level = offset % IMAGE_EFFECT_COMMAND_STRIDE;
        let Some(effect) = ImageEffect::ALL.get(index) else {
            return;
        };
        if level > 3 {
            return;
        }
        if self.config.quality.preset != QualityPreset::Custom {
            self.config.quality.effects = self
                .config
                .quality
                .preset
                .effects(self.config.quality.effects);
            self.config.quality.preset = QualityPreset::Custom;
        }
        let value = effect_strength(level);
        effect.set(&mut self.config.quality.effects, value);
        let name = effect.label();
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("图片效果 {name}：{}", strength_text(value)));
    }

    fn toggle_effect(&mut self, kind: EffectKind) {
        if self.config.quality.preset != QualityPreset::Custom {
            // 从当前预设派生自定义初值，避免切换后参数突然重置。
            self.config.quality.custom = self
                .config
                .quality
                .preset
                .settings(self.config.quality.custom);
            self.config.quality.preset = QualityPreset::Custom;
        }
        let settings = &mut self.config.quality.custom;
        match kind {
            EffectKind::UiAnimations => settings.ui_animations = !settings.ui_animations,
            EffectKind::Shadows => settings.shadows = !settings.shadows,
            EffectKind::WallpaperPlayback => {
                settings.wallpaper_playback = !settings.wallpaper_playback
            }
        }
        let value = kind.get(settings);
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("{}：{}", kind.label(), yes_no(value)));
    }

    fn toggle_auto_rule(&mut self, rule: AutoRule) {
        let auto = &mut self.config.quality.auto;
        let value = match rule {
            AutoRule::Fullscreen => &mut auto.fullscreen,
            AutoRule::Obscured => &mut auto.obscured,
            AutoRule::Battery => &mut auto.battery,
            AutoRule::RemoteSession => &mut auto.remote_session,
        };
        *value = !*value;
        let enabled = *value;
        self.save_config();
        // 用户显式修改规则时清除迟滞状态，让新规则立即生效。
        self.policy_engine.clear();
        self.sync_policy_timer();
        self.evaluate_policy(true);
        self.push_notice(format!(
            "自动规则 {}：{}",
            rule.label(),
            if enabled { "启用" } else { "关闭" }
        ));
    }

    fn add_foreground_app_rule(&mut self) {
        let Some(app) = shell_win::system_state::foreground_state().process_name else {
            self.push_notice("无法识别当前前台应用".into());
            return;
        };
        let already = self
            .config
            .quality
            .auto
            .apps
            .iter()
            .any(|rule| rule.eq_ignore_ascii_case(&app));
        if !already {
            self.config.quality.auto.apps.push(app.clone());
            if self.config.quality.auto.apps.len() > crate::policy::MAX_AUTO_APPS {
                self.config
                    .quality
                    .auto
                    .apps
                    .truncate(crate::policy::MAX_AUTO_APPS);
            }
        }
        self.config.quality.normalize();
        self.save_config();
        self.policy_engine.clear();
        self.sync_policy_timer();
        self.evaluate_policy(true);
        self.push_notice(if already {
            format!("{app} 已在暂停规则中")
        } else {
            format!("已将 {app} 加入暂停规则")
        });
    }

    fn clear_app_rules(&mut self) {
        self.config.quality.auto.apps.clear();
        self.save_config();
        self.policy_engine.clear();
        self.sync_policy_timer();
        self.evaluate_policy(true);
        self.push_notice("已清空应用规则".into());
    }

    fn apply_display_override(&mut self, command: u32) {
        let offset = command - CMD_DISPLAY_BASE;
        let index = (offset / 8) as usize;
        let choice = offset % 8;
        let Some(display) = shell_win::desktop::displays().into_iter().nth(index) else {
            self.push_notice("显示器列表已变化，请重试".into());
            return;
        };
        let preset = match choice {
            0 => None,
            1 => Some(QualityPreset::Eco),
            2 => Some(QualityPreset::Balanced),
            3 => Some(QualityPreset::Ultra),
            4 => Some(QualityPreset::Custom),
            _ => return,
        };
        match preset {
            Some(preset) => {
                self.config.quality.displays.insert(
                    display.id.clone(),
                    crate::policy::DisplayOverride {
                        preset: Some(preset),
                    },
                );
            }
            None => {
                self.config.quality.displays.remove(&display.id);
            }
        }
        self.save_config();
        self.policy_engine.clear();
        self.evaluate_policy(true);
        self.push_notice(format!(
            "{}：{}",
            display.id,
            preset.map(QualityPreset::label).unwrap_or("跟随全局")
        ));
    }

    fn choose_source(&mut self, image: bool) {
        let (title, name, pattern) = if image {
            (
                "选择壁纸图片",
                "图片",
                "*.png;*.jpg;*.jpeg;*.bmp;*.webp;*.gif",
            )
        } else {
            (
                "选择壁纸视频",
                "视频",
                "*.mp4;*.mkv;*.webm;*.mov;*.avi;*.m4v;*.ts",
            )
        };
        let Some(path) = shell_win::shellui::pick_file(title, name, pattern, self.main_hwnd) else {
            return;
        };
        self.config.wallpaper.source = Some(path.display().to_string());
        self.config.wallpaper.kind = Some(if image { "image" } else { "video" }.into());
        self.config.wallpaper.poster = None;
        self.config.wallpaper.paused = false;
        self.wallpaper_stopped = false;
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("壁纸素材：{}", path.display()));
    }

    fn choose_poster(&mut self) {
        if self.config.wallpaper.source.is_none() {
            self.push_notice("请先选择视频素材".into());
            return;
        }
        if self.config.wallpaper.kind.as_deref() == Some("image") {
            self.push_notice("当前素材是图片，不需要静态回退图".into());
            return;
        }
        let Some(path) = shell_win::shellui::pick_file(
            "选择视频加载失败时的静态回退图",
            "图片",
            "*.png;*.jpg;*.jpeg;*.bmp;*.webp",
            self.main_hwnd,
        ) else {
            return;
        };
        self.config.wallpaper.poster = Some(path.display().to_string());
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice(format!("静态回退图：{}", path.display()));
    }

    fn clear_source(&mut self) {
        self.config.wallpaper.source = None;
        self.config.wallpaper.poster = None;
        self.config.wallpaper.kind = None;
        self.wallpaper_stopped = false;
        self.save_config();
        self.evaluate_policy(true);
        self.push_notice("已清除壁纸素材，系统桌面保持显示".into());
    }

    fn toggle_autostart(&mut self) {
        match shell_win::autostart::is_enabled() {
            Ok(enabled) => match shell_win::autostart::set_enabled(!enabled) {
                Ok(()) => self.push_notice(if enabled {
                    "已关闭开机启动".into()
                } else {
                    "已启用开机启动（HKCU Run）".into()
                }),
                Err(error) => self.push_notice(error),
            },
            Err(error) => self.push_notice(error),
        }
    }

    fn show_status(&mut self) {
        let mut text = String::new();
        if self.wallpaper_stopped {
            text.push_str("壁纸：已停止（系统静态桌面）\n");
        }
        text.push_str(&self.policy_report());
        let supervisor_status = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.status());
        text.push('\n');
        text.push_str(&format!(
            "配置：v{} · {}（{}）\n",
            self.config.version,
            self.config_path.display(),
            if self.config_read_only {
                "只读"
            } else {
                "可写"
            }
        ));
        if let Some(supervisor) = &self.supervisor {
            text.push_str(&format!("宿主：{}\n", supervisor.status().describe()));
        } else {
            text.push_str("宿主：未运行\n");
        }
        if let Some(snapshot) = supervisor_status
            .as_ref()
            .and_then(|status| status.snapshot())
        {
            for instance in &snapshot.instances {
                text.push_str(&format!(
                    "实例 {}：{:?}，呈现 {:?}，附着 {:?}，路径 {:?}，播放器 {}，硬解 {:?}，掉帧 {:?}\n",
                    instance.id,
                    instance.state,
                    instance.displayed,
                    instance.attachment,
                    instance.render_path,
                    if instance.player_active {
                        "持有"
                    } else {
                        "已释放"
                    },
                    instance.hardware_decode,
                    instance.dropped_frames
                ));
                if instance.effects.any() {
                    text.push_str(&format!(
                        "  图片效果：{}，pass {}，Present {}，GPU {}\n",
                        instance.effects.labels().join("、"),
                        instance.effect_passes,
                        instance.present_count,
                        instance
                            .gpu_time_us
                            .map(|micros| format!("{:.2} ms", f64::from(micros) / 1000.0))
                            .unwrap_or_else(|| "不可用".into())
                    ));
                }
                if let Some(error) = &instance.effects_error {
                    text.push_str(&format!("  效果说明：{error}\n"));
                }
                if let Some(error) = &instance.last_error {
                    text.push_str(&format!("  错误：{:?} {}\n", error.code, error.message));
                }
            }
        }
        if let Some(source) = &self.config.wallpaper.source {
            text.push_str(&format!("素材：{source}\n"));
        }
        if let Some(display) = &self.config.wallpaper.display {
            text.push_str(&format!("目标显示器：{display}\n"));
        }
        text.push_str(
            "\n说明：图片效果使用 D3D11 管线，静态时只渲染一帧；视频仍由 mpv 直接播放，不叠加图片效果。HDR 与多厂商矩阵需独立验收。",
        );
        shell_win::shellui::info("DeskShell 状态", &text, self.island.hwnd());
    }

    fn restore_icons_from_tray(&mut self) {
        match self.set_desktop_icons(true) {
            Ok(()) => {
                self.config.onboarding.desktop_icons_hidden_by_us = false;
                self.save_config();
                if let Some(path) = ShellStateJournal::default_path()
                    && let Ok(mut journal) = ShellStateJournal::open(path)
                {
                    let _ = journal.mark_recovered("desktop-icons");
                }
                self.push_notice("桌面图标已恢复".into());
            }
            Err(error) => self.push_notice(format!("恢复桌面图标失败：{error}")),
        }
    }
}

/// 自检：验证真实窗口位置、命中区域、角裁剪与空闲停帧。
pub fn run_self_test(state: &mut AppState) -> Result<(), String> {
    state.island_expanded = true;
    state.refresh_dock();
    state.refresh_island();
    state.refresh_library();

    let mut failures = Vec::new();
    let mut passes = Vec::new();
    if state.library.view().items.len() != state.apps.len()
        || state.island.view().items.len() != state.files.len()
    {
        failures.push("浏览列表仍被截断".into());
    } else {
        passes.push("应用库和文件完整列表".into());
    }
    if let Some(entry) = state.apps.last() {
        state
            .library
            .set_filter(entry.name.clone(), String::new())?;
        if state.library.view().filtered_indices().is_empty() {
            failures.push("应用搜索无结果".into());
        } else {
            passes.push("应用库搜索末尾项目".into());
        }
        state.library.set_filter(String::new(), String::new())?;
        state.library.scroll_rows(i32::MAX);
        if state.apps.len() > 8 && state.library.view().offset == 0 {
            failures.push("滚动未到达后续条目".into());
        } else {
            passes.push("完整列表滚动".into());
        }
        state.library.scroll_rows(i32::MIN);
    }
    state.show_settings();
    if let Some(settings) = &state.settings {
        if settings.is_visible()
            && settings
                .window_screen_rect()
                .is_some_and(|(_, _, width, height)| width > 600 && height > 400)
        {
            passes.push("独立设置窗口与真实操作控件".into());
        } else {
            failures.push("设置窗口尺寸/可见性异常".into());
        }
    } else {
        failures.push("设置窗口未创建".into());
    }
    state.settings = None;
    println!(
        "app self-test：应用 {}，图标 {}，桌面 {}，Dock 条目 {}，应用库 {}",
        state.apps.len(),
        state.app_icons.len(),
        state.files.len(),
        state.dock.view().items.len(),
        state.apps.len()
    );
    if state.app_icons.is_empty() {
        failures.push("没有成功加载任何应用图标".to_string());
    }
    for item in &state.dock.view().items {
        println!("  Dock 条目：{}（{}）", item.label, item.id);
    }
    if state.apps.is_empty() {
        failures.push("应用发现为空".to_string());
    }
    if !state.files_ready {
        failures.push("桌面索引未完成".to_string());
    }
    if !desktop_roots_available() {
        failures.push("用户/公共桌面根目录不可访问".to_string());
    }
    if state.dock.view().items.is_empty() {
        failures.push("Dock 没有条目".to_string());
    }

    // 自检需要稳定命中；显式唤起后立即验证，再撤销临时置顶。
    if let Err(error) = state.dock.summon(false) {
        failures.push(format!("唤起 Dock 失败：{error}"));
    }
    if let Err(error) = state.island.summon(false) {
        failures.push(format!("唤起灵动岛失败：{error}"));
    }
    // 两个面板先后唤起会因失焦撤销对方的临时置顶；自检需要确定性命中环境。
    state.dock.set_temporary_topmost(true);
    state.island.set_temporary_topmost(true);
    if state.dock.is_visible() {
        passes.push("Dock 可见".to_string());
    } else {
        failures.push("Dock 不可见".to_string());
    }
    if state.island.is_visible() {
        passes.push("灵动岛可见".to_string());
    } else {
        failures.push("灵动岛不可见".to_string());
    }

    if let Some(dock_rect) = state.dock.window_screen_rect() {
        println!("  Dock 窗口：{dock_rect:?}");
    }
    for (id, rect) in state.dock.item_screen_rects() {
        let point = POINT {
            x: rect.0 + rect.2 / 2,
            y: rect.1 + rect.3 / 2,
        };
        // SAFETY: 只做只读命中测试。
        let hit = unsafe { WindowFromPoint(point) };
        if hit != state.dock.hwnd() {
            failures.push(format!(
                "Dock 条目 {id} 未命中面板窗口：point={},{} rect={rect:?} hit={}",
                point.x,
                point.y,
                describe_window(hit)
            ));
        }
    }
    if !state.dock.corner_clipped() {
        failures.push("Dock 圆角区域未裁剪".to_string());
    } else {
        passes.push("Dock 圆角裁剪".to_string());
    }
    if !state.island.corner_clipped() {
        failures.push("灵动岛圆角区域未裁剪".to_string());
    } else {
        passes.push("灵动岛圆角裁剪".to_string());
    }

    // 阴影是已交付效果：验证创建、点击穿透与关闭后释放。
    let shadows_enabled = state
        .effective_policy
        .as_ref()
        .is_some_and(|policy| policy.settings.shadows);
    if shadows_enabled {
        if state.dock.shadow_ready() && state.island.shadow_ready() {
            passes.push("面板阴影已创建".to_string());
        } else {
            failures.push("阴影策略开启但阴影窗口未创建".to_string());
        }
        if let (Some(shadow), Some(rect)) =
            (state.dock.shadow_hwnd(), state.dock.window_screen_rect())
        {
            let point = POINT {
                x: rect.0 + rect.2 as i32 / 2,
                y: rect.1 + rect.3 as i32 + 6,
            };
            // SAFETY: 只做只读命中测试。
            let hit = unsafe { WindowFromPoint(point) };
            if hit == state.dock.hwnd() || hit == shadow {
                failures.push("阴影区域拦截了点击".to_string());
            } else {
                passes.push("阴影区域点击穿透".to_string());
            }
        }
        state.dock.set_shadows(false);
        if state.dock.shadow_hwnd().is_none() {
            passes.push("关闭阴影已释放窗口与位图".to_string());
        } else {
            failures.push("关闭阴影后仍持有阴影窗口".to_string());
        }
        state.dock.set_shadows(true);
    }

    let dock_rects = state.dock.item_screen_rects();
    if !dock_rects.is_empty() {
        passes.push(format!("Dock 条目命中 {}", dock_rects.len()));
    }
    let island_rows = state.island.item_screen_rects();
    for (id, rect) in &island_rows {
        let point = POINT {
            x: rect.0 + rect.2 / 2,
            y: rect.1 + rect.3 / 2,
        };
        // SAFETY: 只做只读命中测试。
        let hit = unsafe { WindowFromPoint(point) };
        if hit != state.island.hwnd() {
            failures.push(format!(
                "灵动岛文件行 {id} 未命中面板窗口：point={},{} rect={rect:?} hit={}",
                point.x,
                point.y,
                describe_window(hit)
            ));
        }
    }
    if !state.files.is_empty() && island_rows.is_empty() {
        failures.push("桌面有条目但灵动岛没有展开行".to_string());
    }
    if !island_rows.is_empty() {
        passes.push(format!("灵动岛展开行 {}", island_rows.len()));
    }
    if !state.files.is_empty() {
        let visible: usize = state
            .files
            .iter()
            .take(8)
            .filter(|file| !file.cloud_placeholder)
            .count();
        if visible > 0 && state.file_icons.is_empty() {
            failures.push("可见文件行没有加载任何图标/缩略图".to_string());
        } else {
            passes.push(format!("文件图标/缩略图 {}", state.file_icons.len()));
        }
    }

    let (monitor_x, monitor_y, monitor_w, monitor_h) = state.island.monitor_screen_rect();
    if monitor_w == 0 || monitor_h == 0 {
        failures.push("显示器边界为空".to_string());
    } else {
        passes.push(format!(
            "显示器边界 {monitor_x},{monitor_y} {monitor_w}x{monitor_h}"
        ));
    }

    // 确保没有悬停动画在运行；等待期间不泵消息，Present 计数必须完全不变。
    let before = state.dock.presents() + state.island.presents() + state.library.presents();
    thread::sleep(Duration::from_millis(700));
    let after = state.dock.presents() + state.island.presents() + state.library.presents();
    if before != after {
        failures.push(format!("空闲 700 ms 内仍发生 Present：{before} -> {after}"));
    } else {
        passes.push("空闲 700 ms Present 增量 0".to_string());
    }

    state.dock.set_temporary_topmost(false);
    state.island.set_temporary_topmost(false);
    state.dock.release_elevation();
    state.island.release_elevation();

    for line in &passes {
        println!("  PASS：{line}");
    }
    if failures.is_empty() {
        println!("app self-test PASS");
        Ok(())
    } else {
        for line in &failures {
            println!("  FAIL：{line}");
        }
        Err(format!("app self-test {} 项失败", failures.len()))
    }
}

fn describe_window(hwnd: HWND) -> String {
    if hwnd.is_invalid() {
        return "无窗口".into();
    }
    let mut class = [0u16; 128];
    // SAFETY: 输出缓冲区大小与指针匹配。
    let length = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    let name = String::from_utf16_lossy(&class[..length.min(class.len())]);
    let mut rect = RECT::default();
    // SAFETY: 输出结构有效。
    let got = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok();
    if got {
        format!(
            "{name} ({},{},{},{})",
            rect.left, rect.top, rect.right, rect.bottom
        )
    } else {
        name
    }
}

fn app_item(
    entry: &AppEntry,
    icons: &std::collections::HashMap<String, render::IconPixels>,
) -> PanelItem {
    PanelItem {
        id: entry.id.clone(),
        label: entry.name.clone(),
        icon: icons.get(&entry.id).cloned(),
        detail: String::new(),
        running: false,
        accent: (96, 170, 235),
    }
}

fn special_item(id: &str, label: &str, accent: (u8, u8, u8)) -> PanelItem {
    PanelItem {
        id: id.into(),
        label: label.into(),
        icon: None,
        detail: String::new(),
        running: false,
        accent,
    }
}

fn file_item(
    file: &DesktopFile,
    icons: &std::collections::HashMap<String, render::IconPixels>,
) -> PanelItem {
    PanelItem {
        id: file.id.clone(),
        label: file.name.clone(),
        icon: icons.get(&file.id).cloned(),
        detail: if file.is_dir {
            file.category.label().into()
        } else {
            format!("{} · {}", file.category.label(), size_text(file.size))
        },
        running: false,
        accent: category_accent(file.category),
    }
}

fn strength_text(strength: u8) -> &'static str {
    match strength {
        0 => "关闭",
        1..=32 => "低",
        33..=62 => "中",
        _ => "高",
    }
}

fn dock_style(style: crate::config::DockStyle) -> DockStyle {
    match style {
        crate::config::DockStyle::Large => DockStyle::Large,
        crate::config::DockStyle::Standard => DockStyle::Standard,
        crate::config::DockStyle::Labels => DockStyle::Labels,
    }
}

fn category_accent(category: FileCategory) -> (u8, u8, u8) {
    match category {
        FileCategory::Folder => (110, 170, 240),
        FileCategory::Image => (250, 170, 90),
        FileCategory::Document => (120, 210, 160),
        FileCategory::Archive => (200, 160, 250),
        FileCategory::Installer => (250, 130, 130),
        FileCategory::Other => (160, 170, 190),
    }
}

fn size_text(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.0} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}

#[derive(Clone, Copy)]
enum PathAction {
    Open,
    Reveal,
    Recycle,
}

/// 供小部件打开路径使用；复用同一后台 Shell 执行路径。
pub(super) fn open_path(path: PathBuf, notices: Arc<Mutex<Vec<String>>>, notify: isize) {
    spawn_path_action(path, PathAction::Open, notices, notify);
}

fn spawn_path_action(
    path: PathBuf,
    action: PathAction,
    notices: Arc<Mutex<Vec<String>>>,
    notify: isize,
) {
    let _ = thread::Builder::new()
        .name("file-action".into())
        .spawn(move || {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let result = match action {
                PathAction::Open => deskfiles::open_path(&path),
                PathAction::Reveal => deskfiles::reveal_path(&path),
                PathAction::Recycle => deskfiles::recycle_path(&path),
            };
            let message = match result {
                Ok(()) => match action {
                    PathAction::Recycle => format!("已移入回收站：{name}"),
                    PathAction::Open => format!("已打开：{name}"),
                    PathAction::Reveal => format!("已定位：{name}"),
                },
                Err(error) => format!("{name} 操作失败：{error}"),
            };
            push_notice(&notices, message, notify);
        });
}

fn push_notice(notices: &Arc<Mutex<Vec<String>>>, message: String, notify: isize) {
    notices
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(message);
    shell_win::shellui::post_message_raw(notify, WM_APP_NOTICE);
}

/// 供自检读取桌面根目录是否可访问。
pub fn desktop_roots_available() -> bool {
    deskfiles::desktop_roots()
        .0
        .iter()
        .any(|root| Path::new(&root.path).is_dir())
}
