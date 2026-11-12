//! 桌面目录监听：重叠 I/O、去抖合并、溢出标记与可靠取消。
//!
//! 监听只是提示，调用方收到批次后仍应以快照扫描保证最终一致。

use std::{
    collections::BTreeSet,
    ffi::c_void,
    mem::size_of,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            ReadDirectoryChangesW,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects},
        },
    },
    core::PCWSTR,
};

/// 一次监听批次：去抖窗口内合并后的路径集合。
#[derive(Debug, Default, Clone)]
pub struct WatchBatch {
    pub paths: Vec<PathBuf>,
    /// 内核缓冲区溢出：调用方必须做完整快照校验。
    pub overflow: bool,
    pub errors: Vec<String>,
}

/// 桌面目录监听器。后台线程持有句柄；Drop 会取消 I/O 并等待线程退出。
pub struct DesktopWatcher {
    stop: Arc<AtomicBool>,
    stop_event: HANDLE,
    thread: Option<JoinHandle<()>>,
    receiver: Receiver<WatchBatch>,
}

// SAFETY: 句柄只在后台线程内使用；Drop 在停止事件置位后 join 线程才关闭句柄。
unsafe impl Send for DesktopWatcher {}

impl DesktopWatcher {
    /// 监听多个根目录（不递归）。单个目录失败通过批次错误上报，不阻塞其他目录。
    pub fn start(roots: &[PathBuf]) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        // SAFETY: 创建手动重置停止事件；句柄由本对象关闭。
        let stop_event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|error| format!("创建监听停止事件失败：{error}"))?;
        let (sender, receiver) = channel();
        let thread_stop = Arc::clone(&stop);
        let roots = roots.to_vec();
        // 句柄本身不实现 Send；以整数形式交给唯一监听线程，句柄值在 join 前保持有效。
        let thread_event = stop_event.0 as isize;
        let thread = thread::Builder::new()
            .name("desktop-watcher".into())
            .spawn(move || {
                watch_loop(
                    &roots,
                    &thread_stop,
                    HANDLE(thread_event as *mut c_void),
                    sender,
                )
            })
            .map_err(|error| {
                // SAFETY: 线程未创建成功，事件句柄尚未转移。
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                format!("创建桌面监听线程失败：{error}")
            })?;
        Ok(Self {
            stop,
            stop_event,
            thread: Some(thread),
            receiver,
        })
    }

    /// 非阻塞取一个批次。
    pub fn try_recv(&self) -> Option<WatchBatch> {
        self.receiver.try_recv().ok()
    }

    /// 带超时等待批次；用于后台索引线程。
    pub fn recv_timeout(&self, timeout: Duration) -> Option<WatchBatch> {
        self.receiver.recv_timeout(timeout).ok()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        // SAFETY: 事件句柄在 Drop 之前保持有效。
        unsafe {
            let _ = SetEvent(self.stop_event);
        }
    }
}

impl Drop for DesktopWatcher {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: 线程已退出，停止事件不再被等待。
        unsafe {
            let _ = CloseHandle(self.stop_event);
        }
    }
}

struct WatchedRoot {
    path: PathBuf,
    handle: HANDLE,
    event: HANDLE,
    overlapped: Box<OVERLAPPED>,
    buffer: Box<[u8]>,
    error_reported: bool,
}

impl WatchedRoot {
    fn open(path: PathBuf) -> Result<Self, String> {
        let wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 路径以 NUL 结尾；共享与标志允许监听目录变化。
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|error| format!("打开目录失败 {}：{error}", path.display()))?;
        // SAFETY: 创建手动重置事件用于重叠完成通知。
        let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: 目录句柄由本函数打开且尚未转移。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(format!("创建目录事件失败：{error}"));
            }
        };
        let mut overlapped = Box::new(OVERLAPPED::default());
        overlapped.hEvent = event;
        Ok(Self {
            path,
            handle,
            event,
            overlapped,
            buffer: vec![0u8; 64 * 1024].into_boxed_slice(),
            error_reported: false,
        })
    }

    fn issue(&mut self) -> Result<(), String> {
        // 手动重置事件必须在重新挂起监听前复位，否则等待会立即再次返回。
        // SAFETY: 事件句柄由本结构持有且在生命周期内有效。
        unsafe {
            let _ = ResetEvent(self.event);
        }
        // SAFETY: 句柄、缓冲区和 OVERLAPPED 在完成前保持稳定地址；事件由本结构持有。
        unsafe {
            ReadDirectoryChangesW(
                self.handle,
                self.buffer.as_mut_ptr() as *mut c_void,
                self.buffer.len() as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_CREATION,
                None,
                Some((&mut *self.overlapped) as *mut OVERLAPPED),
                None,
            )
        }
        .map_err(|error| format!("启动目录监听失败 {}：{error}", self.path.display()))
    }

    /// 读取完成结果；返回路径、是否溢出与错误。
    fn complete(&mut self) -> (Vec<PathBuf>, bool, Option<String>) {
        let mut transferred = 0u32;
        // SAFETY: 重叠操作由本结构发起；只读取完成状态。
        let result =
            unsafe { GetOverlappedResult(self.handle, &*self.overlapped, &mut transferred, false) };
        if let Err(error) = result {
            return (
                Vec::new(),
                false,
                Some(format!("读取目录变化失败 {}：{error}", self.path.display())),
            );
        }
        if transferred == 0 {
            return (Vec::new(), true, None);
        }
        let mut paths = Vec::new();
        let base = self.buffer.as_ptr();
        let mut offset = 0usize;
        while offset + size_of::<FILE_NOTIFY_INFORMATION>() <= transferred as usize {
            // SAFETY: 内核在 transferred 范围内写入了对齐的 FILE_NOTIFY_INFORMATION 链。
            let info = unsafe { &*(base.add(offset) as *const FILE_NOTIFY_INFORMATION) };
            let length = (info.FileNameLength / 2) as usize;
            if length > 0 {
                // SAFETY: FileName 指向结构尾部 length 个 UTF-16 单元。
                let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), length) };
                paths.push(self.path.join(String::from_utf16_lossy(name)));
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
        }
        (paths, false, None)
    }
}

impl Drop for WatchedRoot {
    fn drop(&mut self) {
        // SAFETY: 取消可能仍在进行的重叠操作，并等待内核结束访问缓冲区。
        unsafe {
            let _ = CancelIoEx(self.handle, Some(&*self.overlapped));
            let mut transferred = 0u32;
            let _ = GetOverlappedResult(self.handle, &*self.overlapped, &mut transferred, true);
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.handle);
        }
    }
}

fn watch_loop(
    roots: &[PathBuf],
    stop: &AtomicBool,
    stop_event: HANDLE,
    sender: Sender<WatchBatch>,
) {
    let mut watched = Vec::new();
    let mut errors = Vec::new();
    for root in roots {
        match WatchedRoot::open(root.clone()) {
            Ok(mut root) => match root.issue() {
                Ok(()) => watched.push(root),
                Err(error) => errors.push(error),
            },
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        let _ = sender.send(WatchBatch {
            paths: Vec::new(),
            overflow: true,
            errors,
        });
    }
    if watched.is_empty() {
        return;
    }

    let mut pending = BTreeSet::new();
    let mut flush_at: Option<Instant> = None;
    let mut overflow = false;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let timeout = flush_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(500))
            .min(Duration::from_millis(500));
        let mut handles = Vec::with_capacity(watched.len() + 1);
        handles.push(stop_event);
        handles.extend(watched.iter().map(|root| root.event));
        // SAFETY: 所有句柄在等待期间有效；只做系统等待。
        let wait =
            unsafe { WaitForMultipleObjects(&handles, false, timeout.as_millis().max(1) as u32) };
        let signalled = wait.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if wait == WAIT_TIMEOUT {
            // 仅用于检查停止标志与去抖截止时间。
        } else if signalled == 0 {
            break;
        } else if signalled <= watched.len() {
            let index = signalled - 1;
            let (paths, root_overflow, error) = watched[index].complete();
            if let Some(error) = error {
                if !watched[index].error_reported {
                    watched[index].error_reported = true;
                    let _ = sender.send(WatchBatch {
                        paths: Vec::new(),
                        overflow: true,
                        errors: vec![error],
                    });
                }
            } else {
                watched[index].error_reported = false;
                overflow |= root_overflow;
                pending.extend(paths);
                if flush_at.is_none() {
                    flush_at = Some(Instant::now() + Duration::from_millis(150));
                }
                if let Err(error) = watched[index].issue() {
                    let _ = sender.send(WatchBatch {
                        paths: Vec::new(),
                        overflow: true,
                        errors: vec![error],
                    });
                }
            }
        }

        if flush_at.is_some_and(|deadline| Instant::now() >= deadline) {
            if !pending.is_empty() || overflow {
                let _ = sender.send(WatchBatch {
                    paths: pending.iter().cloned().collect(),
                    overflow,
                    errors: Vec::new(),
                });
            }
            pending.clear();
            overflow = false;
            flush_at = None;
        }
    }
    // 显式释放：Drop 会取消 I/O 并等待完成，确保内核不再访问缓冲区。
    drop(watched);
}

/// 路径集合合并辅助，供调用方在去抖后比较变化。
pub fn paths_changed(before: &BTreeSet<PathBuf>, after: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    before
        .symmetric_difference(after)
        .cloned()
        .collect::<BTreeSet<_>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "deskshell-watch-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn watcher_reports_created_file_and_stops_cleanly() {
        let root = temp_dir("create");
        let watcher = DesktopWatcher::start(std::slice::from_ref(&root)).unwrap();
        // 等待重叠监听就绪。
        thread::sleep(Duration::from_millis(100));
        std::fs::write(root.join("new-file.txt"), b"x").unwrap();
        let mut found = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(batch) = watcher.recv_timeout(Duration::from_millis(200))
                && batch
                    .paths
                    .iter()
                    .any(|path| path.file_name().is_some_and(|name| name == "new-file.txt"))
            {
                found = true;
                break;
            }
        }
        assert!(found, "未在期限内收到新建文件通知");
        drop(watcher);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_root_reports_error_without_hanging() {
        let watcher =
            DesktopWatcher::start(&[PathBuf::from("C:\\deskshell-missing-root-xyz")]).unwrap();
        let batch = watcher
            .recv_timeout(Duration::from_secs(2))
            .expect("应报告目录打开失败");
        assert!(!batch.errors.is_empty());
        assert!(batch.overflow);
    }

    #[test]
    fn path_diff_is_symmetric() {
        let mut before = BTreeSet::new();
        before.insert(PathBuf::from("a"));
        let mut after = BTreeSet::new();
        after.insert(PathBuf::from("b"));
        let changed = paths_changed(&before, &after);
        assert_eq!(changed.len(), 2);
    }
}
