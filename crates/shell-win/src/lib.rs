//! Windows API 的所有权与 unsafe 边界。
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub use window::run_window;

pub fn platform_supported() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}
