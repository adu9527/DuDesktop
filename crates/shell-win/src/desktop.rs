//! 桌面附着适配：WorkerW 探测、父子校验、显示器完整边界、桌面图标状态与恢复。
//!
//! 兼容性说明：
//! - `Progman` 的 `0x052C` 与 WorkerW 层级属于 Explorer 的非公开实现，不同 Windows 版本可能不同。
//!   本模块集中隔离这些约定，失败时返回明确原因，由调用方回退到系统静态壁纸。
//! - 显示器身份使用设备名（如 `\\.\DISPLAY1`）而不是临时枚举索引；索引只作为没有稳定身份时的回退。
//! - 所有坐标均为物理像素，调用方必须已启用 Per-Monitor V2 DPI 感知。

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW},
        System::RemoteDesktop::{
            NOTIFY_FOR_THIS_SESSION, ProcessIdToSessionId, WTSRegisterSessionNotification,
            WTSUnRegisterSessionNotification,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, FindWindowExW, FindWindowW, GWL_STYLE, GetClassNameW, GetParent,
            GetWindowLongPtrW, GetWindowRect, IsWindow, IsWindowVisible, SMTO_ABORTIFHUNG,
            SMTO_NORMAL, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageTimeoutW,
            SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_COMMAND, WS_CHILD, WS_POPUP,
        },
    },
    core::{BOOL, PCWSTR, w},
};

use crate::enable_per_monitor_dpi_awareness;

/// Explorer 用于生成壁纸 WorkerW 的非公开消息。
const WM_SPAWN_WORKER: u32 = 0x052C;
/// “显示桌面图标”的切换式兼容命令；必须先查询再决定是否发送。
const WM_TOGGLE_DESKTOP_ICONS: usize = 0x7402;
/// 附着有效性复检间隔：低频恢复看门狗，避免 Explorer 重建后长时间空窗。
const ATTACHMENT_GUARD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

const PROGMAN_CLASS: PCWSTR = w!("Progman");
const WORKERW_CLASS: PCWSTR = w!("WorkerW");
const DEFVIEW_CLASS: PCWSTR = w!("SHELLDLL_DefView");
const LISTVIEW_CLASS: PCWSTR = w!("SysListView32");

/// 显示器稳定身份与完整边界（`rcMonitor`，不是工作区）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// 设备名，例如 `\\.\DISPLAY1`；用于跨枚举顺序保存映射。
    pub id: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl DisplayInfo {
    pub fn bounds(&self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.left + self.width as i32,
            bottom: self.top + self.height as i32,
        }
    }
}

/// 附着失败原因；用于状态上报与诊断，不影响系统静态壁纸继续显示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// 未找到 Progman/DefView，Explorer 可能尚未就绪或正在重启。
    ExplorerUnavailable,
    /// 发送非公开消息失败，无法创建壁纸宿主。
    DispatchFailed(String),
    /// 找到宿主但 SetParent 失败。
    SetParentFailed(String),
    /// SetParent 返回成功但父子关系校验不一致。
    ParentMismatch,
    /// 显示器不存在或已断开。
    DisplayUnavailable,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExplorerUnavailable => write!(formatter, "Explorer 桌面窗口不可用"),
            Self::DispatchFailed(message) => write!(formatter, "请求壁纸宿主失败：{message}"),
            Self::SetParentFailed(message) => write!(formatter, "设置父窗口失败：{message}"),
            Self::ParentMismatch => write!(formatter, "父窗口校验不一致"),
            Self::DisplayUnavailable => write!(formatter, "目标显示器不可用"),
        }
    }
}

impl std::error::Error for AttachError {}

/// 枚举所有显示器的完整边界。顺序与系统枚举一致，供索引回退使用。
pub fn displays() -> Vec<DisplayInfo> {
    enable_per_monitor_dpi_awareness();
    let mut collected = Vec::new();
    // SAFETY: 回调只在本次枚举期间写入局部 Vec，指针有效。
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_display),
            LPARAM(&mut collected as *mut Vec<DisplayInfo> as isize),
        );
    }
    collected
}

unsafe extern "system" fn collect_display(
    monitor: HMONITOR,
    _device: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: 调用方保证 data 指向本次枚举期间存活的 Vec<DisplayInfo>。
    let displays = unsafe { &mut *(data.0 as *mut Vec<DisplayInfo>) };
    let mut information = MONITORINFOEXW::default();
    information.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: monitor 由系统提供，输出结构大小已按 MONITORINFOEXW 设置。
    if unsafe { GetMonitorInfoW(monitor, &mut information.monitorInfo) }.as_bool() {
        let end = information
            .szDevice
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(information.szDevice.len());
        let id = String::from_utf16_lossy(&information.szDevice[..end]);
        let rect = information.monitorInfo.rcMonitor;
        displays.push(DisplayInfo {
            id,
            left: rect.left,
            top: rect.top,
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
            primary: information.monitorInfo.dwFlags & 1 != 0,
        });
    }
    true.into()
}

pub fn display_by_index(index: Option<u32>) -> Option<DisplayInfo> {
    let displays = displays();
    match index {
        Some(index) => displays.get(index as usize).cloned(),
        None => displays
            .iter()
            .find(|display| display.primary)
            .cloned()
            .or_else(|| displays.first().cloned()),
    }
}

pub fn display_by_id(id: &str) -> Option<DisplayInfo> {
    displays().into_iter().find(|display| display.id == id)
}

/// 稳定身份优先、索引回退的解析顺序。
pub fn resolve_display(display_id: Option<&str>, index: Option<u32>) -> Option<DisplayInfo> {
    display_id
        .and_then(display_by_id)
        .or_else(|| display_by_index(index))
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    // SAFETY: 输出缓冲区有效；hwnd 由系统提供。
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

unsafe extern "system" fn find_defview_host(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: 调用方保证 data 指向本次枚举期间存活的 Option<HWND>。
    let slot = unsafe { &mut *(data.0 as *mut Option<HWND>) };
    // SAFETY: 只查询窗口层级；不修改窗口状态。
    let defview = unsafe { FindWindowExW(Some(window), None, DEFVIEW_CLASS, PCWSTR::null()) };
    if let Ok(defview) = defview
        && !defview.is_invalid()
    {
        *slot = Some(window);
        return false.into();
    }
    let _ = defview;
    true.into()
}

/// 找到包含 `SHELLDLL_DefView` 的顶层窗口（通常是 WorkerW 或 Progman）。
fn defview_host() -> Option<HWND> {
    let mut host = None;
    // SAFETY: 回调只在本次枚举期间写入局部变量。
    unsafe {
        let _ = EnumWindows(
            Some(find_defview_host),
            LPARAM(&mut host as *mut Option<HWND> as isize),
        );
    }
    host
}

/// 探测壁纸宿主窗口：图标层之后的那个 WorkerW；没有则回退到 DefView 的父窗口。
/// 只有首次附着需要调用；重复调用可能让 Explorer 再创建一个 WorkerW。
pub fn desktop_host() -> Result<HWND, AttachError> {
    // SAFETY: 只向 Explorer 发送已广泛使用的非公开消息；超时避免挂起消息线程。
    if let Ok(progman) = unsafe { FindWindowW(PROGMAN_CLASS, PCWSTR::null()) } {
        // SAFETY: progman 句柄有效；只请求创建壁纸 WorkerW。
        let _ = unsafe {
            SendMessageTimeoutW(
                progman,
                WM_SPAWN_WORKER,
                windows::Win32::Foundation::WPARAM(0xD),
                LPARAM(0),
                SMTO_NORMAL | SMTO_ABORTIFHUNG,
                1000,
                None,
            )
        };
    }
    locate_desktop_host().ok_or(AttachError::ExplorerUnavailable)
}

/// 查找已存在的壁纸宿主，不触发 Explorer 创建新的 WorkerW。
fn locate_desktop_host() -> Option<HWND> {
    let defview_parent = defview_host()?;
    // SAFETY: 只查询父子关系。
    let parent = unsafe { GetParent(defview_parent) }.unwrap_or(defview_parent);
    if class_name(parent) == "WorkerW" {
        // SAFETY: 查找同级的后一个 WorkerW，即壁纸层。
        let next_worker =
            unsafe { FindWindowExW(None, Some(parent), WORKERW_CLASS, PCWSTR::null()) };
        if let Ok(worker) = next_worker
            && !worker.is_invalid()
        {
            return Some(worker);
        }
    }
    Some(parent)
}

/// 把本进程窗口切换为子窗口样式；附着期间必须保持 `WS_CHILD`，
/// 否则 `GetParent` 返回所有者而不是父窗口。
fn apply_child_style(hwnd: HWND) {
    // SAFETY: hwnd 由调用方保证是本进程创建的窗口。
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let new_style = (style | WS_CHILD.0 as isize) & !(WS_POPUP.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);
    }
}

/// 一次成功的桌面附着。Drop 不自动解除，调用方必须显式 `detach` 或调用 `detach_window`。
#[derive(Debug)]
pub struct DesktopAttachment {
    hwnd: HWND,
    worker: HWND,
    display: DisplayInfo,
}

impl DesktopAttachment {
    pub fn attach(hwnd: HWND, display: DisplayInfo) -> Result<Self, AttachError> {
        let worker = desktop_host()?;
        // 先切到子窗口样式，再设置父窗口，避免顶层窗口短暂出现在桌面层。
        apply_child_style(hwnd);
        // SAFETY: 两个窗口句柄均有效；只改变父窗口。
        if let Err(error) = unsafe { SetParent(hwnd, Some(worker)) } {
            return Err(AttachError::SetParentFailed(error.message().to_string()));
        }
        // SAFETY: 校验父子关系。
        let actual = unsafe { GetParent(hwnd) }
            .map_err(|error| AttachError::SetParentFailed(format!("读取父窗口失败：{error}")))?;
        if actual != worker {
            return Err(AttachError::ParentMismatch);
        }
        let mut attachment = Self {
            hwnd,
            worker,
            display,
        };
        let initial = attachment.display.clone();
        attachment.apply_bounds(&initial)?;
        Ok(attachment)
    }

    pub fn display(&self) -> &DisplayInfo {
        &self.display
    }

    /// 附着是否仍然有效：宿主窗口存在且父子关系未被 Explorer 重建破坏。
    pub fn is_valid(&self) -> bool {
        // SAFETY: 只查询窗口状态。
        unsafe {
            IsWindow(Some(self.worker)).as_bool()
                && IsWindow(Some(self.hwnd)).as_bool()
                && GetParent(self.hwnd)
                    .map(|parent| parent == self.worker)
                    .unwrap_or(false)
        }
    }

    /// 宿主失效或显示器变化后重新探测并附着。
    pub fn reattach(&mut self, display: DisplayInfo) -> Result<(), AttachError> {
        if self.is_valid() && self.display == display {
            return Ok(());
        }
        // 优先复用已存在的壁纸宿主；只有找不到时才请求 Explorer 创建。
        let worker = match locate_desktop_host() {
            Some(worker) => worker,
            None => desktop_host()?,
        };
        // 解除附着会恢复顶层样式；重新附着必须再次切换为子窗口。
        apply_child_style(self.hwnd);
        // SAFETY: 句柄有效；重新设置父窗口并校验。
        if let Err(error) = unsafe { SetParent(self.hwnd, Some(worker)) } {
            return Err(AttachError::SetParentFailed(error.message().to_string()));
        }
        // SAFETY: 只查询父子关系。
        let actual = unsafe { GetParent(self.hwnd) }.unwrap_or_default();
        if actual != worker {
            return Err(AttachError::ParentMismatch);
        }
        self.worker = worker;
        self.apply_bounds(&display)
    }

    /// 使用显示器完整边界（`rcMonitor`），并换算为父窗口客户坐标以支持负坐标多屏。
    /// 只调整几何位置，不改变可见性；调用方决定何时显示。
    pub fn apply_bounds(&mut self, display: &DisplayInfo) -> Result<(), AttachError> {
        let mut parent_rect = RECT::default();
        // SAFETY: 两个窗口句柄有效；GetWindowRect 用于换算父窗口屏幕原点。
        if unsafe { GetWindowRect(self.worker, &mut parent_rect) }.is_err() {
            return Err(AttachError::ParentMismatch);
        }
        let x = display.left - parent_rect.left;
        let y = display.top - parent_rect.top;
        // SAFETY: 只调整本进程窗口的位置与尺寸。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                display.width as i32,
                display.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.display = display.clone();
        Ok(())
    }

    /// 附着与几何确认后显示壁纸窗口（不激活）。
    pub fn show(&self) {
        // SAFETY: 只显示本进程窗口。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
        }
    }

    pub fn detach(&mut self) {
        detach_window(self.hwnd);
    }
}

/// 解除桌面附着并恢复为隐藏的顶层窗口。
pub fn detach_window(hwnd: HWND) {
    // SAFETY: 只修改本进程窗口的父窗口与样式。
    unsafe {
        let _ = SetParent(hwnd, None);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let new_style = (style | WS_POPUP.0 as isize) & !(WS_CHILD.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// 桌面图标当前是否可见。查询真实窗口状态，不依赖注册表缓存。
pub fn desktop_icons_visible() -> Result<bool, String> {
    let host = defview_host().ok_or_else(|| "Explorer 桌面窗口不可用".to_string())?;
    // SAFETY: 只查询子窗口。
    let defview = unsafe { FindWindowExW(Some(host), None, DEFVIEW_CLASS, PCWSTR::null()) }
        .map_err(|error| format!("未找到桌面图标视图：{error}"))?;
    // SAFETY: 只查询子窗口；找不到时退回 DefView 本身。
    let listview = unsafe { FindWindowExW(Some(defview), None, LISTVIEW_CLASS, PCWSTR::null()) }
        .unwrap_or(defview);
    // SAFETY: 只查询可见性。
    Ok(unsafe { IsWindowVisible(listview).as_bool() })
}

/// 幂等设置桌面图标可见性；返回是否真的发送了切换命令。
/// `0x7402` 是切换命令，重复发送会反向生效，因此必须先查询。
pub fn set_desktop_icons_visible(visible: bool) -> Result<bool, String> {
    if desktop_icons_visible()? == visible {
        return Ok(false);
    }
    let host = defview_host().ok_or_else(|| "Explorer 桌面窗口不可用".to_string())?;
    // SAFETY: 只查询子窗口。
    let defview = unsafe { FindWindowExW(Some(host), None, DEFVIEW_CLASS, PCWSTR::null()) }
        .map_err(|error| format!("未找到桌面图标视图：{error}"))?;
    // SAFETY: 只向 Explorer 的 DefView 发送兼容命令。
    let sent = unsafe {
        SendMessageTimeoutW(
            defview,
            WM_COMMAND,
            windows::Win32::Foundation::WPARAM(WM_TOGGLE_DESKTOP_ICONS),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        )
    };
    if sent.0 == 0 {
        return Err("桌面图标切换命令未处理".into());
    }
    if desktop_icons_visible()? != visible {
        return Err("桌面图标状态未按请求改变".into());
    }
    Ok(true)
}

/// 订阅会话锁定/解锁通知；窗口销毁前必须调用 `unregister_session_notifications`。
pub fn register_session_notifications(hwnd: HWND) -> Result<(), String> {
    // SAFETY: hwnd 由调用方保证有效；只注册当前会话。
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
        .map_err(|error| format!("注册会话通知失败：{error}"))
}

pub fn unregister_session_notifications(hwnd: HWND) {
    // SAFETY: hwnd 由调用方保证有效或即将销毁。
    unsafe {
        let _ = WTSUnRegisterSessionNotification(hwnd);
    }
}

/// 当前进程所在的终端会话 ID；用于 Shell 修改日志的所有者字段。
pub fn current_session_id() -> Option<u32> {
    let mut session = 0u32;
    // SAFETY: 只查询当前进程的会话 ID。
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }
        .ok()
        .map(|_| session)
}

/// 窗口在屏幕坐标系中的矩形；用于验证桌面附着使用显示器完整边界。
pub fn window_screen_rect(hwnd: HWND) -> Option<(i32, i32, u32, u32)> {
    let mut rect = RECT::default();
    // SAFETY: 只查询窗口矩形。
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    Some((
        rect.left,
        rect.top,
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    ))
}

/// 附着失效复检的时长；供宿主安排低频看门狗。
pub fn attachment_guard_interval() -> std::time::Duration {
    ATTACHMENT_GUARD_INTERVAL
}
