//! 主进程与壁纸宿主之间的版本化数据契约。
//!
//! 本 crate 不依赖窗口、GPU 或传输实现：命名管道只负责搬运这里定义的有界帧。
//! 命令采用完整期望状态（幂等），宿主重连后由主进程重新下发，不依赖增量补发。

use serde::{Deserialize, Serialize};

/// 协议版本。任何破坏兼容性的字段变化都必须递增，并同步更新主进程与宿主。
///
/// v5：加入图片效果参数、实际渲染路径与效果/GPU 可观测字段；主进程与宿主必须同版本部署。
/// v6：加入壁纸缩放方式（cover/contain/stretch/tile）。
pub const PROTOCOL_VERSION: u32 = 6;

/// 图片效果参数 schema 版本；字段语义变化时递增。
pub const EFFECT_SCHEMA_VERSION: u32 = 1;

/// 单帧总大小上限（含 4 字节长度前缀）。用于拒绝异常或恶意输入，避免无界分配。
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// 载荷上限。
pub const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - 4;

/// 单台主机允许的壁纸实例上限，防止错误配置造成无界资源占用。
pub const MAX_INSTANCES: usize = 16;

const MAX_INSTANCE_ID_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    /// 没有素材，播放资源已释放。
    Stopped,
    /// 素材正在加载，尚未确认首帧。
    Loading,
    Playing,
    Paused,
    /// 按质量策略停在首帧/当前帧：播放器仍持有画面，但不再持续解码与呈现。
    Static,
    /// 视频失败后按要求显示静态回退素材。
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    None,
    Image,
    Video,
}

/// 当前实际显示的素材类型；回退时与配置的 `SourceKind` 不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayedSource {
    None,
    Image,
    Video,
    Poster,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    None,
    Image {
        path: String,
    },
    Video {
        path: String,
        /// 硬解不可用或解码失败时可显式显示的静态图；不会自动软件解码视频。
        #[serde(default)]
        poster: Option<String>,
        #[serde(default)]
        loop_playback: bool,
    },
}

impl Source {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::None => SourceKind::None,
            Self::Image { .. } => SourceKind::Image,
            Self::Video { .. } => SourceKind::Video,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Image { path } | Self::Video { path, .. } => Some(path),
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::None => Ok(()),
            Self::Image { path } => validate_path(path),
            Self::Video { path, poster, .. } => {
                validate_path(path)?;
                if let Some(poster) = poster {
                    validate_path(poster)?;
                }
                Ok(())
            }
        }
    }
}

fn validate_path(path: &str) -> Result<(), ConfigError> {
    if path.trim().is_empty() {
        return Err(ConfigError::EmptyPath);
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(ConfigError::PathTooLong);
    }
    Ok(())
}

/// 视频呈现策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPlayback {
    /// 正常播放（Balanced/Ultra 或用户显式要求）。
    #[default]
    Animate,
    /// 停在当前帧，停止持续解码与呈现；用于 Eco 与自动节能规则。
    Static,
}

/// 壁纸缩放方式；默认铺满裁切，避免出现上下黑边。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// 等比放大并裁切溢出，填满显示器。
    #[default]
    Cover,
    /// 等比完整显示，留黑边。
    Contain,
    /// 不保持比例，拉伸到显示器尺寸。
    Stretch,
    /// 按源图原生尺寸平铺；源图大于屏幕时退化为铺满。
    Tile,
}

impl FitMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cover" => Some(Self::Cover),
            "contain" => Some(Self::Contain),
            "stretch" => Some(Self::Stretch),
            "tile" => Some(Self::Tile),
            _ => None,
        }
    }
}

/// 实例级渲染策略；与用户暂停区分，后者由 `paused` 表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderOptions {
    pub playback: RenderPlayback,
    pub effects: ImageEffects,
    pub fit: FitMode,
}

/// 图片效果强度（0..=100）。0 表示关闭；每个效果独立调节。
///
/// 使用整数而非浮点，保持协议/配置的确定性相等比较与稳定序列化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageEffects {
    pub color: u8,
    pub vignette: u8,
    pub blur: u8,
    pub bloom: u8,
    pub particles: u8,
    pub gradient: u8,
}

impl ImageEffects {
    pub const OFF: Self = Self {
        color: 0,
        vignette: 0,
        blur: 0,
        bloom: 0,
        particles: 0,
        gradient: 0,
    };

    /// Balanced：轻量调色与暗角，保持静态停帧。
    pub const BALANCED: Self = Self {
        color: 35,
        vignette: 25,
        blur: 0,
        bloom: 0,
        particles: 0,
        gradient: 0,
    };

    /// Ultra：更多静态效果；粒子默认关闭，避免空闲持续唤醒。
    pub const ULTRA: Self = Self {
        color: 55,
        vignette: 40,
        blur: 20,
        bloom: 30,
        particles: 0,
        gradient: 20,
    };

    pub fn any(&self) -> bool {
        self.color > 0
            || self.vignette > 0
            || self.blur > 0
            || self.bloom > 0
            || self.particles > 0
            || self.gradient > 0
    }

    pub fn animated(&self) -> bool {
        self.particles > 0
    }

    pub fn labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.color > 0 {
            labels.push("调色");
        }
        if self.vignette > 0 {
            labels.push("暗角");
        }
        if self.blur > 0 {
            labels.push("模糊");
        }
        if self.bloom > 0 {
            labels.push("Bloom");
        }
        if self.particles > 0 {
            labels.push("粒子");
        }
        if self.gradient > 0 {
            labels.push("渐变");
        }
        labels
    }

    /// 归一化到 0..=100，避免持久化数据越界。
    pub fn normalized(mut self) -> Self {
        for value in [
            &mut self.color,
            &mut self.vignette,
            &mut self.blur,
            &mut self.bloom,
            &mut self.particles,
            &mut self.gradient,
        ] {
            *value = (*value).min(100);
        }
        self
    }
}

/// 宿主预览窗口位置（物理像素）。M1 用独立预览窗口验证播放，不附着 WorkerW。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
}

/// 壁纸窗口的宿主形态；用于区分预览与桌面附着，并携带失败原因。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AttachmentState {
    /// 独立预览窗口（开发/验收路径），不修改桌面层级。
    #[default]
    Preview,
    /// 已附着到 Explorer 壁纸层，使用显示器完整边界。
    Attached {
        display: String,
        width: u32,
        height: u32,
    },
    /// 附着失败，系统静态壁纸继续显示；reason 说明可重试原因。
    Fallback { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub id: String,
    /// 目标显示器索引；`None` 由宿主选择主显示器。
    #[serde(default)]
    pub monitor: Option<u32>,
    /// 稳定显示器身份（设备名，如 `\\.\DISPLAY1`）；优先于 `monitor` 索引。
    #[serde(default)]
    pub display_id: Option<String>,
    /// 附着到 WorkerW 桌面壁纸层并使用显示器完整边界。
    #[serde(default)]
    pub desktop: bool,
    /// `None` 时宿主使用默认预览窗口位置。
    #[serde(default)]
    pub window: Option<WindowPlacement>,
    pub source: Source,
    #[serde(default)]
    pub paused: bool,
    /// 质量策略要求的呈现方式；`paused` 优先（用户暂停高于自动策略）。
    #[serde(default)]
    pub render: RenderOptions,
}

/// 完整期望状态。`config_version` 单调递增，宿主可安全重复应用同一版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperConfig {
    pub config_version: u64,
    pub instances: Vec<InstanceConfig>,
}

impl WallpaperConfig {
    pub fn empty(config_version: u64) -> Self {
        Self {
            config_version,
            instances: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.instances.len() > MAX_INSTANCES {
            return Err(ConfigError::TooManyInstances {
                found: self.instances.len(),
                limit: MAX_INSTANCES,
            });
        }
        for (index, instance) in self.instances.iter().enumerate() {
            if instance.id.is_empty() || instance.id.len() > MAX_INSTANCE_ID_BYTES {
                return Err(ConfigError::InvalidInstanceId(instance.id.clone()));
            }
            if !instance
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(ConfigError::InvalidInstanceId(instance.id.clone()));
            }
            if self.instances[..index]
                .iter()
                .any(|other| other.id == instance.id)
            {
                return Err(ConfigError::DuplicateInstanceId(instance.id.clone()));
            }
            if let Some(window) = instance.window
                && (window.width == 0 || window.height == 0)
            {
                return Err(ConfigError::InvalidWindow(instance.id.clone()));
            }
            if let Some(display_id) = &instance.display_id
                && (display_id.trim().is_empty() || display_id.len() > 256)
            {
                return Err(ConfigError::InvalidDisplayId(instance.id.clone()));
            }
            instance.source.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    TooManyInstances { found: usize, limit: usize },
    DuplicateInstanceId(String),
    InvalidInstanceId(String),
    InvalidWindow(String),
    InvalidDisplayId(String),
    EmptyPath,
    PathTooLong,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyInstances { found, limit } => {
                write!(formatter, "实例数量 {found} 超过上限 {limit}")
            }
            Self::DuplicateInstanceId(id) => write!(formatter, "实例 ID 重复：{id}"),
            Self::InvalidInstanceId(id) => write!(formatter, "实例 ID 无效：{id}"),
            Self::InvalidWindow(id) => write!(formatter, "实例 {id} 的预览窗口尺寸为 0"),
            Self::InvalidDisplayId(id) => write!(formatter, "实例 {id} 的显示器身份无效"),
            Self::EmptyPath => write!(formatter, "素材路径为空"),
            Self::PathTooLong => write!(formatter, "素材路径过长"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInfo {
    /// mpv 报告的编解码器描述，例如 `h264 (High)`。
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// 容器/视频流帧率；未知时为 0。
    pub fps_milli: u32,
    /// 解码像素格式，例如 `yuv420p10`。
    pub pixel_format: String,
    #[serde(default)]
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HardwareDecode {
    NotApplicable,
    Active { decoder: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnknownInstance,
    SourceNotFound,
    UnsupportedMedia,
    HardwareDecodeUnavailable,
    DecodeFailed,
    PlayerUnavailable,
    NotReady,
    Timeout,
    Disconnected,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 当前实际使用的呈现路径；图片效果失败时回退到 mpv。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPath {
    #[default]
    Mpv,
    /// D3D11 图片效果管线（WIC 解码 + 全屏着色器）。
    GpuEffects,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub id: String,
    pub state: PlaybackState,
    pub configured_source: SourceKind,
    pub displayed: DisplayedSource,
    #[serde(default)]
    pub media: Option<MediaInfo>,
    pub hardware_decode: HardwareDecode,
    /// 窗口宿主形态：预览、桌面附着或静态回退原因。
    #[serde(default)]
    pub attachment: AttachmentState,
    /// VO 掉帧计数；未播放视频时为 `None`。
    #[serde(default)]
    pub dropped_frames: Option<u64>,
    /// 宿主是否仍持有该实例的 mpv 上下文（解码/VO 资源）。
    /// 暂停保留上下文；停止必须释放，供验收脚本区分“状态清空”和“资源已释放”。
    #[serde(default)]
    pub player_active: bool,
    /// 当前实际呈现策略（宿主回报，不是用户意图）。
    #[serde(default)]
    pub render: RenderPlayback,
    /// 视频播放位置（毫秒）。静态模式下应保持不变，供验收区分“暂停解码”和“仍在推进”。
    #[serde(default)]
    pub video_position_ms: Option<u64>,
    /// 实际渲染路径：mpv 直接播放或 D3D11 图片效果。
    #[serde(default)]
    pub render_path: RenderPath,
    /// 实际生效的效果强度（已由策略裁剪，不是用户意图）。
    #[serde(default)]
    pub effects: ImageEffects,
    /// 当前生效的效果名称，便于状态与验收报告。
    #[serde(default)]
    pub effects_active: Vec<String>,
    /// 效果管线失败原因；失败时 `render_path` 回退为 mpv。
    #[serde(default)]
    pub effects_error: Option<String>,
    /// 本次效果渲染的 pass 数（不含 mpv 内部 pass）。
    #[serde(default)]
    pub effect_passes: u32,
    /// 效果管线的累计 Present 次数；静态效果稳定后不再增长。
    #[serde(default)]
    pub present_count: u64,
    /// 最近一次完成的 GPU 时间（微秒）；查询不可用时为 `None`。
    #[serde(default)]
    pub gpu_time_us: Option<u32>,
    #[serde(default)]
    pub last_error: Option<CommandError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub host_pid: u32,
    /// 与信封中的宿主代次一致；使用不同字段名避免扁平化后键冲突。
    pub generation: u64,
    pub mpv_available: bool,
    #[serde(default)]
    pub mpv_version: Option<String>,
    #[serde(default)]
    pub mpv_client_api: Option<String>,
    pub applied_config_version: u64,
    pub instances: Vec<InstanceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHello {
    pub host_pid: u32,
    pub application_version: String,
    pub mpv_available: bool,
    #[serde(default)]
    pub mpv_version: Option<String>,
    #[serde(default)]
    pub mpv_client_api: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HostCommand {
    /// 应用完整期望状态；重复应用同一状态必须安全。
    ApplyState { config: WallpaperConfig },
    /// 查询当前快照，不改变状态。
    Ping,
    /// 清理播放资源并退出宿主进程。
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CommandResult {
    Ok(HostSnapshot),
    Err(CommandError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    /// 状态发生变化时的完整快照，主进程用最新快照覆盖旧值。
    Snapshot(HostSnapshot),
    /// 宿主侧诊断信息；不用于控制流。
    Log { level: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol_version: u32,
    /// 宿主实例代次；每次启动递增，过期消息直接丢弃。
    pub host_generation: u64,
    /// 主进程期望配置版本；响应回显对应请求的版本。
    pub config_version: u64,
    /// 请求 ID；响应用相同 ID 关联。宿主主动事件为 `None`。
    #[serde(default)]
    pub request_id: Option<u64>,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Hello(HostHello),
    Request(HostCommand),
    Response(CommandResult),
    Event(HostEvent),
}

impl Envelope {
    pub fn request(
        host_generation: u64,
        config_version: u64,
        request_id: u64,
        command: HostCommand,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            host_generation,
            config_version,
            request_id: Some(request_id),
            body: Body::Request(command),
        }
    }

    pub fn response(
        host_generation: u64,
        config_version: u64,
        request_id: u64,
        result: CommandResult,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            host_generation,
            config_version,
            request_id: Some(request_id),
            body: Body::Response(result),
        }
    }

    pub fn event(host_generation: u64, config_version: u64, event: HostEvent) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            host_generation,
            config_version,
            request_id: None,
            body: Body::Event(event),
        }
    }

    pub fn is_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Empty,
    TooLarge { found: usize, limit: usize },
    Truncated,
    Json(String),
    UnsupportedVersion { found: u32, expected: u32 },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "空帧"),
            Self::TooLarge { found, limit } => {
                write!(formatter, "帧大小 {found} 超过上限 {limit}")
            }
            Self::Truncated => write!(formatter, "帧长度与实际数据不一致"),
            Self::Json(message) => write!(formatter, "帧 JSON 无效：{message}"),
            Self::UnsupportedVersion { found, expected } => {
                write!(formatter, "协议版本不兼容：收到 {found}，期望 {expected}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// 读取长度前缀并校验上界；调用方在分配缓冲区前必须先经过此函数。
pub fn decode_frame_length(header: [u8; 4]) -> Result<usize, FrameError> {
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_PAYLOAD_BYTES {
        return Err(FrameError::TooLarge {
            found: length,
            limit: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(length)
}

/// 序列化为 JSON 载荷（不含长度前缀），并校验上界。
pub fn encode_payload(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let payload =
        serde_json::to_vec(envelope).map_err(|error| FrameError::Json(error.to_string()))?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(FrameError::TooLarge {
            found: payload.len(),
            limit: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(payload)
}

/// 将信封编码为 `u32 小端长度 + JSON`。传输层使用 `encode_payload`，此函数供测试与离线工具使用。
pub fn encode_envelope(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let payload = encode_payload(envelope)?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// 解析已按长度前缀读取的完整载荷。
pub fn decode_payload(payload: &[u8]) -> Result<Envelope, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(FrameError::TooLarge {
            found: payload.len(),
            limit: MAX_PAYLOAD_BYTES,
        });
    }
    let envelope: Envelope =
        serde_json::from_slice(payload).map_err(|error| FrameError::Json(error.to_string()))?;
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion {
            found: envelope.protocol_version,
            expected: PROTOCOL_VERSION,
        });
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> WallpaperConfig {
        WallpaperConfig {
            config_version: 7,
            instances: vec![
                InstanceConfig {
                    id: "display-0".into(),
                    monitor: Some(0),
                    display_id: Some(r"\\.\DISPLAY1".into()),
                    desktop: true,
                    window: Some(WindowPlacement {
                        x: 100,
                        y: 100,
                        width: 1280,
                        height: 720,
                        visible: true,
                    }),
                    source: Source::Image {
                        path: r"C:\media\wallpaper.png".into(),
                    },
                    paused: false,
                    render: RenderOptions::default(),
                },
                InstanceConfig {
                    id: "display-1".into(),
                    monitor: Some(1),
                    display_id: None,
                    desktop: false,
                    window: None,
                    source: Source::Video {
                        path: r"C:\media\clip.mp4".into(),
                        poster: Some(r"C:\media\poster.png".into()),
                        loop_playback: true,
                    },
                    paused: true,
                    render: RenderOptions {
                        playback: RenderPlayback::Static,
                        effects: ImageEffects::default(),
                        fit: FitMode::Contain,
                    },
                },
            ],
        }
    }

    #[test]
    fn config_validation_accepts_well_formed_state() {
        sample_config().validate().unwrap();
    }

    #[test]
    fn config_validation_rejects_duplicate_ids() {
        let mut config = sample_config();
        config.instances[1].id = config.instances[0].id.clone();
        assert_eq!(
            config.validate(),
            Err(ConfigError::DuplicateInstanceId("display-0".into()))
        );
    }

    #[test]
    fn config_validation_rejects_invalid_window_and_id() {
        let mut config = sample_config();
        config.instances[0].window = Some(WindowPlacement {
            x: 0,
            y: 0,
            width: 0,
            height: 720,
            visible: true,
        });
        assert_eq!(
            config.validate(),
            Err(ConfigError::InvalidWindow("display-0".into()))
        );

        let mut config = sample_config();
        config.instances[0].id = "bad id".into();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidInstanceId(_))
        ));
    }

    #[test]
    fn config_validation_rejects_too_many_instances() {
        let mut config = sample_config();
        while config.instances.len() <= MAX_INSTANCES {
            let id = format!("display-{}", config.instances.len());
            config.instances.push(InstanceConfig {
                id,
                monitor: None,
                display_id: None,
                desktop: false,
                window: None,
                source: Source::None,
                paused: false,
                render: RenderOptions::default(),
            });
        }
        assert!(matches!(
            config.validate(),
            Err(ConfigError::TooManyInstances { .. })
        ));
    }

    #[test]
    fn envelope_roundtrip_preserves_request_identity() {
        let envelope = Envelope::request(
            3,
            7,
            42,
            HostCommand::ApplyState {
                config: sample_config(),
            },
        );
        let frame = encode_envelope(&envelope).unwrap();
        let payload_length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(payload_length, frame.len() - 4);
        assert_eq!(decode_payload(&frame[4..]).unwrap(), envelope);
        assert_eq!(
            decode_frame_length(frame[..4].try_into().unwrap()),
            Ok(payload_length)
        );
    }

    #[test]
    fn rejects_unknown_protocol_version() {
        let mut envelope = Envelope::request(1, 1, 1, HostCommand::Ping);
        envelope.protocol_version = PROTOCOL_VERSION + 1;
        let payload = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(
            decode_payload(&payload),
            Err(FrameError::UnsupportedVersion {
                found: PROTOCOL_VERSION + 1,
                expected: PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn rejects_oversized_declared_length_before_allocation() {
        let length = (MAX_PAYLOAD_BYTES + 1) as u32;
        assert!(matches!(
            decode_frame_length(length.to_le_bytes()),
            Err(FrameError::TooLarge { .. })
        ));
        let maximum = MAX_PAYLOAD_BYTES as u32;
        assert_eq!(
            decode_frame_length(maximum.to_le_bytes()),
            Ok(MAX_PAYLOAD_BYTES)
        );
    }

    #[test]
    fn rejects_empty_and_truncated_frames() {
        assert_eq!(decode_frame_length([0, 0, 0, 0]), Err(FrameError::Empty));
        assert!(matches!(decode_payload(&[]), Err(FrameError::Empty)));
        assert!(matches!(
            decode_payload(br#"{"protocol_version":2"#),
            Err(FrameError::Json(_))
        ));
    }

    #[test]
    fn encoded_frame_never_exceeds_limit() {
        // 超长路径在编码前应被配置校验拒绝；这里验证编码器自身的上界保护。
        let mut config = sample_config();
        config.instances[0].source = Source::Image {
            path: "x".repeat(MAX_FRAME_BYTES),
        };
        assert_eq!(config.validate(), Err(ConfigError::PathTooLong));
        let envelope = Envelope::request(1, 1, 1, HostCommand::ApplyState { config });
        assert!(matches!(
            encode_envelope(&envelope),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn request_payload_has_unique_top_level_keys() {
        let envelope = Envelope::request(
            1,
            1,
            1,
            HostCommand::ApplyState {
                config: sample_config(),
            },
        );
        let payload = encode_payload(&envelope).unwrap();
        let text = String::from_utf8(payload).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["host_generation"], 1);
        assert_eq!(value["type"], "request");
        assert_eq!(value["command"], "apply_state");
    }

    #[test]
    fn response_with_snapshot_roundtrips_without_key_collisions() {
        let snapshot = HostSnapshot {
            host_pid: 4242,
            generation: 3,
            mpv_available: true,
            mpv_version: Some("mpv v0.41.0".into()),
            mpv_client_api: Some("2.6".into()),
            applied_config_version: 9,
            instances: Vec::new(),
        };
        let envelope = Envelope::response(3, 9, 11, CommandResult::Ok(snapshot));
        let payload = encode_payload(&envelope).unwrap();
        assert_eq!(decode_payload(&payload).unwrap(), envelope);
    }

    #[test]
    fn effects_roundtrip_and_labels_are_stable() {
        let effects = ImageEffects {
            color: 40,
            vignette: 20,
            blur: 10,
            bloom: 30,
            particles: 0,
            gradient: 15,
        };
        let json = serde_json::to_string(&effects).unwrap();
        assert_eq!(
            serde_json::from_str::<ImageEffects>(&json).unwrap(),
            effects
        );
        assert_eq!(
            effects.labels(),
            vec!["调色", "暗角", "模糊", "Bloom", "渐变"]
        );
        assert!(effects.any());
        assert!(!effects.animated());
        assert_eq!(
            ImageEffects {
                color: 150,
                ..effects
            }
            .normalized()
            .color,
            100
        );
        let legacy: ImageEffects = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, ImageEffects::OFF);
    }

    #[test]
    fn shutdown_and_ping_remain_valid_envelopes() {
        for command in [HostCommand::Ping, HostCommand::Shutdown] {
            let envelope = Envelope::request(1, 1, 9, command);
            let frame = encode_envelope(&envelope).unwrap();
            assert_eq!(decode_payload(&frame[4..]).unwrap(), envelope);
        }
    }
}
