//! 子进程生命周期：父进程监视、Job 对象兜底清理与诊断所需的进程信息。

use std::{ffi::c_void, mem::size_of};

use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{
                GetProcessHandleCount, GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
                TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
            },
        },
    },
    core::PCWSTR,
};

/// 等待父进程/停止事件的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentWatchOutcome {
    ParentExited,
    StopRequested,
}

#[derive(Debug, Clone)]
pub struct ProcessError {
    operation: &'static str,
    message: String,
}

impl ProcessError {
    fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}失败：{}", self.operation, self.message)
    }
}

impl std::error::Error for ProcessError {}

/// 受 Job 对象约束的宿主进程在主进程消失时由系统终止，作为 IPC 断开与父进程监视之外的兜底。
pub struct KillOnCloseJob {
    handle: HANDLE,
}

impl KillOnCloseJob {
    pub fn create() -> Result<Self, ProcessError> {
        // SAFETY: 创建匿名 Job；成功后由本对象关闭。
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|error| ProcessError::new("创建 Job 对象", error.message().to_string()))?;
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: 结构体大小与指针类型匹配驱动期望的布局。
        let result = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if let Err(error) = result {
            // SAFETY: 句柄由本函数创建且尚未转移。
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(ProcessError::new(
                "设置 Job 终止策略",
                error.message().to_string(),
            ));
        }
        Ok(Self { handle })
    }

    pub fn assign_pid(&self, pid: u32) -> Result<(), ProcessError> {
        // SAFETY: 只获取分配 Job 所需的权限。
        let process = unsafe {
            OpenProcess(
                PROCESS_ACCESS_RIGHTS(PROCESS_SET_QUOTA.0 | PROCESS_TERMINATE.0),
                false,
                pid,
            )
        }
        .map_err(|error| {
            ProcessError::new("打开宿主进程以加入 Job", error.message().to_string())
        })?;
        // SAFETY: 两个句柄均有效；调用不会转移所有权。
        let result = unsafe { AssignProcessToJobObject(self.handle, process) }
            .map_err(|error| ProcessError::new("将宿主加入 Job", error.message().to_string()));
        // SAFETY: 进程句柄由本函数打开。
        unsafe {
            let _ = CloseHandle(process);
        }
        result
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 等待父进程退出或收到主动停止请求。两个事件都是系统等待，不轮询。
pub struct ParentWatch {
    process: HANDLE,
    stop: HANDLE,
}

// SAFETY: ParentWatch 只包含两个等待句柄，方法只执行系统等待/置位，可安全地移交给单个监视线程。
unsafe impl Send for ParentWatch {}
// SAFETY: 共享引用只执行 WaitForMultipleObjects 与 SetEvent，二者对句柄是线程安全的。
unsafe impl Sync for ParentWatch {}

impl ParentWatch {
    pub fn open(parent_pid: u32) -> Result<Self, ProcessError> {
        // SYNCHRONIZE 是通用访问权 0x00100000；仅用于等待进程句柄。
        // SAFETY: 只打开句柄，不修改目标进程。
        let process = unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(0x0010_0000), false, parent_pid) }
            .map_err(|error| ProcessError::new("打开父进程", error.message().to_string()))?;
        // SAFETY: 创建进程内匿名手动重置事件。
        let stop = unsafe {
            windows::Win32::System::Threading::CreateEventW(None, true, false, PCWSTR::null())
        }
        .map_err(|error| ProcessError::new("创建停止事件", error.message().to_string()))?;
        Ok(Self { process, stop })
    }

    pub fn wait(&self) -> Result<ParentWatchOutcome, ProcessError> {
        // SAFETY: 两个句柄均在本对象生命周期内有效。
        let wait = unsafe { WaitForMultipleObjects(&[self.process, self.stop], false, u32::MAX) };
        if wait == WAIT_OBJECT_0 {
            Ok(ParentWatchOutcome::ParentExited)
        } else if wait == windows::Win32::Foundation::WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            Ok(ParentWatchOutcome::StopRequested)
        } else {
            Err(ProcessError::new(
                "等待父进程",
                format!("意外等待结果：{}", wait.0),
            ))
        }
    }

    pub fn request_stop(&self) {
        // SAFETY: 事件句柄有效；失败只影响退出延迟。
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(self.stop);
        }
    }
}

impl Drop for ParentWatch {
    fn drop(&mut self) {
        // SAFETY: 两个句柄由本对象独占。
        unsafe {
            let _ = CloseHandle(self.process);
            let _ = CloseHandle(self.stop);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCpuTimes {
    pub creation_100ns: u64,
    pub kernel_100ns: u64,
    pub user_100ns: u64,
}

impl ProcessCpuTimes {
    pub fn total_100ns(&self) -> u64 {
        self.kernel_100ns.saturating_add(self.user_100ns)
    }
}

fn filetime_to_u64(time: FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
}

pub fn process_cpu_times(pid: u32) -> Option<ProcessCpuTimes> {
    // SAFETY: 只查询进程时间。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: 四个输出指针均指向本地有效存储。
    let result =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    // SAFETY: 句柄由本函数打开。
    unsafe {
        let _ = CloseHandle(process);
    }
    result.ok()?;
    Some(ProcessCpuTimes {
        creation_100ns: filetime_to_u64(creation),
        kernel_100ns: filetime_to_u64(kernel),
        user_100ns: filetime_to_u64(user),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub creation_100ns: Option<u64>,
}

/// 当前所有进程 ID 快照；用于判断启动器返回的 PID 是否是本次新建进程。
pub fn all_process_ids() -> std::collections::HashSet<u32> {
    let mut ids = std::collections::HashSet::new();
    // SAFETY: 快照句柄在函数结束前关闭；结构体按文档初始化大小。
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return ids;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                ids.insert(entry.th32ProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    ids
}

/// 当前进程父子关系快照；用于计算总进程树 CPU，不包含命令行等敏感信息。
pub fn process_parents() -> Vec<(u32, u32)> {
    let mut entries = Vec::new();
    // SAFETY: 快照句柄在函数结束前关闭；结构体按文档初始化大小。
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return entries;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    entries
}

/// 按可执行文件名枚举进程，用于验证没有遗留宿主。
pub fn find_processes_by_name(name: &str) -> Vec<ProcessEntry> {
    let mut entries = Vec::new();
    // SAFETY: 快照句柄在函数结束前关闭；结构体按文档初始化大小。
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return entries;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let length = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                let executable = String::from_utf16_lossy(&entry.szExeFile[..length]);
                if executable.eq_ignore_ascii_case(name) {
                    entries.push(ProcessEntry {
                        pid: entry.th32ProcessID,
                        parent_pid: entry.th32ParentProcessID,
                        creation_100ns: process_cpu_times(entry.th32ProcessID)
                            .map(|times| times.creation_100ns),
                    });
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    entries
}

pub fn terminate_process(pid: u32) -> Result<(), ProcessError> {
    // SAFETY: 只请求终止目标进程。
    let process = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }
        .map_err(|error| ProcessError::new("打开待终止进程", error.message().to_string()))?;
    // SAFETY: 句柄有效；调用后目标进程被终止。
    let result = unsafe { TerminateProcess(process, 1) }
        .map_err(|error| ProcessError::new("终止宿主进程", error.message().to_string()));
    // SAFETY: 句柄由本函数打开。
    unsafe {
        let _ = CloseHandle(process);
    }
    result
}

/// 当前进程句柄数；用于验证连接/事件句柄没有随会话次数增长。
pub fn process_handle_count(pid: u32) -> Option<u32> {
    // SAFETY: 只查询进程句柄数。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut count = 0u32;
    // SAFETY: 输出指针有效；句柄由本函数打开并关闭。
    let result = unsafe { GetProcessHandleCount(process, &mut count) };
    // SAFETY: 句柄由本函数打开。
    unsafe {
        let _ = CloseHandle(process);
    }
    result.ok()?;
    Some(count)
}

pub fn process_alive(pid: u32) -> bool {
    // SAFETY: 只等待进程句柄。
    let Ok(process) = (unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(0x0010_0000), false, pid) })
    else {
        return false;
    };
    // SAFETY: 句柄有效；零超时用于非阻塞检查。
    let wait = unsafe { WaitForSingleObject(process, 0) };
    // SAFETY: 句柄由本函数打开。
    unsafe {
        let _ = CloseHandle(process);
    }
    wait == WAIT_TIMEOUT
}
