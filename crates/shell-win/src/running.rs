//! 运行中应用枚举与窗口激活。
//!
//! 任务栏/Dock 只消费纯 Rust 数据；枚举在 UI 线程按事件触发，不做忙轮询。
//! `RunningWatch` 监听前台切换与窗口创建/销毁，向主窗口投递自定义消息，
//! 由主程序防抖后再枚举。

use std::{
    collections::HashMap,
    mem::size_of,
    sync::atomic::{AtomicIsize, AtomicU32, Ordering},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        System::Threading::{
            AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                BringWindowToTop, EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow,
                GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
                IsIconic, IsWindowVisible, PostMessageW, SW_RESTORE, SetForegroundWindow,
                ShowWindow, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::PWSTR,
};

const OBJID_WINDOW: i32 = 0;
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_HIDE: u32 = 0x8003;
/// 回调在注册线程的消息循环中执行，不注入目标进程。
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

/// 一个可切换的运行中应用窗口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    /// 顶层窗口句柄数值；只在本进程内使用。
    pub hwnd: isize,
    /// 进程可执行文件完整路径；用于图标与去重。
    pub process_path: String,
    /// 窗口标题。
    pub title: String,
    /// 展示名（exe 文件名去扩展名）。
    pub name: String,
}

impl RunningApp {
    /// 稳定身份：按进程路径生成，避免同一应用多个窗口重复占位。
    pub fn id(&self) -> String {
        format!("running:{}", self.process_path.to_ascii_lowercase())
    }
}

/// 枚举可见的顶层应用窗口，按进程路径去重并排序。
pub fn enumerate_running_apps() -> Vec<RunningApp> {
    let mut windows: Vec<RawWindow> = Vec::new();
    // SAFETY: 回调只把纯数据追加到本函数的 Vec；句柄在回调返回后不再使用。
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut Vec<RawWindow> as isize),
        )
    };
    let own_pid = std::process::id();
    let mut grouped: HashMap<String, RunningApp> = HashMap::new();
    for window in windows {
        if window.pid == own_pid || window.title.is_empty() || is_shell_class(&window.class) {
            continue;
        }
        let Some(path) = process_path(window.pid) else {
            continue;
        };
        let key = path.to_ascii_lowercase();
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .and_then(|file| file.strip_suffix(".exe").or(Some(file)))
            .unwrap_or(&path)
            .to_string();
        grouped.entry(key).or_insert(RunningApp {
            hwnd: window.hwnd,
            process_path: path,
            title: window.title,
            name,
        });
    }
    let mut apps: Vec<RunningApp> = grouped.into_values().collect();
    apps.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    apps
}

/// 恢复并激活窗口。
///
/// 任务栏类窗口的点击通常发生在应用本身不是前台时，Windows 会拒绝直接
/// `SetForegroundWindow`；这里临时附加到当前前台线程以取得切换权限。
pub fn activate(hwnd: isize) -> Result<(), String> {
    let hwnd = HWND(hwnd as *mut _);
    // SAFETY: 句柄来自枚举结果；调用只改变窗口显示与前台状态。
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let foreground = GetForegroundWindow();
        let mut foreground_pid = 0u32;
        let foreground_thread = if foreground.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(foreground, Some(&mut foreground_pid))
        };
        let current = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current
            && AttachThreadInput(current, foreground_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let activated = SetForegroundWindow(hwnd).as_bool();
        if attached {
            let _ = AttachThreadInput(current, foreground_thread, false);
        }
        if activated {
            Ok(())
        } else {
            Err("系统拒绝切换到该窗口".into())
        }
    }
}

/// 请求窗口关闭。
pub fn close(hwnd: isize) -> Result<(), String> {
    // SAFETY: 句柄来自枚举结果；WM_CLOSE 由目标窗口自行处理。
    unsafe {
        PostMessageW(
            Some(HWND(hwnd as *mut _)),
            windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
    }
    .map_err(|error| format!("关闭窗口失败：{error}"))
}

struct RawWindow {
    hwnd: isize,
    pid: u32,
    title: String,
    class: String,
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam 是 enumerate_running_apps 传入的 Vec 指针，调用期间有效。
    let target = unsafe { &mut *(lparam.0 as *mut Vec<RawWindow>) };
    // SAFETY: 所有查询只读取窗口属性。
    let (visible, minimized) =
        unsafe { (IsWindowVisible(hwnd).as_bool(), IsIconic(hwnd).as_bool()) };
    if !visible || minimized {
        return true.into();
    }
    let mut cloaked = 0u32;
    // SAFETY: 只写入本函数栈上的 u32。
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            size_of::<u32>() as u32,
        )
    };
    if result.is_ok() && cloaked != 0 {
        return true.into();
    }
    let mut pid = 0u32;
    // SAFETY: 只写入本函数栈上变量。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return true.into();
    }
    let title = window_title(hwnd);
    if title.is_empty() {
        return true.into();
    }
    // SAFETY: 只读取扩展样式。
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if style & WS_EX_TOOLWINDOW.0 != 0 {
        return true.into();
    }
    target.push(RawWindow {
        hwnd: hwnd.0 as isize,
        pid,
        title,
        class: class_name(hwnd),
    });
    true.into()
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: 只查询标题长度。
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: 缓冲区长度与查询长度匹配。
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..copied as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: 缓冲区在调用期间有效。
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

fn is_shell_class(class: &str) -> bool {
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
    ) || class.starts_with("DeskShell.")
}

fn process_path(pid: u32) -> Option<String> {
    // SAFETY: 只请求查询权限；句柄由 ProcessHandle 关闭。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let handle = ProcessHandle(handle);
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: 缓冲区与长度在调用期间有效。
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
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

static WATCH_NOTIFY: AtomicIsize = AtomicIsize::new(0);
static WATCH_MESSAGE: AtomicU32 = AtomicU32::new(0);

/// 窗口事件监听；Drop 时注销，不保留全局回调。
pub struct RunningWatch {
    hooks: [HWINEVENTHOOK; 2],
}

impl RunningWatch {
    pub fn create(notify: HWND, message: u32) -> Result<Self, String> {
        WATCH_NOTIFY.store(notify.0 as isize, Ordering::Release);
        WATCH_MESSAGE.store(message, Ordering::Release);
        // SAFETY: 回调为 extern "system"；只投递消息，不访问被销毁窗口。
        let foreground = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(win_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if foreground.is_invalid() {
            return Err(format!(
                "注册前台事件监听失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: 同上；对象事件范围只覆盖创建/销毁/显示/隐藏。
        let objects = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_HIDE,
                None,
                Some(win_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if objects.is_invalid() {
            // SAFETY: 第一个钩子已创建；失败时注销，避免泄漏。
            unsafe {
                let _ = UnhookWinEvent(foreground);
            }
            return Err(format!(
                "注册窗口对象事件监听失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            hooks: [foreground, objects],
        })
    }
}

impl Drop for RunningWatch {
    fn drop(&mut self) {
        // SAFETY: 钩子由本对象创建，只注销一次。
        unsafe {
            for hook in self.hooks {
                let _ = UnhookWinEvent(hook);
            }
        }
        WATCH_NOTIFY.store(0, Ordering::Release);
    }
}

unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let relevant = match event {
        EVENT_SYSTEM_FOREGROUND => true,
        EVENT_OBJECT_CREATE | EVENT_OBJECT_DESTROY | EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE => {
            id_object == OBJID_WINDOW && id_child == 0
        }
        _ => false,
    };
    if !relevant {
        return;
    }
    let notify = WATCH_NOTIFY.load(Ordering::Acquire);
    if notify == 0 {
        return;
    }
    let message = WATCH_MESSAGE.load(Ordering::Acquire);
    // SAFETY: 目标窗口在主程序生命周期内有效；PostMessage 失败不影响后续事件。
    unsafe {
        let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_id_is_stable_and_lowercase() {
        let app = RunningApp {
            hwnd: 1,
            process_path: r"C:\Apps\Demo.exe".into(),
            title: "Demo".into(),
            name: "Demo".into(),
        };
        assert_eq!(app.id(), r"running:c:\apps\demo.exe");
    }

    #[test]
    fn shell_classes_are_filtered() {
        assert!(is_shell_class("Shell_TrayWnd"));
        assert!(is_shell_class("DeskShell.Panel.Dock"));
        assert!(!is_shell_class("Chrome_WidgetWin_1"));
    }
}
