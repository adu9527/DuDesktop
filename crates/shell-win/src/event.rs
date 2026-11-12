//! 进程内自动重置事件，用于把工作从回调/线程投递到宿主主循环。

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
};

pub struct WaitEvent {
    handle: HANDLE,
}

// SAFETY: 事件句柄可由任意线程安全地唤醒与等待；本类型不转移句柄所有权。
unsafe impl Send for WaitEvent {}
// SAFETY: 多线程共享只读取句柄值并调用线程安全的 SetEvent/WaitForSingleObject。
unsafe impl Sync for WaitEvent {}

impl WaitEvent {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: 创建匿名自动重置事件。
        let handle = unsafe { CreateEventW(None, false, false, windows::core::PCWSTR::null())? };
        Ok(Self { handle })
    }

    pub fn set(&self) {
        // SAFETY: 句柄由本对象独占且有效。
        unsafe {
            let _ = SetEvent(self.handle);
        }
    }

    /// 等待事件或超时；`None` 表示无限等待。返回是否被唤醒。
    pub fn wait(&self, timeout: Option<std::time::Duration>) -> bool {
        let millis = timeout
            .map(|duration| duration.as_millis().min(u32::MAX as u128) as u32)
            .unwrap_or(u32::MAX);
        // SAFETY: 句柄有效。
        let wait = unsafe { WaitForSingleObject(self.handle, millis.max(1)) };
        wait == windows::Win32::Foundation::WAIT_OBJECT_0
    }

    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

/// 等待两个事件之一。返回 `Some(0)`/`Some(1)` 表示对应事件被置位，超时返回 `None`。
/// 两个自动重置事件由各自的所有者维护；本函数不改变句柄所有权。
pub fn wait_two(
    first: &WaitEvent,
    second: &WaitEvent,
    timeout: Option<std::time::Duration>,
) -> Option<usize> {
    let millis = timeout
        .map(|duration| duration.as_millis().min(u32::MAX as u128) as u32)
        .unwrap_or(u32::MAX);
    // SAFETY: 两个句柄在调用期间有效；只做系统等待。
    let wait = unsafe {
        windows::Win32::System::Threading::WaitForMultipleObjects(
            &[first.handle, second.handle],
            false,
            millis,
        )
    };
    if wait == windows::Win32::Foundation::WAIT_OBJECT_0 {
        Some(0)
    } else if wait
        == windows::Win32::Foundation::WAIT_EVENT(windows::Win32::Foundation::WAIT_OBJECT_0.0 + 1)
    {
        Some(1)
    } else {
        None
    }
}

impl Drop for WaitEvent {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
