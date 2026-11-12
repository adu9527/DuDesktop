//! 托盘、全局热键、单实例互斥与原生菜单。
//!
//! 这些是长期运行入口的最小 Shell 集成；托盘图标在 Drop 时移除，
//! 单实例互斥体在进程退出时由系统释放。

use std::{mem::size_of, path::PathBuf};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, POINT, WPARAM,
        },
        System::{Com::CoTaskMemFree, Console::GetConsoleWindow, Threading::CreateMutexW},
        UI::{
            Controls::Dialogs::{
                GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
                OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            Input::KeyboardAndMouse::{
                HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey,
            },
            Shell::{
                BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, NIF_ICON, NIF_MESSAGE,
                NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, SHBrowseForFolderW,
                SHGetPathFromIDListW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyMenu, FindWindowW, GetCursorPos,
                IDI_APPLICATION, IDYES, LoadIconW, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
                MB_YESNO, MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MessageBoxW,
                PostMessageW, SW_HIDE, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

/// 单实例互斥体；`None` 表示已有实例运行。
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire(name: &str) -> Result<Option<Self>, String> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: 名称以 NUL 结尾；成功后句柄归本对象所有。
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }
            .map_err(|error| format!("创建单实例互斥体失败：{error}"))?;
        // SAFETY: 只读取当前线程 LastError。
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: 句柄由本函数打开，不转移所有权。
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(None);
        }
        Ok(Some(Self { handle }))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 检测命名互斥体是否已被占用；不创建、不持有锁，用于验收脚本预检。
pub fn instance_running(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 名称以 NUL 结尾；成功时只持有同步查询句柄并立即关闭。
    let result = unsafe {
        windows::Win32::System::Threading::OpenMutexW(
            windows::Win32::System::Threading::SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000),
            false,
            PCWSTR(wide.as_ptr()),
        )
    };
    match result {
        Ok(handle) => {
            // SAFETY: 句柄由本函数打开且只关闭一次。
            unsafe {
                let _ = CloseHandle(handle);
            }
            true
        }
        Err(_) => false,
    }
}

/// 查询类名对应的顶层窗口是否存在；只读，不发送消息。
pub fn window_exists(class: &str) -> bool {
    let class: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 类名以 NUL 结尾；只查询顶层窗口。
    unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.is_ok()
}

/// 向已存在实例的隐藏窗口发送唤醒消息；返回是否找到窗口。
pub fn post_to_window(class: &str, message: u32) -> bool {
    let class: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 类名以 NUL 结尾；只查询顶层窗口。
    let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }) else {
        return false;
    };
    // SAFETY: 句柄有效；只投递消息，不等待。
    unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)).is_ok() }
}

/// 托盘图标；Drop 时从通知区移除。
pub struct TrayIcon {
    data: NOTIFYICONDATAW,
    added: bool,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, callback_message: u32, tooltip: &str) -> Result<Self, String> {
        // SAFETY: 加载系统共享图标；不销毁由系统拥有的图标。
        let icon = unsafe { LoadIconW(None, IDI_APPLICATION) }
            .map_err(|error| format!("加载托盘图标失败：{error}"))?;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: callback_message,
            hIcon: icon,
            ..Default::default()
        };
        let tip: Vec<u16> = tooltip.encode_utf16().take(127).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        // SAFETY: 结构按文档初始化；调用期间有效。
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        if !added {
            return Err("添加托盘图标失败".into());
        }
        Ok(Self { data, added })
    }

    pub fn set_tooltip(&mut self, tooltip: &str) {
        let tip: Vec<u16> = tooltip.encode_utf16().take(127).collect();
        self.data.szTip = [0; 128];
        self.data.szTip[..tip.len()].copy_from_slice(&tip);
        self.data.uFlags = NIF_TIP;
        // SAFETY: 结构有效；失败时旧提示仍然可用。
        unsafe {
            let _ = Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_MODIFY, &self.data);
        }
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        if self.added {
            // SAFETY: 只移除本对象添加的图标。
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            }
            self.added = false;
        }
    }
}

/// 菜单项；`separator` 为真时忽略 id、label 与子项。
#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub id: u32,
    pub label: String,
    pub separator: bool,
    pub checked: bool,
    /// 置灰项用于解释“为什么没有该开关”，不参与命令。
    pub enabled: bool,
    /// 非空时作为子菜单；子项命令 id 由调用方统一分配。
    pub children: Vec<MenuEntry>,
}

impl MenuEntry {
    pub fn item(id: u32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            separator: false,
            checked: false,
            enabled: true,
            children: Vec::new(),
        }
    }

    pub fn checked(id: u32, label: impl Into<String>, checked: bool) -> Self {
        Self {
            checked,
            ..Self::item(id, label)
        }
    }

    /// 置灰的原因说明项；不产生命令。
    pub fn note(label: impl Into<String>) -> Self {
        Self {
            enabled: false,
            ..Self::item(0, label)
        }
    }

    pub fn submenu(label: impl Into<String>, children: Vec<MenuEntry>) -> Self {
        Self {
            children,
            ..Self::item(0, label)
        }
    }

    pub fn separator() -> Self {
        Self {
            id: 0,
            label: String::new(),
            separator: true,
            checked: false,
            enabled: false,
            children: Vec::new(),
        }
    }
}

/// 在鼠标位置弹出原生菜单，返回选中命令；取消返回 None。
pub fn popup_menu(hwnd: HWND, entries: &[MenuEntry]) -> Option<u32> {
    // SAFETY: 创建菜单对象；后续所有路径都会 DestroyMenu。
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    let mut labels: Vec<Vec<u16>> = Vec::new();
    populate_menu(menu, entries, &mut labels);
    let mut point = POINT::default();
    // SAFETY: 只读取光标位置。
    unsafe {
        let _ = GetCursorPos(&mut point);
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
    }
    // SAFETY: 菜单与窗口句柄有效；TPM_RETURNCMD 返回命令 id。
    let command = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    };
    // SAFETY: 菜单由本函数创建；DestroyMenu 会递归销毁子菜单。
    unsafe {
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), 0, WPARAM(0), LPARAM(0));
    }
    let command = command.0 as u32;
    (command != 0).then_some(command)
}

fn populate_menu(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    entries: &[MenuEntry],
    labels: &mut Vec<Vec<u16>>,
) {
    for entry in entries {
        if entry.separator {
            // SAFETY: 菜单句柄有效。
            unsafe {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            continue;
        }
        let label = entry
            .label
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        labels.push(label);
        // 先取出稳定指针再递归；Vec<u16> 的堆缓冲区不会因外层 Vec 增长而移动。
        let label_ptr = labels.last().expect("刚插入的标签").as_ptr();
        if !entry.children.is_empty() {
            // SAFETY: 创建子菜单并在返回前填充；父菜单销毁时系统递归销毁子菜单。
            let Some(submenu) = (unsafe { CreatePopupMenu() }).ok() else {
                continue;
            };
            populate_menu(submenu, &entry.children, labels);
            // SAFETY: 父子菜单句柄均有效；子菜单所有权交给父菜单。
            unsafe {
                let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR(label_ptr));
            }
            continue;
        }
        let mut flags = MF_STRING;
        if entry.checked {
            flags |= windows::Win32::UI::WindowsAndMessaging::MF_CHECKED;
        }
        flags |= if entry.enabled { MF_ENABLED } else { MF_GRAYED };
        // SAFETY: 标签在菜单销毁前保持有效。
        unsafe {
            let _ = AppendMenuW(menu, flags, entry.id as usize, PCWSTR(label_ptr));
        }
    }
}

/// 信息提示框；用于状态与故障原因，不阻塞后台工作线程（只在 UI 线程调用）。
pub fn info(title: &str, text: &str, owner: HWND) {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 两个字符串在调用期间有效。
    unsafe {
        let _ = MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// 原生打开文件对话框；必须在 UI/STA 线程调用。
pub fn pick_file(
    title: &str,
    filter_name: &str,
    filter_pattern: &str,
    owner: HWND,
) -> Option<PathBuf> {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let filter: Vec<u16> = filter_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .chain(filter_pattern.encode_utf16())
        .chain(std::iter::once(0))
        .chain(std::iter::once(0))
        .collect();
    let mut buffer = vec![0u16; 32 * 1024];
    let mut options = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: 结构、过滤器与文件缓冲区在调用期间有效。
    if !unsafe { GetOpenFileNameW(&mut options) }.as_bool() {
        return None;
    }
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
    if end == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
}

/// 选择文件夹；取消或失败返回 None。
pub fn pick_folder(title: &str, owner: HWND) -> Option<PathBuf> {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut display = [0u16; 260];
    let info = BROWSEINFOW {
        hwndOwner: owner,
        pszDisplayName: PWSTR(display.as_mut_ptr()),
        lpszTitle: PCWSTR(title.as_ptr()),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..Default::default()
    };
    // SAFETY: 结构在调用期间有效；返回的 PIDL 由调用方释放。
    let id = unsafe { SHBrowseForFolderW(&info) };
    if id.is_null() {
        return None;
    }
    let mut buffer = [0u16; 260];
    // SAFETY: PIDL 有效，缓冲区长度足够。
    let ok = unsafe { SHGetPathFromIDListW(id, &mut buffer) }.as_bool();
    // SAFETY: PIDL 由 SHBrowseForFolderW 分配，这里释放一次。
    unsafe {
        CoTaskMemFree(Some(id as *const core::ffi::c_void));
    }
    if !ok {
        return None;
    }
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
    if end == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
}

/// 注册全局热键（Ctrl+Alt+按键）；失败返回原因。
pub fn register_hotkey(hwnd: HWND, id: i32, key: u32) -> Result<(), String> {
    // SAFETY: 窗口句柄有效；热键由同一线程注销。
    unsafe {
        RegisterHotKey(
            Some(hwnd),
            id,
            HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0),
            key,
        )
    }
    .map_err(|error| format!("注册热键 {id} 失败：{error}"))
}

pub fn unregister_hotkey(hwnd: HWND, id: i32) {
    // SAFETY: 只注销本程序注册的热键。
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
    }
}

/// 是/否确认框；用于删除与隐藏桌面图标等不可静默执行的操作。
pub fn confirm(title: &str, text: &str, owner: HWND) -> bool {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 两个字符串在调用期间有效。
    let result = unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    result == IDYES
}

/// 隐藏控制台窗口，避免 GUI 常驻模式出现黑色控制台；标准输出重定向仍然有效。
pub fn hide_console_window() {
    // SAFETY: 只查询并隐藏当前进程拥有的控制台窗口。
    unsafe {
        let console = GetConsoleWindow();
        if !console.is_invalid() {
            let _ = ShowWindow(console, SW_HIDE);
        }
    }
}

/// 从后台线程唤醒 UI 窗口；`hwnd_raw` 由调用方保证在消息处理前有效。
pub fn post_message_raw(hwnd_raw: isize, message: u32) {
    // SAFETY: 后台线程只投递消息，不访问窗口状态；窗口由主线程保证存活。
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd_raw as *mut core::ffi::c_void)),
            message,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// 托盘回调使用的消息 id，避免与窗口消息冲突。
pub const WM_APP_TRAY: u32 = 0x8000 + 1;
