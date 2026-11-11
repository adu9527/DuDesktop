//! 跨进程数据契约，不依赖窗口或 GPU 后端。
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityPreset {
    Eco,
    Balanced,
    Ultra,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackState {
    Playing,
    Throttled,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHello {
    pub protocol_version: u32,
    pub process_id: u32,
    pub application_version: String,
    pub state: PlaybackState,
}

impl HostHello {
    pub fn current() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            state: PlaybackState::Stopped,
        }
    }

    pub fn is_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_protocol_version() {
        let mut hello = HostHello::current();
        assert!(hello.is_compatible());
        hello.protocol_version += 1;
        assert!(!hello.is_compatible());
    }
}
