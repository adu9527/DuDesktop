//! 独立图片效果自检：不经过 IPC，渲染一组参考效果并写 PNG。
//!
//! 用于 M5 验收与诊断：确认着色器、WIC 解码、交换链与参考截图路径可用。
//! 该模式不修改桌面、不启动 mpv、不影响用户配置。

use std::{path::Path, time::Duration};

use render::{EffectPipeline, graph::ImageEffects};
use shell_win::preview::{PreviewPlacement, PreviewWindow};

pub struct Options {
    pub file: String,
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub particles: bool,
}

pub fn run(options: Options) -> Result<(), String> {
    shell_win::enable_per_monitor_dpi_awareness();
    let _apartment = crate::host::ComApartment::enter()?;
    let width = options.width.clamp(64, 7680);
    let height = options.height.clamp(64, 4320);
    let window = PreviewWindow::create(
        "DeskShell 效果参考",
        PreviewPlacement {
            x: 80,
            y: 80,
            width,
            height,
            visible: true,
        },
    )?;
    let mut pipeline =
        EffectPipeline::new(window.hwnd(), width, height).map_err(|error| error.to_string())?;
    pipeline
        .set_image(Path::new(&options.file))
        .map_err(|error| error.to_string())?;
    let effects = ImageEffects {
        color: 50,
        vignette: 40,
        blur: 20,
        bloom: 30,
        particles: if options.particles { 40 } else { 0 },
        gradient: 20,
    };
    pipeline
        .configure(effects)
        .map_err(|error| error.to_string())?;
    pipeline.request_capture(options.output.clone().into());
    let stats = pipeline
        .render(Duration::ZERO, options.particles)
        .map_err(|error| error.to_string())?;
    println!("效果参考已保存：{}（{}×{}）", options.output, width, height);
    println!(
        "pass {}，Present {}，GPU {}，降级 {:?}",
        stats.effect_passes,
        stats.present_count,
        stats
            .gpu_time_us
            .map(|micros| format!("{:.3} ms", f64::from(micros) / 1000.0))
            .unwrap_or_else(|| "不可用（驱动时间戳限制）".into()),
        pipeline.plan().degraded
    );
    // 保持窗口一帧，确保交换链内容与截图一致；不进入持续渲染。
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}
