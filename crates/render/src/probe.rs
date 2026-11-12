//! 适配器与显示输出探测（M0）。
//!
//! 只读取系统当前状态，不创建渲染设备。厂商信息仅用于诊断，
//! 功能启用仍需按查询到的能力决定。

use std::fmt;

use crate::AdapterVendor;

#[derive(Debug, Clone, Default)]
pub struct DisplayTopology {
    pub adapters: Vec<AdapterInfo>,
}

#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub index: u32,
    pub name: String,
    pub vendor: AdapterVendor,
    pub vendor_id: u32,
    pub device_id: u32,
    /// 高 32 位为 LUID.HighPart，低 32 位为 LUID.LowPart。
    pub luid: u64,
    pub dedicated_video_memory: u64,
    pub shared_system_memory: u64,
    pub software: bool,
    pub outputs: Vec<OutputInfo>,
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub device_name: String,
    pub attached: bool,
    pub desktop_rect: (i32, i32, i32, i32),
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<u32>,
    pub rotation: &'static str,
    pub dpi_x: u32,
    pub dpi_y: u32,
}

impl OutputInfo {
    pub fn scale_percent(&self) -> u32 {
        (f64::from(self.dpi_x) / 96.0 * 100.0).round() as u32
    }
}

impl DisplayTopology {
    pub fn output_count(&self) -> usize {
        self.adapters
            .iter()
            .map(|adapter| adapter.outputs.len())
            .sum()
    }
}

#[derive(Debug, Clone)]
pub struct ProbeError {
    message: String,
}

impl ProbeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProbeError {}

pub fn probe_displays() -> Result<DisplayTopology, ProbeError> {
    imp::probe()
}

#[cfg(windows)]
mod imp {
    use std::mem::size_of;

    use windows::{
        Win32::{
            Graphics::{
                Dxgi::{
                    Common::{
                        DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_ROTATE90,
                        DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
                        DXGI_MODE_ROTATION_UNSPECIFIED,
                    },
                    CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
                    DXGI_ERROR_NOT_FOUND, IDXGIFactory2,
                },
                Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW},
            },
            UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        },
        core::{Error as WinError, PCWSTR},
    };

    use super::{AdapterInfo, AdapterVendor, DisplayTopology, OutputInfo, ProbeError};

    pub fn probe() -> Result<DisplayTopology, ProbeError> {
        // SAFETY: 不传入外部指针；工厂对象在返回后被 Windows 引用计数管理。
        let factory: IDXGIFactory2 =
            unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }.map_err(to_probe_error)?;
        let mut adapters = Vec::new();
        let mut index = 0u32;
        loop {
            // SAFETY: 索引枚举由 DXGI 校验；失败时错误码用于区分越界与真实故障。
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(to_probe_error(error)),
            };
            // SAFETY: 适配器由上面的工厂调用返回，描述结构为固定大小输出。
            let desc = unsafe { adapter.GetDesc1() }.map_err(to_probe_error)?;
            let software = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
            let mut outputs = Vec::new();
            let mut output_index = 0u32;
            loop {
                // SAFETY: 同适配器枚举；越界返回 DXGI_ERROR_NOT_FOUND。
                let output = match unsafe { adapter.EnumOutputs(output_index) } {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(error) => return Err(to_probe_error(error)),
                };
                // SAFETY: 输出由适配器枚举返回；描述结构为固定大小输出。
                let output_desc = unsafe { output.GetDesc() }.map_err(to_probe_error)?;
                outputs.push(output_info(&output_desc));
                output_index += 1;
            }
            adapters.push(AdapterInfo {
                index,
                name: wide_to_string(&desc.Description),
                vendor: AdapterVendor::from_pci_id(desc.VendorId),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                luid: (u64::from(desc.AdapterLuid.HighPart as u32) << 32)
                    | u64::from(desc.AdapterLuid.LowPart),
                dedicated_video_memory: desc.DedicatedVideoMemory as u64,
                shared_system_memory: desc.SharedSystemMemory as u64,
                software,
                outputs,
            });
            index += 1;
        }
        if adapters.is_empty() {
            return Err(ProbeError::new("未枚举到任何 DXGI 适配器"));
        }
        Ok(DisplayTopology { adapters })
    }

    fn output_info(desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC) -> OutputInfo {
        let rect = desc.DesktopCoordinates;
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        // SAFETY: HMONITOR 来自 DXGI 输出描述；失败时保留 96 DPI 回退。
        let _ =
            unsafe { GetDpiForMonitor(desc.Monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        OutputInfo {
            device_name: wide_to_string(&desc.DeviceName),
            attached: desc.AttachedToDesktop.as_bool(),
            desktop_rect: (rect.left, rect.top, rect.right, rect.bottom),
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
            refresh_hz: refresh_hz(&desc.DeviceName),
            rotation: rotation_label(desc.Rotation),
            dpi_x,
            dpi_y,
        }
    }

    fn refresh_hz(device_name: &[u16]) -> Option<u32> {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: 设备名以 NUL 结尾（DXGI 固定数组），结构体已设置 dmSize。
        let ok = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(device_name.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        };
        ok.as_bool().then_some(mode.dmDisplayFrequency)
    }

    fn rotation_label(
        rotation: windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION,
    ) -> &'static str {
        match rotation {
            DXGI_MODE_ROTATION_IDENTITY => "0°",
            DXGI_MODE_ROTATION_ROTATE90 => "90°",
            DXGI_MODE_ROTATION_ROTATE180 => "180°",
            DXGI_MODE_ROTATION_ROTATE270 => "270°",
            DXGI_MODE_ROTATION_UNSPECIFIED => "未指定",
            _ => "未知",
        }
    }

    fn wide_to_string(buffer: &[u16]) -> String {
        let length = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..length])
    }

    fn to_probe_error(error: WinError) -> ProbeError {
        ProbeError::new(format!("DXGI 调用失败：{error}"))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{DisplayTopology, ProbeError};

    pub fn probe() -> Result<DisplayTopology, ProbeError> {
        Err(ProbeError::new("显示探测仅支持 Windows"))
    }
}
