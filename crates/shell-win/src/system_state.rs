//! 自动画质规则所需的系统信号：前台窗口、全屏/遮挡、电源与远程会话。
//!
//! 这里只提供只读平台事实，不做画质决策；策略与迟滞在 `deskshell::policy` 中实现。
//! 所有调用都很短，不涉及 IPC 或阻塞等待，可由 UI 线程按低频定时器采样。

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
                MonitorFromWindow,
            },
        },
        System::{
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
        UI::WindowsAndMessaging::{
            GWL_STYLE, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowLongW,
            GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SM_REMOTESESSION,
            WS_MAXIMIZE,
        },
    },
    core::PWSTR,
};

use std::mem::size_of;

/// 前台窗口的只读状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundState {
    /// 前台进程可执行文件名（如 `game.exe`）；系统 Shell 或本进程为 `None`。
    pub process_name: Option<String>,
    /// 前台窗口完整覆盖的显示器设备名（如 `\\.\DISPLAY1`）。
    pub fullscreen_display: Option<String>,
    /// 前台窗口覆盖工作区但未完整覆盖显示器的显示器设备名。
    pub covered_display: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerState {
    pub on_battery: bool,
    pub battery_saver: bool,
}

pub fn power_state() -> PowerState {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: 只填充本函数的栈上结构。
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return PowerState::default();
    }
    PowerState {
        // ACLineStatus: 0 = 电池供电，1 = 交流电，255 = 未知。
        on_battery: status.ACLineStatus == 0,
        // SystemStatusFlag: 1 = 节能模式开启。
        battery_saver: status.SystemStatusFlag == 1,
    }
}

pub fn remote_session() -> bool {
    // SAFETY: 只读系统度量。
    unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
}

/// 采样前台窗口。系统 Shell 窗口（桌面、任务栏、开始菜单宿主）不计为全屏/遮挡，
/// 避免用户短暂打开开始菜单就把壁纸降档并保持 30 秒。
pub fn foreground_state() -> ForegroundState {
    // SAFETY: 只读取前台窗口句柄；后续调用都只查询该窗口属性。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return ForegroundState::default();
    }
    // SAFETY: hwnd 由前台窗口 API 返回；两个查询都只读窗口状态。
    let (visible, minimized) =
        unsafe { (IsWindowVisible(hwnd).as_bool(), IsIconic(hwnd).as_bool()) };
    if !visible || minimized {
        return ForegroundState::default();
    }
    let class = class_name(hwnd);
    if is_shell_window(&class) || is_cloaked(hwnd) {
        return ForegroundState::default();
    }
    let mut process_id = 0u32;
    // SAFETY: 只写入本函数的栈上变量。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    if process_id == 0 || process_id == std::process::id() {
        return ForegroundState::default();
    }
    let mut rect = RECT::default();
    // SAFETY: 只写入本函数的栈上矩形。
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return ForegroundState::default();
    }
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return ForegroundState::default();
    }
    // SAFETY: hwnd 有效；只查询最近显示器。
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let Some((display, monitor_rect, work_rect)) = monitor_rects(monitor) else {
        return ForegroundState::default();
    };
    let mut state = ForegroundState {
        process_name: process_name(process_id),
        ..ForegroundState::default()
    };
    // 隐藏任务栏时 rcWork 等于 rcMonitor，最大化窗口也会看起来“覆盖整屏”；
    // 用 WS_MAXIMIZE 排除普通最大化窗口，避免把任务栏式面板误判为全屏而隐藏。
    // SAFETY: hwnd 有效；只读取窗口样式。
    let maximized = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & WS_MAXIMIZE.0 != 0;
    if !maximized && covers(&rect, &monitor_rect) {
        state.fullscreen_display = Some(display);
    } else if covers(&rect, &work_rect) {
        state.covered_display = Some(display);
    }
    state
}

fn covers(window: &RECT, target: &RECT) -> bool {
    // 允许 2 像素误差，兼容无边框全屏与 DPI 取整。
    window.left <= target.left + 2
        && window.top <= target.top + 2
        && window.right >= target.right - 2
        && window.bottom >= target.bottom - 2
}

fn monitor_rects(monitor: HMONITOR) -> Option<(String, RECT, RECT)> {
    let mut information = MONITORINFOEXW::default();
    information.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: monitor 由系统返回，结构大小已按 MONITORINFOEXW 设置。
    if !unsafe { GetMonitorInfoW(monitor, &mut information.monitorInfo) }.as_bool() {
        return None;
    }
    let end = information
        .szDevice
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(information.szDevice.len());
    Some((
        String::from_utf16_lossy(&information.szDevice[..end]),
        information.monitorInfo.rcMonitor,
        information.monitorInfo.rcWork,
    ))
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: 缓冲区在调用期间有效，长度按元素个数传入。
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

fn is_shell_window(class: &str) -> bool {
    matches!(
        class,
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Windows.UI.Core.CoreWindow"
            | "XamlExplorerHostIslandWindow"
            | "MultitaskingViewFrame"
            | "ForegroundStaging"
    )
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: 只写入本函数的栈上 u32，大小与 DWMWA_CLOAKED 匹配。
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

fn process_name(process_id: u32) -> Option<String> {
    // SAFETY: 只请求查询权限；句柄由 ProcessHandle 负责关闭。
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let handle = ProcessHandle(handle);
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: 缓冲区与长度都在调用期间有效；成功时 length 为写入的字符数。
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .ok()?;
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_owned)
}

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcess 创建且只在此处关闭一次。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
