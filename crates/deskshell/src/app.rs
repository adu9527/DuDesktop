//! M3 主程序：单实例、托盘、Dock、灵动岛、应用库与壁纸宿主监督协同。
//!
//! 线程模型：Win32 消息循环、面板状态与托盘全部在 UI 线程；应用发现、图标、
//! 桌面扫描和壁纸宿主启动在后台线程，结果通过有界状态 + PostMessage 回到 UI。
//! Win32 调用只做窗口/消息循环编排，具体 Shell 操作由 `shell-win` 封装。

mod app_ui;
mod control_ui;
mod settings;
mod widgets;

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use render::{
    IconPixels, PanelKind, PanelOptions, PanelView, PanelWindow, SettingsWindow, WidgetWindow,
};
use shell_win::{
    apps::{AppEntry, AppLaunch},
    deskfiles::DesktopFile,
    running::{RunningApp, RunningWatch},
    shellui::{self, SingleInstance, TrayIcon},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
            LibraryLoader::GetModuleHandleW,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IDC_ARROW, KillTimer,
            MSG, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, WM_DISPLAYCHANGE,
            WM_DPICHANGED, WM_HOTKEY, WM_POWERBROADCAST, WM_QUIT, WM_TIMER, WM_WTSSESSION_CHANGE,
            WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::PCWSTR,
};

use crate::{
    app_workers::{self, AppsReady, FilesReady},
    config::{self, Config},
    control::ControlBridge,
    diagnostics,
    host::{HostSupervisor, SupervisorOptions, locate_host_executable},
    policy::{EffectivePolicy, PlaybackIntent, PolicyEngine, PolicySignals},
    shellstate::ShellStateJournal,
};

pub const MAIN_CLASS: &str = "DeskShell.App.Main";
pub const WM_APP_APPS: u32 = 0x8000 + 10;
pub const WM_APP_FILES: u32 = 0x8000 + 11;
pub const WM_APP_SHOW: u32 = 0x8000 + 12;
pub const WM_APP_NOTICE: u32 = 0x8000 + 13;
pub const WM_APP_HOST: u32 = 0x8000 + 14;
pub const WM_APP_CONTROL: u32 = 0x8000 + 15;
pub const WM_APP_RUNNING: u32 = 0x8000 + 16;
pub const WM_APP_RUNNING_ICONS: u32 = 0x8000 + 17;
pub const WM_APP_WEATHER: u32 = 0x8000 + 18;
pub const WM_APP_MUSIC: u32 = 0x8000 + 19;
pub const WM_APP_WIDGET_ICONS: u32 = 0x8000 + 20;
pub const WM_APP_CONTROLS: u32 = 0x8000 + 21;

const TIMER_EXIT: usize = 10;
const TIMER_SELF_TEST_TIMEOUT: usize = 11;
/// 自动规则采样周期；只在至少一条自动规则启用时注册。
const TIMER_POLICY: usize = 12;
/// 控件时钟刷新；只在控件可见时注册。
const TIMER_WIDGET_CLOCK: usize = 13;
/// 窗口事件防抖；只在运行中应用监听启用时使用。
const TIMER_RUNNING_EVENT: usize = 14;
/// 窗口事件钩子不可用时的低频回退。
const TIMER_RUNNING_FALLBACK: usize = 15;
/// 天气刷新；只在天气小部件启用时注册。
const TIMER_WEATHER: usize = 16;
/// 热键注册重试；旧实例退出较慢时热键仍被占用。
const TIMER_HOTKEY_RETRY: usize = 17;
/// 热键重试上限，约 20 秒后放弃并记录日志。
const HOTKEY_RETRY_LIMIT: u32 = 10;
const WEATHER_REFRESH_MS: u32 = 30 * 60 * 1000;
const RUNNING_DEBOUNCE_MS: u32 = 300;
const RUNNING_FALLBACK_MS: u32 = 5_000;
const POLICY_SAMPLE_MS: u32 = 2_000;
const HOTKEY_DOCK: i32 = 1;
const HOTKEY_ISLAND: i32 = 2;
const HOTKEY_SETTINGS: i32 = 3;

pub struct HostReady {
    pub supervisor: Option<HostSupervisor>,
    pub instances: Vec<protocol::InstanceConfig>,
    pub error: Option<String>,
}

type BrowserIconBatch = Arc<Mutex<Option<Vec<(bool, String, IconPixels)>>>>;
type IconBatch = Arc<Mutex<Option<Vec<(String, IconPixels)>>>>;

pub struct AppState {
    pub main_hwnd: HWND,
    pub dock: PanelWindow,
    pub island: PanelWindow,
    pub library: PanelWindow,
    /// 按需创建的自绘设置窗；关闭后释放窗口与阴影。
    pub settings: Option<SettingsWindow>,
    /// 小部件窗口；每个启用的小部件一个无边框玻璃窗口。
    pub widgets: Vec<WidgetWindow>,
    pub widget_weather: Option<crate::weather::WeatherData>,
    pub widget_weather_error: Option<String>,
    pub widget_weather_busy: bool,
    pub widget_weather_store: Arc<Mutex<Option<Result<crate::weather::WeatherData, String>>>>,
    pub widget_music: Option<crate::music::MusicState>,
    pub music_worker: Option<crate::music::MusicWorker>,
    pub widget_controls: Option<render::ControlsView>,
    pub controls_worker: Option<crate::controls::ControlsWorker>,
    pub widget_icons: HashMap<String, IconPixels>,
    pub widget_icons_attempted: std::collections::HashSet<String>,
    pub widget_icon_batch: IconBatch,
    /// 运行中应用窗口；只在启用运行项时枚举。
    pub running_apps: Vec<RunningApp>,
    pub running_icons: HashMap<String, IconPixels>,
    pub running_icons_attempted: std::collections::HashSet<String>,
    pub running_icon_batch: IconBatch,
    pub running_watch: Option<RunningWatch>,
    pub running_fallback: bool,
    pub tray: TrayIcon,
    pub config: Config,
    pub config_path: std::path::PathBuf,
    pub config_read_only: bool,
    /// 原始配置根表：保存时保留未知字段。
    pub config_raw: Option<toml::Value>,
    pub policy_engine: PolicyEngine,
    pub effective_policy: Option<EffectivePolicy>,
    /// 壁纸目标显示器的稳定身份；用于逐屏覆盖与自动规则作用域。
    pub policy_display: Option<String>,
    pub host_bootstrapping: bool,
    /// 托盘“恢复桌面”是本次运行的临时停止，不丢失素材配置。
    pub wallpaper_stopped: bool,
    pub apps: Vec<AppEntry>,
    pub app_icons: HashMap<String, IconPixels>,
    pub files: Vec<DesktopFile>,
    pub file_icons: HashMap<String, IconPixels>,
    pub file_warnings: Vec<String>,
    pub island_expanded: bool,
    pub island_tab: render::IslandTab,
    pub island_selected: Option<String>,
    pub island_status: Option<String>,
    pub island_visible: bool,
    pub dock_visible: bool,
    pub apps_ready: bool,
    pub files_ready: bool,
    pub supervisor: Option<HostSupervisor>,
    pub supervisor_instances: Vec<protocol::InstanceConfig>,
    pub files_stop: Arc<AtomicBool>,
    pub apps_store: Arc<Mutex<Option<AppsReady>>>,
    pub files_store: Arc<Mutex<Option<FilesReady>>>,
    pub host_store: Arc<Mutex<Option<HostReady>>>,
    pub notices: Arc<Mutex<Vec<String>>>,
    pub self_test: bool,
    pub self_test_finished: bool,
    pub self_test_error: Option<String>,
    pub hotkeys: Vec<i32>,
    /// 首次注册失败、等待重试的热键（id, 虚拟键）。
    pub hotkey_pending: Vec<(i32, u32)>,
    pub hotkey_retry: u32,
    /// 版本化 CLI 控制通道；请求由 UI 线程处理。
    pub control: Arc<ControlBridge>,
    pub control_thread: Option<thread::JoinHandle<()>>,
    pub icon_batch: BrowserIconBatch,
    pub icons_loading: bool,
    pub icons_attempted: std::collections::HashSet<String>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut self_test = false;
    let mut seconds = None;
    let mut config_override = None;
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--self-test" => self_test = true,
            "--seconds" => {
                seconds = Some(
                    arguments
                        .next()
                        .ok_or("--seconds 缺少参数")?
                        .parse::<u64>()
                        .map_err(|_| "--seconds 需要数字")?,
                )
            }
            "--config" => {
                config_override = Some(std::path::PathBuf::from(
                    arguments.next().ok_or("--config 缺少参数")?,
                ))
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    shell_win::enable_per_monitor_dpi_awareness();
    shell_win::shellui::hide_console_window();
    let _apartment = ComApartment::enter()?;
    let Some(_instance) = SingleInstance::acquire(r"Local\DeskShell.Instance")
        .map_err(|error| format!("单实例初始化失败：{error}"))?
    else {
        // 已存在实例：请求它显示灵动岛后退出。
        shellui::post_to_window(MAIN_CLASS, WM_APP_SHOW);
        return Ok(());
    };

    let loaded = match &config_override {
        Some(path) => config::load_from(path)?,
        None => config::load()?,
    };
    let config = loaded.config;
    let config_path = loaded.path;
    let config_read_only = loaded.read_only;
    let mut loaded_warnings = loaded.warnings;
    let config_raw = loaded.raw;
    let policy_display = target_display_id(&config);
    if !config_read_only
        && loaded.migrated_from.is_some()
        && let Err(error) = config::save_with_raw(&config, &config_path, config_raw.as_ref())
    {
        loaded_warnings.push(format!("写入迁移后配置失败：{error}"));
    }

    let main_hwnd = create_main_window()?;
    if let Err(error) = diagnostics::init() {
        eprintln!("初始化诊断日志失败：{error}");
    }
    diagnostics::log(
        "info",
        "app_start",
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "protocol_version": protocol::PROTOCOL_VERSION,
            "config_version": config.version,
            "config_path": config_path.display().to_string(),
            "read_only": config_read_only,
        }),
    );
    for warning in &loaded_warnings {
        diagnostics::log(
            "warn",
            "config_warning",
            serde_json::json!({ "message": warning }),
        );
    }
    let control = ControlBridge::new()?;
    let control_thread = control.start(main_hwnd.0 as isize)?;
    let pinned = config.dock.pinned.clone();
    let dock = PanelWindow::create(
        PanelKind::Dock,
        PanelView::dock(Vec::new(), config.dock.fisheye),
        PanelOptions {
            monitor: None,
            visible: true,
        },
    )?;
    let island = PanelWindow::create(
        PanelKind::Island,
        PanelView::island(Vec::new(), "正在加载桌面文件……".into()),
        PanelOptions {
            monitor: None,
            visible: false,
        },
    )?;
    let library = PanelWindow::create(
        PanelKind::Island,
        PanelView::island(Vec::new(), "应用库".into()),
        PanelOptions {
            monitor: None,
            visible: false,
        },
    )?;
    let tray = TrayIcon::new(main_hwnd, shellui::WM_APP_TRAY, "DeskShell")?;
    let mut hotkeys = Vec::new();
    let mut hotkey_pending = Vec::new();
    for (id, key) in [
        (HOTKEY_DOCK, 0x44u32),
        (HOTKEY_ISLAND, 0x49u32),
        (HOTKEY_SETTINGS, 0x53u32),
    ] {
        match shellui::register_hotkey(main_hwnd, id, key) {
            Ok(()) => hotkeys.push(id),
            Err(error) => {
                eprintln!("{error}");
                hotkey_pending.push((id, key));
            }
        }
    }
    if !hotkey_pending.is_empty() {
        // 旧实例退出/系统启动阶段可能短暂占用热键，稍后重试。
        // SAFETY: 只在本进程窗口上注册一次性定时器。
        unsafe {
            let _ = SetTimer(Some(main_hwnd), TIMER_HOTKEY_RETRY, 2_000, None);
        }
    }

    let files_stop = Arc::new(AtomicBool::new(false));
    let notices = Arc::new(Mutex::new(loaded_warnings));
    let manual: Vec<(String, std::path::PathBuf)> = config
        .dock
        .manual_apps
        .iter()
        .map(|entry| (entry.name.clone(), std::path::PathBuf::from(&entry.path)))
        .collect();
    let apps_store = app_workers::spawn_apps(manual, pinned, main_hwnd.0 as isize, WM_APP_APPS);
    let files_store =
        app_workers::spawn_files(main_hwnd.0 as isize, WM_APP_FILES, Arc::clone(&files_stop));

    let mut state = AppState {
        main_hwnd,
        dock,
        island,
        library,
        settings: None,
        widgets: Vec::new(),
        widget_weather: None,
        widget_weather_error: None,
        widget_weather_busy: false,
        widget_weather_store: Arc::new(Mutex::new(None)),
        widget_music: None,
        music_worker: None,
        widget_controls: None,
        controls_worker: None,
        widget_icons: HashMap::new(),
        widget_icons_attempted: Default::default(),
        widget_icon_batch: Arc::new(Mutex::new(None)),
        running_apps: Vec::new(),
        running_icons: HashMap::new(),
        running_icons_attempted: Default::default(),
        running_icon_batch: Arc::new(Mutex::new(None)),
        running_watch: None,
        running_fallback: false,
        tray,
        config,
        config_path,
        config_read_only,
        config_raw,
        policy_engine: PolicyEngine::default(),
        effective_policy: None,
        policy_display,
        host_bootstrapping: false,
        wallpaper_stopped: false,
        apps: Vec::new(),
        app_icons: HashMap::new(),
        files: Vec::new(),
        file_icons: HashMap::new(),
        file_warnings: Vec::new(),
        island_expanded: false,
        island_tab: render::IslandTab::Files,
        island_selected: None,
        island_status: None,
        island_visible: false,
        dock_visible: true,
        apps_ready: false,
        files_ready: false,
        supervisor: None,
        supervisor_instances: Vec::new(),
        files_stop,
        apps_store,
        files_store,
        host_store: Arc::new(Mutex::new(None)),
        notices,
        self_test,
        self_test_finished: false,
        self_test_error: None,
        hotkeys,
        hotkey_pending,
        hotkey_retry: 0,
        control,
        control_thread: Some(control_thread),
        icon_batch: Arc::new(Mutex::new(None)),
        icons_loading: false,
        icons_attempted: Default::default(),
    };
    state.evaluate_policy(true);
    state.refresh_dock();
    state.refresh_island();
    state.sync_running_watch();
    state.refresh_running_apps();
    state.start_wallpaper();
    state.sync_policy_timer();
    state.sync_widgets();
    state.sync_widget_workers();
    if let Err(error) = shell_win::desktop::register_session_notifications(main_hwnd) {
        eprintln!("注册会话通知失败：{error}");
    }
    if self_test {
        // 自检不弹出首次引导，也不做持久化修改。
        state.config.onboarding.desktop_icons_asked = true;
        // SAFETY: 只在本窗口上安排一次性超时定时器。
        unsafe {
            SetTimer(Some(main_hwnd), TIMER_SELF_TEST_TIMEOUT, 12_000, None);
        }
    } else if let Some(seconds) = seconds {
        // SAFETY: 一次性退出定时器。
        unsafe {
            SetTimer(
                Some(main_hwnd),
                TIMER_EXIT,
                seconds.min(3600) as u32 * 1000,
                None,
            );
        }
    }

    let exit_code = message_loop(&mut state)?;
    state.shutdown()?;
    if let Some(error) = state.self_test_error.take() {
        return Err(error);
    }
    if exit_code != 0 {
        return Err(format!("消息循环异常退出：{exit_code}"));
    }
    Ok(())
}

fn message_loop(state: &mut AppState) -> Result<i32, String> {
    let mut message = MSG::default();
    loop {
        // SAFETY: 标准 Win32 消息循环；只处理本线程创建的窗口。
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == 0 {
            return Ok(message.wParam.0 as i32);
        }
        if result.0 == -1 {
            return Err(format!(
                "读取窗口消息失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        if message.message == WM_QUIT {
            return Ok(message.wParam.0 as i32);
        }
        if state.island.owns_input(message.hwnd) || state.library.owns_input(message.hwnd) {
            if message.message == 0x0100 && message.wParam.0 == 0x09 {
                let panel = if state.island.owns_input(message.hwnd) {
                    &state.island
                } else {
                    &state.library
                };
                panel.cycle_input(message.hwnd);
                continue;
            }
            // 不截获 Enter/Escape/方向键：它们属于 IME 组合和下拉框的原生语义。
            if message.message == 0x0100 && message.wParam.0 == 0x75 {
                let panel = if state.island.owns_input(message.hwnd) {
                    &state.island
                } else {
                    &state.library
                };
                panel.focus_list();
                continue;
            }
            // SAFETY: 原生编辑控件处理真实字符和 IME 消息。
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            continue;
        }
        if message.hwnd == state.dock.hwnd() {
            let events = state
                .dock
                .handle_message(message.message, message.wParam, message.lParam);
            state.handle_dock_events(events);
            continue;
        }
        if message.hwnd == state.island.hwnd() {
            let events =
                state
                    .island
                    .handle_message(message.message, message.wParam, message.lParam);
            state.handle_island_events(events);
            state.load_browser_icons();
            continue;
        }
        if message.hwnd == state.library.hwnd() {
            let events =
                state
                    .library
                    .handle_message(message.message, message.wParam, message.lParam);
            state.handle_library_events(events);
            state.load_browser_icons();
            continue;
        }
        if message.hwnd == state.main_hwnd {
            state.handle_main_message(message.message, message.wParam, message.lParam)?;
            continue;
        }
        if state
            .settings
            .as_ref()
            .is_some_and(|settings| settings.hwnd() == message.hwnd)
        {
            let events = state
                .settings
                .as_mut()
                .expect("设置窗仍存在")
                .handle_message(message.message, message.wParam, message.lParam);
            state.handle_settings_events(events);
            continue;
        }
        if let Some(index) = state
            .widgets
            .iter()
            .position(|widget| widget.hwnd() == message.hwnd)
        {
            let events = state.widgets[index].handle_message(
                message.message,
                message.wParam,
                message.lParam,
            );
            state.handle_widget_events(message.hwnd, events);
            continue;
        }
        // SAFETY: 非本程序面板的窗口消息交给系统默认处理。
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

impl AppState {
    fn handle_main_message(
        &mut self,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<(), String> {
        match message {
            0x8061 => {
                let batch = self
                    .icon_batch
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                self.icons_loading = false;
                if let Some(batch) = batch {
                    for (app, id, icon) in batch {
                        if app {
                            self.app_icons.insert(id, icon);
                        } else {
                            self.file_icons.insert(id, icon);
                        }
                    }
                    let keep_apps: std::collections::HashSet<String> = self
                        .library
                        .view()
                        .visible_indices()
                        .into_iter()
                        .filter_map(|index| {
                            self.library
                                .view()
                                .items
                                .get(index)
                                .map(|item| item.id.clone())
                        })
                        .chain(self.config.dock.pinned.iter().cloned())
                        .collect();
                    let keep_files: std::collections::HashSet<String> = self
                        .island
                        .view()
                        .visible_indices()
                        .into_iter()
                        .filter_map(|index| {
                            self.island
                                .view()
                                .items
                                .get(index)
                                .map(|item| item.id.clone())
                        })
                        .collect();
                    if self.app_icons.len() > 64 {
                        self.app_icons.retain(|id, _| keep_apps.contains(id));
                    }
                    if self.file_icons.len() > 32 {
                        self.file_icons.retain(|id, _| keep_files.contains(id));
                    }
                    self.icons_attempted
                        .retain(|id| keep_apps.contains(id) || keep_files.contains(id));
                    self.refresh_library();
                    self.refresh_island();
                }
                self.load_browser_icons();
                Ok(())
            }
            WM_HOTKEY => {
                match wparam.0 as i32 {
                    HOTKEY_SETTINGS => self.show_settings(),
                    HOTKEY_DOCK => {
                        self.refresh_dock();
                        self.dock_visible = true;
                        self.dock.summon(false)?;
                    }
                    HOTKEY_ISLAND => {
                        self.library.hide();
                        self.island_expanded = true;
                        self.island_visible = true;
                        self.refresh_island();
                        self.island.summon(true)?;
                    }
                    _ => {}
                }
                Ok(())
            }
            shellui::WM_APP_TRAY => {
                // lparam 低字是托盘鼠标消息；右键弹出菜单，双击唤起灵动岛。
                let mouse = (lparam.0 & 0xFFFF) as u32;
                if mouse == 0x0205 || mouse == 0x007B {
                    self.show_tray_menu();
                } else if mouse == 0x0203 {
                    self.island_expanded = true;
                    self.island_visible = true;
                    self.refresh_island();
                    self.island.summon(true)?;
                }
                Ok(())
            }
            WM_APP_APPS => {
                let ready = self
                    .apps_store
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some(ready) = ready {
                    self.apps = ready.entries;
                    self.app_icons = ready.icons;
                    for warning in ready.warnings {
                        self.push_notice(warning);
                    }
                    self.apps_ready = true;
                    self.refresh_dock();
                    self.refresh_library();
                    self.maybe_finish_self_test();
                }
                Ok(())
            }
            WM_APP_FILES => {
                let ready = self
                    .files_store
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some(ready) = ready {
                    self.files = ready.files;
                    self.file_icons = ready.icons;
                    self.file_warnings = ready.warnings;
                    self.files_ready = true;
                    self.refresh_island();
                    if !self.self_test && !self.config.onboarding.desktop_icons_asked {
                        self.ask_about_desktop_icons();
                    }
                    self.maybe_finish_self_test();
                }
                Ok(())
            }
            WM_APP_NOTICE => {
                let notices = std::mem::take(
                    &mut *self
                        .notices
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()),
                );
                for notice in notices {
                    self.push_notice(notice);
                }
                Ok(())
            }
            WM_APP_HOST => {
                let ready = self
                    .host_store
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                self.host_bootstrapping = false;
                if let Some(ready) = ready {
                    let connected = ready.supervisor.is_some();
                    if connected {
                        self.supervisor_instances = ready.instances;
                    }
                    self.supervisor = ready.supervisor;
                    if let Some(error) = ready.error {
                        diagnostics::log(
                            "error",
                            "host_bootstrap_failed",
                            serde_json::json!({ "message": error }),
                        );
                        self.push_notice(format!("壁纸宿主启动失败：{error}"));
                    } else {
                        diagnostics::log(
                            "info",
                            "host_bootstrap_connected",
                            serde_json::json!({
                                "status": self
                                    .supervisor
                                    .as_ref()
                                    .map(|supervisor| supervisor.status().describe())
                                    .unwrap_or_default(),
                            }),
                        );
                        self.push_notice("壁纸宿主已连接".into());
                        // 握手期间用户可能已修改画质或素材，连接后立即收敛。
                        self.evaluate_policy(true);
                    }
                }
                Ok(())
            }
            WM_APP_SHOW => {
                self.island_expanded = true;
                self.island_visible = true;
                self.refresh_island();
                self.island.summon(true)?;
                Ok(())
            }
            WM_APP_CONTROL => {
                self.handle_control();
                Ok(())
            }
            WM_APP_RUNNING => {
                // 窗口事件可能连续到达；重置防抖定时器，安静后再枚举。
                // SAFETY: 只操作主窗口定时器。
                unsafe {
                    let _ = SetTimer(
                        Some(self.main_hwnd),
                        TIMER_RUNNING_EVENT,
                        RUNNING_DEBOUNCE_MS,
                        None,
                    );
                }
                Ok(())
            }
            WM_APP_RUNNING_ICONS => {
                let batch = self
                    .running_icon_batch
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some(batch) = batch {
                    for (id, icon) in batch {
                        self.running_icons.insert(id, icon);
                    }
                    self.refresh_dock();
                }
                Ok(())
            }
            WM_APP_WIDGET_ICONS => {
                let batch = self
                    .widget_icon_batch
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some(batch) = batch {
                    for (id, icon) in batch {
                        self.widget_icons.insert(id, icon);
                    }
                    self.refresh_widgets();
                }
                Ok(())
            }
            WM_APP_WEATHER => {
                let result = self
                    .widget_weather_store
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                self.widget_weather_busy = false;
                if let Some(result) = result {
                    match result {
                        Ok(weather) => {
                            self.widget_weather = Some(weather);
                            self.widget_weather_error = None;
                        }
                        Err(error) => self.widget_weather_error = Some(error),
                    }
                    self.refresh_widgets();
                }
                Ok(())
            }
            WM_APP_MUSIC => {
                if let Some(worker) = &self.music_worker {
                    self.widget_music = worker.take();
                    self.refresh_widgets();
                }
                Ok(())
            }
            WM_APP_CONTROLS => {
                if let Some(worker) = &self.controls_worker {
                    self.widget_controls = worker.take();
                    self.refresh_widgets();
                }
                Ok(())
            }
            WM_POWERBROADCAST => {
                // 只在恢复/电源状态变化时重采样；挂起事件本身不触发工作。
                if matches!(wparam.0 as u32, 0x0007 | 0x0012 | 0x000A) {
                    self.evaluate_policy(true);
                }
                Ok(())
            }
            WM_WTSSESSION_CHANGE => {
                // 锁定/解锁后重新校验宿主与策略；实际附着由宿主处理系统事件。
                self.evaluate_policy(true);
                Ok(())
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                for panel in [&mut self.dock, &mut self.island, &mut self.library] {
                    let _ = panel.handle_message(message, wparam, lparam);
                }
                for widget in &mut self.widgets {
                    let _ = widget.handle_message(message, wparam, lparam);
                }
                self.refresh_policy_display();
                self.evaluate_policy(true);
                Ok(())
            }
            0x0015 | 0x001A | 0x031A => {
                for panel in [&mut self.dock, &mut self.island, &mut self.library] {
                    let _ = panel.handle_message(message, wparam, lparam);
                }
                for widget in &mut self.widgets {
                    let _ = widget.handle_message(message, wparam, lparam);
                }
                Ok(())
            }
            WM_TIMER => {
                let timer = wparam.0;
                if timer == TIMER_POLICY {
                    self.evaluate_policy(false);
                } else if timer == TIMER_WIDGET_CLOCK {
                    self.refresh_widgets();
                } else if timer == TIMER_RUNNING_EVENT {
                    // SAFETY: 只停止一次性防抖定时器。
                    unsafe {
                        let _ = KillTimer(Some(self.main_hwnd), TIMER_RUNNING_EVENT);
                    }
                    self.refresh_running_apps();
                } else if timer == TIMER_RUNNING_FALLBACK {
                    self.refresh_running_apps();
                } else if timer == TIMER_WEATHER {
                    self.fetch_weather();
                } else if timer == TIMER_HOTKEY_RETRY {
                    self.retry_hotkeys();
                } else if timer == TIMER_EXIT {
                    // SAFETY: 只结束本线程消息循环。
                    unsafe {
                        PostQuitMessage(0);
                    }
                } else if timer == TIMER_SELF_TEST_TIMEOUT
                    && self.self_test
                    && !self.self_test_finished
                {
                    self.self_test_finished = true;
                    self.self_test_error = Some(format!(
                        "自检超时：应用 {}，文件 {}，宿主 {}",
                        self.apps_ready,
                        self.files_ready,
                        self.supervisor.is_some()
                    ));
                    // SAFETY: 只结束本线程消息循环。
                    unsafe {
                        PostQuitMessage(1);
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn shutdown(&mut self) -> Result<(), String> {
        diagnostics::log("info", "app_shutdown", serde_json::json!({}));
        self.running_watch = None;
        self.settings = None;
        self.widgets.clear();
        self.music_worker = None;
        self.controls_worker = None;
        self.control.stop();
        if let Some(thread) = self.control_thread.take() {
            let _ = thread.join();
        }
        self.files_stop.store(true, Ordering::Release);
        self.restore_desktop_icons();
        // SAFETY: 只清理本窗口上的定时器。
        unsafe {
            let _ = KillTimer(Some(self.main_hwnd), TIMER_POLICY);
            let _ = KillTimer(Some(self.main_hwnd), TIMER_WIDGET_CLOCK);
            let _ = KillTimer(Some(self.main_hwnd), TIMER_RUNNING_EVENT);
            let _ = KillTimer(Some(self.main_hwnd), TIMER_RUNNING_FALLBACK);
            let _ = KillTimer(Some(self.main_hwnd), TIMER_WEATHER);
            let _ = KillTimer(Some(self.main_hwnd), TIMER_HOTKEY_RETRY);
        }
        shell_win::desktop::unregister_session_notifications(self.main_hwnd);
        for id in std::mem::take(&mut self.hotkeys) {
            shellui::unregister_hotkey(self.main_hwnd, id);
        }
        if let Some(mut supervisor) = self.supervisor.take() {
            supervisor.shutdown();
        }
        if !self.config_read_only
            && !self.self_test
            && let Err(error) =
                config::save_with_raw(&self.config, &self.config_path, self.config_raw.as_ref())
        {
            eprintln!("保存配置失败：{error}");
        }
        // SAFETY: 所有面板与托盘在 AppState 释放时清理；这里只销毁隐藏主窗口。
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.main_hwnd);
        }
        Ok(())
    }

    /// 重试启动阶段被占用的热键；全部成功或达到上限后停止定时器。
    fn retry_hotkeys(&mut self) {
        let pending = std::mem::take(&mut self.hotkey_pending);
        for (id, key) in pending {
            match shellui::register_hotkey(self.main_hwnd, id, key) {
                Ok(()) => self.hotkeys.push(id),
                Err(_) => self.hotkey_pending.push((id, key)),
            }
        }
        self.hotkey_retry += 1;
        if self.hotkey_pending.is_empty() || self.hotkey_retry >= HOTKEY_RETRY_LIMIT {
            // SAFETY: 只停止本窗口上的重试定时器。
            unsafe {
                let _ = KillTimer(Some(self.main_hwnd), TIMER_HOTKEY_RETRY);
            }
            for (id, _) in std::mem::take(&mut self.hotkey_pending) {
                eprintln!("热键 {id} 重试后仍未注册：可能被其他程序占用");
            }
        }
    }

    fn push_notice(&mut self, notice: String) {
        eprintln!("{notice}");
        self.island_status = Some(notice.clone());
        let short: String = notice.chars().take(40).collect();
        self.tray.set_tooltip(&format!("DeskShell · {short}"));
        self.refresh_island();
    }

    fn ask_about_desktop_icons(&mut self) {
        self.config.onboarding.desktop_icons_asked = true;
        if !shell_win::desktop::desktop_icons_visible().unwrap_or(true) {
            // 用户已经自行隐藏，不覆盖。
            self.save_config();
            return;
        }
        if shellui::confirm(
            "DeskShell 桌面文件",
            "桌面文件入口已经就绪。\n是否隐藏系统桌面图标？\n可以随时从托盘菜单恢复。",
            self.island.hwnd(),
        ) {
            match self.set_desktop_icons(false) {
                Ok(()) => {
                    self.config.onboarding.desktop_icons_hidden_by_us = true;
                    self.push_notice("桌面图标已隐藏，可从托盘恢复".into());
                }
                Err(error) => self.push_notice(format!("隐藏桌面图标失败：{error}")),
            }
        }
        self.save_config();
    }

    fn set_desktop_icons(&mut self, visible: bool) -> Result<(), String> {
        let current = shell_win::desktop::desktop_icons_visible()?;
        if current == visible {
            return Ok(());
        }
        let path = ShellStateJournal::default_path().ok_or("无法解析 Shell 状态日志路径")?;
        let mut journal = ShellStateJournal::open(path)?;
        journal.begin(
            "desktop-icons",
            if current { "visible" } else { "hidden" }.into(),
            if visible { "visible" } else { "hidden" }.into(),
        )?;
        match shell_win::desktop::set_desktop_icons_visible(visible) {
            Ok(_) => {
                journal.mark_applied("desktop-icons")?;
                Ok(())
            }
            Err(error) => {
                let _ = journal.mark_failed("desktop-icons", &error);
                Err(error)
            }
        }
    }

    fn restore_desktop_icons(&mut self) {
        if !self.config.onboarding.desktop_icons_hidden_by_us {
            return;
        }
        match shell_win::desktop::desktop_icons_visible() {
            Ok(true) => {
                self.config.onboarding.desktop_icons_hidden_by_us = false;
            }
            Ok(false) => match self.set_desktop_icons(true) {
                Ok(()) => {
                    self.config.onboarding.desktop_icons_hidden_by_us = false;
                    if let Some(path) = ShellStateJournal::default_path()
                        && let Ok(mut journal) = ShellStateJournal::open(path)
                    {
                        let _ = journal.mark_recovered("desktop-icons");
                    }
                }
                Err(error) => eprintln!("退出时恢复桌面图标失败：{error}"),
            },
            Err(error) => eprintln!("查询桌面图标状态失败：{error}"),
        }
    }

    fn save_config(&mut self) {
        if self.config_read_only || self.self_test {
            return;
        }
        if let Err(error) =
            config::save_with_raw(&self.config, &self.config_path, self.config_raw.as_ref())
        {
            self.push_notice(format!("保存配置失败：{error}"));
        }
    }

    /// 采样平台信号并求值；`force_apply` 用于用户显式修改或系统事件。
    /// 只在有效播放策略变化时下发宿主配置，避免定时器造成版本抖动。
    pub fn evaluate_policy(&mut self, force_apply: bool) {
        let power = shell_win::system_state::power_state();
        let foreground = shell_win::system_state::foreground_state();
        let foreground_display = foreground
            .fullscreen_display
            .clone()
            .or_else(|| foreground.covered_display.clone());
        let on_target = match (&self.policy_display, &foreground_display) {
            (Some(target), Some(display)) => target.eq_ignore_ascii_case(display),
            (None, _) => true,
            _ => false,
        };
        let signals = PolicySignals {
            fullscreen: on_target && foreground.fullscreen_display.is_some(),
            obscured: on_target && foreground.covered_display.is_some(),
            on_battery: power.on_battery,
            battery_saver: power.battery_saver,
            remote_session: shell_win::system_state::remote_session(),
            foreground_app: if on_target {
                foreground.process_name
            } else {
                None
            },
        };
        let mut profile = self.config.quality.clone();
        if self.self_test {
            // 自检期间固定基线策略，避免前台窗口状态导致不确定的宿主重配。
            profile.auto.fullscreen = false;
            profile.auto.obscured = false;
            profile.auto.battery = false;
            profile.auto.remote_session = false;
            profile.auto.apps.clear();
        }
        let before = self.effective_policy.clone();
        let policy = self.policy_engine.evaluate(
            &profile,
            self.policy_display.as_deref(),
            self.config.wallpaper.paused,
            &signals,
            Instant::now(),
        );
        let playback_changed =
            before.as_ref().map(|previous| previous.playback) != Some(policy.playback);
        let ui_changed = before
            .as_ref()
            .map(|previous| previous.settings != policy.settings)
            .unwrap_or(true);
        self.effective_policy = Some(policy.clone());
        if ui_changed || force_apply {
            self.apply_panel_effects();
            self.refresh_dock();
        }
        if playback_changed || force_apply {
            self.apply_policy_to_wallpaper();
        }
        if !self.self_test && before.is_some() && playback_changed {
            diagnostics::log(
                "info",
                "policy_playback_changed",
                serde_json::json!({
                    "playback": format!("{:?}", policy.playback),
                    "preset": policy.preset.label(),
                    "active_reduction": policy.active_reduction,
                    "pending_reduction": policy.pending_reduction,
                    "effects": policy.effects,
                }),
            );
            let message = match policy.playback {
                PlaybackIntent::Animate => "壁纸已恢复播放".to_string(),
                PlaybackIntent::Static => format!(
                    "壁纸已停在当前帧：{}",
                    policy
                        .active_reduction
                        .clone()
                        .or_else(|| policy.pending_reduction.clone())
                        .unwrap_or_else(|| policy.preset.label().into())
                ),
                PlaybackIntent::Paused => "壁纸已暂停".to_string(),
            };
            self.push_notice(message);
        }
        self.update_tray_tooltip(&policy);
        self.refresh_widgets();
    }

    fn apply_policy_to_wallpaper(&mut self) {
        self.recover_wallpaper_source();
        if self.wallpaper_stopped {
            if let Some(supervisor) = &self.supervisor {
                let _ = supervisor.set_instances(Vec::new());
                self.supervisor_instances.clear();
            }
            return;
        }
        let instance = match build_wallpaper_instance(&self.config, self.effective_policy.as_ref())
        {
            Ok(Some(instance)) => instance,
            Ok(None) => {
                if let Some(supervisor) = &self.supervisor {
                    let _ = supervisor.set_instances(Vec::new());
                    self.supervisor_instances.clear();
                }
                return;
            }
            Err(error) => {
                self.push_notice(error);
                return;
            }
        };
        if let Some(supervisor) = &self.supervisor {
            if self.supervisor_instances.len() == 1 && self.supervisor_instances[0] == instance {
                return;
            }
            match supervisor.set_instances(vec![instance.clone()]) {
                Ok(_) => {
                    self.supervisor_instances = vec![instance];
                    self.remember_good_source();
                }
                Err(error) => self.push_notice(format!("应用壁纸策略失败：{}", error.message)),
            }
        } else if !self.host_bootstrapping {
            self.start_wallpaper();
        }
    }

    /// 当前素材缺失时回退到最近一次成功素材；不移动或删除用户文件。
    fn recover_wallpaper_source(&mut self) {
        let Some(source) = self.config.wallpaper.source.clone() else {
            return;
        };
        if Path::new(&source).is_file() {
            return;
        }
        let fallback = self.config.wallpaper.last_good_source.clone();
        if let Some(fallback) = fallback
            && fallback != source
            && Path::new(&fallback).is_file()
        {
            self.config.wallpaper.source = Some(fallback.clone());
            self.config.wallpaper.managed = fallback
                .to_ascii_lowercase()
                .contains("\\deskshell\\assets\\");
            self.push_notice(format!("素材缺失，已回退到上次可用素材：{fallback}"));
            self.save_config();
        } else {
            self.push_notice(format!("壁纸素材不存在：{source}"));
        }
    }

    fn remember_good_source(&mut self) {
        let Some(source) = self.config.wallpaper.source.clone() else {
            return;
        };
        if self.config.wallpaper.last_good_source.as_deref() != Some(source.as_str()) {
            self.config.wallpaper.last_good_source = Some(source);
            self.save_config();
        }
    }

    /// 确保宿主已启动；素材为空时保持系统静态桌面。
    pub fn start_wallpaper(&mut self) {
        self.recover_wallpaper_source();
        if self.host_bootstrapping || self.supervisor.is_some() || self.wallpaper_stopped {
            return;
        }
        match build_wallpaper_instance(&self.config, self.effective_policy.as_ref()) {
            Ok(Some(instance)) => {
                self.host_store = start_host(instance.clone(), self.main_hwnd);
                self.host_bootstrapping = true;
                self.supervisor_instances = vec![instance];
            }
            Ok(None) => {}
            Err(error) => self.push_notice(error),
        }
    }

    /// 自动规则启用时才注册采样定时器；关闭后立即停止唤醒。
    pub fn sync_policy_timer(&self) {
        let enabled = !self.self_test && self.config.quality.auto.any_enabled();
        // SAFETY: 只操作本窗口的定时器。
        unsafe {
            if enabled {
                if SetTimer(Some(self.main_hwnd), TIMER_POLICY, POLICY_SAMPLE_MS, None) == 0 {
                    eprintln!("注册画质采样定时器失败");
                }
            } else {
                let _ = KillTimer(Some(self.main_hwnd), TIMER_POLICY);
            }
        }
    }

    /// 把有效策略里的效果开关应用到面板；关闭阴影会销毁分层窗口与位图。
    pub fn apply_panel_effects(&mut self) {
        let shadows = self
            .effective_policy
            .as_ref()
            .map(|policy| policy.settings.shadows)
            .unwrap_or(true);
        self.dock.set_shadows(shadows);
        self.island.set_shadows(shadows);
        self.library.set_shadows(shadows);
    }

    pub fn refresh_policy_display(&mut self) {
        self.policy_display = target_display_id(&self.config);
        self.policy_engine.clear();
    }

    fn update_tray_tooltip(&mut self, policy: &EffectivePolicy) {
        let playback = match policy.playback {
            PlaybackIntent::Animate => "播放",
            PlaybackIntent::Static => "静态",
            PlaybackIntent::Paused => "暂停",
        };
        self.tray.set_tooltip(&format!(
            "DeskShell · {} · {playback}",
            policy.preset.label()
        ));
    }

    /// 状态面板使用的完整策略描述；不包含未验证的硬件能力声明。
    pub fn policy_report(&self) -> String {
        let Some(policy) = &self.effective_policy else {
            return "画质策略尚未求值".into();
        };
        let mut lines = Vec::new();
        lines.push(format!("预设：{}", policy.preset.label()));
        lines.push(format!(
            "实际呈现：{}",
            match policy.playback {
                PlaybackIntent::Animate => "正常播放",
                PlaybackIntent::Static => "停在当前帧（不持续解码）",
                PlaybackIntent::Paused => "用户暂停",
            }
        ));
        lines.push(format!(
            "UI 动画：{}；阴影：{}；壁纸播放：{}",
            yes_no(policy.settings.ui_animations),
            yes_no(policy.settings.shadows),
            yes_no(policy.settings.wallpaper_playback)
        ));
        let effects = policy.effects;
        let labels = effects.labels();
        lines.push(format!(
            "图片效果：{}",
            if labels.is_empty() {
                "关闭".to_string()
            } else {
                format!(
                    "调色 {} / 暗角 {} / 模糊 {} / Bloom {} / 粒子 {} / 渐变 {}",
                    effects.color,
                    effects.vignette,
                    effects.blur,
                    effects.bloom,
                    effects.particles,
                    effects.gradient
                )
            }
        ));
        match self.dock.shadow_error() {
            Some(error) => lines.push(format!("阴影不可用：{error}")),
            None => lines.push(format!(
                "阴影窗口：{}",
                if self.dock.shadow_ready() {
                    "已启用"
                } else {
                    "已关闭"
                }
            )),
        }
        for reason in policy.reasons() {
            lines.push(format!("原因：{reason}"));
        }
        if let Some(display) = &policy.display {
            lines.push(format!("作用显示器：{display}"));
        }
        lines.join("\n")
    }

    fn maybe_finish_self_test(&mut self) {
        if !self.self_test || self.self_test_finished || !self.apps_ready || !self.files_ready {
            return;
        }
        self.self_test_finished = true;
        match app_ui::run_self_test(self) {
            Ok(()) => {
                // SAFETY: 只结束本线程消息循环。
                unsafe {
                    PostQuitMessage(0);
                }
            }
            Err(error) => {
                self.self_test_error = Some(error);
                // SAFETY: 只结束本线程消息循环。
                unsafe {
                    PostQuitMessage(1);
                }
            }
        }
    }
}

/// 壁纸目标显示器的稳定身份；未显式配置时使用主显示器。
fn target_display_id(config: &Config) -> Option<String> {
    config
        .wallpaper
        .display
        .clone()
        .or_else(|| shell_win::desktop::display_by_index(None).map(|display| display.id))
}

fn yes_no(value: bool) -> &'static str {
    if value { "开" } else { "关" }
}

/// 壁纸缩放方式的稳定短标签；CLI 与菜单共用。
pub(crate) fn fit_label(fit: protocol::FitMode) -> &'static str {
    match fit {
        protocol::FitMode::Cover => "cover",
        protocol::FitMode::Contain => "contain",
        protocol::FitMode::Stretch => "stretch",
        protocol::FitMode::Tile => "tile",
    }
}

fn build_wallpaper_instance(
    config: &Config,
    policy: Option<&EffectivePolicy>,
) -> Result<Option<protocol::InstanceConfig>, String> {
    let Some(source) = config.wallpaper.source.as_deref() else {
        return Ok(None);
    };
    let path = Path::new(source);
    if !path.is_file() {
        return Err(format!("壁纸素材不存在：{source}"));
    }
    let is_video = match config.wallpaper.kind.as_deref() {
        Some("video") => true,
        Some("image") => false,
        Some(other) => return Err(format!("壁纸类型只能是 image 或 video：{other}")),
        None => matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
                .as_deref(),
            Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "ts")
        ),
    };
    let source = if is_video {
        protocol::Source::Video {
            path: source.to_owned(),
            poster: config.wallpaper.poster.clone(),
            loop_playback: true,
        }
    } else {
        protocol::Source::Image {
            path: source.to_owned(),
        }
    };
    // 用户暂停由 `paused` 表达；其余情况下 Eco 与自动规则以静态模式停止持续解码。
    let playback = match policy.map(|policy| policy.playback) {
        Some(PlaybackIntent::Animate) => protocol::RenderPlayback::Animate,
        _ => protocol::RenderPlayback::Static,
    };
    let effects = policy
        .map(|policy| policy.effects)
        .unwrap_or(protocol::ImageEffects::OFF);
    Ok(Some(protocol::InstanceConfig {
        id: "wallpaper-main".into(),
        monitor: None,
        display_id: config.wallpaper.display.clone(),
        desktop: config.wallpaper.desktop,
        window: None,
        source,
        paused: config.wallpaper.paused,
        render: protocol::RenderOptions {
            playback,
            effects,
            fit: config.wallpaper.fit,
        },
    }))
}

fn start_host(
    instance: protocol::InstanceConfig,
    main_hwnd: HWND,
) -> Arc<Mutex<Option<HostReady>>> {
    let store = Arc::new(Mutex::new(None));
    let result = Arc::clone(&store);
    let notify = main_hwnd.0 as isize;
    let _ = thread::Builder::new()
        .name("host-bootstrap".into())
        .spawn(move || {
            let outcome =
                (|| -> Result<(HostSupervisor, Vec<protocol::InstanceConfig>), String> {
                    let mut options =
                        SupervisorOptions::new(locate_host_executable()?, vec![instance.clone()]);
                    options.apply_timeout = Duration::from_secs(30);
                    let supervisor = HostSupervisor::start(options)?;
                    supervisor
                        .wait_connected(Duration::from_secs(20))
                        .ok_or_else(|| {
                            format!("等待宿主连接超时：{}", supervisor.status().describe())
                        })?;
                    supervisor
                        .apply_instances(vec![instance.clone()])
                        .map_err(|error| error.message)?;
                    Ok((supervisor, vec![instance]))
                })();
            let ready = match outcome {
                Ok((supervisor, instances)) => HostReady {
                    supervisor: Some(supervisor),
                    instances,
                    error: None,
                },
                Err(error) => HostReady {
                    supervisor: None,
                    instances: Vec::new(),
                    error: Some(error),
                },
            };
            *result.lock().unwrap_or_else(|error| error.into_inner()) = Some(ready);
            shellui::post_message_raw(notify, WM_APP_HOST);
        });
    store
}

fn create_main_window() -> Result<HWND, String> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            let class: Vec<u16> = MAIN_CLASS
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: 只注册隐藏消息窗口类。
            let module = unsafe { GetModuleHandleW(None) }.map_err(|error| error.to_string())?;
            // SAFETY: 类名在注册调用期间有效。
            let atom = unsafe {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(main_window_proc),
                    hInstance: windows::Win32::Foundation::HINSTANCE(module.0),
                    hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(None, IDC_ARROW)
                        .map_err(|error| error.to_string())?,
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                })
            };
            if atom == 0 {
                return Err(format!(
                    "注册主窗口类失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })
        .clone()?;
    let class: Vec<u16> = MAIN_CLASS
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = "DeskShell"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: 创建隐藏的顶层工具窗口，用于托盘回调、热键和后台通知。
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            GetModuleHandleW(None)
                .ok()
                .map(|module| windows::Win32::Foundation::HINSTANCE(module.0)),
            None,
        )
    }
    .map_err(|error| format!("创建主窗口失败：{error}"))
}

unsafe extern "system" fn main_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 默认窗口过程由系统调用；业务消息由主循环分发。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct ComApartment {
    owned: bool,
}

impl ComApartment {
    fn enter() -> Result<Self, String> {
        // SAFETY: 只初始化当前 UI 线程的 COM apartment。
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_ok() {
            return Ok(Self { owned: true });
        }
        if result.0 as u32 == 0x8001_0106 {
            return Ok(Self { owned: false });
        }
        Err(format!("初始化 UI COM 失败：{result:?}"))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: 与 enter 中的 CoInitializeEx 配对。
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// 应用库条目使用的 Shell 可执行身份。
pub fn launch_entry(entry: &AppEntry) -> Result<Option<u32>, String> {
    match &entry.launch {
        AppLaunch::Aumid(_) | AppLaunch::ShellPath(_) => shell_win::apps::launch_app(entry),
    }
}
