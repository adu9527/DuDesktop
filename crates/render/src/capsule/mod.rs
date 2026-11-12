//! M0 透明胶囊原型：D3D11 + DXGI 合成交换链 + DirectComposition + Direct2D。
//!
//! 只用于验证透明合成、命中区域和事件驱动帧调度；不是产品 UI 实现。

mod composition;
mod window;

use std::fmt;

/// 胶囊逻辑尺寸（DIP）。窗口按最大宽度创建，小尺寸时由窗口区域收回命中范围。
pub(crate) const CAPSULE_WIDTH_MIN: f32 = 360.0;
pub(crate) const CAPSULE_WIDTH_MAX: f32 = 408.0;
pub(crate) const CAPSULE_HEIGHT: f32 = 48.0;

/// 阴影层向胶囊四周扩展的逻辑边距（DIP）。
pub(crate) const SHADOW_MARGIN_X: f32 = 48.0;
pub(crate) const SHADOW_MARGIN_Y: f32 = 30.0;
pub(crate) const SHADOW_WINDOW_WIDTH: f32 = CAPSULE_WIDTH_MAX + SHADOW_MARGIN_X * 2.0;
pub(crate) const SHADOW_WINDOW_HEIGHT: f32 = CAPSULE_HEIGHT + SHADOW_MARGIN_Y * 2.0;

pub(crate) fn capsule_logical_width(hover: f32) -> f32 {
    CAPSULE_WIDTH_MIN + (CAPSULE_WIDTH_MAX - CAPSULE_WIDTH_MIN) * hover.clamp(0.0, 1.0)
}

pub(crate) fn capsule_width_px(hover: f32, dpi: u32) -> i32 {
    let scale = dpi as f32 / 96.0;
    (capsule_logical_width(hover) * scale).round() as i32
}

/// 返回胶囊窗口内的区域矩形（像素），用于圆角裁剪与命中测试。
pub(crate) fn capsule_region(
    window_width_px: u32,
    height_px: u32,
    hover: f32,
    dpi: u32,
) -> (i32, i32, i32, i32) {
    let width = capsule_width_px(hover, dpi).clamp(1, window_width_px as i32);
    let left = (window_width_px as i32 - width) / 2;
    (left, 0, left + width, height_px as i32)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CapsuleOptions {
    /// 自动退出时间；None 表示保持到用户关闭。
    pub seconds: Option<u64>,
    /// 创建后自动执行命中测试与空闲帧检查，然后退出。
    pub self_test: bool,
    /// 移动真实鼠标执行点击穿透与焦点验证，仅用于显式测试。
    pub input_test: bool,
    /// 显式注入目标/设备失效分类，验证资源重建与后续呈现。
    pub recovery_test: bool,
    /// 目标显示器索引；None 使用主显示器。
    pub monitor: Option<usize>,
}

#[derive(Debug)]
pub enum CompositionError {
    DeviceLost,
    TargetLost,
    Other(String),
}

impl fmt::Display for CompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceLost => formatter.write_str("图形设备丢失"),
            Self::TargetLost => formatter.write_str("绘制目标失效"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl From<CompositionError> for String {
    fn from(error: CompositionError) -> Self {
        error.to_string()
    }
}

pub fn run_capsule_prototype(options: CapsuleOptions) -> Result<(), String> {
    window::run(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_width_scales_with_dpi() {
        assert_eq!(capsule_width_px(0.0, 96), 360);
        assert_eq!(capsule_width_px(1.0, 96), 408);
        assert_eq!(capsule_width_px(0.0, 120), 450);
        assert_eq!(capsule_width_px(1.0, 120), 510);
        assert_eq!(capsule_width_px(0.0, 144), 540);
        assert_eq!(capsule_width_px(1.0, 144), 612);
    }

    #[test]
    fn capsule_region_stays_centered() {
        for dpi in [96u32, 120, 144] {
            let window_width = (CAPSULE_WIDTH_MAX * f32::from(dpi as u16) / 96.0).round() as u32;
            for hover in [0.0f32, 0.5, 1.0] {
                let (left, top, right, bottom) = capsule_region(window_width, 60, hover, dpi);
                assert_eq!(top, 0);
                assert_eq!(bottom, 60);
                assert!(left >= 0 && right <= window_width as i32);
                assert_eq!(
                    (left + right) / 2,
                    window_width as i32 / 2,
                    "dpi={dpi} hover={hover}"
                );
            }
        }
    }
}
