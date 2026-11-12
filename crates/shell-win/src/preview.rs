//! 宿主内独立预览窗口。M1 只验证播放，不附着 WorkerW，也不修改桌面图标。

use std::sync::Once;

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, EndPaint, EnumDisplayMonitors, FillRect, GetMonitorInfoW,
            HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_ARROW, IsWindow, LoadCursorW,
            RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
            ShowWindow, WM_CLOSE, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_CLIPCHILDREN,
            WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::{PCWSTR, w},
};

use crate::enable_per_monitor_dpi_awareness;

const CLASS_NAME: windows::core::PCWSTR = w!("DeskShell.WallpaperPreview");
static REGISTER_CLASS: Once = Once::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
}

/// 返回指定显示器的物理像素工作区；越界时返回 `None`。
pub fn monitor_work_area(index: Option<u32>) -> Option<RECT> {
    enable_per_monitor_dpi_awareness();
    let mut areas = Vec::new();
    // SAFETY: 回调只在本次枚举期间写入局部 Vec，指针有效。
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut areas as *mut Vec<RECT> as isize),
        );
    }
    if areas.is_empty() {
        return None;
    }
    match index {
        Some(index) => areas.get(index as usize).copied(),
        None => {
            // SAFETY: 查询主显示器信息，输出结构由本函数初始化。
            unsafe {
                let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
                let mut information = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(monitor, &mut information).as_bool() {
                    Some(information.rcWork)
                } else {
                    areas.first().copied()
                }
            }
        }
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _device: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: 调用方保证 data 指向本次枚举期间存活的 Vec<RECT>。
    let areas = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
    let mut information = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: monitor 由系统提供，输出结构有效。
    if unsafe { GetMonitorInfoW(monitor, &mut information) }.as_bool() {
        areas.push(information.rcWork);
    }
    true.into()
}

/// 默认预览位置：工作区内居中，占宽度的 45%，保持 16:9，避免遮挡整个桌面。
pub fn default_placement(work_area: RECT) -> PreviewPlacement {
    let width = (work_area.right - work_area.left).max(320) as u32 * 45 / 100;
    let width = width.clamp(320, 1600);
    let height = width * 9 / 16;
    PreviewPlacement {
        x: work_area.left + (((work_area.right - work_area.left) as u32 - width) / 2) as i32,
        y: work_area.top + (((work_area.bottom - work_area.top) as u32 - height) / 2) as i32,
        width,
        height,
        visible: true,
    }
}

pub struct PreviewWindow {
    hwnd: HWND,
}

impl PreviewWindow {
    pub fn create(title: &str, placement: PreviewPlacement) -> Result<Self, String> {
        enable_per_monitor_dpi_awareness();
        REGISTER_CLASS.call_once(|| {
            // SAFETY: 类名与窗口过程在进程生命周期内有效；重复注册失败不影响后续创建。
            unsafe {
                if let Ok(instance) = GetModuleHandleW(None) {
                    let class = WNDCLASSW {
                        hInstance: instance.into(),
                        lpszClassName: CLASS_NAME,
                        lpfnWndProc: Some(window_proc),
                        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                        hbrBackground: CreateSolidBrush(windows::Win32::Foundation::COLORREF(0)),
                        ..Default::default()
                    };
                    let _ = RegisterClassW(&class);
                }
            }
        });
        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: 类已注册；标题在调用期间有效；窗口由本对象拥有并在 Drop 中销毁。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                placement.x,
                placement.y,
                placement.width as i32,
                placement.height as i32,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| format!("创建壁纸预览窗口失败：{error}"))?;
        let window = Self { hwnd };
        window.set_placement(placement);
        // 会话锁定/解锁会广播 WM_WTSSESSION_CHANGE；失败只影响恢复灵敏度。
        let _ = crate::desktop::register_session_notifications(hwnd);
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 传给 mpv `wid` 选项的窗口句柄数值。
    pub fn window_id(&self) -> isize {
        self.hwnd.0 as isize
    }

    pub fn set_placement(&self, placement: PreviewPlacement) {
        // SAFETY: 窗口句柄由本对象持有且有效；失败只影响预览位置。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                placement.x,
                placement.y,
                placement.width as i32,
                placement.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(
                self.hwnd,
                if placement.visible {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
        }
    }

    pub fn is_alive(&self) -> bool {
        // SAFETY: 只查询窗口句柄。
        unsafe { IsWindow(Some(self.hwnd)).as_bool() }
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        // SAFETY: 窗口句柄由本对象独占。
        unsafe {
            crate::desktop::unregister_session_notifications(self.hwnd);
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // SAFETY: 使用窗口自身 DC 填充黑底；mpv 子窗口覆盖前避免闪烁。
            unsafe {
                let mut paint = PAINTSTRUCT::default();
                let device = BeginPaint(hwnd, &mut paint);
                if !device.is_invalid() {
                    let mut client = RECT::default();
                    let _ =
                        windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
                    let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0));
                    FillRect(device, &client, brush);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());
                }
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 用户关闭预览只隐藏窗口，不销毁播放资源；宿主退出时统一清理。
            // SAFETY: 隐藏自身窗口。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: 默认处理其余消息。
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}
