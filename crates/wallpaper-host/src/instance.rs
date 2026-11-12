//! 单个壁纸实例：预览窗口 + 独立 mpv 上下文 + 状态机。
//!
//! 生命周期语义（可由 `InstanceStatus::player_active` 观察）：
//! - **暂停**保留 mpv 上下文与已解码画面，只暂停时钟，恢复时无需重新加载。
//! - **停止**（`Source::None`）释放播放器、解码器与 VO 资源，桌面由系统静态壁纸接管。
//! - **切换素材**先释放旧播放器再创建新播放器，因此旧素材的迟到 mpv 事件不可能污染新状态。
//! - 图片、视频与海报回退都带加载期限；超时按失败处理，不依赖“低 CPU”作为成功证据。
//!
//! 素材错误不会静默切换为软件视频解码，只会在配置了海报素材时显示静态回退。

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use protocol::{
    AttachmentState, CommandError, DisplayedSource, ErrorCode, FitMode, HardwareDecode,
    ImageEffects, InstanceConfig, InstanceStatus, MediaInfo, PlaybackState, RenderPath,
    RenderPlayback, Source, SourceKind,
};
use render::{EffectPipeline, graph};
use shell_win::{
    desktop::{self, DesktopAttachment, DisplayInfo},
    preview::{PreviewPlacement, PreviewWindow, default_placement, monitor_work_area},
};

use crate::mpv::{END_FILE_REASON_EOF, MpvLibrary, Player};

type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

/// 单个素材（含海报回退）允许的加载期限。超时视为失败，而不是无限期等待。
pub const LOAD_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadStage {
    Primary,
    Poster,
}

#[derive(Debug, Clone, Copy)]
struct PendingLoad {
    stage: LoadStage,
    started: Instant,
}

impl PendingLoad {
    fn new(stage: LoadStage) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }

    fn deadline(&self) -> Instant {
        self.started + LOAD_DEADLINE
    }
}

pub struct Instance {
    id: String,
    config: InstanceConfig,
    placement: PreviewPlacement,
    /// 桌面模式的目标显示器；预览模式为 `None`。
    target_display: Option<DisplayInfo>,
    /// 当前有效的桌面附着；失败或预览模式为 `None`。
    attachment: Option<DesktopAttachment>,
    desktop_mode: bool,
    next_attachment_check: Instant,
    // 先于窗口声明：释放时先终止 mpv 子窗口，再销毁父窗口。
    player: Option<Player>,
    window: Option<PreviewWindow>,
    library: Option<Arc<MpvLibrary>>,
    on_change: ChangeCallback,
    status: InstanceStatus,
    loaded_path: Option<String>,
    poster_path: Option<String>,
    pending: Option<PendingLoad>,
    fallback_active: bool,
    /// 主素材失败只处理一次，避免重复的 END_FILE 事件覆盖已确定的状态。
    primary_error_handled: bool,
    /// 海报失败与主素材失败分开记录，否则会被 `primary_error_handled` 屏蔽。
    poster_error_handled: bool,
    /// 当前暂停是由 Eco/自动策略触发，而不是用户暂停；策略恢复时必须解除。
    render_hold: bool,
    /// 图片效果管线；只在图片素材启用效果时存在。
    effect_pipeline: Option<EffectPipeline>,
    /// 动画效果从该时刻开始计算时间。
    effect_started: Instant,
    /// 下一次动画渲染的绝对期限；静态效果为 `None`。
    effect_deadline: Option<Instant>,
    /// 效果管线失败原因；失败时已回退 mpv 图片路径。
    effect_error: Option<String>,
    /// 效果管线已失败并回退；新配置或重新启用效果时清除，避免无限重试。
    effects_failed: bool,
}

impl Instance {
    pub fn new(
        config: &InstanceConfig,
        library: Option<Arc<MpvLibrary>>,
        on_change: ChangeCallback,
    ) -> Result<Self, CommandError> {
        let (placement, display) = target_for(config)?;
        let initial_visible = !config.desktop;
        let window = PreviewWindow::create(
            &format!("DeskShell 壁纸预览 - {}", config.id),
            PreviewPlacement {
                visible: initial_visible,
                ..placement
            },
        )
        .map_err(|error| CommandError::new(ErrorCode::Internal, error))?;
        let status = InstanceStatus {
            id: config.id.clone(),
            state: PlaybackState::Stopped,
            configured_source: config.source.kind(),
            displayed: DisplayedSource::None,
            media: None,
            hardware_decode: HardwareDecode::NotApplicable,
            attachment: if config.desktop {
                AttachmentState::Fallback {
                    reason: "尚未附着".into(),
                }
            } else {
                AttachmentState::Preview
            },
            dropped_frames: None,
            player_active: false,
            render: config.render.playback,
            video_position_ms: None,
            render_path: RenderPath::Mpv,
            effects: ImageEffects::OFF,
            effects_active: Vec::new(),
            effects_error: None,
            effect_passes: 0,
            present_count: 0,
            gpu_time_us: None,
            last_error: None,
        };
        let mut instance = Self {
            id: config.id.clone(),
            config: config.clone(),
            placement,
            target_display: display,
            attachment: None,
            desktop_mode: config.desktop,
            next_attachment_check: Instant::now() + desktop::attachment_guard_interval(),
            player: None,
            window: Some(window),
            library,
            on_change,
            status,
            loaded_path: None,
            poster_path: None,
            pending: None,
            fallback_active: false,
            primary_error_handled: false,
            poster_error_handled: false,
            render_hold: false,
            effect_pipeline: None,
            effect_started: Instant::now(),
            effect_deadline: None,
            effect_error: None,
            effects_failed: false,
        };
        if config.source.kind() != SourceKind::None {
            instance.start_source()?;
        }
        Ok(instance)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// 应用同一实例的新配置。源、桌面目标和宿主形态变化会重建；仅暂停/窗口位置不会重启解码。
    pub fn apply(&mut self, config: &InstanceConfig) -> Result<(), CommandError> {
        let (placement, display) = target_for(config)?;
        let source_changed = self.config.source != config.source;
        let desktop_changed = config.desktop != self.desktop_mode;
        let display_changed = display != self.target_display;
        let effects_changed = self.config.render.effects != config.render.effects;
        let fit_changed = self.config.render.fit != config.render.fit;
        let pause_changed =
            self.config.paused != config.paused || self.config.render != config.render;

        if effects_changed || fit_changed || self.effects_failed {
            // 新期望状态（含用户“重新应用”）允许重试一次效果管线；
            // 连续失败仍在本次应用内回退 mpv，不形成后台重试循环。
            self.effects_failed = false;
        }
        self.config = config.clone();
        self.desktop_mode = config.desktop;
        self.placement = placement;
        self.target_display = display;
        self.status.render = config.render.playback;

        if source_changed || desktop_changed || display_changed || fit_changed {
            return self.start_source();
        }

        if matches!(self.config.source, Source::Image { .. }) {
            let pipeline_active = self.effect_pipeline.is_some();
            if self.effects_failed == pipeline_active {
                // 呈现路径变化：mpv 图片 ↔ D3D11 效果管线。
                return self.start_source();
            }
            if pipeline_active && (effects_changed || pause_changed) {
                self.apply_effects_playback()?;
            }
        }
        if !self.desktop_mode
            && let Some(window) = &self.window
        {
            window.set_placement(placement);
        }
        if pause_changed && self.effect_pipeline.is_none() {
            self.apply_pause()?;
        }
        Ok(())
    }

    fn start_source(&mut self) -> Result<(), CommandError> {
        // 旧管线引用旧窗口，必须在可能重建窗口前释放。
        self.release_effects();
        self.ensure_window()?;
        // 切换素材必须释放旧播放器：旧事件线程结束后再创建新上下文，
        // 这样旧素材的 END_FILE/属性事件不会污染新素材状态。
        self.release_player();
        self.detach_desktop();
        self.pending = None;
        self.fallback_active = false;
        self.primary_error_handled = false;
        self.poster_error_handled = false;
        self.poster_path = None;
        self.loaded_path = None;
        self.render_hold = false;
        self.effect_error = None;
        self.status.render = self.config.render.playback;
        self.status.video_position_ms = None;
        self.status.last_error = None;
        self.status.media = None;
        self.status.dropped_frames = None;
        self.status.hardware_decode = HardwareDecode::NotApplicable;
        self.status.configured_source = self.config.source.kind();
        self.status.render_path = RenderPath::Mpv;
        self.status.effects = ImageEffects::OFF;
        self.status.effects_active.clear();
        self.status.effects_error = None;
        self.status.effect_passes = 0;
        self.status.present_count = 0;
        self.status.gpu_time_us = None;
        self.next_attachment_check = Instant::now() + desktop::attachment_guard_interval();

        if self.desktop_mode {
            if !self.attach_desktop() {
                // 附着失败：不继续解码不可见的视频，交由系统静态壁纸显示。
                self.apply_attachment_fallback();
                return Ok(());
            }
        } else {
            self.status.attachment = AttachmentState::Preview;
            if let Some(window) = &self.window {
                window.set_placement(self.placement);
            }
        }

        match self.config.source.clone() {
            Source::None => {
                self.status.state = PlaybackState::Stopped;
                self.status.displayed = DisplayedSource::None;
                Ok(())
            }
            Source::Image { path } => {
                ensure_file(&path)?;
                // 静态图片统一走 GPU 管线：即使效果关闭，也由着色器处理缩放/黑边/平铺，
                // 避免 mpv 默认 keepaspect 留下上下黑边。
                if !self.effects_failed {
                    match self.start_image_effects(&path) {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            // 效果管线失败不阻止图片显示：记录原因并回退 mpv 图片路径。
                            self.effect_error = Some(error);
                            self.effects_failed = true;
                            self.release_effects();
                            self.status.effects_error = self.effect_error.clone();
                        }
                    }
                }
                self.ensure_player()?;
                let player = self.player.as_ref().expect("播放器已创建");
                player
                    .set_fit(self.config.render.fit)
                    .map_err(|error| CommandError::new(ErrorCode::Internal, error.to_string()))?;
                player.load(&path, self.config.paused).map_err(|error| {
                    CommandError::new(ErrorCode::DecodeFailed, error.to_string())
                })?;
                self.loaded_path = Some(path);
                self.pending = Some(PendingLoad::new(LoadStage::Primary));
                self.status.state = PlaybackState::Loading;
                self.status.displayed = DisplayedSource::None;
                Ok(())
            }
            Source::Video { path, poster, .. } => {
                ensure_file(&path)?;
                if let Some(poster) = &poster {
                    ensure_file(poster)?;
                }
                // 视频仍走 mpv 直接播放；图片效果不叠加，状态中明确说明。
                self.status.effects_error =
                    Some("视频使用 mpv 直接播放；图片效果仅支持静态图片".into());
                self.ensure_player()?;
                let player = self.player.as_ref().expect("播放器已创建");
                player
                    .set_fit(self.config.render.fit)
                    .map_err(|error| CommandError::new(ErrorCode::Internal, error.to_string()))?;
                player.load(&path, self.config.paused).map_err(|error| {
                    CommandError::new(ErrorCode::DecodeFailed, error.to_string())
                })?;
                self.loaded_path = Some(path);
                self.poster_path = poster;
                self.pending = Some(PendingLoad::new(LoadStage::Primary));
                self.status.state = PlaybackState::Loading;
                self.status.displayed = DisplayedSource::None;
                Ok(())
            }
        }
    }

    /// 创建 D3D11 图片效果管线并完成首帧渲染。
    fn start_image_effects(&mut self, path: &str) -> Result<(), String> {
        let window = self
            .window
            .as_ref()
            .ok_or_else(|| "壁纸窗口不可用".to_string())?;
        let width = self.placement.width.max(1);
        let height = self.placement.height.max(1);
        let mut pipeline =
            EffectPipeline::new(window.hwnd(), width, height).map_err(|error| error.to_string())?;
        pipeline
            .set_image(Path::new(path))
            .map_err(|error| error.to_string())?;
        pipeline.set_fit(render_fit(self.config.render.fit));
        pipeline
            .configure(to_render_effects(self.config.render.effects))
            .map_err(|error| error.to_string())?;
        let animate = self.effects_animating();
        let stats = pipeline
            .render(Duration::ZERO, animate)
            .map_err(|error| error.to_string())?;
        self.effect_pipeline = Some(pipeline);
        self.effect_started = Instant::now();
        self.effect_deadline = animate.then(|| Instant::now() + graph::ANIMATION_INTERVAL);
        self.loaded_path = Some(path.to_owned());
        self.update_effect_status(&stats);
        self.status.state = self.effects_state();
        self.status.displayed = DisplayedSource::Image;
        self.status.media = None;
        self.status.dropped_frames = None;
        self.status.hardware_decode = HardwareDecode::NotApplicable;
        Ok(())
    }

    fn apply_pause(&mut self) -> Result<(), CommandError> {
        if self.effect_pipeline.is_some() {
            return self.apply_effects_playback();
        }
        // Eco 暂停只对视频有意义；图片本来就是静态帧。
        // 加载阶段不提前暂停，避免首帧/硬解确认永远等不到；加载完成后由 `refresh` 接管。
        let static_video = matches!(self.config.source, Source::Video { .. })
            && self.config.render.playback == RenderPlayback::Static;
        let loaded = matches!(
            self.status.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Static
        );
        self.render_hold = static_video && !self.config.paused && loaded;
        if let Some(player) = &self.player {
            let pause = self.config.paused || self.render_hold;
            player
                .set_pause(pause)
                .map_err(|error| CommandError::new(ErrorCode::Internal, error.to_string()))?;
            if matches!(
                self.status.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Static
            ) {
                self.status.state = if self.config.paused {
                    PlaybackState::Paused
                } else if self.render_hold {
                    PlaybackState::Static
                } else {
                    PlaybackState::Playing
                };
            }
        }
        Ok(())
    }

    fn ensure_player(&mut self) -> Result<(), CommandError> {
        if self.player.is_some() {
            return Ok(());
        }
        let library = self.library.as_ref().ok_or_else(|| {
            CommandError::new(
                ErrorCode::PlayerUnavailable,
                "libmpv 不可用，无法显示图片或视频；请检查宿主运行目录",
            )
        })?;
        let window = self.window.as_ref().ok_or_else(|| {
            CommandError::new(ErrorCode::Internal, "预览窗口已销毁，无法创建播放器")
        })?;
        let player = Player::new(library, window.window_id(), Arc::clone(&self.on_change))
            .map_err(|error| CommandError::new(ErrorCode::PlayerUnavailable, error.to_string()))?;
        self.player = Some(player);
        Ok(())
    }

    /// 释放 mpv 上下文、解码器与 VO 子窗口；窗口本身保留给后续素材复用。
    fn release_player(&mut self) {
        self.player = None;
        self.render_hold = false;
        self.status.player_active = false;
        self.status.video_position_ms = None;
    }

    /// 释放 D3D11 效果管线（纹理、交换链、着色器）并清除效果观测值。
    fn release_effects(&mut self) {
        self.effect_pipeline = None;
        self.effect_deadline = None;
        self.status.render_path = RenderPath::Mpv;
        self.status.effect_passes = 0;
        self.status.present_count = 0;
        self.status.gpu_time_us = None;
    }

    fn effects_animating(&self) -> bool {
        self.config.render.playback == RenderPlayback::Animate
            && !self.config.paused
            && self.config.render.effects.animated()
    }

    fn effects_state(&self) -> PlaybackState {
        if self.config.paused {
            PlaybackState::Paused
        } else if self.config.render.playback == RenderPlayback::Static {
            PlaybackState::Static
        } else {
            PlaybackState::Playing
        }
    }

    fn update_effect_status(&mut self, stats: &render::RenderStats) {
        self.status.render_path = RenderPath::GpuEffects;
        self.status.effects = self.config.render.effects;
        self.status.effects_active = self
            .status
            .effects
            .labels()
            .into_iter()
            .map(str::to_owned)
            .collect();
        self.status.effects_error = self.effect_error.clone();
        self.status.effect_passes = stats.effect_passes;
        self.status.present_count = stats.present_count;
        self.status.gpu_time_us = stats.gpu_time_us;
        self.status.player_active = false;
        self.status.video_position_ms = None;
    }

    /// 暂停/恢复或播放策略变化时更新效果渲染；静态时不安排下一帧。
    fn apply_effects_playback(&mut self) -> Result<(), CommandError> {
        let animate = self.effects_animating();
        let elapsed = self.effect_started.elapsed();
        let Some(pipeline) = &mut self.effect_pipeline else {
            return Ok(());
        };
        if let Err(error) = pipeline.configure(to_render_effects(self.config.render.effects)) {
            self.effects_failure(error.to_string());
            return Ok(());
        }
        match pipeline.render(elapsed, animate) {
            Ok(stats) => {
                self.update_effect_status(&stats);
                self.effect_deadline = animate.then(|| Instant::now() + graph::ANIMATION_INTERVAL);
                self.status.state = self.effects_state();
                Ok(())
            }
            Err(error) => {
                self.effects_failure(error.to_string());
                Ok(())
            }
        }
    }

    /// 效果管线运行失败：标记降级原因并回退 mpv 图片路径，避免拖垮桌面入口。
    fn effects_failure(&mut self, reason: String) {
        self.effects_failed = true;
        self.release_effects();
        // 先回退到 mpv，再写入失败原因，避免 start_source 的字段重置覆盖它。
        let _ = self.start_source();
        self.effect_error = Some(reason.clone());
        self.status.effects_error = Some(reason);
        self.status.effects = ImageEffects::OFF;
        self.status.effects_active.clear();
    }

    /// Explorer 重建会销毁它拥有的子窗口，此时必须重建窗口与播放器。
    fn ensure_window(&mut self) -> Result<(), CommandError> {
        if self.window.as_ref().is_some_and(|window| window.is_alive()) {
            return Ok(());
        }
        let (placement, _) = target_for(&self.config)?;
        let window = PreviewWindow::create(
            &format!("DeskShell 壁纸预览 - {}", self.id),
            PreviewPlacement {
                visible: !self.desktop_mode,
                ..placement
            },
        )
        .map_err(|error| CommandError::new(ErrorCode::Internal, error))?;
        self.window = Some(window);
        self.attachment = None;
        Ok(())
    }

    /// 尝试附着到桌面壁纸层；失败时记录可重试原因并返回 `false`。
    fn attach_desktop(&mut self) -> bool {
        let Some(display) = self.target_display.clone() else {
            self.status.attachment = AttachmentState::Fallback {
                reason: "找不到目标显示器".into(),
            };
            return false;
        };
        let Some(window) = &self.window else {
            self.status.attachment = AttachmentState::Fallback {
                reason: "壁纸窗口不可用".into(),
            };
            return false;
        };
        match DesktopAttachment::attach(window.hwnd(), display.clone()) {
            Ok(attachment) => {
                attachment.show();
                self.attachment = Some(attachment);
                self.status.attachment = AttachmentState::Attached {
                    display: display.id,
                    width: display.width,
                    height: display.height,
                };
                true
            }
            Err(error) => {
                self.status.attachment = AttachmentState::Fallback {
                    reason: format!("{error}"),
                };
                false
            }
        }
    }

    fn detach_desktop(&mut self) {
        if let Some(mut attachment) = self.attachment.take() {
            attachment.detach();
        }
    }

    /// 静态回退：不显示本程序窗口，由系统静态壁纸接管；释放不可见的解码资源。
    fn apply_attachment_fallback(&mut self) {
        self.release_player();
        self.release_effects();
        self.pending = None;
        self.fallback_active = false;
        self.status.effects = ImageEffects::OFF;
        self.status.effects_active.clear();
        self.status.state = PlaybackState::Stopped;
        self.status.displayed = DisplayedSource::None;
        self.status.media = None;
        self.status.dropped_frames = None;
        self.status.hardware_decode = HardwareDecode::NotApplicable;
        if !matches!(self.status.attachment, AttachmentState::Fallback { .. }) {
            self.status.attachment = AttachmentState::Fallback {
                reason: "桌面附着不可用".into(),
            };
        }
    }

    /// 系统事件或低频看门狗触发的附着复检；返回状态是否变化。
    pub fn revalidate(&mut self) -> bool {
        if !self.desktop_mode {
            return false;
        }
        self.next_attachment_check = Instant::now() + desktop::attachment_guard_interval();
        let was_fallback = matches!(self.status.attachment, AttachmentState::Fallback { .. });

        if !self.window.as_ref().is_some_and(|window| window.is_alive()) {
            // 父窗口被销毁会连带销毁子窗口；播放器与效果管线持有旧 HWND，必须整体重建。
            self.release_player();
            self.release_effects();
            self.attachment = None;
            if self.ensure_window().is_err() {
                self.apply_attachment_fallback();
                return true;
            }
            let _ = self.start_source();
            return true;
        }

        let Some(display) =
            desktop::resolve_display(self.config.display_id.as_deref(), self.config.monitor)
        else {
            self.detach_desktop();
            self.status.attachment = AttachmentState::Fallback {
                reason: "目标显示器已不可用".into(),
            };
            self.apply_attachment_fallback();
            return true;
        };
        self.target_display = Some(display.clone());
        if let Some(attachment) = &self.attachment
            && attachment.is_valid()
            && attachment.display() == &display
        {
            return false;
        }

        if let Some(mut attachment) = self.attachment.take() {
            match attachment.reattach(display.clone()) {
                Ok(()) => {
                    attachment.show();
                    self.attachment = Some(attachment);
                    self.status.attachment = AttachmentState::Attached {
                        display: display.id,
                        width: display.width,
                        height: display.height,
                    };
                    if was_fallback {
                        let _ = self.start_source();
                    }
                    return true;
                }
                Err(_) => attachment.detach(),
            }
        }

        if self.attach_desktop() {
            if was_fallback {
                let _ = self.start_source();
            }
        } else {
            self.apply_attachment_fallback();
        }
        true
    }

    /// 由宿主在收到播放器事件后调用：把 mpv 观察到的状态转换为协议状态。
    pub fn refresh(&mut self) {
        if self.effect_pipeline.is_some() {
            self.refresh_effects();
            return;
        }
        // 先复制观察结果与日志，随后才能修改自身状态（播放器借用不能跨越修改）。
        let (observed, log_error) = {
            let Some(player) = &self.player else {
                return;
            };
            (player.observed(), player.last_log_error())
        };
        if self.config.source.kind() == SourceKind::None {
            // 停止后残留事件不得让旧状态复活。
            self.status.state = PlaybackState::Stopped;
            self.status.displayed = DisplayedSource::None;
            self.status.media = None;
            self.status.dropped_frames = None;
            self.status.hardware_decode = HardwareDecode::NotApplicable;
            return;
        }

        if self.fallback_active {
            self.refresh_fallback(&observed, log_error);
            return;
        }

        // 宿主负责循环：EOF 后重新加载，避免 mpv loop-file 在失败时无限重试。
        if observed.end_reason == Some(END_FILE_REASON_EOF) {
            if matches!(
                &self.config.source,
                Source::Video {
                    loop_playback: true,
                    ..
                }
            ) && let Some(path) = self.loaded_path.clone()
            {
                let load_result = {
                    let player = self.player.as_ref().expect("播放器存在");
                    player.load(&path, self.config.paused)
                };
                match load_result {
                    Ok(()) => {
                        // 重新加载同样需要恢复加载期限，否则看门狗不再监控。
                        self.pending = Some(PendingLoad::new(LoadStage::Primary));
                        self.status.state = PlaybackState::Loading;
                        self.status.displayed = DisplayedSource::None;
                        self.status.dropped_frames = None;
                        self.status.media = None;
                        return;
                    }
                    Err(error) => {
                        if !self.primary_error_handled {
                            self.handle_failure(format!("循环重新加载失败：{error}"));
                        }
                        return;
                    }
                }
            }
            self.pending = None;
            self.status.state = PlaybackState::Stopped;
            self.status.displayed = DisplayedSource::None;
            self.status.media = None;
            self.status.dropped_frames = None;
            return;
        }

        if let Some(reason) = observed.end_error.clone() {
            if !self.primary_error_handled {
                let message = log_error.unwrap_or(reason);
                self.handle_failure(message);
            }
            return;
        }

        if !observed.file_loaded {
            return;
        }

        let is_video = matches!(self.config.source, Source::Video { .. });
        if is_video {
            let hwdec = observed.hwdec_current.as_deref();
            if hwdec == Some("no") || hwdec == Some("") {
                if !self.primary_error_handled {
                    let message = log_error.unwrap_or_else(|| "硬件解码未启用".into());
                    self.handle_failure(format!("硬件解码不可用：{message}"));
                }
                return;
            }
            if self.pending.is_some() {
                let width = observed.width.unwrap_or(0);
                let height = observed.height.unwrap_or(0);
                if width == 0 || height == 0 || hwdec.is_none() {
                    // 等待首帧与硬解确认；超时由宿主按绝对期限判定为失败。
                    self.status.state = if observed.paused.unwrap_or(self.config.paused) {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Loading
                    };
                    return;
                }
                self.pending = None;
            }
            if let Some(decoder) = hwdec {
                self.status.hardware_decode = HardwareDecode::Active {
                    decoder: decoder.to_owned(),
                };
            }
            self.status.media = Some(media_info(&observed));
            self.status.dropped_frames = self
                .player
                .as_ref()
                .and_then(|player| player.get_number("frame-drop-count"));
            self.status.displayed = DisplayedSource::Video;
        } else {
            // 图片也需要明确的加载确认；只看到 file_loaded 才结束加载阶段。
            self.pending = None;
            self.status.hardware_decode = HardwareDecode::NotApplicable;
            self.status.media = None;
            self.status.dropped_frames = None;
            self.status.displayed = DisplayedSource::Image;
        }

        // 用户暂停优先；否则按质量策略决定是否停在当前帧。
        if self.config.paused {
            self.render_hold = false;
            self.status.state = PlaybackState::Paused;
        } else if is_video && self.config.render.playback == RenderPlayback::Static {
            if !self.render_hold {
                if let Some(player) = &self.player {
                    player
                        .set_pause(true)
                        .map_err(|error| CommandError::new(ErrorCode::Internal, error.to_string()))
                        .ok();
                }
                self.render_hold = true;
            }
            self.status.state = PlaybackState::Static;
        } else {
            if self.render_hold {
                if let Some(player) = &self.player {
                    player.set_pause(false).ok();
                }
                self.render_hold = false;
            }
            self.status.state = PlaybackState::Playing;
        }
    }

    /// 效果路径的状态回报；不读取 mpv 状态，避免旧播放器事件污染。
    fn refresh_effects(&mut self) {
        let Some(pipeline) = &self.effect_pipeline else {
            return;
        };
        let stats = render::RenderStats {
            drawn: false,
            effect_passes: pipeline.plan().effect_passes(),
            gpu_time_us: pipeline.last_gpu_time_us(),
            present_count: pipeline.present_count(),
        };
        self.update_effect_status(&stats);
        self.status.state = self.effects_state();
        self.status.displayed = DisplayedSource::Image;
        self.status.media = None;
        self.status.dropped_frames = None;
        self.status.hardware_decode = HardwareDecode::NotApplicable;
    }

    fn refresh_fallback(
        &mut self,
        observed: &crate::mpv::ObservedState,
        log_error: Option<String>,
    ) {
        if observed.file_loaded {
            self.pending = None;
            self.status.state = PlaybackState::Fallback;
            self.status.displayed = DisplayedSource::Poster;
            self.status.media = None;
            self.status.dropped_frames = None;
            if let Some(error) = &self.status.last_error {
                self.status.hardware_decode = HardwareDecode::Failed {
                    reason: error.message.clone(),
                };
            }
            return;
        }
        if let Some(reason) = &observed.end_error
            && !self.poster_error_handled
        {
            let message = log_error.unwrap_or_else(|| reason.clone());
            self.poster_failure(format!("静态回退素材也无法显示：{message}"));
        }
    }

    fn poster_failure(&mut self, reason: String) {
        self.poster_error_handled = true;
        self.pending = None;
        self.fallback_active = false;
        self.release_player();
        self.status.state = PlaybackState::Stopped;
        self.status.displayed = DisplayedSource::None;
        let previous = self
            .status
            .last_error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default();
        self.status.last_error = Some(CommandError::new(
            ErrorCode::DecodeFailed,
            if previous.is_empty() {
                reason
            } else {
                format!("{previous}；{reason}")
            },
        ));
    }

    fn handle_failure(&mut self, reason: String) {
        self.primary_error_handled = true;
        self.pending = None;
        let lower = reason.to_lowercase();
        let hardware_failed = lower.contains("硬件解码")
            || lower.contains("hwdec")
            || lower.contains("hardware")
            || lower.contains("d3d11");
        let unsupported = lower.contains("format")
            || lower.contains("codec")
            || lower.contains("recogn")
            || lower.contains("unsupported")
            || lower.contains("无法识别");
        let code = if hardware_failed {
            ErrorCode::HardwareDecodeUnavailable
        } else if unsupported {
            ErrorCode::UnsupportedMedia
        } else {
            ErrorCode::DecodeFailed
        };
        let path = self
            .loaded_path
            .clone()
            .unwrap_or_else(|| "<未知素材>".into());
        let error = CommandError::new(code, format!("{path}：{reason}"));
        self.status.last_error = Some(error);
        self.status.media = None;
        self.status.dropped_frames = None;
        self.status.hardware_decode = if hardware_failed {
            HardwareDecode::Failed {
                reason: reason.clone(),
            }
        } else {
            HardwareDecode::NotApplicable
        };

        // 失败后先释放视频播放资源，再用全新播放器加载海报，
        // 避免失败素材的残留事件继续影响回退状态。
        self.release_player();

        let Some(poster) = self.poster_path.clone() else {
            self.status.state = PlaybackState::Stopped;
            self.status.displayed = DisplayedSource::None;
            return;
        };
        match self.ensure_player() {
            Ok(()) => {
                let player = self.player.as_ref().expect("播放器已创建");
                match player.load(&poster, false) {
                    Ok(()) => {
                        self.fallback_active = true;
                        self.poster_error_handled = false;
                        self.pending = Some(PendingLoad::new(LoadStage::Poster));
                        self.status.state = PlaybackState::Loading;
                        self.status.displayed = DisplayedSource::None;
                    }
                    Err(failure) => {
                        self.poster_failure(format!("静态回退加载失败：{failure}"));
                    }
                }
            }
            Err(failure) => {
                self.poster_failure(format!("静态回退加载失败：{}", failure.message));
            }
        }
    }

    /// 宿主看门狗：按绝对期限检查效果动画、加载阶段与桌面附着；返回状态是否发生变化。
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if self
            .effect_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let animate = self.effects_animating();
            let elapsed = self.effect_started.elapsed();
            let rendered = self
                .effect_pipeline
                .as_mut()
                .map(|pipeline| pipeline.render(elapsed, animate));
            match rendered {
                Some(Ok(stats)) => {
                    self.update_effect_status(&stats);
                    self.effect_deadline =
                        animate.then(|| Instant::now() + graph::ANIMATION_INTERVAL);
                    self.status.state = self.effects_state();
                }
                Some(Err(error)) => self.effects_failure(error.to_string()),
                None => self.effect_deadline = None,
            }
            changed = true;
        }
        if let Some(pending) = self.pending
            && Instant::now() >= pending.deadline()
        {
            match pending.stage {
                LoadStage::Primary => {
                    let is_video = matches!(self.config.source, Source::Video { .. });
                    let reason = if is_video {
                        "视频在加载期限内未产生有效画面；可能不支持该编码或硬件解码".to_string()
                    } else {
                        "图片在加载期限内未完成解码".to_string()
                    };
                    self.handle_failure(reason);
                }
                LoadStage::Poster => {
                    self.poster_failure("静态回退素材在加载期限内未完成解码".into());
                }
            }
            changed = true;
        }
        if self.desktop_mode && Instant::now() >= self.next_attachment_check {
            changed |= self.revalidate();
        }
        changed
    }

    /// 下一次需要看门狗唤醒的绝对时刻；无加载任务、无效果动画且非桌面模式时为 `None`。
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut deadlines = Vec::new();
        if let Some(load) = self.pending {
            deadlines.push(load.deadline());
        }
        if self.desktop_mode {
            deadlines.push(self.next_attachment_check);
        }
        if let Some(effect) = self.effect_deadline {
            deadlines.push(effect);
        }
        deadlines.into_iter().min()
    }

    pub fn snapshot(&mut self) -> InstanceStatus {
        self.refresh();
        self.status.player_active = self.player.is_some();
        self.status.render = self.config.render.playback;
        self.status.video_position_ms = self
            .player
            .as_ref()
            .filter(|_| matches!(self.config.source, Source::Video { .. }))
            .and_then(|player| player.time_position_ms());
        if let Some(pipeline) = &self.effect_pipeline {
            self.status.effect_passes = pipeline.plan().effect_passes();
            self.status.present_count = pipeline.present_count();
            self.status.gpu_time_us = pipeline.last_gpu_time_us();
        }
        self.status.clone()
    }

    pub fn shutdown(&mut self) {
        self.detach_desktop();
        self.release_effects();
        self.player = None;
        self.window = None;
        self.status.player_active = false;
    }
}

fn to_render_effects(effects: ImageEffects) -> graph::ImageEffects {
    graph::ImageEffects {
        color: effects.color,
        vignette: effects.vignette,
        blur: effects.blur,
        bloom: effects.bloom,
        particles: effects.particles,
        gradient: effects.gradient,
    }
}

fn render_fit(fit: FitMode) -> graph::FitMode {
    match fit {
        FitMode::Cover => graph::FitMode::Cover,
        FitMode::Contain => graph::FitMode::Contain,
        FitMode::Stretch => graph::FitMode::Stretch,
        FitMode::Tile => graph::FitMode::Tile,
    }
}

fn target_for(
    config: &InstanceConfig,
) -> Result<(PreviewPlacement, Option<DisplayInfo>), CommandError> {
    if config.desktop {
        let display = desktop::resolve_display(config.display_id.as_deref(), config.monitor)
            .ok_or_else(|| {
                CommandError::new(
                    ErrorCode::InvalidRequest,
                    format!(
                        "找不到显示器：{}{}",
                        config.display_id.as_deref().unwrap_or("主显示器"),
                        config
                            .monitor
                            .map(|index| format!("（索引 {index}）"))
                            .unwrap_or_default()
                    ),
                )
            })?;
        return Ok((
            PreviewPlacement {
                x: display.left,
                y: display.top,
                width: display.width,
                height: display.height,
                visible: true,
            },
            Some(display),
        ));
    }
    if let Some(window) = config.window {
        return Ok((
            PreviewPlacement {
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
                visible: window.visible,
            },
            None,
        ));
    }
    let area = monitor_work_area(config.monitor).ok_or_else(|| {
        CommandError::new(
            ErrorCode::InvalidRequest,
            format!(
                "显示器索引 {} 无效",
                config
                    .monitor
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "默认".into())
            ),
        )
    })?;
    Ok((default_placement(area), None))
}

fn ensure_file(path: &str) -> Result<(), CommandError> {
    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorCode::SourceNotFound,
            format!("素材不存在：{path}"),
        ))
    }
}

fn media_info(observed: &crate::mpv::ObservedState) -> MediaInfo {
    let pixel_format = observed.pixel_format.clone().unwrap_or_default();
    MediaInfo {
        codec: observed.codec.clone().unwrap_or_else(|| "unknown".into()),
        width: observed.width.unwrap_or(0),
        height: observed.height.unwrap_or(0),
        fps_milli: observed.fps_milli.unwrap_or(0),
        bit_depth: bit_depth_from_pixel_format(&pixel_format),
        pixel_format,
    }
}

fn bit_depth_from_pixel_format(format: &str) -> Option<u32> {
    if format.is_empty() {
        return None;
    }
    let lower = format.to_lowercase();
    if lower.contains("p010")
        || lower.contains("10le")
        || lower.contains("10be")
        || lower.contains("p10")
    {
        Some(10)
    } else if lower.contains("p012")
        || lower.contains("12le")
        || lower.contains("12be")
        || lower.contains("p12")
    {
        Some(12)
    } else if lower.contains("p016")
        || lower.contains("16le")
        || lower.contains("16be")
        || lower.contains("p16")
    {
        Some(16)
    } else {
        Some(8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_depth_is_reported_for_common_formats() {
        assert_eq!(bit_depth_from_pixel_format("yuv420p"), Some(8));
        assert_eq!(bit_depth_from_pixel_format("yuv420p10"), Some(10));
        assert_eq!(bit_depth_from_pixel_format("p010"), Some(10));
        assert_eq!(bit_depth_from_pixel_format("yuv420p12le"), Some(12));
        assert_eq!(bit_depth_from_pixel_format("p016"), Some(16));
        assert_eq!(bit_depth_from_pixel_format(""), None);
    }

    #[test]
    fn pending_load_deadline_is_absolute() {
        let pending = PendingLoad::new(LoadStage::Primary);
        let expected = pending.started + LOAD_DEADLINE;
        assert_eq!(pending.deadline(), expected);
        assert!(pending.deadline() > Instant::now());
    }
}
