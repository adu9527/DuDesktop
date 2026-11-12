//! Windows API 的所有权与 unsafe 边界。
#[cfg(windows)]
pub mod apps;
#[cfg(windows)]
pub mod autostart;
#[cfg(windows)]
pub mod deskfiles;
#[cfg(windows)]
pub mod desktop;
#[cfg(windows)]
pub mod event;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod preview;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod running;
#[cfg(windows)]
pub mod shellui;
#[cfg(windows)]
pub mod system_state;
#[cfg(windows)]
pub mod watcher;
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub use window::run_window;

#[cfg(windows)]
pub fn enable_per_monitor_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: 只设置当前进程的 DPI 感知模式；已设置时返回失败但不影响后续调用。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
pub fn enable_per_monitor_dpi_awareness() {}

pub fn platform_supported() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}
