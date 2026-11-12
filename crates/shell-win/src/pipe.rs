//! 本机命名管道传输：限制为当前用户可访问，使用重叠 I/O 实现有界超时与取消。
//!
//! 生命周期与线程约定：
//! - 同一连接上只允许一个读取者；`read_frame` 内部持有读锁，误用多线程也不会交错解析。
//! - 写入由可超时互斥锁串行化，等待锁的时间计入调用方总截止时间。
//! - 任何部分帧读写失败或协议错误都会把连接标记为失效，避免按损坏字节流继续解析。
//! - 取消后必须确认内核操作已经完成；无法确认时宁可泄漏 OVERLAPPED/缓冲区，也不悬垂释放。
//! - 帧格式为 `u32 小端长度 + 载荷`，长度上界由调用方传入，避免无界分配。

use std::{
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError,
            HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
            WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
            },
            Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
        },
    },
    core::{PCWSTR, w},
};

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const MAX_TIMEOUT_MS: u32 = u32::MAX;
/// 取消后等待内核确认完成的时限；超时则泄漏操作缓冲区而不是悬垂释放。
const HARVEST_TIMEOUT_MS: u32 = 5_000;
/// ERROR_INVALID_HANDLE：句柄在收割前已关闭，无法证明内核不再访问缓冲区。
const ERROR_INVALID_HANDLE: u32 = 6;

/// 命名管道传输错误。区分超时、断开与协议/参数问题，方便监督器决定是否重启。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeError {
    Timeout,
    Disconnected,
    Busy,
    AccessDenied,
    Protocol(String),
    Io { code: u32, message: String },
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(formatter, "命名管道操作超时"),
            Self::Disconnected => write!(formatter, "命名管道已断开"),
            Self::Busy => write!(formatter, "命名管道忙"),
            Self::AccessDenied => write!(formatter, "命名管道访问被拒绝"),
            Self::Protocol(message) => write!(formatter, "命名管道协议错误：{message}"),
            Self::Io { code, message } => write!(formatter, "命名管道 I/O 错误 {code}：{message}"),
        }
    }
}

impl std::error::Error for PipeError {}

fn win32_code(error: &windows::core::Error) -> u32 {
    let hresult = error.code().0 as u32;
    if hresult & 0xFFFF_0000 == 0x8007_0000 {
        hresult & 0xFFFF
    } else {
        hresult
    }
}

fn map_io_error(error: windows::core::Error) -> PipeError {
    let code = win32_code(&error);
    match code {
        // ERROR_BROKEN_PIPE(109)、ERROR_PIPE_NOT_CONNECTED(233)、ERROR_OPERATION_ABORTED(995)。
        109 | 233 | 995 => PipeError::Disconnected,
        5 => PipeError::AccessDenied,
        other => PipeError::Io {
            code: other,
            message: error.message().to_string(),
        },
    }
}

fn io_error(code: u32, message: &str) -> PipeError {
    PipeError::Io {
        code,
        message: message.into(),
    }
}

fn timeout_ms(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => MAX_TIMEOUT_MS,
        Some(duration) => {
            let millis = duration.as_millis().min(MAX_TIMEOUT_MS as u128) as u32;
            millis.max(1)
        }
    }
}

fn remaining_ms(deadline: Option<Instant>) -> u32 {
    match deadline {
        None => MAX_TIMEOUT_MS,
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                1
            } else {
                timeout_ms(Some(remaining))
            }
        }
    }
}

/// 只授予对象所有者（当前用户）与 SYSTEM 的访问权；拒绝其他用户与本机远程客户端。
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn current_user_only() -> Result<Self, PipeError> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: 传入静态 UTF-16 SDDL 与有效的输出指针；成功后由 Drop 调用 LocalFree。
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;OW)(A;;GA;;;SY)"),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(map_io_error)?;
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: 描述符由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配。
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 独占拥有的内核句柄；只在本类型销毁时关闭。
struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占；调用方保证没有未收割的重叠操作仍引用它。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn create_event() -> Result<OwnedHandle, PipeError> {
    // SAFETY: 创建匿名手动重置事件；失败时返回错误。
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map(OwnedHandle::new)
        .map_err(map_io_error)
}

/// 一次重叠 I/O 的内核可见状态。`OVERLAPPED` 与缓冲区都放在堆上以保持地址稳定，
/// 只有确认操作完成后才会释放；无法确认时由调用方 `mem::forget` 泄漏。
struct IoOp {
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u8>,
}

impl IoOp {
    fn new(event: HANDLE, buffer: Vec<u8>) -> Self {
        Self {
            overlapped: Box::new(OVERLAPPED {
                hEvent: event,
                ..Default::default()
            }),
            buffer,
        }
    }

    fn overlapped_ptr(&self) -> *mut OVERLAPPED {
        self.overlapped.as_ref() as *const OVERLAPPED as *mut OVERLAPPED
    }
}

struct CompletionError {
    error: PipeError,
    /// 内核是否已确认操作完成；`false` 时调用方不得释放 `IoOp`。
    harvested: bool,
    /// 取消前已经传输的字节数；用于判断超时是否破坏了流状态。
    transferred: u32,
}

/// 等待已完成的重叠操作返回结果。
fn completed_result(handle: HANDLE, op: &IoOp) -> Result<u32, CompletionError> {
    let mut transferred = 0u32;
    // SAFETY: 事件已经置位，操作已完成；只读取结果。
    match unsafe { GetOverlappedResult(handle, op.overlapped.as_ref(), &mut transferred, false) } {
        Ok(()) => Ok(transferred),
        Err(error) => Err(CompletionError {
            error: map_io_error(error),
            harvested: true,
            transferred,
        }),
    }
}

/// 等待重叠操作完成；超时或等待异常时取消并同步收割。
fn await_completion(
    handle: HANDLE,
    op: &IoOp,
    event: HANDLE,
    deadline: Option<Instant>,
) -> Result<u32, CompletionError> {
    // SAFETY: 事件与重叠结构在等待期间保持有效。
    let wait = unsafe { WaitForSingleObject(event, remaining_ms(deadline)) };
    if wait == WAIT_OBJECT_0 {
        return completed_result(handle, op);
    }
    let timeout = wait == WAIT_TIMEOUT;
    let error = if timeout {
        PipeError::Timeout
    } else {
        io_error(wait.0, "等待重叠 I/O 完成失败")
    };
    // 取消后必须等内核确认，否则调用方释放缓冲区会造成内核写悬垂内存。
    // SAFETY: 句柄与重叠结构有效；只针对本次操作取消。
    unsafe {
        let _ = CancelIoEx(handle, Some(op.overlapped.as_ref()));
    }
    // SAFETY: 事件句柄在收割期间有效，等待不访问重叠缓冲区。
    let harvest_wait = unsafe { WaitForSingleObject(event, HARVEST_TIMEOUT_MS) };
    let (harvested, transferred) = if harvest_wait == WAIT_OBJECT_0 {
        let mut transferred = 0u32;
        // 成功或 ERROR_OPERATION_ABORTED 都表示操作已经结束。
        // SAFETY: 事件已置位，操作完成，只读取结果。
        let _ =
            unsafe { GetOverlappedResult(handle, op.overlapped.as_ref(), &mut transferred, false) };
        (true, transferred)
    } else {
        let mut transferred = 0u32;
        // 阻塞式收割：任何返回（包括取消错误）都表示操作结束，但句柄已失效时无法证明。
        // SAFETY: 句柄与重叠结构仍有效；阻塞到内核确认操作结束。
        match unsafe { GetOverlappedResult(handle, op.overlapped.as_ref(), &mut transferred, true) }
        {
            Ok(()) => (true, transferred),
            Err(error) if win32_code(&error) != ERROR_INVALID_HANDLE => (true, transferred),
            Err(_) => (false, transferred),
        }
    };
    Err(CompletionError {
        error,
        harvested,
        transferred,
    })
}

/// 创建管道实例并等待宿主连接。超时会取消未完成的连接并关闭本次实例。
pub struct PipeServer {
    name: String,
}

impl PipeServer {
    pub fn new(name: &str) -> Result<Self, PipeError> {
        if !name.starts_with(r"\\.\pipe\") || name.len() > 200 || name.len() < 12 {
            return Err(PipeError::Protocol(format!("管道名无效：{name}")));
        }
        Ok(Self {
            name: name.to_owned(),
        })
    }

    pub fn accept(&self, timeout: Duration) -> Result<PipeConnection, PipeError> {
        let security = SecurityDescriptor::current_user_only()?;
        let attributes = security.attributes();
        let name = wide(&self.name);
        // SAFETY: 名称以 NUL 结尾且在整个调用期间有效；安全属性结构同样保持有效。
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                Some(&attributes),
            )
        };
        if handle.is_invalid() {
            // SAFETY: 仅读取当前线程的 LastError。
            return Err(io_error(
                unsafe { GetLastError() }.0,
                "CreateNamedPipeW 失败",
            ));
        }
        let handle = OwnedHandle::new(handle);
        let event = create_event()?;
        let op = IoOp::new(event.raw(), Vec::new());
        // SAFETY: 重叠结构在等待完成前保持有效。
        let connected = unsafe { ConnectNamedPipe(handle.raw(), Some(op.overlapped_ptr())) };
        match connected {
            Ok(()) => {}
            Err(error) if win32_code(&error) == ERROR_IO_PENDING.0 => {
                let deadline = Instant::now() + timeout;
                if let Err(completion) =
                    await_completion(handle.raw(), &op, event.raw(), Some(deadline))
                {
                    if !completion.harvested {
                        // 无法证明内核已结束访问；泄漏操作状态而不是悬垂释放。
                        std::mem::forget(op);
                    }
                    return Err(completion.error);
                }
            }
            Err(error) if win32_code(&error) == ERROR_PIPE_CONNECTED.0 => {}
            Err(error) => return Err(map_io_error(error)),
        }

        let read_event = create_event()?;
        let write_event = create_event()?;
        Ok(PipeConnection {
            handle,
            read_event,
            write_event,
            read_lock: Mutex::new(()),
            write_gate: TimedMutex::new(),
            broken: AtomicBool::new(false),
        })
    }
}

/// 已经建立的管道连接。句柄在内存释放时关闭。
pub struct PipeConnection {
    handle: OwnedHandle,
    read_event: OwnedHandle,
    write_event: OwnedHandle,
    read_lock: Mutex<()>,
    write_gate: TimedMutex,
    /// 出现部分帧读写或协议错误后流状态未知，后续调用直接失败。
    broken: AtomicBool,
}

// SAFETY: Windows 命名管道句柄支持全双重叠 I/O；本类型用读锁强制单读取者，
// 并用可超时互斥锁串行化写入。句柄不会在存在未完成操作时主动关闭。
unsafe impl Send for PipeConnection {}
// SAFETY: 共享引用只执行受锁保护的读写；所有 I/O 缓冲区在确认完成前保持存活。
unsafe impl Sync for PipeConnection {}

impl PipeConnection {
    /// 以客户端身份连接已存在的管道服务端。
    pub fn connect(name: &str, timeout: Duration) -> Result<Self, PipeError> {
        let deadline = Instant::now() + timeout;
        let name_w = wide(name);
        loop {
            // SAFETY: 名称以 NUL 结尾；返回句柄在成功时由本对象拥有。
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(name_w.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OVERLAPPED.0),
                    None,
                )
            };
            match handle {
                Ok(handle) => {
                    let handle = OwnedHandle::new(handle);
                    let read_event = create_event()?;
                    let write_event = create_event()?;
                    return Ok(Self {
                        handle,
                        read_event,
                        write_event,
                        read_lock: Mutex::new(()),
                        write_gate: TimedMutex::new(),
                        broken: AtomicBool::new(false),
                    });
                }
                Err(error)
                    if matches!(win32_code(&error), 2 | 3)
                        || win32_code(&error) == ERROR_PIPE_BUSY.0 =>
                {
                    // 服务端可能尚未创建实例；在连接超时内重试，避免启动顺序竞态。
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(PipeError::Timeout);
                    }
                    // SAFETY: 仅等待命名管道实例可用。
                    let _ = unsafe {
                        WaitNamedPipeW(PCWSTR(name_w.as_ptr()), timeout_ms(Some(remaining)).min(50))
                    };
                    std::thread::sleep(Duration::from_millis(10));
                    if Instant::now() >= deadline {
                        return Err(PipeError::Timeout);
                    }
                }
                Err(error) => return Err(map_io_error(error)),
            }
        }
    }

    fn poisoned(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    fn poison(&self, error: PipeError) -> PipeError {
        self.broken.store(true, Ordering::Release);
        error
    }

    /// 读取一帧；先校验长度上界再分配载荷缓冲区。
    ///
    /// 超时只在该帧尚未消费任何字节时保持连接可用；一旦读到部分帧，连接会被标记失效，
    /// 调用方必须重建会话，不能继续按损坏字节流解析。
    pub fn read_frame(
        &self,
        max_payload: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, PipeError> {
        if self.poisoned() {
            return Err(PipeError::Disconnected);
        }
        // 读锁覆盖整帧（长度前缀 + 载荷），否则两个读取者可能在帧中途交错。
        let _guard = self
            .read_lock
            .lock()
            .map_err(|_| PipeError::Protocol("读取锁已损坏".into()))?;
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut header = [0u8; 4];
        self.read_exact_locked(&mut header, deadline, 0)?;
        let length = u32::from_le_bytes(header) as usize;
        if length == 0 {
            return Err(self.poison(PipeError::Protocol("帧长度为 0".into())));
        }
        if length > max_payload {
            return Err(self.poison(PipeError::Protocol(format!(
                "帧长度 {length} 超过上限 {max_payload}"
            ))));
        }
        let mut payload = vec![0u8; length];
        self.read_exact_locked(&mut payload, deadline, header.len())?;
        Ok(payload)
    }

    /// 调用方必须持有读锁。`consumed` 是本帧此前已读取的字节数：
    /// 只有整帧尚未消费任何字节的空闲超时才能保持连接可用，否则字节流位置未知。
    fn read_exact_locked(
        &self,
        buffer: &mut [u8],
        deadline: Option<Instant>,
        consumed: usize,
    ) -> Result<(), PipeError> {
        if self.poisoned() {
            return Err(PipeError::Disconnected);
        }
        let mut offset = 0;
        while offset < buffer.len() {
            let mut op = IoOp::new(self.read_event.raw(), vec![0u8; buffer.len() - offset]);
            let mut transferred = 0u32;
            let overlapped = op.overlapped_ptr();
            // SAFETY: 缓冲区与重叠结构在确认完成前保持有效；事件在调用前复位。
            let read = unsafe {
                let _ = ResetEvent(self.read_event.raw());
                ReadFile(
                    self.handle.raw(),
                    Some(op.buffer.as_mut_slice()),
                    Some(&mut transferred),
                    Some(overlapped),
                )
            };
            let transferred = match read {
                Ok(()) => transferred,
                Err(error) if win32_code(&error) == ERROR_IO_PENDING.0 => {
                    match await_completion(self.handle.raw(), &op, self.read_event.raw(), deadline)
                    {
                        Ok(transferred) => transferred,
                        Err(completion) => {
                            if !completion.harvested {
                                std::mem::forget(op);
                            }
                            // 整帧尚未开始读取的超时不会改变流状态，允许调用方稍后重试。
                            if completion.error == PipeError::Timeout
                                && consumed == 0
                                && offset == 0
                                && completion.transferred == 0
                            {
                                return Err(PipeError::Timeout);
                            }
                            return Err(self.poison(completion.error));
                        }
                    }
                }
                Err(error) => {
                    let mapped = map_io_error(error);
                    return Err(if matches!(mapped, PipeError::Disconnected) {
                        mapped
                    } else {
                        self.poison(mapped)
                    });
                }
            };
            if transferred == 0 {
                self.broken.store(true, Ordering::Release);
                return Err(PipeError::Disconnected);
            }
            let chunk = transferred as usize;
            let end = (offset + chunk).min(buffer.len());
            buffer[offset..end].copy_from_slice(&op.buffer[..end - offset]);
            offset = end;
        }
        Ok(())
    }

    /// 写入一帧（长度前缀 + 载荷）。写入由内部可超时互斥锁串行化。
    pub fn write_frame(&self, payload: &[u8], timeout: Option<Duration>) -> Result<(), PipeError> {
        if payload.is_empty() {
            return Err(PipeError::Protocol("不能发送空载荷".into()));
        }
        let length = u32::try_from(payload.len())
            .map_err(|_| PipeError::Protocol("载荷长度超出 u32".into()))?;
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(payload);
        self.write_all(&frame, timeout)
    }

    fn write_all(&self, buffer: &[u8], timeout: Option<Duration>) -> Result<(), PipeError> {
        if self.poisoned() {
            return Err(PipeError::Disconnected);
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let _guard = self.write_gate.acquire(deadline)?;
        let mut offset = 0;
        while offset < buffer.len() {
            let op = IoOp::new(self.write_event.raw(), buffer[offset..].to_vec());
            let mut transferred = 0u32;
            let overlapped = op.overlapped_ptr();
            // SAFETY: 缓冲区与重叠结构在确认完成前保持有效；事件在调用前复位。
            let write = unsafe {
                let _ = ResetEvent(self.write_event.raw());
                WriteFile(
                    self.handle.raw(),
                    Some(op.buffer.as_slice()),
                    Some(&mut transferred),
                    Some(overlapped),
                )
            };
            let transferred = match write {
                Ok(()) => transferred,
                Err(error) if win32_code(&error) == ERROR_IO_PENDING.0 => {
                    match await_completion(self.handle.raw(), &op, self.write_event.raw(), deadline)
                    {
                        Ok(transferred) => transferred,
                        Err(completion) => {
                            if !completion.harvested {
                                std::mem::forget(op);
                            }
                            if completion.error == PipeError::Timeout
                                && offset == 0
                                && completion.transferred == 0
                            {
                                return Err(PipeError::Timeout);
                            }
                            return Err(self.poison(completion.error));
                        }
                    }
                }
                Err(error) => {
                    let mapped = map_io_error(error);
                    return Err(if matches!(mapped, PipeError::Disconnected) {
                        mapped
                    } else {
                        self.poison(mapped)
                    });
                }
            };
            if transferred == 0 {
                // 写操作返回 0 字节而没有错误：对端不会再消费，继续循环会无限忙等。
                self.broken.store(true, Ordering::Release);
                return Err(PipeError::Disconnected);
            }
            offset += transferred as usize;
        }
        Ok(())
    }

    /// 取消当前挂起的 I/O（用于父进程退出或需要立即唤醒读取线程）。
    pub fn cancel_io(&self) {
        // SAFETY: CancelIoEx 只取消该句柄上的挂起操作，不关闭句柄。
        unsafe {
            let _ = CancelIoEx(self.handle.raw(), None);
        }
    }

    /// 连接出现部分帧或协议错误后必须重建；此状态可用于诊断。
    pub fn is_broken(&self) -> bool {
        self.poisoned()
    }
}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        // SAFETY: 三个句柄均由本对象独占；先取消未完成 I/O，再按字段顺序关闭句柄。
        unsafe {
            let _ = CancelIoEx(self.handle.raw(), None);
        }
    }
}

/// 支持截止时间的互斥锁：写锁等待计入调用方总超时，避免违反调用契约。
struct TimedMutex {
    held: Mutex<bool>,
    ready: Condvar,
}

impl TimedMutex {
    fn new() -> Self {
        Self {
            held: Mutex::new(false),
            ready: Condvar::new(),
        }
    }

    fn acquire(&self, deadline: Option<Instant>) -> Result<TimedGuard<'_>, PipeError> {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if !*held {
                *held = true;
                return Ok(TimedGuard { mutex: self });
            }
            held = match deadline {
                None => self
                    .ready
                    .wait(held)
                    .unwrap_or_else(|error| error.into_inner()),
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(PipeError::Timeout);
                    }
                    self.ready
                        .wait_timeout(held, remaining)
                        .unwrap_or_else(|error| error.into_inner())
                        .0
                }
            };
        }
    }
}

struct TimedGuard<'a> {
    mutex: &'a TimedMutex,
}

impl Drop for TimedGuard<'_> {
    fn drop(&mut self) {
        let mut held = self
            .mutex
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *held = false;
        drop(held);
        self.mutex.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_name(tag: &str) -> String {
        format!(
            r"\\.\pipe\deskshell-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn rejects_invalid_pipe_names() {
        assert!(PipeServer::new(r"\\.\pipe\deskshell-test").is_ok());
        assert!(PipeServer::new("deskshell-test").is_err());
        assert!(PipeServer::new(r"\\.\pipe\").is_err());
    }

    #[test]
    fn timeout_conversion_is_bounded() {
        assert_eq!(timeout_ms(None), u32::MAX);
        assert_eq!(timeout_ms(Some(Duration::ZERO)), 1);
        assert_eq!(timeout_ms(Some(Duration::from_millis(1500))), 1500);
    }

    #[test]
    fn server_and_client_exchange_one_frame() {
        let name = unique_name("basic");
        let server = PipeServer::new(&name).unwrap();
        let server_thread = std::thread::spawn(move || -> Result<Vec<u8>, PipeError> {
            let connection = server.accept(Duration::from_secs(5))?;
            connection.read_frame(1024, Some(Duration::from_secs(5)))
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        client
            .write_frame(b"hello", Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(server_thread.join().unwrap().unwrap(), b"hello");
    }

    #[test]
    fn oversized_frame_is_rejected_before_payload_read() {
        let name = unique_name("limit");
        let server = PipeServer::new(&name).unwrap();
        let server_thread = std::thread::spawn(move || -> Result<Vec<u8>, PipeError> {
            let connection = server.accept(Duration::from_secs(5))?;
            connection.read_frame(8, Some(Duration::from_secs(5)))
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        client
            .write_frame(b"payload-larger-than-limit", Some(Duration::from_secs(5)))
            .unwrap();
        assert!(matches!(
            server_thread.join().unwrap(),
            Err(PipeError::Protocol(_))
        ));
    }

    #[test]
    fn idle_timeout_keeps_stream_usable() {
        let name = unique_name("idle");
        let server = PipeServer::new(&name).unwrap();
        let server_thread = std::thread::spawn(move || -> Result<Vec<u8>, PipeError> {
            let connection = server.accept(Duration::from_secs(5))?;
            // 第一帧没有任何数据：超时后连接仍应可用，随后可读出真正的一帧。
            let idle = connection.read_frame(1024, Some(Duration::from_millis(100)));
            assert_eq!(idle, Err(PipeError::Timeout));
            assert!(!connection.is_broken());
            connection.read_frame(1024, Some(Duration::from_secs(5)))
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        client
            .write_frame(b"after-idle", Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(server_thread.join().unwrap().unwrap(), b"after-idle");
    }

    #[test]
    fn partial_frame_timeout_invalidates_session() {
        let name = unique_name("partial");
        let server = PipeServer::new(&name).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let connection = server.accept(Duration::from_secs(5)).unwrap();
            let first = connection.read_frame(1024, Some(Duration::from_millis(150)));
            sender.send((first, connection.is_broken())).unwrap();
            // 让连接对象在测试结束前保持存活。
            std::thread::sleep(Duration::from_millis(200));
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        // 只发送长度前缀与部分载荷，制造部分帧。
        let mut partial = 10u32.to_le_bytes().to_vec();
        partial.extend_from_slice(b"abc");
        client
            .write_all(&partial, Some(Duration::from_secs(5)))
            .unwrap();
        let (first, broken) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, Err(PipeError::Timeout));
        assert!(broken, "部分帧超时后连接必须标记失效");
        let _ = server_thread.join();
    }

    #[test]
    fn protocol_error_poisoned_connection_fails_fast() {
        let name = unique_name("poison");
        let server = PipeServer::new(&name).unwrap();
        let server_thread = std::thread::spawn(move || -> (bool, Option<PipeError>) {
            let connection = server.accept(Duration::from_secs(5)).unwrap();
            let first = connection.read_frame(8, Some(Duration::from_secs(5)));
            assert!(matches!(first, Err(PipeError::Protocol(_))));
            let second = connection.read_frame(8, Some(Duration::from_secs(1)));
            (connection.is_broken(), second.err())
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        client
            .write_frame(b"way-too-long-for-limit", Some(Duration::from_secs(5)))
            .unwrap();
        let (broken, second) = server_thread.join().unwrap();
        assert!(broken);
        assert_eq!(second, Some(PipeError::Disconnected));
    }

    #[test]
    fn concurrent_readers_never_split_frames() {
        let name = unique_name("concurrent");
        let server = PipeServer::new(&name).unwrap();
        let server_handle = std::thread::spawn(move || {
            std::sync::Arc::new(server.accept(Duration::from_secs(5)).unwrap())
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        let connection = server_handle.join().unwrap();
        let writer = std::thread::spawn(move || {
            for index in 0..20u8 {
                let payload = vec![index; 64];
                client
                    .write_frame(&payload, Some(Duration::from_secs(5)))
                    .unwrap();
            }
        });
        let mut readers = Vec::new();
        for _ in 0..4 {
            let connection = std::sync::Arc::clone(&connection);
            readers.push(std::thread::spawn(move || -> Vec<Vec<u8>> {
                let mut frames = Vec::new();
                while let Ok(payload) = connection.read_frame(1024, Some(Duration::from_secs(2))) {
                    frames.push(payload);
                }
                frames
            }));
        }
        writer.join().unwrap();
        let mut total = 0;
        let mut seen = std::collections::BTreeSet::new();
        for reader in readers {
            for frame in reader.join().unwrap() {
                // 并发读取允许被读锁串行化，但帧内容必须完整一致。
                assert_eq!(frame.len(), 64);
                assert!(frame.iter().all(|byte| *byte == frame[0]));
                seen.insert(frame[0]);
                total += 1;
            }
        }
        assert_eq!(total, 20, "帧数量不符：{total}");
        assert_eq!(seen.len(), 20);
    }

    #[test]
    fn write_timeout_without_reader_does_not_wedge() {
        let name = unique_name("write-timeout");
        let server = PipeServer::new(&name).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            // 接受连接但从不读取，管道缓冲写满后写入方超时。
            let connection = server.accept(Duration::from_secs(5)).unwrap();
            let _ = receiver.recv_timeout(Duration::from_secs(10));
            connection.cancel_io();
        });
        let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
        let payload = vec![b'x'; 2 * 1024 * 1024];
        let started = Instant::now();
        let result = client.write_frame(&payload, Some(Duration::from_millis(200)));
        assert_eq!(result, Err(PipeError::Timeout));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "写超时未按截止时间返回：{:?}",
            started.elapsed()
        );
        let _ = sender.send(());
        let _ = server_thread.join();
    }

    #[test]
    fn repeated_accept_release_does_not_leak_handles() {
        fn round() {
            let name = unique_name("handles");
            let server = PipeServer::new(&name).unwrap();
            let thread = std::thread::spawn(move || {
                let connection = server.accept(Duration::from_secs(5)).unwrap();
                // 读满一帧再释放，避免与客户端写入竞争导致 ERROR_PIPE_CLOSING。
                connection
                    .read_frame(64, Some(Duration::from_secs(5)))
                    .unwrap();
            });
            let client = PipeConnection::connect(&name, Duration::from_secs(5)).unwrap();
            client
                .write_frame(b"x", Some(Duration::from_secs(5)))
                .unwrap();
            drop(client);
            thread.join().unwrap();
        }

        // 进程首次创建线程、TLS 与系统缓存时会一次性增加句柄；先预热再测持续增长。
        for _ in 0..10 {
            round();
        }
        let before = crate::process::process_handle_count(std::process::id())
            .expect("无法读取当前进程句柄数");
        for _ in 0..30 {
            round();
        }
        let after = crate::process::process_handle_count(std::process::id())
            .expect("无法读取当前进程句柄数");
        assert!(
            after <= before + 8,
            "反复建立连接后句柄增长过多：{before} -> {after}"
        );
    }
}
