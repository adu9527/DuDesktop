//! 小部件接入：窗口生命周期、配置、收纳内容与天气/音乐数据。
//!
//! 渲染层只负责绘制与命中；本模块把配置展开为视图，并把移动/调宽/打开条目等
//! 事件写回配置。网络与媒体查询只在后台线程执行。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use render::{
    MusicView, WeatherView, WidgetEvent, WidgetItem, WidgetKind, WidgetSpec, WidgetView,
    WidgetWindow,
};
use shell_win::shellui::MenuEntry;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{AppState, TIMER_WEATHER, WEATHER_REFRESH_MS, WM_APP_WEATHER, WM_APP_WIDGET_ICONS};
use crate::{
    config,
    controls::{ControlsCommand, ControlsWorker},
    music::{MusicCommand, MusicWorker},
    weather,
};

const CMD_OPEN_ITEM: u32 = 1;
const CMD_REMOVE_ITEM: u32 = 2;

impl AppState {
    /// 按配置创建/销毁窗口，并刷新所有视图。
    pub(super) fn sync_widgets(&mut self) {
        self.sync_widget_windows();
        self.refresh_widgets();
    }

    /// 媒体/天气工作线程只在对应小部件启用时存在。
    pub(super) fn sync_widget_workers(&mut self) {
        let music_enabled = self.config.widgets.music.enabled;
        if music_enabled {
            if self.music_worker.is_none() {
                self.music_worker = Some(MusicWorker::spawn(
                    self.main_hwnd.0 as isize,
                    crate::app::WM_APP_MUSIC,
                ));
            }
        } else {
            self.music_worker = None;
            self.widget_music = None;
        }
        let weather_enabled = self.config.widgets.weather.base.enabled;
        let clock_enabled = self.config.widgets.clock.enabled;
        let controls_enabled = self.config.widgets.controls.enabled;
        if controls_enabled {
            if self.controls_worker.is_none() {
                self.controls_worker = Some(ControlsWorker::spawn(
                    self.main_hwnd.0 as isize,
                    crate::app::WM_APP_CONTROLS,
                ));
            }
        } else {
            self.controls_worker = None;
            self.widget_controls = None;
        }
        // SAFETY: 只操作主窗口定时器。
        unsafe {
            if weather_enabled {
                let _ = SetTimer(
                    Some(self.main_hwnd),
                    TIMER_WEATHER,
                    WEATHER_REFRESH_MS,
                    None,
                );
            } else {
                let _ = KillTimer(Some(self.main_hwnd), TIMER_WEATHER);
            }
            if clock_enabled {
                let _ = SetTimer(
                    Some(self.main_hwnd),
                    super::TIMER_WIDGET_CLOCK,
                    30_000,
                    None,
                );
            } else {
                let _ = KillTimer(Some(self.main_hwnd), super::TIMER_WIDGET_CLOCK);
            }
        }
        if weather_enabled {
            self.fetch_weather();
        }
    }

    pub(super) fn set_widget_enabled(&mut self, kind: WidgetKind, enabled: bool) {
        if let Some(config) = self.widget_config_mut(kind) {
            config.enabled = enabled;
        }
        self.save_config();
        self.sync_widgets();
        self.sync_widget_workers();
    }

    pub(super) fn toggle_widget(&mut self, kind: WidgetKind) {
        let enabled = self.widget_enabled(kind);
        self.set_widget_enabled(kind, !enabled);
    }

    pub(super) fn widget_enabled(&self, kind: WidgetKind) -> bool {
        match kind {
            WidgetKind::Organizer => self.config.widgets.organizer.base.enabled,
            WidgetKind::Clock => self.config.widgets.clock.enabled,
            WidgetKind::Weather => self.config.widgets.weather.base.enabled,
            WidgetKind::Music => self.config.widgets.music.enabled,
            WidgetKind::Controls => self.config.widgets.controls.enabled,
        }
    }

    pub(super) fn add_organizer_app(&mut self, app_id: &str) {
        let key = format!("app:{app_id}");
        if !self.config.widgets.organizer.items.contains(&key) {
            self.config.widgets.organizer.items.push(key);
        }
        self.config.widgets.organizer.base.enabled = true;
        self.save_config();
        self.sync_widgets();
    }

    pub(super) fn add_organizer_path(&mut self, path: &Path, folder: bool) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let key = format!(
            "{}:{}",
            if folder { "folder" } else { "path" },
            path.display()
        );
        if !self.config.widgets.organizer.items.contains(&key) {
            self.config.widgets.organizer.items.push(key);
        }
        self.config.widgets.organizer.base.enabled = true;
        self.save_config();
        self.sync_widgets();
    }

    pub(super) fn fetch_weather(&mut self) {
        if self.widget_weather_busy {
            return;
        }
        self.widget_weather_busy = true;
        let city = self.config.widgets.weather.city.clone();
        let store = Arc::clone(&self.widget_weather_store);
        let notify = self.main_hwnd.0 as isize;
        let spawn = thread::Builder::new()
            .name("widget-weather".into())
            .spawn(move || {
                let result = weather::fetch(&city);
                *store.lock().unwrap_or_else(|error| error.into_inner()) = Some(result);
                shell_win::shellui::post_message_raw(notify, WM_APP_WEATHER);
            });
        if spawn.is_err() {
            self.widget_weather_busy = false;
        }
    }

    pub(super) fn handle_widget_events(
        &mut self,
        hwnd: windows::Win32::Foundation::HWND,
        events: Vec<WidgetEvent>,
    ) {
        let Some(kind) = self
            .widgets
            .iter()
            .find(|window| window.hwnd() == hwnd)
            .map(WidgetWindow::kind)
        else {
            return;
        };
        for event in events {
            match event {
                WidgetEvent::Moved { x, y } => {
                    if let Some(config) = self.widget_config_mut(kind) {
                        config.x = x;
                        config.y = y;
                    }
                    self.save_config();
                }
                WidgetEvent::Resized { width } => {
                    if let Some(config) = self.widget_config_mut(kind) {
                        config.width = width;
                    }
                    self.save_config();
                }
                WidgetEvent::OpenItem(id) => self.open_widget_item(&id),
                WidgetEvent::ItemMenu(id) => self.widget_item_menu(&id),
                WidgetEvent::ToggleMusic => {
                    if let Some(worker) = &self.music_worker {
                        worker.send(MusicCommand::Toggle);
                    }
                }
                WidgetEvent::NextMusic => {
                    if let Some(worker) = &self.music_worker {
                        worker.send(MusicCommand::Next);
                    }
                }
                WidgetEvent::PreviousMusic => {
                    if let Some(worker) = &self.music_worker {
                        worker.send(MusicCommand::Previous);
                    }
                }
                WidgetEvent::VolumeChanged(volume) => {
                    if let Some(worker) = &self.controls_worker {
                        worker.send(ControlsCommand::SetVolume(volume));
                    }
                }
                WidgetEvent::ToggleMute => {
                    if let Some(worker) = &self.controls_worker {
                        worker.send(ControlsCommand::ToggleMute);
                    }
                }
                WidgetEvent::Bluetooth => {
                    if let Some(worker) = &self.controls_worker {
                        worker.send(ControlsCommand::ToggleBluetooth);
                    }
                }
                WidgetEvent::Wifi | WidgetEvent::OpenNetwork => {
                    crate::app::app_ui::open_path(
                        PathBuf::from("ms-availablenetworks:"),
                        self.notices.clone(),
                        self.main_hwnd.0 as isize,
                    );
                }
                WidgetEvent::OpenSettings => self.show_settings(),
                WidgetEvent::OpenDisplay => crate::app::app_ui::open_path(
                    PathBuf::from("ms-settings:display"),
                    self.notices.clone(),
                    self.main_hwnd.0 as isize,
                ),
                WidgetEvent::OpenSound => crate::app::app_ui::open_path(
                    PathBuf::from("ms-settings:sound"),
                    self.notices.clone(),
                    self.main_hwnd.0 as isize,
                ),
                WidgetEvent::Dismissed => {
                    if let Some(config) = self.widget_config_mut(kind) {
                        config.enabled = false;
                    }
                    self.save_config();
                    self.widgets.retain(|window| window.hwnd() != hwnd);
                    self.sync_widget_workers();
                }
            }
        }
    }

    /// 所有已启用小部件的最新视图。
    pub(super) fn refresh_widgets(&mut self) {
        if self.widgets.is_empty() {
            return;
        }
        self.load_widget_icons();
        let views: Vec<(WidgetKind, WidgetView)> = self
            .widgets
            .iter()
            .map(|window| (window.kind(), self.widget_view(window.kind())))
            .collect();
        let mut error = None;
        for (kind, view) in views {
            if let Some(window) = self.widgets.iter_mut().find(|window| window.kind() == kind) {
                window.set_view(view);
                if error.is_none() {
                    error = window.take_error();
                }
            }
        }
        if let Some(error) = error {
            self.push_notice(format!("小部件绘制失败：{error}"));
        }
    }

    fn sync_widget_windows(&mut self) {
        let specs = self.widget_specs();
        self.widgets
            .retain(|window| specs.iter().any(|(kind, _)| *kind == window.kind()));
        for (kind, spec) in specs {
            if self.widgets.iter().any(|window| window.kind() == kind) {
                continue;
            }
            let view = self.widget_view(kind);
            match WidgetWindow::create(spec, view) {
                Ok(window) => self.widgets.push(window),
                Err(error) => self.push_notice(format!("创建小部件失败：{error}")),
            }
        }
    }

    fn widget_specs(&self) -> Vec<(WidgetKind, WidgetSpec)> {
        let widgets = &self.config.widgets;
        let mut specs = Vec::new();
        let mut push = |kind: WidgetKind, config: &config::WidgetConfig| {
            if config.enabled {
                specs.push((
                    kind,
                    WidgetSpec {
                        kind,
                        x: config.x,
                        y: config.y,
                        width: config.width,
                    },
                ));
            }
        };
        push(WidgetKind::Organizer, &widgets.organizer.base);
        push(WidgetKind::Clock, &widgets.clock);
        push(WidgetKind::Weather, &widgets.weather.base);
        push(WidgetKind::Music, &widgets.music);
        push(WidgetKind::Controls, &widgets.controls);
        specs
    }

    fn widget_config_mut(&mut self, kind: WidgetKind) -> Option<&mut config::WidgetConfig> {
        match kind {
            WidgetKind::Organizer => Some(&mut self.config.widgets.organizer.base),
            WidgetKind::Clock => Some(&mut self.config.widgets.clock),
            WidgetKind::Weather => Some(&mut self.config.widgets.weather.base),
            WidgetKind::Music => Some(&mut self.config.widgets.music),
            WidgetKind::Controls => Some(&mut self.config.widgets.controls),
        }
    }

    fn widget_view(&self, kind: WidgetKind) -> WidgetView {
        match kind {
            WidgetKind::Organizer => WidgetView::organizer(
                self.organizer_items(),
                self.config.widgets.organizer.columns,
            ),
            WidgetKind::Clock => {
                let (clock, date) = local_time();
                WidgetView::clock(clock, date)
            }
            WidgetKind::Weather => {
                let city = self.config.widgets.weather.city.clone();
                let view = if let Some(data) = &self.widget_weather {
                    WeatherView {
                        city: data.city.clone(),
                        temperature: format!("{:.0}°", data.temperature),
                        text: data.text.clone(),
                        code: data.code,
                    }
                } else {
                    WeatherView {
                        city,
                        temperature: "--°".into(),
                        text: self
                            .widget_weather_error
                            .clone()
                            .unwrap_or_else(|| "正在获取…".into()),
                        code: 0,
                    }
                };
                WidgetView::weather(view)
            }
            WidgetKind::Music => {
                let view = self
                    .widget_music
                    .as_ref()
                    .map(|state| MusicView {
                        title: state.title.clone(),
                        artist: state.artist.clone(),
                        source: source_label(&state.source),
                        playing: state.playing,
                    })
                    .unwrap_or_default();
                WidgetView::music(view)
            }
            WidgetKind::Controls => {
                WidgetView::controls(self.widget_controls.clone().unwrap_or_default())
            }
        }
    }

    fn organizer_items(&self) -> Vec<WidgetItem> {
        let mut items = Vec::new();
        for raw in &self.config.widgets.organizer.items {
            if let Some(id) = raw.strip_prefix("app:") {
                let label = self
                    .apps
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.name.clone())
                    .unwrap_or_else(|| id.to_string());
                let icon = self
                    .widget_icons
                    .get(raw)
                    .cloned()
                    .or_else(|| self.app_icons.get(id).cloned());
                items.push(WidgetItem {
                    id: raw.clone(),
                    label,
                    icon,
                });
            } else if let Some(path) = raw
                .strip_prefix("path:")
                .or_else(|| raw.strip_prefix("folder:"))
            {
                let label = Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string();
                let icon = self
                    .widget_icons
                    .get(raw)
                    .cloned()
                    .or_else(|| self.file_icons.get(path).cloned());
                items.push(WidgetItem {
                    id: raw.clone(),
                    label,
                    icon,
                });
            }
        }
        items
    }

    fn load_widget_icons(&mut self) {
        if !self.config.widgets.organizer.base.enabled {
            return;
        }
        let mut requests: Vec<(String, Result<shell_win::apps::AppEntry, PathBuf>)> = Vec::new();
        for raw in &self.config.widgets.organizer.items {
            if self.widget_icons.contains_key(raw)
                || !self.widget_icons_attempted.insert(raw.clone())
            {
                continue;
            }
            if let Some(id) = raw.strip_prefix("app:") {
                if let Some(entry) = self.apps.iter().find(|entry| entry.id == id).cloned() {
                    requests.push((raw.clone(), Ok(entry)));
                }
            } else if let Some(path) = raw
                .strip_prefix("path:")
                .or_else(|| raw.strip_prefix("folder:"))
            {
                requests.push((raw.clone(), Err(PathBuf::from(path))));
            }
        }
        if requests.is_empty() {
            return;
        }
        let store = Arc::clone(&self.widget_icon_batch);
        let notify = self.main_hwnd.0 as isize;
        let _ = thread::Builder::new()
            .name("widget-icons".into())
            .spawn(move || {
                let mut batch = Vec::new();
                for (id, source) in requests {
                    let icon = match source {
                        Ok(entry) => shell_win::apps::app_icon(&entry, 64),
                        Err(path) => shell_win::apps::file_image(&path, 64, false),
                    };
                    if let Ok(icon) = icon {
                        batch.push((
                            id,
                            render::IconPixels {
                                width: icon.width,
                                height: icon.height,
                                bgra: icon.bgra,
                            },
                        ));
                    }
                }
                *store.lock().unwrap_or_else(|error| error.into_inner()) = Some(batch);
                shell_win::shellui::post_message_raw(notify, WM_APP_WIDGET_ICONS);
            });
    }

    fn open_widget_item(&mut self, id: &str) {
        if let Some(app_id) = id.strip_prefix("app:") {
            self.launch_app(app_id);
            return;
        }
        if let Some(path) = id
            .strip_prefix("path:")
            .or_else(|| id.strip_prefix("folder:"))
        {
            crate::app::app_ui::open_path(
                PathBuf::from(path),
                self.notices.clone(),
                self.main_hwnd.0 as isize,
            );
        }
    }

    fn widget_item_menu(&mut self, id: &str) {
        let entries = vec![
            MenuEntry::item(CMD_OPEN_ITEM, "打开"),
            MenuEntry::separator(),
            MenuEntry::item(CMD_REMOVE_ITEM, "从桌面收纳移除"),
        ];
        let Some(command) = shell_win::shellui::popup_menu(self.main_hwnd, &entries) else {
            return;
        };
        match command {
            CMD_OPEN_ITEM => self.open_widget_item(id),
            CMD_REMOVE_ITEM => {
                self.config
                    .widgets
                    .organizer
                    .items
                    .retain(|item| item != id);
                self.widget_icons.remove(id);
                self.save_config();
                self.refresh_widgets();
            }
            _ => {}
        }
    }
}

fn source_label(source: &str) -> String {
    if source.eq_ignore_ascii_case("QQMusic.exe") {
        return "QQ 音乐".into();
    }
    source.strip_suffix(".exe").unwrap_or(source).to_string()
}

fn local_time() -> (String, String) {
    // SAFETY: 调用只读取系统时间。
    let value = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let weekday = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"]
        .get(value.wDayOfWeek as usize % 7)
        .copied()
        .unwrap_or("");
    (
        format!("{:02}:{:02}", value.wHour, value.wMinute),
        format!("{}月{}日 {}", value.wMonth, value.wDay, weekday),
    )
}
