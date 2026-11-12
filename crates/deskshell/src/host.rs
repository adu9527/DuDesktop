//! 壁纸宿主监督：命名管道会话、指数退避重启、超时与生命周期清理。
//!
//! 线程模型：
//! - 调用方线程只更新“期望配置”并等待（可选）确认，不直接操作 IPC；
//! - 监督线程负责启动宿主、握手、下发配置、收敛重连期间的变化并处理断开；
//! - 会话读取线程把响应/事件投递给监督状态，断开时唤醒监督线程。
//!
//! 期望配置与版本在同一把锁内原子更新；宿主明确拒绝非法配置时回滚到最近成功配置，
//! 瞬时失败（超时/断开）则保留期望状态，供重连后重放。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use protocol::{
    Body, CommandError, CommandResult, Envelope, ErrorCode, HostCommand, HostEvent, HostHello,
    HostSnapshot, InstanceConfig, MAX_PAYLOAD_BYTES, Source, WallpaperConfig,
};
use shell_win::{
    event::{WaitEvent, wait_two},
    pipe::{PipeConnection, PipeServer},
    process::{KillOnCloseJob, terminate_process},
};

#[derive(Debug, Clone)]
pub enum SupervisorStatus {
    Starting {
        generation: u64,
    },
    Connected {
        generation: u64,
        host_pid: u32,
        snapshot: HostSnapshot,
    },
    Backoff {
        attempt: u32,
        reason: String,
        retry_in_ms: u64,
    },
    Failed {
        reason: String,
    },
    Stopped,
}

impl SupervisorStatus {
    pub fn describe(&self) -> String {
        match self {
            Self::Starting { generation } => format!("启动中（代次 {generation}）"),
            Self::Connected {
                generation,
                host_pid,
                snapshot,
            } => format!(
                "已连接（代次 {generation}，PID {host_pid}，配置版本 {}，{} 个实例）",
                snapshot.applied_config_version,
                snapshot.instances.len()
            ),
            Self::Backoff {
                attempt,
                reason,
                retry_in_ms,
            } => format!("退避重试 {attempt}（{retry_in_ms} ms 后）：{reason}"),
            Self::Failed { reason } => format!("宿主不可用：{reason}"),
            Self::Stopped => "已停止".into(),
        }
    }

    pub fn snapshot(&self) -> Option<&HostSnapshot> {
        match self {
            Self::Connected { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }

    pub fn host_pid(&self) -> Option<u32> {
        match self {
            Self::Connected { host_pid, .. } => Some(*host_pid),
            _ => None,
        }
    }
}

pub struct SupervisorOptions {
    pub executable: PathBuf,
    pub pipe_name: String,
    pub initial_instances: Vec<InstanceConfig>,
    /// 普通命令（Ping、Shutdown）的响应超时。
    pub command_timeout: Duration,
    /// 应用配置包含素材加载，允许更长超时。
    pub apply_timeout: Duration,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_attempts: u32,
    pub backoff_base: Duration,
    /// 仅用于故障注入测试，正常运行为空。
    pub host_args: Vec<String>,
}

impl SupervisorOptions {
    pub fn new(executable: PathBuf, initial_instances: Vec<InstanceConfig>) -> Self {
        Self {
            executable,
            pipe_name: unique_pipe_name(),
            initial_instances,
            command_timeout: Duration::from_secs(5),
            apply_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            max_attempts: 4,
            backoff_base: Duration::from_millis(500),
            host_args: Vec::new(),
        }
    }
}

pub struct HostSupervisor {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl HostSupervisor {
    pub fn start(options: SupervisorOptions) -> Result<Self, String> {
        let initial = WallpaperConfig {
            config_version: 1,
            instances: options.initial_instances.clone(),
        };
        let inner = Arc::new(Inner {
            status: Mutex::new(SupervisorStatus::Starting { generation: 0 }),
            desired: Mutex::new(DesiredState {
                version: initial.config_version,
                instances: initial.instances,
                last_good: None,
            }),
            session: Mutex::new(None),
            stop: AtomicBool::new(false),
            stop_pair: (Mutex::new(()), Condvar::new()),
            change: (Mutex::new(0u64), Condvar::new()),
            apply_error: Mutex::new(None),
            desired_changed: WaitEvent::new()
                .map_err(|error| format!("创建期望配置事件失败：{error}"))?,
            current_pid: AtomicU32::new(0),
            command_timeout: options.command_timeout,
            apply_timeout: options.apply_timeout,
        });
        let thread_inner = Arc::clone(&inner);
        let thread = thread::Builder::new()
            .name("host-supervisor".into())
            .spawn(move || supervisor_loop(thread_inner, options))
            .map_err(|error| format!("创建宿主监督线程失败：{error}"))?;
        Ok(Self {
            inner,
            thread: Some(thread),
        })
    }

    pub fn status(&self) -> SupervisorStatus {
        self.inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// 等待首次连接完成；用于开发命令与验收脚本。
    pub fn wait_connected(&self, timeout: Duration) -> Option<HostSnapshot> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(snapshot) = self.status().snapshot().cloned() {
                return Some(snapshot);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// 只更新期望配置并立即返回版本号；UI 线程可随后通过 `status`/`wait_applied` 观察结果。
    /// 本地先校验配置，非法配置不会污染现有期望状态。
    pub fn set_instances(&self, instances: Vec<InstanceConfig>) -> Result<u64, CommandError> {
        validate_instances(&instances)?;
        let version = {
            let mut desired = self
                .inner
                .desired
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let version = desired.version.saturating_add(1);
            desired.version = version;
            desired.instances = instances;
            version
        };
        self.inner.desired_changed.set();
        Ok(version)
    }

    /// 同步应用：更新期望状态并等待宿主确认（或明确失败），用于 CLI 与验收脚本。
    pub fn apply_instances(
        &self,
        instances: Vec<InstanceConfig>,
    ) -> Result<HostSnapshot, CommandError> {
        let version = self.set_instances(instances)?;
        self.wait_applied(version, self.inner.apply_timeout)
    }

    /// 等待指定配置版本被宿主应用；事件驱动，不轮询。
    pub fn wait_applied(
        &self,
        version: u64,
        timeout: Duration,
    ) -> Result<HostSnapshot, CommandError> {
        let deadline = Instant::now() + timeout;
        let mut change = self
            .inner
            .change
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            // 持有 change 锁时读取状态，避免在“读取状态”和“进入等待”之间丢失通知。
            let status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some((error_version, error)) = self
                .inner
                .apply_error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                && error_version == version
            {
                return Err(error);
            }
            match &status {
                SupervisorStatus::Connected { snapshot, .. }
                    if snapshot.applied_config_version >= version =>
                {
                    return Ok(snapshot.clone());
                }
                SupervisorStatus::Failed { reason } => {
                    return Err(CommandError::new(
                        ErrorCode::NotReady,
                        format!("宿主不可用：{reason}"),
                    ));
                }
                _ => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CommandError::new(
                    ErrorCode::Timeout,
                    format!("等待配置版本 {version} 应用超时：{}", status.describe()),
                ));
            }
            change = match self.inner.change.1.wait_timeout(change, remaining) {
                Ok((guard, _)) => guard,
                Err(error) => error.into_inner().0,
            };
        }
    }

    pub fn ping(&self) -> Result<HostSnapshot, CommandError> {
        let version = self.desired_version();
        self.send_command(HostCommand::Ping, version)
            .map_err(|error| self.with_context(error))
    }

    pub fn snapshot(&self) -> Result<HostSnapshot, CommandError> {
        match self.status() {
            SupervisorStatus::Connected { snapshot, .. } => Ok(snapshot),
            other => Err(CommandError::new(
                ErrorCode::NotReady,
                format!("宿主尚未就绪：{}", other.describe()),
            )),
        }
    }

    fn desired_version(&self) -> u64 {
        self.inner
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .version
    }

    fn send_command(
        &self,
        command: HostCommand,
        config_version: u64,
    ) -> Result<HostSnapshot, CommandError> {
        let session = self
            .inner
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let Some(session) = session else {
            return Err(CommandError::new(
                ErrorCode::NotReady,
                format!("宿主尚未连接：{}", self.status().describe()),
            ));
        };
        match session.request(command, config_version, self.inner.command_timeout) {
            Ok(snapshot) => {
                self.inner.update_snapshot(snapshot.clone());
                Ok(snapshot)
            }
            Err(error) => {
                if error.code == ErrorCode::Timeout {
                    // 无响应的宿主不能继续承载桌面壁纸；终止后由监督线程按退避重启。
                    session.fail("命令超时");
                    let pid = session.host_pid;
                    let _ = terminate_process(pid);
                }
                Err(error)
            }
        }
    }

    fn with_context(&self, error: CommandError) -> CommandError {
        if error.code == ErrorCode::Disconnected {
            CommandError::new(
                ErrorCode::Disconnected,
                format!("{}（{}）", error.message, self.status().describe()),
            )
        } else {
            error
        }
    }

    pub fn shutdown(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        {
            // 持锁唤醒，避免监督线程在“检查停止标志”和进入等待之间丢失通知。
            let (lock, condition) = &self.inner.stop_pair;
            let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
            condition.notify_all();
        }
        self.inner.desired_changed.set();

        let session = self
            .inner
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(session) = session {
            let _ = session.request(
                HostCommand::Shutdown,
                self.desired_version(),
                Duration::from_secs(2),
            );
            session.fail("主进程退出");
        }
        let pid = self.inner.current_pid.load(Ordering::Acquire);
        if pid != 0 {
            let _ = terminate_process(pid);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = SupervisorStatus::Stopped;
    }
}

impl Drop for HostSupervisor {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

struct DesiredState {
    version: u64,
    instances: Vec<InstanceConfig>,
    /// 最近一次宿主确认成功的完整配置；用于拒绝非法配置时回滚。
    last_good: Option<WallpaperConfig>,
}

struct Inner {
    status: Mutex<SupervisorStatus>,
    desired: Mutex<DesiredState>,
    session: Mutex<Option<Arc<Session>>>,
    stop: AtomicBool,
    stop_pair: (Mutex<()>, Condvar),
    /// 状态/快照变化序号，供 `wait_applied` 事件驱动等待。
    change: (Mutex<u64>, Condvar),
    /// 最近一次被宿主明确拒绝的配置版本与原因。
    apply_error: Mutex<Option<(u64, CommandError)>>,
    desired_changed: WaitEvent,
    current_pid: AtomicU32,
    command_timeout: Duration,
    apply_timeout: Duration,
}

impl Inner {
    fn set_status(&self, status: SupervisorStatus) {
        {
            let mut current = self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *current = status;
        }
        self.bump_change();
    }

    fn bump_change(&self) {
        let mut sequence = self
            .change
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *sequence = sequence.wrapping_add(1);
        drop(sequence);
        self.change.1.notify_all();
    }

    fn status_snapshot(&self) -> SupervisorStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn applied_version(&self) -> u64 {
        self.status_snapshot()
            .snapshot()
            .map(|snapshot| snapshot.applied_config_version)
            .unwrap_or(0)
    }

    fn desired_snapshot(&self) -> WallpaperConfig {
        let desired = self
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        WallpaperConfig {
            config_version: desired.version,
            instances: desired.instances.clone(),
        }
    }

    fn set_last_good(&self, config: &WallpaperConfig) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        desired.last_good = Some(config.clone());
    }

    /// 宿主明确拒绝配置时回滚期望状态；只在拒绝的正是当前期望版本时生效。
    fn reject_desired(&self, rejected: &WallpaperConfig, error: &CommandError) {
        if matches!(
            error.code,
            ErrorCode::InvalidRequest | ErrorCode::SourceNotFound
        ) {
            let mut desired = self
                .desired
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if desired.version == rejected.config_version
                && desired.instances == rejected.instances
                && let Some(good) = desired.last_good.clone()
            {
                desired.version = good.config_version;
                desired.instances = good.instances;
            }
        }
        self.record_apply_error(rejected.config_version, error.clone());
    }

    fn record_apply_error(&self, version: u64, error: CommandError) {
        {
            let mut slot = self
                .apply_error
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *slot = Some((version, error));
        }
        self.bump_change();
    }

    fn clear_apply_error(&self, version: u64) {
        let mut slot = self
            .apply_error
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot
            .as_ref()
            .is_some_and(|(slot_version, _)| *slot_version <= version)
        {
            *slot = None;
        }
    }

    fn update_snapshot(&self, snapshot: HostSnapshot) {
        let changed = {
            let mut status = self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let SupervisorStatus::Connected {
                generation,
                host_pid,
                snapshot: current,
            } = &mut *status
                && *generation == snapshot.generation
                && *host_pid == snapshot.host_pid
                && snapshot.applied_config_version >= current.applied_config_version
            {
                // 旧响应/旧事件不得覆盖更新的配置状态。
                *current = snapshot;
                true
            } else {
                false
            }
        };
        if changed {
            self.bump_change();
        }
    }

    fn wait_backoff(&self, delay: Duration) {
        let (lock, condition) = &self.stop_pair;
        let guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        let _ = condition.wait_timeout(guard, delay);
    }
}

struct Session {
    connection: Arc<PipeConnection>,
    generation: u64,
    host_pid: u32,
    next_request: AtomicU64,
    pending: Mutex<HashMap<u64, Sender<CommandResult>>>,
    closed: AtomicBool,
}

impl Session {
    fn new(connection: Arc<PipeConnection>, generation: u64, host_pid: u32) -> Self {
        Self {
            connection,
            generation,
            host_pid,
            next_request: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn request(
        &self,
        command: HostCommand,
        config_version: u64,
        timeout: Duration,
    ) -> Result<HostSnapshot, CommandError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CommandError::new(ErrorCode::Disconnected, "宿主会话已关闭"));
        }
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id, sender);
        let envelope = Envelope::request(self.generation, config_version, request_id, command);
        let payload = match protocol::encode_payload(&envelope) {
            Ok(payload) => payload,
            Err(error) => {
                self.remove_pending(request_id);
                return Err(CommandError::new(ErrorCode::Internal, error.to_string()));
            }
        };
        let deadline = Instant::now() + timeout;
        if let Err(error) = self.connection.write_frame(&payload, Some(timeout)) {
            self.remove_pending(request_id);
            return Err(CommandError::new(
                ErrorCode::Disconnected,
                format!("写入宿主命令失败：{error}"),
            ));
        }
        // 写入与等待共用同一个截止时间，避免总耗时为超时的两倍。
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = if remaining.is_zero() {
            Duration::from_millis(1)
        } else {
            remaining
        };
        match receiver.recv_timeout(wait) {
            Ok(CommandResult::Ok(snapshot)) => {
                if snapshot.generation != self.generation {
                    return Err(CommandError::new(
                        ErrorCode::Internal,
                        format!(
                            "宿主响应代次不匹配：{} != {}",
                            snapshot.generation, self.generation
                        ),
                    ));
                }
                Ok(snapshot)
            }
            Ok(CommandResult::Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.remove_pending(request_id);
                Err(CommandError::new(
                    ErrorCode::Timeout,
                    format!("宿主命令 {} ms 未响应", timeout.as_millis()),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CommandError::new(
                ErrorCode::Disconnected,
                "宿主会话在等待响应时断开",
            )),
        }
    }

    fn remove_pending(&self, request_id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id);
    }

    fn resolve(&self, request_id: u64, result: CommandResult) {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id);
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }

    /// 标记会话失效并唤醒所有等待者；可重复调用。
    fn fail(&self, reason: &str) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending: Vec<(u64, Sender<CommandResult>)> = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .collect();
        for (_, sender) in pending {
            let _ = sender.send(CommandResult::Err(CommandError::new(
                ErrorCode::Disconnected,
                reason.to_owned(),
            )));
        }
        self.connection.cancel_io();
    }
}

/// 瞬时失败：保留期望状态，由重连重放；其它错误视为配置被拒绝。
fn is_transient(error: &CommandError) -> bool {
    matches!(
        error.code,
        ErrorCode::Timeout | ErrorCode::Disconnected | ErrorCode::Internal | ErrorCode::NotReady
    )
}

/// 本地校验，避免明显非法的配置进入期望状态。
fn validate_instances(instances: &[InstanceConfig]) -> Result<(), CommandError> {
    WallpaperConfig {
        config_version: 0,
        instances: instances.to_vec(),
    }
    .validate()
    .map_err(|error| CommandError::new(ErrorCode::InvalidRequest, error.to_string()))?;
    for instance in instances {
        if let Some(path) = instance.source.path()
            && !Path::new(path).is_file()
        {
            return Err(CommandError::new(
                ErrorCode::SourceNotFound,
                format!("实例 {} 的素材不存在：{path}", instance.id),
            ));
        }
        if let Source::Video {
            poster: Some(poster),
            ..
        } = &instance.source
            && !Path::new(poster).is_file()
        {
            return Err(CommandError::new(
                ErrorCode::SourceNotFound,
                format!("实例 {} 的静态回退素材不存在：{poster}", instance.id),
            ));
        }
    }
    Ok(())
}

fn supervisor_loop(inner: Arc<Inner>, options: SupervisorOptions) {
    let job = match KillOnCloseJob::create() {
        Ok(job) => Some(job),
        Err(error) => {
            eprintln!("监督器：创建 Job 对象失败，将依赖父进程监视兜底：{error}");
            None
        }
    };
    let mut generation = 0u64;
    let mut attempt = 0u32;

    while !inner.stop.load(Ordering::Acquire) {
        generation += 1;
        inner.set_status(SupervisorStatus::Starting { generation });
        let started_at = Instant::now();

        let reason = run_generation(&inner, &options, &job, generation)
            .err()
            .unwrap_or_else(|| "宿主会话结束".into());
        if inner.stop.load(Ordering::Acquire) {
            break;
        }
        // 连接稳定运行一段时间后清零退避计数，避免偶发崩溃累积成永久失败。
        if started_at.elapsed() >= Duration::from_secs(30) {
            attempt = 0;
        }
        attempt += 1;
        if attempt > options.max_attempts {
            inner.set_status(SupervisorStatus::Failed {
                reason: format!("连续 {attempt} 次异常退出：{reason}"),
            });
            break;
        }
        let delay = backoff_delay(options.backoff_base, attempt);
        inner.set_status(SupervisorStatus::Backoff {
            attempt,
            reason,
            retry_in_ms: delay.as_millis() as u64,
        });
        inner.wait_backoff(delay);
    }

    if let Some(session) = inner
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        session.fail("监督器停止");
    }
    inner.current_pid.store(0, Ordering::Release);
    if inner.stop.load(Ordering::Acquire) {
        inner.set_status(SupervisorStatus::Stopped);
    }
}

fn run_generation(
    inner: &Arc<Inner>,
    options: &SupervisorOptions,
    job: &Option<KillOnCloseJob>,
    generation: u64,
) -> Result<(), String> {
    let mut child = spawn_host(options, generation)?;
    let pid = child.id();
    inner.current_pid.store(pid, Ordering::Release);
    if let Some(job) = job
        && let Err(error) = job.assign_pid(pid)
    {
        eprintln!("监督器：宿主加入 Job 失败，依赖父进程监视兜底：{error}");
    }

    let result = (|| -> Result<(), String> {
        let server = PipeServer::new(&options.pipe_name).map_err(|error| error.to_string())?;
        let connection = Arc::new(
            server
                .accept(options.connect_timeout)
                .map_err(|error| error.to_string())?,
        );
        let hello = read_hello(&connection, options.handshake_timeout, generation)?;
        if hello.host_pid != pid {
            return Err(format!("宿主进程身份不匹配：{} != {pid}", hello.host_pid));
        }
        if !hello.mpv_available {
            eprintln!("监督器：宿主报告 libmpv 不可用，图片/视频命令将返回明确错误");
        }

        let initial_snapshot = HostSnapshot {
            host_pid: hello.host_pid,
            generation,
            mpv_available: hello.mpv_available,
            mpv_version: hello.mpv_version,
            mpv_client_api: hello.mpv_client_api,
            applied_config_version: 0,
            instances: Vec::new(),
        };
        let session = Arc::new(Session::new(Arc::clone(&connection), generation, pid));
        let disconnect =
            Arc::new(WaitEvent::new().map_err(|error| format!("创建断开事件失败：{error}"))?);
        let disconnect_reason = Arc::new(Mutex::new(String::new()));
        let reader_session = Arc::clone(&session);
        let reader_reason = Arc::clone(&disconnect_reason);
        let reader_disconnect = Arc::clone(&disconnect);
        let reader_inner = Arc::clone(inner);
        let reader = thread::Builder::new()
            .name("host-session-reader".into())
            .spawn(move || {
                let reason = session_read_loop(&reader_inner, &reader_session);
                *reader_reason
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = reason.clone();
                reader_session.fail(&reason);
                reader_disconnect.set();
            })
            .map_err(|error| format!("创建宿主会话读取线程失败：{error}"))?;

        let desired = inner.desired_snapshot();
        let applied = match session.request(
            HostCommand::ApplyState {
                config: desired.clone(),
            },
            desired.config_version,
            inner.apply_timeout,
        ) {
            Ok(snapshot) => {
                inner.set_last_good(&desired);
                inner.clear_apply_error(desired.config_version);
                Some(snapshot)
            }
            Err(error) if is_transient(&error) => {
                session.fail("初始配置应用失败");
                let _ = reader.join();
                return Err(format!("初始配置应用失败：{error:?}"));
            }
            Err(error) => {
                // 宿主明确拒绝：记录原因，保持连接以便用户修正配置后重试。
                eprintln!("监督器：初始配置被宿主拒绝：{error:?}");
                inner.record_apply_error(desired.config_version, error);
                None
            }
        };
        if session.closed.load(Ordering::Acquire) {
            let _ = reader.join();
            return Err(disconnect_reason
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone());
        }

        // 初始配置应用完成后再发布连接状态，避免命令与首次配置下发交错。
        *inner
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&session));
        inner.set_status(SupervisorStatus::Connected {
            generation,
            host_pid: pid,
            snapshot: applied.unwrap_or(initial_snapshot),
        });

        // 收敛循环：连接建立期间调用方可能已经更新了期望配置，需要继续下发。
        loop {
            loop {
                let desired = inner.desired_snapshot();
                if desired.config_version <= inner.applied_version() {
                    break;
                }
                let already_rejected = inner
                    .apply_error
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .is_some_and(|(version, _)| *version == desired.config_version);
                if already_rejected {
                    break;
                }
                match session.request(
                    HostCommand::ApplyState {
                        config: desired.clone(),
                    },
                    desired.config_version,
                    inner.apply_timeout,
                ) {
                    Ok(snapshot) => {
                        inner.set_last_good(&desired);
                        inner.clear_apply_error(desired.config_version);
                        inner.update_snapshot(snapshot);
                    }
                    Err(error) if is_transient(&error) => {
                        session.fail("应用配置失败");
                        break;
                    }
                    Err(error) => {
                        eprintln!(
                            "监督器：配置版本 {} 被拒绝：{error:?}",
                            desired.config_version
                        );
                        inner.reject_desired(&desired, &error);
                        break;
                    }
                }
            }

            if session.closed.load(Ordering::Acquire) || inner.stop.load(Ordering::Acquire) {
                break;
            }
            // 等期望配置变化或会话断开；两者都是事件，不做周期轮询。
            match wait_two(&inner.desired_changed, &disconnect, None) {
                Some(0) => continue,
                _ => break,
            }
        }

        let reason = disconnect_reason
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let _ = reader.join();
        *inner
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        if inner.stop.load(Ordering::Acquire) {
            Ok(())
        } else if reason.is_empty() {
            Err("宿主会话结束".into())
        } else {
            Err(reason)
        }
    })();

    inner.current_pid.store(0, Ordering::Release);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn spawn_host(options: &SupervisorOptions, generation: u64) -> Result<Child, String> {
    Command::new(&options.executable)
        .arg("--pipe")
        .arg(&options.pipe_name)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--generation")
        .arg(generation.to_string())
        .args(&options.host_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("启动宿主 {} 失败：{error}", options.executable.display()))
}

fn read_hello(
    connection: &PipeConnection,
    timeout: Duration,
    generation: u64,
) -> Result<HostHello, String> {
    let payload = connection
        .read_frame(MAX_PAYLOAD_BYTES, Some(timeout))
        .map_err(|error| format!("读取宿主握手失败：{error}"))?;
    let envelope =
        protocol::decode_payload(&payload).map_err(|error| format!("宿主握手协议错误：{error}"))?;
    if envelope.host_generation != generation {
        return Err(format!(
            "宿主握手代次不匹配：{} != {generation}",
            envelope.host_generation
        ));
    }
    match envelope.body {
        Body::Hello(hello) => Ok(hello),
        _ => Err("宿主首条消息不是 Hello".into()),
    }
}

fn session_read_loop(inner: &Inner, session: &Arc<Session>) -> String {
    loop {
        let payload = match session.connection.read_frame(MAX_PAYLOAD_BYTES, None) {
            Ok(payload) => payload,
            Err(error) => return format!("宿主连接中断：{error}"),
        };
        let envelope = match protocol::decode_payload(&payload) {
            Ok(envelope) => envelope,
            Err(error) => return format!("宿主消息协议错误：{error}"),
        };
        if envelope.host_generation != session.generation {
            return format!(
                "宿主消息代次过期：{} != {}",
                envelope.host_generation, session.generation
            );
        }
        match envelope.body {
            Body::Response(result) => {
                if let Some(request_id) = envelope.request_id {
                    session.resolve(request_id, result);
                }
            }
            Body::Event(HostEvent::Snapshot(snapshot)) => {
                inner.update_snapshot(snapshot);
            }
            Body::Event(HostEvent::Log { level, message }) => {
                eprintln!("宿主日志[{level}]：{message}");
            }
            Body::Hello(_) => return "宿主重复发送 Hello".into(),
            Body::Request(_) => return "宿主向主进程发送了命令".into(),
        }
    }
}

/// 指数退避：第 1 次失败等待 base，之后每次翻倍。
pub fn backoff_delay(base: Duration, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(10);
    base.saturating_mul(1u32 << exponent)
}

pub fn unique_pipe_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        r"\\.\pipe\deskshell-{}-{nanos}-{sequence}",
        std::process::id()
    )
}

pub fn locate_host_executable() -> Result<PathBuf, String> {
    let filename = format!("wallpaper-host{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe()
        .map_err(|error| format!("获取当前程序路径失败：{error}"))?
        .with_file_name(filename);
    if !executable.is_file() {
        return Err(format!(
            "未找到宿主：{}；请先执行 scripts/dev.ps1 build",
            executable.display()
        ));
    }
    Ok(executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let base = Duration::from_millis(500);
        assert_eq!(backoff_delay(base, 1), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, 2), Duration::from_millis(1000));
        assert_eq!(backoff_delay(base, 3), Duration::from_millis(2000));
        assert_eq!(backoff_delay(base, 4), Duration::from_millis(4000));
        assert_eq!(backoff_delay(base, 11), Duration::from_millis(512_000));
        assert_eq!(backoff_delay(base, 0), Duration::from_millis(500));
    }

    #[test]
    fn pipe_names_are_unique_and_local() {
        let first = unique_pipe_name();
        let second = unique_pipe_name();
        assert_ne!(first, second);
        assert!(first.starts_with(r"\\.\pipe\deskshell-"));
    }

    #[test]
    fn local_validation_rejects_missing_sources_before_desired_update() {
        let config = InstanceConfig {
            id: "missing".into(),
            monitor: None,
            display_id: None,
            desktop: false,
            window: None,
            source: Source::Image {
                path: r"C:\definitely-missing\wallpaper.png".into(),
            },
            paused: false,
            render: Default::default(),
        };
        assert_eq!(
            validate_instances(&[config]).unwrap_err().code,
            ErrorCode::SourceNotFound
        );
    }
}
