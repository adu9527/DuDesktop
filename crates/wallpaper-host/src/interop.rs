//! M5 视频互操作独立原型：mpv D3D11 render API 直接写入调用方纹理。
//!
//! 验证内容：
//! - 固定 libmpv 构建支持 `MPV_RENDER_API_TYPE_D3D11`；
//! - 同适配器 D3D11 设备共享，mpv 渲染到本程序创建的纹理；
//! - 正常播放路径每帧 0 次 GPU→CPU→GPU 拷贝；仅结束时 1 次诊断回读；
//! - 测量渲染墙钟耗时、帧数、硬解与最终 PNG。
//!
//! 该原型不接入日常播放；设备重置/真实驱动失效恢复尚未验证。

use std::{path::Path, time::Duration, time::Instant};

use render::effects::image;
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_SDK_VERSION,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
            },
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
    },
    core::Interface,
};

use crate::mpv::{MpvLibrary, RenderSession};

pub struct Options {
    pub file: String,
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub seconds: u64,
}

pub fn run(options: Options) -> Result<(), String> {
    let _apartment = crate::host::ComApartment::enter()?;
    let library = MpvLibrary::load().map_err(|error| error.to_string())?;
    let (device, context) = create_device()?;
    let width = options.width.clamp(16, 7680);
    let height = options.height.clamp(16, 4320);
    let texture = create_texture(&device, width, height)?;

    let session = RenderSession::new(&library, device.as_raw())
        .map_err(|error| format!("创建 D3D11 render 会话失败：{error}"))?;
    session
        .load(&options.file)
        .map_err(|error| format!("加载视频失败：{error}"))?;

    let deadline = Instant::now() + Duration::from_secs(options.seconds.max(1));
    let mut frame_times = Vec::new();
    let mut first_frame_after = None;
    let started = Instant::now();
    while Instant::now() < deadline {
        if !session.wait_update(Duration::from_millis(100)) {
            continue;
        }
        if !session.has_frame() {
            continue;
        }
        let frame_start = Instant::now();
        session
            .render_frame(texture.as_raw())
            .map_err(|error| format!("渲染视频帧失败：{error}"))?;
        frame_times.push(frame_start.elapsed());
        if first_frame_after.is_none() {
            first_frame_after = Some(started.elapsed());
        }
    }
    if frame_times.is_empty() {
        return Err("在期限内没有渲染任何视频帧".into());
    }
    let hwdec = session.property("hwdec-current").unwrap_or_default();
    let codec = session.property("video-codec").unwrap_or_default();
    let video_width = session.property("width").unwrap_or_default();
    let video_height = session.property("height").unwrap_or_default();
    if !hwdec.to_ascii_lowercase().contains("d3d11") {
        return Err(format!("未使用 D3D11 硬解：hwdec-current={hwdec}"));
    }

    // 诊断回读：正常每帧路径不做 CPU 回读，仅结束时保存一张参考图。
    let staging = create_staging(&device, width, height)?;
    let source: ID3D11Resource = texture.cast().map_err(|error| error.to_string())?;
    let destination: ID3D11Resource = staging.cast().map_err(|error| error.to_string())?;
    // SAFETY: 同设备资源复制；mpv 已在 render 返回前提交工作。
    unsafe { context.CopyResource(&destination, &source) };
    let bgra = read_staging(&context, &staging, width, height)?;
    image::save_png(Path::new(&options.output), width, height, &bgra)?;

    let mut sorted = frame_times.clone();
    sorted.sort();
    let average_ms = sorted
        .iter()
        .map(|time| time.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / sorted.len() as f64;
    let p95_ms = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)].as_secs_f64() * 1000.0;
    let max_ms = sorted
        .last()
        .map(|time| time.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    println!("M5 视频互操作原型");
    println!("  视频：{codec} {video_width}x{video_height}，硬解 {hwdec}");
    println!(
        "  渲染 {} 帧，首帧 {} ms，平均 {:.3} ms，p95 {:.3} ms，最大 {:.3} ms",
        frame_times.len(),
        first_frame_after
            .map(|duration| duration.as_millis().to_string())
            .unwrap_or_else(|| "-".into()),
        average_ms,
        p95_ms,
        max_ms
    );
    println!("  纹理：BGRA8 {width}x{height}，同一 D3D11 设备，正常路径每帧拷贝 0 次");
    println!("  诊断回读 1 次，参考图：{}", options.output);
    println!("  未验证：真实设备重置/驱动失效恢复、长时间资源趋势、接入日常播放路径");
    Ok(())
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut device = None;
    let mut context = None;
    let mut feature_level = D3D_FEATURE_LEVEL::default();
    let levels = [
        D3D_FEATURE_LEVEL(0xb100),
        D3D_FEATURE_LEVEL(0xb000),
        D3D_FEATURE_LEVEL(0xa100),
        D3D_FEATURE_LEVEL(0xa000),
    ];
    // SAFETY: 使用默认硬件适配器；不使用单线程标志，d3d11va 解码线程需要多线程保护。
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )
    }
    .map_err(|error| format!("创建 D3D11 设备失败：{error}"))?;
    Ok((
        device.ok_or("D3D11 设备为空")?,
        context.ok_or("D3D11 上下文为空")?,
    ))
}

fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: 输出纹理由调用方持有；格式受 mpv D3D11 FBO 支持。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|error| format!("创建视频目标纹理失败：{error}"))?;
    texture.ok_or_else(|| "视频目标纹理为空".into())
}

fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: 只读回读；纹理由调用方持有。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|error| format!("创建诊断回读纹理失败：{error}"))?;
    texture.ok_or_else(|| "诊断回读纹理为空".into())
}

fn read_staging(
    context: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut mapped = windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: staging 纹理 CPU 可读；映射期间没有其他使用者。
    unsafe {
        context
            .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|error| format!("映射诊断回读纹理失败：{error}"))?;
    }
    let mut bgra = vec![0u8; width as usize * 4 * height as usize];
    // SAFETY: 映射行宽可能大于紧凑行宽；逐行复制。
    unsafe {
        for row in 0..height as usize {
            let source = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
            let destination = bgra.as_mut_ptr().add(row * width as usize * 4);
            std::ptr::copy_nonoverlapping(source, destination, width as usize * 4);
        }
        context.Unmap(staging, 0);
    }
    Ok(bgra)
}
