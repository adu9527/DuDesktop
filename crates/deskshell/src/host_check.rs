//! M1 验收：真实 4K 播放、错误恢复、宿主杀死/重启、多实例与孤儿进程检查。
//!
//! 需要 `scripts/fetch-mpv.ps1` 与 `scripts/fetch-test-media.ps1` 准备的运行资源。
//! 本命令全部使用独立预览窗口，不修改桌面层级；桌面附着由 `desktop-check` 验证。

use std::{
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};

use protocol::{
    DisplayedSource, ErrorCode, HardwareDecode, ImageEffects, InstanceConfig, InstanceStatus,
    PlaybackState, RenderPath, RenderPlayback, Source, WindowPlacement,
};
use shell_win::process::{find_processes_by_name, process_cpu_times, terminate_process};

use crate::host::{HostSupervisor, SupervisorOptions, SupervisorStatus, locate_host_executable};

const PRIMARY: &str = "primary";
const LEFT: &str = "left";
const RIGHT: &str = "right";

struct MediaPaths {
    image: PathBuf,
    video: PathBuf,
    unsupported_video: PathBuf,
    bad_video: PathBuf,
    poster: PathBuf,
}

impl MediaPaths {
    fn locate() -> Result<Self, String> {
        let root = std::env::var_os("DESKSHELL_TEST_MEDIA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("testdata"));
        let image = root.join("4k-pattern.png");
        let video = root.join("4k-h264-30fps.mp4");
        let unsupported_video = root.join("unsupported-mpeg4.mp4");
        let bad_video = root.join("not-a-video.mp4");
        let poster = root.join("poster.png");
        for path in [&image, &video, &unsupported_video, &bad_video, &poster] {
            if !path.is_file() {
                return Err(format!(
                    "缺少测试素材 {}；请执行 scripts/fetch-test-media.ps1",
                    path.display()
                ));
            }
        }
        Ok(Self {
            image: absolute(image)?,
            video: absolute(video)?,
            unsupported_video: absolute(unsupported_video)?,
            bad_video: absolute(bad_video)?,
            poster: absolute(poster)?,
        })
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("解析素材路径 {} 失败：{error}", path.display()))
}

pub fn run() -> Result<(), String> {
    let media = MediaPaths::locate()?;
    println!("M1/M5 host-check");
    println!("  图片：{}", media.image.display());
    println!("  视频：{}", media.video.display());
    println!("  错误素材：{}", media.bad_video.display());

    normal_scenarios(&media)?;
    fault_scenarios(&media)?;
    orphan_scenarios(&media)?;

    println!("\nM1/M5 host-check 全部通过。");
    Ok(())
}

fn normal_scenarios(media: &MediaPaths) -> Result<(), String> {
    let video_window = WindowPlacement {
        x: 80,
        y: 80,
        width: 1280,
        height: 720,
        visible: true,
    };
    let primary_image = image_config(media, PRIMARY, Some(video_window), false);
    let host_executable = locate_host_executable()?;
    let mut options = SupervisorOptions::new(host_executable, vec![primary_image.clone()]);
    options.backoff_base = Duration::from_millis(250);
    options.max_attempts = 3;
    let supervisor = HostSupervisor::start(options)?;

    let hello = supervisor
        .wait_connected(Duration::from_secs(20))
        .ok_or_else(|| format!("等待宿主连接超时：{}", supervisor.status().describe()))?;
    if !hello.mpv_available {
        return Err(format!(
            "宿主未加载 libmpv（{}）；请执行 scripts/fetch-mpv.ps1",
            hello.mpv_version.as_deref().unwrap_or("无版本")
        ));
    }
    println!(
        "[PASS] 宿主连接：PID {}，代次 {}，mpv {}，client API {}",
        hello.host_pid,
        hello.generation,
        hello.mpv_version.as_deref().unwrap_or("-"),
        hello.mpv_client_api.as_deref().unwrap_or("-")
    );
    let host_pid = hello.host_pid;

    // 1. 静态图片生命周期。
    let image_status = wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing && status.displayed == DisplayedSource::Image
    })?;
    if image_status.last_error.is_some() {
        return Err(format!("静态图片加载失败：{image_status:?}"));
    }
    let idle_cpu = cpu_percent_one_core(host_pid, Duration::from_millis(1500))?;
    println!("[INFO] 静态图片 CPU：{idle_cpu:.2}% 单核");
    if idle_cpu > 10.0 {
        return Err(format!("静态图片 CPU 过高：{idle_cpu:.2}% 单核"));
    }
    println!("[PASS] 静态图片显示，状态 {:?}", image_status.state);

    // 1b. M5 图片效果：D3D11 路径、逐项 pass 裁剪、静态停帧与动画停帧。
    effects_scenario(media, &supervisor, host_pid, video_window)?;
    supervisor
        .apply_instances(vec![primary_image.clone()])
        .map_err(|error| format!("效果验收后恢复图片失败：{error:?}"))?;
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing && status.last_error.is_none()
    })?;

    // 2. 4K 视频播放与指标。
    let primary_video = video_config(media, PRIMARY, Some(video_window), false, None);
    supervisor
        .apply_instances(vec![primary_video.clone()])
        .map_err(|error| format!("应用 4K 视频失败：{error:?}"))?;
    let video_status = wait_instance(&supervisor, PRIMARY, Duration::from_secs(30), |status| {
        status.state == PlaybackState::Playing && status.displayed == DisplayedSource::Video
    })?;
    if let Some(error) = &video_status.last_error {
        return Err(format!("4K 视频播放失败：{error:?}"));
    }
    std::thread::sleep(Duration::from_secs(5));
    let snapshot = supervisor
        .ping()
        .map_err(|error| format!("查询播放指标失败：{error:?}"))?;
    let status = instance(&snapshot, PRIMARY)?;
    let info = status.media.as_ref().ok_or("宿主未上报媒体信息")?.clone();
    if info.width != 3840 || info.height != 2160 {
        return Err(format!("4K 视频分辨率不符：{}x{}", info.width, info.height));
    }
    if !(25_000..=35_000).contains(&info.fps_milli) {
        return Err(format!("4K 视频帧率异常：{} mFPS", info.fps_milli));
    }
    let codec_lower = info.codec.to_ascii_lowercase();
    if !codec_lower.contains("h264")
        && !codec_lower.contains("h.264")
        && !codec_lower.contains("avc")
    {
        return Err(format!("4K 视频编码不符：{}", info.codec));
    }
    if info.bit_depth != Some(8) {
        return Err(format!("4K 视频位深异常：{:?}", info.bit_depth));
    }
    let decoder = match &status.hardware_decode {
        HardwareDecode::Active { decoder } => decoder.clone(),
        other => return Err(format!("4K 视频未启用硬解：{other:?}")),
    };
    if decoder == "no" || !decoder.to_ascii_lowercase().contains("d3d11") {
        return Err(format!("4K 视频硬解后端异常：{decoder}"));
    }
    if !status.player_active {
        return Err("视频播放中宿主未持有播放器上下文".into());
    }
    println!(
        "[PASS] 4K 视频：{}，{}x{}，{:.2} fps，{}，{} bit，硬解 {}，掉帧 {}",
        info.codec,
        info.width,
        info.height,
        info.fps_milli as f64 / 1000.0,
        info.pixel_format,
        info.bit_depth.unwrap_or(0),
        decoder,
        status
            .dropped_frames
            .map(|dropped| dropped.to_string())
            .unwrap_or_else(|| "未报告".into())
    );

    // 3. 暂停：停止持续解码且资源保留。
    let paused_config = video_config(media, PRIMARY, Some(video_window), true, None);
    supervisor
        .apply_instances(vec![paused_config.clone()])
        .map_err(|error| format!("暂停失败：{error:?}"))?;
    let paused_status = wait_instance(&supervisor, PRIMARY, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Paused
    })?;
    if !paused_status.player_active {
        return Err("暂停必须保留播放器上下文（恢复不应重新加载）".into());
    }
    let before = supervisor.ping().map_err(|error| error.message)?;
    let before_drops = instance(&before, PRIMARY)?.dropped_frames.unwrap_or(0);
    let paused_cpu = cpu_percent_one_core(host_pid, Duration::from_secs(2))?;
    let after = supervisor.ping().map_err(|error| error.message)?;
    let after_drops = instance(&after, PRIMARY)?.dropped_frames.unwrap_or(0);
    let drop_delta = after_drops.saturating_sub(before_drops);
    println!("[INFO] 暂停 2 秒 CPU：{paused_cpu:.2}% 单核，掉帧增量 {drop_delta}");
    if paused_cpu > 5.0 {
        return Err(format!("暂停后 CPU 过高：{paused_cpu:.2}% 单核"));
    }
    if drop_delta > 5 {
        return Err(format!("暂停后仍在持续掉帧：{drop_delta}"));
    }
    println!("[PASS] 暂停停止持续解码");
    supervisor
        .apply_instances(vec![primary_video])
        .map_err(|error| format!("恢复播放失败：{error:?}"))?;
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(10), |status| {
        status.state == PlaybackState::Playing
    })?;
    println!("[PASS] 恢复播放");

    // 3b. Eco/自动策略静态模式：保留当前帧与播放器，但停止推进与持续解码。
    let mut static_config = video_config(media, PRIMARY, Some(video_window), false, None);
    static_config.render.playback = RenderPlayback::Static;
    supervisor
        .apply_instances(vec![static_config])
        .map_err(|error| format!("切换静态模式失败：{error:?}"))?;
    let static_status = wait_instance(&supervisor, PRIMARY, Duration::from_secs(10), |status| {
        status.state == PlaybackState::Static && status.displayed == DisplayedSource::Video
    })?;
    if !static_status.player_active {
        return Err("静态模式应保留播放器上下文，以便恢复播放".into());
    }
    if static_status.render != RenderPlayback::Static {
        return Err(format!("宿主未回报静态模式：{:?}", static_status.render));
    }
    let first_position = static_status
        .video_position_ms
        .ok_or("静态模式未上报播放位置")?;
    std::thread::sleep(Duration::from_secs(2));
    let static_snapshot = supervisor.ping().map_err(|error| error.message)?;
    let static_after = instance(&static_snapshot, PRIMARY)?.clone();
    let second_position = static_after
        .video_position_ms
        .ok_or("静态模式第二次查询未上报播放位置")?;
    let advance = second_position.saturating_sub(first_position);
    let static_cpu = cpu_percent_one_core(host_pid, Duration::from_secs(2))?;
    println!("[INFO] 静态模式 2 秒：CPU {static_cpu:.2}% 单核，播放位置推进 {advance} ms");
    if advance > 20 {
        return Err(format!("静态模式仍在推进视频：{advance} ms"));
    }
    if static_cpu > 5.0 {
        return Err(format!("静态模式 CPU 过高：{static_cpu:.2}% 单核"));
    }
    println!("[PASS] Eco 静态模式停止推进与持续解码，播放器保留");
    supervisor
        .apply_instances(vec![video_config(
            media,
            PRIMARY,
            Some(video_window),
            false,
            None,
        )])
        .map_err(|error| format!("从静态模式恢复播放失败：{error:?}"))?;
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(10), |status| {
        status.state == PlaybackState::Playing
    })?;
    println!("[PASS] 静态模式恢复播放");

    // 4. 停止：释放播放资源。
    let stopped_config = stop_config(PRIMARY, Some(video_window));
    supervisor
        .apply_instances(vec![stopped_config])
        .map_err(|error| format!("停止失败：{error:?}"))?;
    let stopped = wait_instance(&supervisor, PRIMARY, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Stopped
    })?;
    if stopped.displayed != DisplayedSource::None
        || stopped.media.is_some()
        || stopped.dropped_frames.is_some()
    {
        return Err(format!("停止后仍保留播放状态：{stopped:?}"));
    }
    if stopped.player_active {
        return Err("停止后宿主仍持有 mpv 播放上下文，未释放播放资源".into());
    }
    // 释放后再查询一次，确认不是只清空了状态字段。
    let stopped_snapshot = supervisor.ping().map_err(|error| error.message)?;
    if instance(&stopped_snapshot, PRIMARY)?.player_active {
        return Err("停止后播放器上下文重新出现".into());
    }
    // 等待解码器与 VO 释放完成后再采样，避免把停止命令本身的收尾开销计入稳态。
    std::thread::sleep(Duration::from_secs(1));
    let stopped_cpu = cpu_percent_one_core(host_pid, Duration::from_secs(2))?;
    println!("[INFO] 停止 2 秒 CPU：{stopped_cpu:.2}% 单核");
    if stopped_cpu > 5.0 {
        return Err(format!("停止后 CPU 过高：{stopped_cpu:.2}% 单核"));
    }
    println!("[PASS] 停止释放播放资源");

    // 5. 错误素材有明确反馈，之后恢复正常素材。
    let missing = InstanceConfig {
        id: PRIMARY.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window: Some(video_window),
        source: Source::Video {
            path: media
                .image
                .with_file_name("missing.mp4")
                .display()
                .to_string(),
            poster: None,
            loop_playback: true,
        },
        paused: false,
        render: Default::default(),
    };
    match supervisor.apply_instances(vec![missing]) {
        Err(error) if error.code == ErrorCode::SourceNotFound => {
            println!("[PASS] 缺失素材返回 SourceNotFound")
        }
        other => return Err(format!("缺失素材未返回明确错误：{other:?}")),
    }

    let bad_config = InstanceConfig {
        id: PRIMARY.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window: Some(video_window),
        source: Source::Video {
            path: media.bad_video.display().to_string(),
            poster: None,
            loop_playback: true,
        },
        paused: false,
        render: Default::default(),
    };
    supervisor
        .apply_instances(vec![bad_config])
        .map_err(|error| format!("提交错误编码素材失败：{error:?}"))?;
    let bad_status = wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.last_error.is_some()
    })?;
    let error = bad_status
        .last_error
        .clone()
        .ok_or("错误素材没有错误信息")?;
    if !matches!(
        error.code,
        ErrorCode::DecodeFailed
            | ErrorCode::UnsupportedMedia
            | ErrorCode::HardwareDecodeUnavailable
    ) {
        return Err(format!("错误素材的错误码不明确：{error:?}"));
    }
    println!("[PASS] 错误素材反馈：{:?} {}", error.code, error.message);

    supervisor
        .apply_instances(vec![primary_image.clone()])
        .map_err(|error| format!("错误后恢复素材失败：{error:?}"))?;
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing && status.last_error.is_none()
    })?;
    println!("[PASS] 错误后仍可播放正常素材");

    // 5b. 硬解不可用的编码：明确报错并显示静态回退，不静默软件解码。
    let fallback_config = InstanceConfig {
        id: PRIMARY.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window: Some(video_window),
        source: Source::Video {
            path: media.unsupported_video.display().to_string(),
            poster: Some(media.poster.display().to_string()),
            loop_playback: true,
        },
        paused: false,
        render: Default::default(),
    };
    supervisor
        .apply_instances(vec![fallback_config])
        .map_err(|error| format!("提交硬解不可用素材失败：{error:?}"))?;
    let fallback = wait_instance(&supervisor, PRIMARY, Duration::from_secs(25), |status| {
        status.state == PlaybackState::Fallback
    })?;
    if fallback.displayed != DisplayedSource::Poster {
        return Err(format!("硬解失败后未显示静态回退：{fallback:?}"));
    }
    let fallback_error = fallback.last_error.clone().ok_or("静态回退缺少错误原因")?;
    if !matches!(
        fallback_error.code,
        ErrorCode::HardwareDecodeUnavailable
            | ErrorCode::UnsupportedMedia
            | ErrorCode::DecodeFailed
    ) {
        return Err(format!("静态回退错误码不明确：{fallback_error:?}"));
    }
    println!(
        "[PASS] 硬解不可用素材：{:?} {}；已显示静态回退",
        fallback_error.code, fallback_error.message
    );

    supervisor
        .apply_instances(vec![primary_image.clone()])
        .map_err(|error| format!("静态回退后恢复图片失败：{error:?}"))?;
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing
    })?;
    println!("[PASS] 静态回退后仍可切换正常素材");

    // 5c. 快速切源：旧素材的迟到事件不得污染新素材状态。
    for round in 0..2 {
        supervisor
            .apply_instances(vec![video_config(
                media,
                PRIMARY,
                Some(video_window),
                false,
                None,
            )])
            .map_err(|error| format!("快速切源第 {round} 轮视频失败：{error:?}"))?;
        std::thread::sleep(Duration::from_millis(120));
        supervisor
            .apply_instances(vec![image_config(
                media,
                PRIMARY,
                Some(video_window),
                false,
            )])
            .map_err(|error| format!("快速切源第 {round} 轮图片失败：{error:?}"))?;
        std::thread::sleep(Duration::from_millis(120));
    }
    let switched = wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing
            && status.displayed == DisplayedSource::Image
            && status.last_error.is_none()
    })?;
    if switched.render_path != RenderPath::GpuEffects {
        return Err(format!(
            "快速切源后未回到 GPU 图片管线：{:?}",
            switched.render_path
        ));
    }
    println!("[PASS] 快速切源后状态保持在新素材");

    // 6. 杀死宿主：主进程保持可用，监督器有限退避后恢复。
    let old_pid = supervisor.status().host_pid().ok_or("无法获取宿主 PID")?;
    terminate_process(old_pid).map_err(|error| format!("终止宿主失败：{error}"))?;
    let down_deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_down = false;
    while Instant::now() < down_deadline {
        match supervisor.status() {
            SupervisorStatus::Connected { host_pid, .. } if host_pid == old_pid => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                saw_down = true;
                break;
            }
        }
    }
    if !saw_down {
        return Err("宿主被终止后监督状态未改变".into());
    }
    let started = Instant::now();
    let ping_error = supervisor.ping().unwrap_err();
    let ping_elapsed = started.elapsed();
    if ping_elapsed > Duration::from_secs(3) {
        return Err(format!(
            "宿主不可用时主进程命令阻塞过久：{} ms",
            ping_elapsed.as_millis()
        ));
    }
    println!(
        "[PASS] 宿主不可用期间主进程可响应：{:?}（{} ms）",
        ping_error.code,
        ping_elapsed.as_millis()
    );

    let reconnected = wait_status(
        &supervisor,
        Duration::from_secs(20),
        |status| matches!(status, SupervisorStatus::Connected { host_pid, .. } if *host_pid != old_pid),
    )?;
    let new_pid = reconnected.host_pid().ok_or("重连后缺少 PID")?;
    let new_generation = match &reconnected {
        SupervisorStatus::Connected { generation, .. } => *generation,
        _ => 0,
    };
    println!("[PASS] 宿主自动恢复：PID {old_pid} -> {new_pid}，代次 {new_generation}");
    // 不重新下发命令：验证监督器在重连时自动重放完整期望状态。
    wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing && status.last_error.is_none()
    })?;
    println!("[PASS] 重连后按完整期望状态恢复播放");

    // 7. 多实例独立控制。
    let left_image_window = WindowPlacement {
        x: 40,
        y: 80,
        width: 960,
        height: 540,
        visible: true,
    };
    let right_video_window = WindowPlacement {
        x: 1040,
        y: 80,
        width: 960,
        height: 540,
        visible: true,
    };
    let left_playing = image_config(media, LEFT, Some(left_image_window), false);
    let right_playing = video_config(media, RIGHT, Some(right_video_window), false, None);
    supervisor
        .apply_instances(vec![left_playing.clone(), right_playing.clone()])
        .map_err(|error| format!("创建双实例失败：{error:?}"))?;
    wait_instance(&supervisor, LEFT, Duration::from_secs(20), |status| {
        status.state == PlaybackState::Playing
    })?;
    wait_instance(&supervisor, RIGHT, Duration::from_secs(30), |status| {
        status.state == PlaybackState::Playing && status.displayed == DisplayedSource::Video
    })?;
    println!("[PASS] 双实例同时播放");

    let left_paused = image_config(media, LEFT, Some(left_image_window), true);
    supervisor
        .apply_instances(vec![left_paused.clone(), right_playing.clone()])
        .map_err(|error| format!("暂停左实例失败：{error:?}"))?;
    wait_instance(&supervisor, LEFT, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Paused
    })?;
    wait_instance(&supervisor, RIGHT, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Playing
    })?;
    println!("[PASS] 暂停左实例不影响右实例");

    let right_stopped = stop_config(RIGHT, Some(right_video_window));
    supervisor
        .apply_instances(vec![left_paused.clone(), right_stopped])
        .map_err(|error| format!("停止右实例失败：{error:?}"))?;
    wait_instance(&supervisor, RIGHT, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Stopped
    })?;
    wait_instance(&supervisor, LEFT, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Paused
    })?;
    println!("[PASS] 停止右实例不影响左实例");

    let left_resumed = image_config(media, LEFT, Some(left_image_window), false);
    supervisor
        .apply_instances(vec![left_resumed])
        .map_err(|error| format!("恢复左实例失败：{error:?}"))?;
    wait_instance(&supervisor, LEFT, Duration::from_secs(5), |status| {
        status.state == PlaybackState::Playing
    })?;
    supervisor
        .apply_instances(Vec::new())
        .map_err(|error| format!("清理实例失败：{error:?}"))?;
    println!("[PASS] 多实例独立控制");

    let mut supervisor = supervisor;
    supervisor.shutdown();
    Ok(())
}

/// M5：真实 D3D11 图片效果路径。验证 pass 裁剪、静态停帧、动画停帧与关闭效果回退。
fn effects_scenario(
    media: &MediaPaths,
    supervisor: &HostSupervisor,
    host_pid: u32,
    window: WindowPlacement,
) -> Result<(), String> {
    let static_effects = ImageEffects {
        color: 50,
        vignette: 40,
        blur: 20,
        bloom: 30,
        gradient: 20,
        particles: 0,
    };
    let mut config = effects_image_config(media, PRIMARY, Some(window), static_effects);
    supervisor
        .apply_instances(vec![config.clone()])
        .map_err(|error| format!("应用图片效果失败：{error:?}"))?;
    let status = wait_instance(supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.render_path == RenderPath::GpuEffects
    })?;
    if status.last_error.is_some() {
        return Err(format!("图片效果路径报告错误：{status:?}"));
    }
    if status.displayed != DisplayedSource::Image || status.state != PlaybackState::Playing {
        return Err(format!("图片效果未显示图片：{status:?}"));
    }
    if !status.effects_active.iter().any(|name| name == "Bloom") {
        return Err(format!("效果状态缺少 Bloom：{status:?}"));
    }
    // 调色 1 + 模糊 2 + Bloom 4 + 暗角 1 + 渐变 1 = 9。
    if status.effect_passes != 9 {
        return Err(format!(
            "静态效果 pass 数不符：期望 9，实际 {}",
            status.effect_passes
        ));
    }
    if status.player_active {
        return Err("图片效果路径不应持有 mpv 播放器".into());
    }
    println!(
        "[PASS] D3D11 图片效果：{}，pass {}，Present {}，GPU {}",
        status.effects_active.join("、"),
        status.effect_passes,
        status.present_count,
        status
            .gpu_time_us
            .map(|micros| format!("{:.3} ms", f64::from(micros) / 1000.0))
            .unwrap_or_else(|| "不可用（驱动时间戳限制）".into())
    );

    // 静态效果：等待稳定后不得继续 Present。
    std::thread::sleep(Duration::from_millis(300));
    let before = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    std::thread::sleep(Duration::from_millis(1_200));
    let after = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    let delta = after.present_count.saturating_sub(before.present_count);
    let static_cpu = cpu_percent_one_core(host_pid, Duration::from_millis(1_200))?;
    println!("[INFO] 静态效果 1.2 秒：Present 增量 {delta}，CPU {static_cpu:.2}% 单核");
    if delta != 0 {
        return Err(format!("静态效果稳定后仍在 Present：{delta}"));
    }
    if static_cpu > 5.0 {
        return Err(format!("静态效果 CPU 过高：{static_cpu:.2}% 单核"));
    }
    println!("[PASS] 静态效果只渲染一帧后停帧");

    // 关闭 Bloom：pass 必须减少，且重新渲染一次后停帧。
    config.render.effects.bloom = 0;
    supervisor
        .apply_instances(vec![config.clone()])
        .map_err(|error| format!("关闭 Bloom 失败：{error:?}"))?;
    let without_bloom = wait_instance(supervisor, PRIMARY, Duration::from_secs(10), |status| {
        status.render_path == RenderPath::GpuEffects && status.effect_passes == 5
    })?;
    if without_bloom
        .effects_active
        .iter()
        .any(|name| name == "Bloom")
    {
        return Err(format!("关闭 Bloom 后状态仍报告 Bloom：{without_bloom:?}"));
    }
    std::thread::sleep(Duration::from_millis(300));
    let bloom_before = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    std::thread::sleep(Duration::from_millis(1_000));
    let bloom_after = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    if bloom_after.present_count != bloom_before.present_count {
        return Err("关闭 Bloom 后仍在重复 Present".into());
    }
    println!(
        "[PASS] 关闭 Bloom 后 pass 9 -> {}，并停帧",
        without_bloom.effect_passes
    );

    // 关闭全部静态效果：仍由 GPU 管线做缩放/铺满，只保留 Color(0) 映射 pass。
    config.render.effects = ImageEffects::OFF;
    supervisor
        .apply_instances(vec![config.clone()])
        .map_err(|error| format!("关闭全部效果失败：{error:?}"))?;
    let mapped = wait_instance(supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.render_path == RenderPath::GpuEffects
            && status.state == PlaybackState::Playing
            && status.displayed == DisplayedSource::Image
            && status.effect_passes == 1
    })?;
    if mapped.player_active {
        return Err("关闭效果后仍激活 mpv 图片播放器".into());
    }
    if !mapped.effects_active.is_empty() {
        return Err(format!(
            "关闭效果后仍报告活动效果：{:?}",
            mapped.effects_active
        ));
    }
    println!("[PASS] 关闭全部效果后仍由 GPU 管线铺满（映射 pass 1）");

    // 粒子动画：仅在 Animate 时持续 Present；切换 Static 后停帧。
    config.render.effects = ImageEffects {
        color: 30,
        vignette: 20,
        blur: 0,
        bloom: 0,
        particles: 40,
        gradient: 0,
    };
    config.render.playback = RenderPlayback::Animate;
    supervisor
        .apply_instances(vec![config.clone()])
        .map_err(|error| format!("启用粒子动画失败：{error:?}"))?;
    wait_instance(supervisor, PRIMARY, Duration::from_secs(20), |status| {
        status.render_path == RenderPath::GpuEffects && status.state == PlaybackState::Playing
    })?;
    let animated_before = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    std::thread::sleep(Duration::from_millis(1_000));
    let animated_after = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    let animated_presents = animated_after
        .present_count
        .saturating_sub(animated_before.present_count);
    let animated_cpu = cpu_percent_one_core(host_pid, Duration::from_millis(1_000))?;
    println!(
        "[INFO] 粒子动画 1 秒：Present {animated_presents}，CPU {animated_cpu:.2}% 单核，pass {}",
        animated_after.effect_passes
    );
    if animated_presents < 10 {
        return Err(format!("粒子动画帧率过低：{animated_presents} 次/秒"));
    }
    if animated_after.effect_passes != 3 {
        return Err(format!(
            "粒子动画 pass 数不符：期望 3，实际 {}",
            animated_after.effect_passes
        ));
    }

    config.render.playback = RenderPlayback::Static;
    supervisor
        .apply_instances(vec![config])
        .map_err(|error| format!("切换效果静态模式失败：{error:?}"))?;
    let static_effect = wait_instance(supervisor, PRIMARY, Duration::from_secs(10), |status| {
        status.state == PlaybackState::Static && status.render_path == RenderPath::GpuEffects
    })?;
    let static_before = static_effect.present_count;
    std::thread::sleep(Duration::from_millis(1_200));
    let static_after = wait_instance(supervisor, PRIMARY, Duration::from_secs(5), |_| true)?;
    let stopped = static_after.present_count.saturating_sub(static_before);
    if stopped != 0 {
        return Err(format!("效果切换 Static 后仍在 Present：{stopped}"));
    }
    let paused_cpu = cpu_percent_one_core(host_pid, Duration::from_millis(1_000))?;
    if paused_cpu > 5.0 {
        return Err(format!("效果静态模式 CPU 过高：{paused_cpu:.2}% 单核"));
    }
    println!("[PASS] 粒子动画切换 Static 后停帧，CPU {paused_cpu:.2}% 单核");
    Ok(())
}

fn effects_image_config(
    media: &MediaPaths,
    id: &str,
    window: Option<WindowPlacement>,
    effects: ImageEffects,
) -> InstanceConfig {
    InstanceConfig {
        id: id.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window,
        source: Source::Image {
            path: media.image.display().to_string(),
        },
        paused: false,
        render: protocol::RenderOptions {
            playback: RenderPlayback::Animate,
            effects,
            fit: Default::default(),
        },
    }
}

fn fault_scenarios(media: &MediaPaths) -> Result<(), String> {
    // 协议错误与断开只注入首个宿主代次：监督器应退避重启并恢复服务。
    for mode in ["invalid-protocol", "disconnect"] {
        let mut options = SupervisorOptions::new(locate_host_executable()?, Vec::new());
        options.host_args = vec![
            "--fault".into(),
            mode.into(),
            "--fault-generation".into(),
            "1".into(),
        ];
        options.max_attempts = 2;
        options.backoff_base = Duration::from_millis(100);
        options.command_timeout = Duration::from_secs(1);
        options.apply_timeout = Duration::from_secs(3);
        options.connect_timeout = Duration::from_secs(3);
        options.handshake_timeout = Duration::from_secs(3);
        let supervisor = HostSupervisor::start(options)?;
        let connected = wait_status(&supervisor, Duration::from_secs(15), |status| {
            matches!(status, SupervisorStatus::Connected { generation, .. } if *generation >= 2)
        })
        .map_err(|error| format!("{mode} 后未恢复：{error}"))?;
        println!("[PASS] {mode} 故障后恢复：{}", connected.describe());
        supervisor
            .apply_instances(vec![image_config(media, PRIMARY, None, false)])
            .map_err(|error| format!("{mode} 恢复后无法应用配置：{error:?}"))?;
        wait_instance(&supervisor, PRIMARY, Duration::from_secs(20), |status| {
            status.state == PlaybackState::Playing
        })?;
        println!("[PASS] {mode} 恢复后可正常播放");
        let mut supervisor = supervisor;
        supervisor.shutdown();
    }

    // 超限消息与无响应宿主：有限次尝试后进入失败状态，不形成重启风暴。
    for (mode, expected) in [("oversized", "上限"), ("hang", "未响应")] {
        let mut options = SupervisorOptions::new(locate_host_executable()?, Vec::new());
        options.host_args = vec![
            "--fault".into(),
            mode.into(),
            "--fault-generation".into(),
            "1".into(),
        ];
        options.max_attempts = 0;
        options.backoff_base = Duration::from_millis(100);
        options.command_timeout = Duration::from_secs(1);
        options.apply_timeout = Duration::from_secs(2);
        options.connect_timeout = Duration::from_secs(3);
        options.handshake_timeout = Duration::from_secs(3);
        let supervisor = HostSupervisor::start(options)?;
        let failed = wait_status(&supervisor, Duration::from_secs(15), |status| {
            matches!(status, SupervisorStatus::Failed { .. })
        })
        .map_err(|error| format!("{mode} 未进入失败状态：{error}"))?;
        let reason = match &failed {
            SupervisorStatus::Failed { reason } => reason.clone(),
            _ => unreachable!(),
        };
        if !reason.contains(expected) {
            return Err(format!("{mode} 的失败原因不明确：{reason}"));
        }
        if host_count_for_parent(std::process::id()) > 0 {
            return Err(format!("{mode} 触发重启风暴：仍有宿主存活"));
        }
        let started = Instant::now();
        let ping = supervisor.ping().unwrap_err();
        if started.elapsed() > Duration::from_secs(1) {
            return Err(format!("{mode} 失败后主进程命令阻塞过久"));
        }
        println!(
            "[PASS] {mode} 有限失败：{}（命令返回 {:?}）",
            reason, ping.code
        );
        let mut supervisor = supervisor;
        supervisor.shutdown();
    }
    Ok(())
}

fn orphan_scenarios(media: &MediaPaths) -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("获取当前程序路径失败：{error}"))?;
    let image = media.image.display().to_string();

    // 正常退出：主进程结束后不应留下其宿主。
    let mut child = ProcessCommand::new(&executable)
        .args(["wallpaper", "--file", &image, "--seconds", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动壁纸子进程失败：{error}"))?;
    let child_pid = child.id();
    let status = child
        .wait()
        .map_err(|error| format!("等待壁纸子进程失败：{error}"))?;
    if !status.success() {
        return Err(format!("壁纸子进程退出失败：{status}"));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && host_count_for_parent(child_pid) > 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
    let leaked = host_count_for_parent(child_pid);
    if leaked > 0 {
        return Err(format!("正常退出后仍有 {leaked} 个宿主存活"));
    }
    println!("[PASS] 主进程正常退出后无孤儿宿主");

    // 强制终止主进程：Job 对象与父进程监视应清理宿主。
    let mut child = ProcessCommand::new(&executable)
        .args(["wallpaper", "--file", &image, "--seconds", "30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动强杀测试子进程失败：{error}"))?;
    let child_pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_host = false;
    while Instant::now() < deadline {
        if host_count_for_parent(child_pid) > 0 {
            observed_host = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !observed_host {
        let _ = child.kill();
        let _ = child.wait();
        return Err("强杀测试未观察到子进程宿主".into());
    }
    child
        .kill()
        .map_err(|error| format!("强制终止壁纸子进程失败：{error}"))?;
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && host_count_for_parent(child_pid) > 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
    let leaked = host_count_for_parent(child_pid);
    if leaked > 0 {
        return Err(format!("主进程被强杀后仍有 {leaked} 个宿主存活"));
    }
    println!("[PASS] 主进程被强杀后宿主被清理");
    Ok(())
}

fn host_count_for_parent(parent_pid: u32) -> usize {
    find_processes_by_name("wallpaper-host.exe")
        .into_iter()
        .filter(|process| process.parent_pid == parent_pid)
        .count()
}

fn instance<'a>(
    snapshot: &'a protocol::HostSnapshot,
    id: &str,
) -> Result<&'a InstanceStatus, String> {
    snapshot
        .instances
        .iter()
        .find(|instance| instance.id == id)
        .ok_or_else(|| format!("快照中缺少实例 {id}"))
}

fn wait_instance(
    supervisor: &HostSupervisor,
    id: &str,
    timeout: Duration,
    predicate: impl Fn(&InstanceStatus) -> bool,
) -> Result<InstanceStatus, String> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<String>;
    loop {
        match supervisor.snapshot() {
            Ok(snapshot) => match snapshot.instances.iter().find(|instance| instance.id == id) {
                Some(instance) => {
                    if predicate(instance) {
                        return Ok(instance.clone());
                    }
                    last = Some(format!(
                        "状态 {:?}，显示 {:?}，错误 {:?}",
                        instance.state, instance.displayed, instance.last_error
                    ));
                }
                None => {
                    last = Some(format!(
                        "实例缺失（当前 {} 个实例）",
                        snapshot.instances.len()
                    ))
                }
            },
            Err(error) => last = Some(format!("{:?}：{}", error.code, error.message)),
        }
        if Instant::now() >= deadline {
            return Err(format!("等待实例 {id} 超时：{}", last.unwrap_or_default()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_status(
    supervisor: &HostSupervisor,
    timeout: Duration,
    predicate: impl Fn(&SupervisorStatus) -> bool,
) -> Result<SupervisorStatus, String> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<String>;
    loop {
        let status = supervisor.status();
        if predicate(&status) {
            return Ok(status);
        }
        last = Some(status.describe());
        if Instant::now() >= deadline {
            return Err(format!("等待监督状态超时：{}", last.unwrap_or_default()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cpu_percent_one_core(pid: u32, interval: Duration) -> Result<f64, String> {
    let start = process_cpu_times(pid).ok_or_else(|| format!("无法读取宿主 {pid} 的 CPU 时间"))?;
    let wall = Instant::now();
    std::thread::sleep(interval);
    let end = process_cpu_times(pid).ok_or_else(|| format!("无法读取宿主 {pid} 的 CPU 时间"))?;
    let cpu_100ns = end.total_100ns().saturating_sub(start.total_100ns()) as f64;
    let wall_100ns = wall.elapsed().as_nanos() as f64 / 100.0;
    Ok(cpu_100ns / wall_100ns.max(1.0) * 100.0)
}

fn image_config(
    media: &MediaPaths,
    id: &str,
    window: Option<WindowPlacement>,
    paused: bool,
) -> InstanceConfig {
    InstanceConfig {
        id: id.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window,
        source: Source::Image {
            path: media.image.display().to_string(),
        },
        paused,
        render: Default::default(),
    }
}

fn video_config(
    media: &MediaPaths,
    id: &str,
    window: Option<WindowPlacement>,
    paused: bool,
    poster: Option<&Path>,
) -> InstanceConfig {
    InstanceConfig {
        id: id.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window,
        source: Source::Video {
            path: media.video.display().to_string(),
            poster: poster.map(|path| path.display().to_string()),
            loop_playback: true,
        },
        paused,
        render: Default::default(),
    }
}

fn stop_config(id: &str, window: Option<WindowPlacement>) -> InstanceConfig {
    InstanceConfig {
        id: id.into(),
        monitor: None,
        display_id: None,
        desktop: false,
        window,
        source: Source::None,
        paused: false,
        render: Default::default(),
    }
}
