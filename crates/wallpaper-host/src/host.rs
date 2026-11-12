//! 宿主主循环：命名管道命令、实例管理、mpv 事件聚合与生命周期清理。
//!
//! 主线程运行 Win32 消息循环并处理所有实例状态；管道读取、管道写入、mpv 事件与父进程监视
//! 各自在独立线程中工作，通过自动重置事件唤醒主循环，不使用忙轮询。
//! 主线程不做可能阻塞的管道写入：响应先进入有界队列，由写入线程按截止时间发送。

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use protocol::{
    Body, CommandError, CommandResult, Envelope, ErrorCode, HostCommand, HostEvent, HostHello,
    HostSnapshot, MAX_PAYLOAD_BYTES, PROTOCOL_VERSION, WallpaperConfig,
};
use shell_win::{event::WaitEvent, pipe::PipeConnection, preview, process::ParentWatch};
use windows::Win32::{
    Foundation::{WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT},
    UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage, WM_DISPLAYCHANGE, WM_DPICHANGED,
        WM_POWERBROADCAST, WM_WTSSESSION_CHANGE,
    },
};

use crate::{instance::Instance, mpv::MpvLibrary};

/// 命令队列上限。主进程按请求-响应节奏发送，正常不会接近；超过说明对端行为异常。
const MAX_CONTROL_MESSAGES: usize = 64;
/// 响应/事件发送队列上限，避免主进程不读时无限占用内存。
const MAX_OUTBOUND_FRAMES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    InvalidProtocol,
    Oversized,
    Hang,
    Disconnect,
}

impl FaultMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "invalid-protocol" => Some(Self::InvalidProtocol),
            "oversized" => Some(Self::Oversized),
            "hang" => Some(Self::Hang),
            "disconnect" => Some(Self::Disconnect),
            _ => None,
        }
    }
}

pub struct HostOptions {
    pub pipe_name: String,
    pub generation: u64,
    pub parent_pid: u32,
    pub fault: Option<FaultMode>,
    pub fault_generation: Option<u64>,
}

pub enum HostMessage {
    Command {
        request_id: u64,
        config_version: u64,
        command: HostCommand,
    },
    /// 同一实例的重复通知合并为一次，避免视频播放时事件堆积。
    PlayerChanged {
        instance_id: String,
    },
    PeerDisconnected {
        reason: String,
    },
    ParentExited,
}

struct QueueState {
    control: VecDeque<HostMessage>,
    changed: BTreeSet<String>,
}

struct QueueBatch {
    control: Vec<HostMessage>,
    changed: Vec<String>,
}

pub struct HostQueue {
    state: Mutex<QueueState>,
    event: WaitEvent,
}

impl HostQueue {
    fn new() -> Result<Self, String> {
        Ok(Self {
            state: Mutex::new(QueueState {
                control: VecDeque::new(),
                changed: BTreeSet::new(),
            }),
            event: WaitEvent::new().map_err(|error| format!("创建宿主事件失败：{error}"))?,
        })
    }

    pub fn push(&self, message: HostMessage) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            match message {
                HostMessage::PlayerChanged { instance_id } => {
                    state.changed.insert(instance_id);
                }
                HostMessage::Command { .. } => {
                    if state.control.len() >= MAX_CONTROL_MESSAGES {
                        // 明确背压：丢弃积压命令并断开，而不是无界增长或静默丢单条命令。
                        state.control.clear();
                        state.control.push_back(HostMessage::PeerDisconnected {
                            reason: format!("命令队列超过 {MAX_CONTROL_MESSAGES} 条，断开连接"),
                        });
                    } else {
                        state.control.push_back(message);
                    }
                }
                other => state.control.push_back(other),
            }
        }
        self.event.set();
    }

    fn drain(&self) -> QueueBatch {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        QueueBatch {
            control: state.control.drain(..).collect(),
            changed: state.changed.iter().cloned().collect(),
        }
    }

    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        self.event.handle()
    }
}

/// 管道写入线程：主循环只入队，不因对端不读而卡住消息处理。
struct IpcWriter {
    sender: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl IpcWriter {
    fn spawn(connection: Arc<PipeConnection>, queue: Arc<HostQueue>) -> Result<Self, String> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<u8>>(MAX_OUTBOUND_FRAMES);
        let thread = std::thread::Builder::new()
            .name("host-ipc-writer".into())
            .spawn(move || {
                while let Ok(payload) = receiver.recv() {
                    if let Err(error) =
                        connection.write_frame(&payload, Some(Duration::from_secs(2)))
                    {
                        queue.push(HostMessage::PeerDisconnected {
                            reason: format!("写入主进程失败：{error}"),
                        });
                        return;
                    }
                }
            })
            .map_err(|error| format!("创建 IPC 写入线程失败：{error}"))?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    fn send(&self, envelope: &Envelope) -> Result<(), String> {
        let payload = protocol::encode_payload(envelope).map_err(|error| error.to_string())?;
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| "写入线程已停止".to_string())?;
        sender.try_send(payload).map_err(|error| match error {
            std::sync::mpsc::TrySendError::Full(_) => {
                format!("发送队列已满（上限 {MAX_OUTBOUND_FRAMES}）")
            }
            std::sync::mpsc::TrySendError::Disconnected(_) => "写入线程已停止".into(),
        })
    }

    /// 停止入队并等待已排队消息写完；最多受每条消息的写超时约束。
    fn shutdown(&mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn run(options: HostOptions) -> Result<(), String> {
    shell_win::enable_per_monitor_dpi_awareness();
    // WIC 图片解码与 COM 组件需要当前线程初始化 COM；MTA 避免与 mpv/窗口消息争用 STA。
    let _apartment = ComApartment::enter()?;

    let library = match MpvLibrary::load() {
        Ok(library) => Some(library),
        Err(error) => {
            eprintln!("宿主：libmpv 不可用：{error}");
            None
        }
    };
    let mpv_version = library.as_ref().and_then(|library| library.probe_version());
    let mpv_client_api = library.as_ref().map(|library| library.client_api_string());

    let connection = Arc::new(
        PipeConnection::connect(&options.pipe_name, Duration::from_secs(10))
            .map_err(|error| format!("连接主进程管道失败：{error}"))?,
    );

    let hello = HostHello {
        host_pid: std::process::id(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        mpv_available: library.is_some(),
        mpv_version: mpv_version.clone(),
        mpv_client_api: mpv_client_api.clone(),
    };
    send_envelope(
        &connection,
        &Envelope {
            protocol_version: PROTOCOL_VERSION,
            host_generation: options.generation,
            config_version: 0,
            request_id: None,
            body: Body::Hello(hello),
        },
    )
    .map_err(|error| format!("发送握手失败：{error}"))?;

    if options.fault.is_some_and(|_| {
        options
            .fault_generation
            .map(|generation| generation == options.generation)
            .unwrap_or(true)
    }) {
        return run_fault(&connection, options.fault.expect("已检查"));
    }

    let queue = Arc::new(HostQueue::new()?);

    let reader_queue = Arc::clone(&queue);
    let reader_connection = Arc::clone(&connection);
    let generation = options.generation;
    let reader = std::thread::Builder::new()
        .name("host-ipc-reader".into())
        .spawn(move || read_loop(reader_connection, reader_queue, generation))
        .map_err(|error| format!("创建 IPC 读取线程失败：{error}"))?;

    // 任何后续初始化失败都必须显式取消读取并回收线程，避免半初始化状态泄漏。
    let parent_watch = match ParentWatch::open(options.parent_pid) {
        Ok(watch) => Arc::new(watch),
        Err(error) => {
            connection.cancel_io();
            let _ = reader.join();
            return Err(format!("监视父进程失败：{error}"));
        }
    };
    let writer = match IpcWriter::spawn(Arc::clone(&connection), Arc::clone(&queue)) {
        Ok(writer) => writer,
        Err(error) => {
            connection.cancel_io();
            let _ = reader.join();
            return Err(error);
        }
    };

    let watcher_watch = Arc::clone(&parent_watch);
    let watcher_queue = Arc::clone(&queue);
    let watcher_connection = Arc::clone(&connection);
    let watcher = match std::thread::Builder::new()
        .name("host-parent-watch".into())
        .spawn(move || {
            match watcher_watch.wait() {
                Ok(shell_win::process::ParentWatchOutcome::ParentExited) => {
                    watcher_queue.push(HostMessage::ParentExited);
                }
                Ok(shell_win::process::ParentWatchOutcome::StopRequested) => {}
                Err(error) => eprintln!("宿主：等待父进程失败：{error}"),
            }
            // 无论父进程退出还是主动停止，都取消挂起读取以唤醒主循环。
            watcher_connection.cancel_io();
        }) {
        Ok(thread) => thread,
        Err(error) => {
            connection.cancel_io();
            let _ = reader.join();
            let mut writer = writer;
            writer.shutdown();
            return Err(format!("创建父进程监视线程失败：{error}"));
        }
    };

    let mut app = HostApp::new(
        options.generation,
        library,
        mpv_version,
        mpv_client_api,
        Arc::clone(&queue),
        writer,
    );

    let mut running = true;
    while running {
        // 看门狗使用绝对期限：无论被 IPC、窗口消息还是超时唤醒，都会在循环末尾检查。
        let timeout = match app.next_deadline() {
            None => u32::MAX,
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                (remaining.as_millis().min(u32::MAX as u128) as u32).max(1)
            }
        };
        // SAFETY: 等待本进程事件与窗口消息。
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&[queue.handle()]),
                timeout,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if wait == WAIT_OBJECT_0 {
            let batch = queue.drain();
            for message in batch.control {
                if app.handle(message) {
                    running = false;
                    break;
                }
            }
            if running {
                for instance_id in batch.changed {
                    app.player_changed(&instance_id);
                }
            }
        } else if wait == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            let events = pump_messages();
            if events.any() && running {
                app.on_system_events(events);
            }
        } else if wait == WAIT_TIMEOUT {
            // 由绝对期限检查统一处理。
        } else {
            eprintln!("宿主：消息等待失败，退出码 {}", wait.0);
            running = false;
        }
        if running {
            app.check_deadlines();
        }
        if !app.flush_snapshot() {
            running = false;
        }
    }

    app.shutdown_all();
    parent_watch.request_stop();
    connection.cancel_io();
    let _ = watcher.join();
    let _ = reader.join();
    app.shutdown_writer();
    Ok(())
}

fn send_envelope(connection: &PipeConnection, envelope: &Envelope) -> Result<(), String> {
    let payload = protocol::encode_payload(envelope).map_err(|error| error.to_string())?;
    connection
        .write_frame(&payload, Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())
}

fn read_loop(connection: Arc<PipeConnection>, queue: Arc<HostQueue>, generation: u64) {
    loop {
        let payload = match connection.read_frame(MAX_PAYLOAD_BYTES, None) {
            Ok(payload) => payload,
            Err(error) => {
                queue.push(HostMessage::PeerDisconnected {
                    reason: error.to_string(),
                });
                return;
            }
        };
        let envelope = match protocol::decode_payload(&payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                queue.push(HostMessage::PeerDisconnected {
                    reason: format!("协议错误：{error}"),
                });
                return;
            }
        };
        if envelope.host_generation != generation {
            queue.push(HostMessage::PeerDisconnected {
                reason: format!(
                    "宿主代次不匹配：收到 {}，当前 {generation}",
                    envelope.host_generation
                ),
            });
            return;
        }
        let request_id = match envelope.request_id {
            Some(request_id) => request_id,
            None => {
                queue.push(HostMessage::PeerDisconnected {
                    reason: "请求缺少请求 ID".into(),
                });
                return;
            }
        };
        match envelope.body {
            Body::Request(command) => queue.push(HostMessage::Command {
                request_id,
                config_version: envelope.config_version,
                command,
            }),
            _ => {
                queue.push(HostMessage::PeerDisconnected {
                    reason: "客户端发送了非请求消息".into(),
                });
                return;
            }
        }
    }
}

fn pump_messages() -> SystemEvents {
    let mut events = SystemEvents::default();
    let mut message = MSG::default();
    // SAFETY: 只处理本线程窗口消息；广播消息在派发前记录，用于重新校验桌面附着。
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            match message.message {
                WM_DISPLAYCHANGE | WM_DPICHANGED => events.display_change = true,
                WM_WTSSESSION_CHANGE => events.session_change = true,
                WM_POWERBROADCAST => events.resume = true,
                _ => {}
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    events
}

/// 需要重新校验桌面附着的系统事件。
#[derive(Debug, Clone, Copy, Default)]
struct SystemEvents {
    display_change: bool,
    session_change: bool,
    resume: bool,
}

impl SystemEvents {
    fn any(&self) -> bool {
        self.display_change || self.session_change || self.resume
    }
}

fn run_fault(connection: &PipeConnection, fault: FaultMode) -> Result<(), String> {
    match fault {
        FaultMode::InvalidProtocol => {
            let mut envelope = Envelope::request(0, 0, 1, HostCommand::Ping);
            envelope.protocol_version = PROTOCOL_VERSION + 1;
            let payload = protocol::encode_payload(&envelope).map_err(|error| error.to_string())?;
            let _ = connection.write_frame(&payload, Some(Duration::from_secs(2)));
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        }
        FaultMode::Oversized => {
            let payload = vec![b'x'; MAX_PAYLOAD_BYTES + 1];
            let _ = connection.write_frame(&payload, Some(Duration::from_secs(2)));
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        }
        FaultMode::Hang => {
            // 不读取命令也不响应，等待主进程按超时策略终止。
            std::thread::sleep(Duration::from_secs(60));
            Ok(())
        }
        FaultMode::Disconnect => Ok(()),
    }
}

struct HostApp {
    generation: u64,
    library: Option<Arc<MpvLibrary>>,
    mpv_version: Option<String>,
    mpv_client_api: Option<String>,
    queue: Arc<HostQueue>,
    writer: IpcWriter,
    instances: BTreeMap<String, Instance>,
    applied: Option<WallpaperConfig>,
    /// 最近一次应用是否全部实例成功；失败时允许用同一版本重试。
    applied_ok: bool,
    pending_snapshot: bool,
    write_failed: bool,
}

impl HostApp {
    fn new(
        generation: u64,
        library: Option<Arc<MpvLibrary>>,
        mpv_version: Option<String>,
        mpv_client_api: Option<String>,
        queue: Arc<HostQueue>,
        writer: IpcWriter,
    ) -> Self {
        Self {
            generation,
            library,
            mpv_version,
            mpv_client_api,
            queue,
            writer,
            instances: BTreeMap::new(),
            applied: None,
            applied_ok: false,
            pending_snapshot: false,
            write_failed: false,
        }
    }

    fn handle(&mut self, message: HostMessage) -> bool {
        match message {
            HostMessage::Command {
                request_id,
                config_version,
                command,
            } => {
                let result = match command {
                    HostCommand::ApplyState { config } => match self.apply_state(config) {
                        Ok(snapshot) => CommandResult::Ok(snapshot),
                        Err(error) => CommandResult::Err(error),
                    },
                    HostCommand::Ping => CommandResult::Ok(self.snapshot()),
                    HostCommand::Shutdown => {
                        let snapshot = self.snapshot();
                        self.respond(request_id, config_version, CommandResult::Ok(snapshot));
                        self.shutdown_all();
                        return true;
                    }
                };
                self.respond(request_id, config_version, result);
                self.write_failed
            }
            HostMessage::PlayerChanged { instance_id } => {
                self.player_changed(&instance_id);
                false
            }
            HostMessage::PeerDisconnected { reason } => {
                eprintln!("宿主：主进程连接断开：{reason}");
                true
            }
            HostMessage::ParentExited => {
                eprintln!("宿主：父进程已退出，清理并结束");
                true
            }
        }
    }

    fn player_changed(&mut self, instance_id: &str) {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.refresh();
        }
        self.pending_snapshot = true;
    }

    /// 显示变化、会话锁定/解锁与休眠恢复后重新校验桌面附着。
    fn on_system_events(&mut self, events: SystemEvents) {
        if !events.any() {
            return;
        }
        let mut changed = false;
        for instance in self.instances.values_mut() {
            changed |= instance.revalidate();
        }
        if changed {
            self.pending_snapshot = true;
        }
    }

    fn apply_state(&mut self, config: WallpaperConfig) -> Result<HostSnapshot, CommandError> {
        config
            .validate()
            .map_err(|error| CommandError::new(ErrorCode::InvalidRequest, error.to_string()))?;

        if let Some(applied) = &self.applied {
            if config.config_version < applied.config_version {
                return Err(CommandError::new(
                    ErrorCode::InvalidRequest,
                    format!(
                        "配置版本 {} 小于已应用版本 {}",
                        config.config_version, applied.config_version
                    ),
                ));
            }
            if config.config_version == applied.config_version {
                if config.instances != applied.instances {
                    return Err(CommandError::new(
                        ErrorCode::InvalidRequest,
                        format!(
                            "配置版本 {} 对应不同实例列表；版本必须随内容单调递增",
                            config.config_version
                        ),
                    ));
                }
                if self.applied_ok {
                    return Ok(self.snapshot());
                }
                // 上次应用存在实例级失败：同一版本允许重试，语义保持一致。
            }
        }

        // 先验证全部素材与显示器，避免部分实例进入不可恢复状态。
        for instance in &config.instances {
            if let Some(path) = instance.source.path()
                && !Path::new(path).is_file()
            {
                return Err(CommandError::new(
                    ErrorCode::SourceNotFound,
                    format!("实例 {} 的素材不存在：{path}", instance.id),
                ));
            }
            if let protocol::Source::Video {
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
            if instance.window.is_none() && preview::monitor_work_area(instance.monitor).is_none() {
                return Err(CommandError::new(
                    ErrorCode::InvalidRequest,
                    format!("实例 {} 指定的显示器不存在", instance.id),
                ));
            }
        }

        let desired: BTreeSet<&str> = config
            .instances
            .iter()
            .map(|instance| instance.id.as_str())
            .collect();

        // 先创建新实例；失败时丢弃临时实例并保留旧状态。
        let mut created = Vec::new();
        for instance_config in &config.instances {
            if self.instances.contains_key(&instance_config.id) {
                continue;
            }
            let queue = Arc::clone(&self.queue);
            let id = instance_config.id.clone();
            let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                queue.push(HostMessage::PlayerChanged {
                    instance_id: id.clone(),
                });
            });
            match Instance::new(instance_config, self.library.clone(), callback) {
                Ok(instance) => created.push(instance),
                Err(error) => {
                    for mut instance in created {
                        instance.shutdown();
                    }
                    return Err(error);
                }
            }
        }
        for instance in created {
            self.instances.insert(instance.id().to_owned(), instance);
        }

        let removed: Vec<String> = self
            .instances
            .keys()
            .filter(|id| !desired.contains(id.as_str()))
            .cloned()
            .collect();
        for id in removed {
            if let Some(mut instance) = self.instances.remove(&id) {
                instance.shutdown();
            }
        }

        let mut first_error = None;
        for instance_config in &config.instances {
            if let Some(instance) = self.instances.get_mut(&instance_config.id)
                && let Err(error) = instance.apply(instance_config)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }

        self.applied_ok = first_error.is_none();
        self.applied = Some(config);
        self.pending_snapshot = true;
        match first_error {
            Some(error) => Err(error),
            None => Ok(self.snapshot()),
        }
    }

    fn snapshot(&mut self) -> HostSnapshot {
        HostSnapshot {
            host_pid: std::process::id(),
            generation: self.generation,
            mpv_available: self.library.is_some(),
            mpv_version: self.mpv_version.clone(),
            mpv_client_api: self.mpv_client_api.clone(),
            applied_config_version: self
                .applied
                .as_ref()
                .map(|config| config.config_version)
                .unwrap_or(0),
            instances: self
                .instances
                .values_mut()
                .map(|instance| instance.snapshot())
                .collect(),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.instances
            .values()
            .filter_map(|instance| instance.next_deadline())
            .min()
    }

    /// 绝对期限检查：不受事件频率影响，避免持续 IPC/mpv 事件让看门狗饥饿。
    fn check_deadlines(&mut self) {
        let now = Instant::now();
        let due = self
            .instances
            .values()
            .filter_map(|instance| instance.next_deadline())
            .any(|deadline| deadline <= now);
        if !due {
            return;
        }
        let mut changed = false;
        for instance in self.instances.values_mut() {
            if instance
                .next_deadline()
                .is_some_and(|deadline| deadline <= now)
                && instance.tick()
            {
                changed = true;
            }
        }
        if changed {
            self.pending_snapshot = true;
        }
    }

    fn flush_snapshot(&mut self) -> bool {
        if !self.pending_snapshot {
            return !self.write_failed;
        }
        self.pending_snapshot = false;
        let snapshot = self.snapshot();
        let envelope = Envelope::event(
            self.generation,
            snapshot.applied_config_version,
            HostEvent::Snapshot(snapshot),
        );
        if let Err(error) = self.writer.send(&envelope) {
            eprintln!("宿主：发送状态事件失败：{error}");
            self.write_failed = true;
        }
        !self.write_failed
    }

    fn respond(&mut self, request_id: u64, config_version: u64, result: CommandResult) {
        let envelope = Envelope::response(self.generation, config_version, request_id, result);
        if let Err(error) = self.writer.send(&envelope) {
            eprintln!("宿主：发送响应失败：{error}");
            self.write_failed = true;
        }
    }

    fn shutdown_all(&mut self) {
        for (_, mut instance) in std::mem::take(&mut self.instances) {
            instance.shutdown();
        }
    }

    fn shutdown_writer(&mut self) {
        self.writer.shutdown();
    }
}

/// 宿主主线程的 COM 初始化守卫；WIC 解码需要 COM。
pub(crate) struct ComApartment {
    owned: bool,
}

impl ComApartment {
    pub(crate) fn enter() -> Result<Self, String> {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        // SAFETY: 只初始化当前宿主主线程的 COM apartment。
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            return Ok(Self { owned: true });
        }
        // RPC_E_CHANGED_MODE：线程已以其它模型初始化；继续使用已有 apartment。
        if result.0 as u32 == 0x8001_0106 {
            return Ok(Self { owned: false });
        }
        Err(format!("初始化宿主 COM 失败：{result:?}"))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: 与 enter 中的 CoInitializeEx 配对。
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}
