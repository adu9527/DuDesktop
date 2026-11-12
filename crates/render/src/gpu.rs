//! UI 线程独占的共享 GPU 上下文：D3D11、DirectComposition、Direct2D、DirectWrite。
//!
//! Dock、灵动岛与设置窗共享同一设备与文字格式；每个窗口自行持有交换链与
//! 合成视觉，设备丢失时一并重建。

use std::rc::Rc;

use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct2D::{D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory1},
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext,
            },
            DirectComposition::{DCompositionCreateDevice, IDCompositionDevice},
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING,
                DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
            },
            Dxgi::{IDXGIDevice, IDXGIFactory2},
        },
    },
    core::{Interface, PCWSTR},
};

use crate::surface::RenderError;

pub(crate) struct Gpu {
    device: ID3D11Device,
    _context: ID3D11DeviceContext,
    pub dcomp: IDCompositionDevice,
    pub factory: IDXGIFactory2,
    pub d2d: ID2D1Factory1,
    pub display: IDWriteTextFormat,
    /// 控件大号时钟。
    pub clock: IDWriteTextFormat,
    /// 控件温度等大号数值。
    pub temperature: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    pub strong: IDWriteTextFormat,
    pub row: IDWriteTextFormat,
    pub caption: IDWriteTextFormat,
    pub action: IDWriteTextFormat,
    pub symbol: IDWriteTextFormat,
    pub symbol_small: IDWriteTextFormat,
}

impl Gpu {
    pub fn new() -> Result<Rc<Self>, RenderError> {
        // SAFETY: 只创建当前线程拥有的设备；输出参数由本函数初始化。
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        let levels = [
            D3D_FEATURE_LEVEL(0xb100),
            D3D_FEATURE_LEVEL(0xb000),
            D3D_FEATURE_LEVEL(0xa100),
            D3D_FEATURE_LEVEL(0xa000),
        ];
        // SAFETY: 不传入外部适配器；返回值由本函数检查。
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
        .map_err(|error| RenderError::Other(format!("创建 D3D11 设备失败：{error}")))?;
        let device = device.ok_or_else(|| RenderError::Other("D3D11 设备为空".into()))?;
        let context = context.ok_or_else(|| RenderError::Other("D3D11 上下文为空".into()))?;
        // SAFETY: 设备由当前线程持有，DComp 设备引用该 DXGI 设备。
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|error| RenderError::Other(format!("获取 IDXGIDevice 失败：{error}")))?;
        // SAFETY: 同上。
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device) }
            .map_err(|error| {
                RenderError::Other(format!("创建 DirectComposition 设备失败：{error}"))
            })?;
        // SAFETY: 适配器与工厂由 DXGI 设备持有。
        let adapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| RenderError::Other(format!("获取适配器失败：{error}")))?;
        // SAFETY: 工厂查询只读取父对象。
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| RenderError::Other(format!("获取 DXGI 工厂失败：{error}")))?;
        // SAFETY: 单线程 UI 使用单线程 D2D 工厂。
        let d2d: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|error| RenderError::Other(format!("创建 Direct2D 工厂失败：{error}")))?;
        // SAFETY: 共享 DirectWrite 工厂线程安全。
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|error| RenderError::Other(format!("创建 DirectWrite 工厂失败：{error}")))?;
        Ok(Rc::new(Self {
            display: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                20.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            clock: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                34.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            temperature: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                26.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            title: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                15.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            strong: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                13.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            row: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                13.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            caption: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                11.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            action: text_format(
                &dwrite,
                "Microsoft YaHei UI",
                12.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            symbol: text_format(
                &dwrite,
                "Segoe Fluent Icons",
                22.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            symbol_small: text_format(
                &dwrite,
                "Segoe Fluent Icons",
                13.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            device,
            _context: context,
            dcomp,
            factory,
            d2d,
        }))
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }
}

fn text_format(
    dwrite: &IDWriteFactory,
    family: &str,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat, RenderError> {
    let family: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let locale: Vec<u16> = "zh-CN".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 字体名与区域字符串在调用期间有效。
    let format = unsafe {
        dwrite.CreateTextFormat(
            PCWSTR::from_raw(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR::from_raw(locale.as_ptr()),
        )
    }
    .map_err(|error| RenderError::Other(format!("创建文本格式失败：{error}")))?;
    // SAFETY: 文本格式归本模块所有。
    unsafe {
        format
            .SetTextAlignment(alignment)
            .map_err(|error| RenderError::Other(format!("设置文本对齐失败：{error}")))?;
        format
            .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
            .map_err(|error| RenderError::Other(format!("设置段落对齐失败：{error}")))?;
        format
            .SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
            .map_err(|error| RenderError::Other(format!("设置换行模式失败：{error}")))?;
        let ellipsis = dwrite
            .CreateEllipsisTrimmingSign(&format)
            .map_err(|error| RenderError::Other(format!("创建省略号失败：{error}")))?;
        format
            .SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                },
                &ellipsis,
            )
            .map_err(|error| RenderError::Other(format!("设置文本截断失败：{error}")))?;
    }
    Ok(format)
}
