use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{COLOR_WINDOW, HBRUSH},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::{Result, w},
};

pub fn run_window() -> Result<()> {
    // SAFETY: 在调用线程创建和运行窗口；类名为静态 UTF-16，回调存活至进程退出。
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DeskShell.Foundation.Window");
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut _),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("DeskShell - development foundation"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960,
            600,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut message = MSG::default();
        loop {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status == -1 {
                return Err(windows::core::Error::from_win32());
            }
            if status == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 参数来自系统消息分发；不解引用消息附带的指针，不允许 panic 跨 FFI。
    unsafe {
        if message == WM_DESTROY {
            PostQuitMessage(0);
            LRESULT(0)
        } else {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}
