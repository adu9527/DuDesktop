//! 渲染边界骨架。M0 原型通过前不固定 wgpu/DirectComposition 实现。

/// 品牌只用于诊断；功能启用必须依据探测到的能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterVendor {
    Amd,
    Nvidia,
    Intel,
    Other(u32),
}

impl AdapterVendor {
    pub fn from_pci_id(id: u32) -> Self {
        match id {
            0x1002 => Self::Amd,
            0x10de => Self::Nvidia,
            0x8086 => Self::Intel,
            other => Self::Other(other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderCapabilities {
    pub vendor: AdapterVendor,
    pub adapter_name: String,
    pub composition_supported: bool,
    pub video_interop_supported: bool,
}

/// 无脏内容且无动画时无需安排下一帧。实际等待/呈现由后端实现。
#[derive(Debug, Default)]
pub struct FrameDemand {
    dirty: bool,
    animating: bool,
}

impl FrameDemand {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn set_animating(&mut self, value: bool) {
        self.animating = value;
    }

    pub fn needs_frame(&self) -> bool {
        self.dirty || self.animating
    }

    pub fn presented(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_settles_without_continuous_frames() {
        let mut demand = FrameDemand::default();
        assert!(!demand.needs_frame());
        demand.invalidate();
        assert!(demand.needs_frame());
        demand.presented();
        assert!(!demand.needs_frame());
        demand.set_animating(true);
        demand.presented();
        assert!(demand.needs_frame());
        demand.set_animating(false);
        assert!(!demand.needs_frame());
    }
}
