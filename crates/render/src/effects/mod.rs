//! M5 图片效果：Render Graph 计划、WIC 解码与 D3D11 管线。
//!
//! 产品层通过本模块的 `EffectPipeline` 渲染图片效果；Render Graph 的纯逻辑
//! 可独立单测，不依赖窗口或 GPU。

pub mod image;
pub mod pipeline;
pub mod shaders;

pub use pipeline::{EffectError, EffectPipeline, RenderStats};
