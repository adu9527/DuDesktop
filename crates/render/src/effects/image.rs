//! WIC 图片解码与 PNG 导出。
//!
//! 解码只在素材加载或参考截图时执行一次，不进入逐帧路径。
//! 调用线程必须已初始化 COM（宿主主线程在启动时初始化 MTA）。
//! 单张图片尺寸与内存有上限，避免异常素材造成无界分配。

use std::path::Path;

use windows::{
    Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE},
        Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
            IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
            WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::PCWSTR,
};

/// 单边最大像素；8K 为 7680，留出余量。
pub const MAX_IMAGE_DIMENSION: u32 = 16384;
/// 单张解码结果上限（BGRA）。
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// 紧凑排列的 BGRA8（stride = width * 4）。
    pub bgra: Vec<u8>,
}

impl DecodedImage {
    pub fn stride(&self) -> u32 {
        self.width * 4
    }
}

fn factory() -> Result<IWICImagingFactory, String> {
    // SAFETY: 调用线程已初始化 COM；CLSID 为系统 WIC 工厂。
    unsafe {
        CoCreateInstance::<_, IWICImagingFactory>(
            &CLSID_WICImagingFactory,
            None,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(|error| format!("创建 WIC 工厂失败：{error}"))
}

fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 把图片解码为 BGRA8；失败返回可上报的原因。
pub fn decode(path: &Path) -> Result<DecodedImage, String> {
    let factory = factory()?;
    let wide = wide_path(path);
    // SAFETY: 路径以 NUL 结尾；解码器归本函数所有。
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        )
    }
    .map_err(|error| format!("打开图片失败 {}：{error}", path.display()))?;
    // SAFETY: 解码器已创建。
    let frame = unsafe { decoder.GetFrame(0) }
        .map_err(|error| format!("读取图片帧失败 {}：{error}", path.display()))?;
    // SAFETY: 帧有效；格式转换器由本函数创建。
    let converter = unsafe { factory.CreateFormatConverter() }
        .map_err(|error| format!("创建 WIC 格式转换器失败：{error}"))?;
    // SAFETY: 输出格式固定为 BGRA8；不使用调色板。
    unsafe {
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .map_err(|error| format!("图片格式转换失败 {}：{error}", path.display()))?;

    let mut width = 0u32;
    let mut height = 0u32;
    // SAFETY: 输出尺寸由本函数初始化。
    unsafe { converter.GetSize(&mut width, &mut height) }
        .map_err(|error| format!("读取图片尺寸失败：{error}"))?;
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(format!(
            "图片尺寸 {width}×{height} 无效或超过上限 {MAX_IMAGE_DIMENSION}"
        ));
    }
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| "图片行宽溢出".to_string())?;
    let total = u64::from(stride) * u64::from(height);
    if total > MAX_IMAGE_BYTES {
        return Err(format!(
            "图片解码需要 {} MiB，超过上限 {} MiB",
            total / (1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let mut bgra = vec![0u8; total as usize];
    // SAFETY: 目标缓冲区按 stride * height 分配，尺寸与格式匹配。
    unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut bgra) }
        .map_err(|error| format!("解码图片像素失败：{error}"))?;
    Ok(DecodedImage {
        width,
        height,
        bgra,
    })
}

/// 把 BGRA8 像素写成 PNG；用于效果参考截图与互操作原型诊断。
pub fn save_png(path: &Path, width: u32, height: u32, bgra: &[u8]) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("截图尺寸为 0".into());
    }
    let expected = width as usize * 4 * height as usize;
    if bgra.len() < expected {
        return Err(format!(
            "截图缓冲区不足：需要 {expected} 字节，实际 {}",
            bgra.len()
        ));
    }
    let factory = factory()?;
    let wide = wide_path(path);
    // SAFETY: 路径以 NUL 结尾；流归本函数所有。
    let stream = unsafe { factory.CreateStream() }
        .map_err(|error| format!("创建 WIC 输出流失败：{error}"))?;
    // SAFETY: 目标文件可写；失败返回明确错误。
    unsafe { stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0) }
        .map_err(|error| format!("打开截图输出失败 {}：{error}", path.display()))?;
    // SAFETY: PNG 容器格式与工厂均有效。
    let encoder = unsafe { factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()) }
        .map_err(|error| format!("创建 PNG 编码器失败：{error}"))?;
    // SAFETY: 流与编码器有效。
    unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }
        .map_err(|error| format!("初始化 PNG 编码器失败：{error}"))?;
    let mut frame = None;
    // SAFETY: 输出帧由本函数初始化；不使用编码器选项。
    unsafe { encoder.CreateNewFrame(&mut frame, std::ptr::null_mut()) }
        .map_err(|error| format!("创建 PNG 帧失败：{error}"))?;
    let frame = frame.ok_or("PNG 编码器未返回帧")?;
    // SAFETY: 帧有效；不使用属性包。
    unsafe {
        frame.Initialize(None::<&windows::Win32::System::Com::StructuredStorage::IPropertyBag2>)
    }
    .map_err(|error| format!("初始化 PNG 帧失败：{error}"))?;
    // SAFETY: 尺寸与像素格式固定。
    unsafe {
        frame
            .SetSize(width, height)
            .map_err(|error| format!("设置 PNG 尺寸失败：{error}"))?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame
            .SetPixelFormat(&mut format)
            .map_err(|error| format!("设置 PNG 像素格式失败：{error}"))?;
        frame
            .WritePixels(height, width * 4, &bgra[..expected])
            .map_err(|error| format!("写入 PNG 像素失败：{error}"))?;
        frame
            .Commit()
            .map_err(|error| format!("提交 PNG 帧失败：{error}"))?;
        encoder
            .Commit()
            .map_err(|error| format!("提交 PNG 文件失败：{error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_screenshot_buffer() {
        let path = std::env::temp_dir().join("deskshell-empty.png");
        let error = save_png(&path, 4, 4, &[0u8; 8]).unwrap_err();
        assert!(error.contains("缓冲区不足"), "{error}");
        assert!(!path.exists());
    }
}
