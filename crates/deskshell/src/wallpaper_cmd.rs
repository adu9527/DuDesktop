//! 开发用壁纸命令：启动监督器并把单个素材投放到独立预览窗口。

use std::{
    path::Path,
    time::{Duration, Instant},
};

use protocol::{
    AttachmentState, DisplayedSource, HardwareDecode, ImageEffects, InstanceConfig, InstanceStatus,
    MediaInfo, PlaybackState, RenderPath, Source, WindowPlacement,
};

use crate::host::{HostSupervisor, SupervisorOptions, locate_host_executable};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut kind = None;
    let mut poster = None;
    let mut monitor = None;
    let mut display = None;
    let mut desktop = false;
    let mut paused = false;
    let mut fit = protocol::FitMode::Cover;
    let mut effects = ImageEffects::OFF;
    let mut seconds = None;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;

    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--file" => file = Some(next(&mut arguments, "--file")?),
            "--kind" => kind = Some(next(&mut arguments, "--kind")?),
            "--poster" => poster = Some(next(&mut arguments, "--poster")?),
            "--monitor" => {
                monitor = Some(
                    next(&mut arguments, "--monitor")?
                        .parse()
                        .map_err(|_| "--monitor 需要数字")?,
                )
            }
            "--display" => display = Some(next(&mut arguments, "--display")?),
            "--desktop" => desktop = true,
            "--paused" => paused = true,
            "--fit" => {
                let value = next(&mut arguments, "--fit")?;
                fit = protocol::FitMode::parse(&value).ok_or_else(|| {
                    format!("--fit 只能是 cover、contain、stretch 或 tile：{value}")
                })?;
            }
            "--effects" => {
                // 开发验收入口：启用一组静态图片效果；粒子需显式 --particles。
                effects = ImageEffects {
                    color: 50,
                    vignette: 40,
                    blur: 20,
                    bloom: 30,
                    particles: 0,
                    gradient: 20,
                };
            }
            "--particles" => {
                effects.particles = 40;
            }
            "--seconds" => {
                seconds = Some(
                    next(&mut arguments, "--seconds")?
                        .parse()
                        .map_err(|_| "--seconds 需要数字")?,
                )
            }
            "--x" => {
                x = Some(
                    next(&mut arguments, "--x")?
                        .parse()
                        .map_err(|_| "--x 需要数字")?,
                )
            }
            "--y" => {
                y = Some(
                    next(&mut arguments, "--y")?
                        .parse()
                        .map_err(|_| "--y 需要数字")?,
                )
            }
            "--width" => {
                width = Some(
                    next(&mut arguments, "--width")?
                        .parse()
                        .map_err(|_| "--width 需要数字")?,
                )
            }
            "--height" => {
                height = Some(
                    next(&mut arguments, "--height")?
                        .parse()
                        .map_err(|_| "--height 需要数字")?,
                )
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }

    let file = file.ok_or("缺少 --file")?;
    let path = Path::new(&file);
    if !path.is_file() {
        return Err(format!("素材不存在：{file}"));
    }
    let is_video = match kind.as_deref() {
        Some("video") => true,
        Some("image") => false,
        Some(other) => return Err(format!("--kind 只能是 image 或 video：{other}")),
        None => is_video_extension(path),
    };
    if poster.is_some() && !is_video {
        return Err("--poster 仅用于视频".into());
    }
    if effects.any() && is_video {
        return Err("--effects/--particles 只适用于图片素材；视频仍由 mpv 直接播放".into());
    }
    let source = if is_video {
        Source::Video {
            path: file.clone(),
            poster,
            loop_playback: true,
        }
    } else {
        Source::Image { path: file.clone() }
    };

    let window = if x.is_some() || y.is_some() || width.is_some() || height.is_some() {
        Some(WindowPlacement {
            x: x.unwrap_or(80),
            y: y.unwrap_or(80),
            width: width.unwrap_or(1280),
            height: height.unwrap_or(720),
            visible: true,
        })
    } else {
        None
    };
    if let Some(window) = window
        && (window.width == 0 || window.height == 0)
    {
        return Err("--width/--height 必须大于 0".into());
    }
    if desktop && window.is_some() {
        return Err("--desktop 与 --x/--y/--width/--height 不能同时使用".into());
    }

    let config = InstanceConfig {
        id: "preview-0".into(),
        monitor,
        display_id: display,
        desktop,
        window,
        source,
        paused,
        render: protocol::RenderOptions {
            playback: Default::default(),
            effects,
            fit,
        },
    };
    let mut options = SupervisorOptions::new(locate_host_executable()?, vec![config.clone()]);
    options.apply_timeout = Duration::from_secs(30);
    let supervisor = HostSupervisor::start(options)?;

    let snapshot = supervisor
        .wait_connected(Duration::from_secs(20))
        .ok_or_else(|| format!("等待宿主连接超时：{}", supervisor.status().describe()))?;
    println!(
        "宿主已连接：PID {}，代次 {}，mpv {}（client API {}）",
        snapshot.host_pid,
        snapshot.generation,
        snapshot.mpv_version.as_deref().unwrap_or("不可用"),
        snapshot.mpv_client_api.as_deref().unwrap_or("-")
    );

    if let Err(error) = supervisor.apply_instances(vec![config]) {
        println!("应用素材失败：{:?} {}", error.code, error.message);
        return Err(error.message);
    }

    let mut last_line = String::new();
    let wait_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut settled = false;
        match supervisor.snapshot() {
            Ok(snapshot) => {
                if let Some(instance) = snapshot.instances.first() {
                    let line = describe(instance);
                    if line != last_line {
                        println!("{line}");
                        last_line = line;
                    }
                    settled = matches!(
                        instance.state,
                        PlaybackState::Playing
                            | PlaybackState::Paused
                            | PlaybackState::Static
                            | PlaybackState::Fallback
                    ) || instance.last_error.is_some();
                }
            }
            Err(error) => {
                println!("状态不可用：{:?} {}", error.code, error.message);
            }
        }
        if settled {
            break;
        }
        if Instant::now() >= wait_deadline {
            println!("等待播放状态超时。");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    match seconds {
        Some(seconds) => {
            println!("保持播放 {seconds} 秒……");
            std::thread::sleep(Duration::from_secs(seconds));
        }
        None => {
            println!("按 Enter 停止并退出……");
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        }
    }

    if let Ok(snapshot) = supervisor.snapshot()
        && let Some(instance) = snapshot.instances.first()
    {
        println!("最终状态：{}", describe(instance));
        println!("{}", describe_metrics(instance));
    }
    let mut supervisor = supervisor;
    supervisor.shutdown();
    println!("宿主已清理。");
    Ok(())
}

fn is_video_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "ts")
    )
}

fn describe(instance: &InstanceStatus) -> String {
    let attachment = match &instance.attachment {
        AttachmentState::Preview => "预览窗口",
        AttachmentState::Attached {
            display,
            width,
            height,
        } => {
            return format!(
                "实例 {}：{:?}，配置 {:?}，显示 {:?}，桌面附着 {} {}x{}{}{}",
                instance.id,
                instance.state,
                instance.configured_source,
                instance.displayed,
                display,
                width,
                height,
                if instance.hardware_decode == HardwareDecode::NotApplicable {
                    String::new()
                } else {
                    format!("，硬解 {}", hardware_text(&instance.hardware_decode))
                },
                instance
                    .last_error
                    .as_ref()
                    .map(|error| format!("，错误 {:?}：{}", error.code, error.message))
                    .unwrap_or_default()
            );
        }
        AttachmentState::Fallback { reason } => {
            return format!(
                "实例 {}：{:?}，配置 {:?}，显示 {:?}，静态回退（{reason}）",
                instance.id, instance.state, instance.configured_source, instance.displayed
            );
        }
    };
    format!(
        "实例 {}：{:?}，配置 {:?}，显示 {:?}，{attachment}{}{}",
        instance.id,
        instance.state,
        instance.configured_source,
        instance.displayed,
        if instance.hardware_decode == HardwareDecode::NotApplicable {
            String::new()
        } else {
            format!("，硬解 {}", hardware_text(&instance.hardware_decode))
        },
        instance
            .last_error
            .as_ref()
            .map(|error| format!("，错误 {:?}：{}", error.code, error.message))
            .unwrap_or_default()
    )
}

fn hardware_text(hardware: &HardwareDecode) -> String {
    match hardware {
        HardwareDecode::NotApplicable => "不适用".into(),
        HardwareDecode::Active { decoder } => decoder.clone(),
        HardwareDecode::Failed { reason } => format!("失败（{reason}）"),
    }
}

fn media_text(media: &MediaInfo) -> String {
    format!(
        "{}，{}x{}，{:.2} fps，{}，{} bit",
        media.codec,
        media.width,
        media.height,
        media.fps_milli as f64 / 1000.0,
        media.pixel_format,
        media
            .bit_depth
            .map(|depth| depth.to_string())
            .unwrap_or_else(|| "?".into())
    )
}

fn describe_metrics(instance: &InstanceStatus) -> String {
    let mut parts = Vec::new();
    if let Some(media) = &instance.media {
        parts.push(format!("媒体 {}", media_text(media)));
    }
    if instance.render_path == RenderPath::GpuEffects {
        parts.push(format!(
            "路径 D3D11 效果，pass {}，Present {}，GPU {}",
            instance.effect_passes,
            instance.present_count,
            instance
                .gpu_time_us
                .map(|micros| format!("{:.2} ms", f64::from(micros) / 1000.0))
                .unwrap_or_else(|| "不可用".into())
        ));
    }
    if instance.displayed == DisplayedSource::Video {
        parts.push(format!("硬解 {}", hardware_text(&instance.hardware_decode)));
        if let Some(dropped) = instance.dropped_frames {
            parts.push(format!("掉帧 {dropped}"));
        }
    }
    if parts.is_empty() {
        "（无播放指标）".into()
    } else {
        parts.join("；")
    }
}

fn next<'a>(
    arguments: &mut impl Iterator<Item = &'a String>,
    name: &str,
) -> Result<String, String> {
    arguments
        .next()
        .map(|value| value.to_owned())
        .ok_or_else(|| format!("{name} 缺少参数"))
}
