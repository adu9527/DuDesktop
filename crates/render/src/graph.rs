//! 最小实用 Render Graph：节点声明、逐项裁剪、帧缓存与显存预算。
//!
//! 本模块是纯逻辑，不创建 D3D 对象，可在无 GPU 的单元测试中验证：
//! - 关闭某个效果后对应节点从计划中消失，pass 数下降；
//! - 时间无关且输入未变化时不重绘（静态停帧）；
//! - 粒子等时间相关节点只在允许动画时产生新的帧键；
//! - 临时纹理超过预算时先降级 Bloom，再拒绝无法满足的基础缓冲。
//!
//! 实际纹理创建、着色器与 Present 由 `effects::pipeline` 执行。

use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    time::Duration,
};

/// 效果参数 schema 版本；持久化与协议变化时递增。
pub const EFFECT_SCHEMA_VERSION: u32 = 1;

/// 动画效果的最小帧间隔（约 30 fps）。
pub const ANIMATION_INTERVAL: Duration = Duration::from_millis(33);

/// 默认临时纹理预算：256 MiB。4K 图片约需 82 MiB（含 Bloom 半分辨率），8K 会被降级或拒绝。
pub const DEFAULT_TEXTURE_BUDGET: u64 = 256 * 1024 * 1024;

/// 每像素字节数（BGRA8）。
const BYTES_PER_PIXEL: u64 = 4;

/// 源图在输出表面的缩放方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// 等比放大并裁切溢出，填满输出。
    #[default]
    Cover,
    /// 等比完整显示，多余区域为黑色。
    Contain,
    /// 不保持比例，拉伸到输出尺寸。
    Stretch,
    /// 按源图原生像素尺寸平铺；源图大于输出时退化为 Cover。
    Tile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectKind {
    Color,
    Vignette,
    Blur,
    Bloom,
    Particles,
    Gradient,
}

impl EffectKind {
    pub const ALL: [Self; 6] = [
        Self::Color,
        Self::Vignette,
        Self::Blur,
        Self::Bloom,
        Self::Particles,
        Self::Gradient,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Color => "调色",
            Self::Vignette => "暗角",
            Self::Blur => "模糊",
            Self::Bloom => "Bloom",
            Self::Particles => "粒子",
            Self::Gradient => "渐变",
        }
    }

    /// 该效果自身消耗的 pass 数；最后输出 pass 不计入。
    pub fn pass_count(self) -> u32 {
        match self {
            Self::Color | Self::Vignette | Self::Particles | Self::Gradient => 1,
            Self::Blur => 2,
            Self::Bloom => 4,
        }
    }

    pub fn animated(self) -> bool {
        matches!(self, Self::Particles)
    }
}

/// 用户可独立调节的图片效果强度（0..=100）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ImageEffects {
    pub color: u8,
    pub vignette: u8,
    pub blur: u8,
    pub bloom: u8,
    pub particles: u8,
    pub gradient: u8,
}

impl ImageEffects {
    pub const OFF: Self = Self {
        color: 0,
        vignette: 0,
        blur: 0,
        bloom: 0,
        particles: 0,
        gradient: 0,
    };

    pub const BALANCED: Self = Self {
        color: 35,
        vignette: 25,
        blur: 0,
        bloom: 0,
        particles: 0,
        gradient: 0,
    };

    pub const ULTRA: Self = Self {
        color: 55,
        vignette: 40,
        blur: 20,
        bloom: 30,
        particles: 0,
        gradient: 20,
    };

    pub fn any(&self) -> bool {
        EffectKind::ALL.iter().any(|kind| self.strength(*kind) > 0)
    }

    pub fn animated(&self) -> bool {
        EffectKind::ALL
            .iter()
            .any(|kind| kind.animated() && self.strength(*kind) > 0)
    }

    pub fn strength(&self, kind: EffectKind) -> u8 {
        match kind {
            EffectKind::Color => self.color,
            EffectKind::Vignette => self.vignette,
            EffectKind::Blur => self.blur,
            EffectKind::Bloom => self.bloom,
            EffectKind::Particles => self.particles,
            EffectKind::Gradient => self.gradient,
        }
    }

    pub fn set(&mut self, kind: EffectKind, value: u8) {
        let value = value.min(100);
        match kind {
            EffectKind::Color => self.color = value,
            EffectKind::Vignette => self.vignette = value,
            EffectKind::Blur => self.blur = value,
            EffectKind::Bloom => self.bloom = value,
            EffectKind::Particles => self.particles = value,
            EffectKind::Gradient => self.gradient = value,
        }
    }

    pub fn normalized(mut self) -> Self {
        for kind in EffectKind::ALL {
            self.set(kind, self.strength(kind));
        }
        self
    }

    /// 已启用的效果，按固定顺序返回，供状态与验收报告使用。
    pub fn enabled(&self) -> Vec<EffectKind> {
        EffectKind::ALL
            .into_iter()
            .filter(|kind| self.strength(*kind) > 0)
            .collect()
    }

    /// 效果名列表（中文），用于 `InstanceStatus.effects_active`。
    pub fn labels(&self) -> Vec<&'static str> {
        self.enabled().into_iter().map(EffectKind::label).collect()
    }
}

/// 图中的纹理槽；ping/pong 在运行时交替。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureSlot {
    Source,
    Front,
    Back,
    HalfA,
    HalfB,
    BackBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PassKind {
    Color,
    Vignette,
    BlurH,
    BlurV,
    BloomThreshold,
    BloomBlurH,
    BloomBlurV,
    BloomComposite,
    Particles,
    Gradient,
    Output,
}

impl PassKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Color => "调色",
            Self::Vignette => "暗角",
            Self::BlurH => "模糊（水平）",
            Self::BlurV => "模糊（垂直）",
            Self::BloomThreshold => "Bloom 阈值",
            Self::BloomBlurH => "Bloom 模糊（水平）",
            Self::BloomBlurV => "Bloom 模糊（垂直）",
            Self::BloomComposite => "Bloom 合成",
            Self::Particles => "粒子",
            Self::Gradient => "渐变",
            Self::Output => "输出",
        }
    }
}

/// 节点静态声明：输入/输出、时间依赖与资源成本。
#[derive(Debug, Clone, Copy)]
pub struct NodeSpec {
    pub pass: PassKind,
    pub effect: Option<EffectKind>,
    pub inputs: &'static [TextureSlot],
    pub output: TextureSlot,
    pub animated: bool,
    /// 输出纹理相对屏幕的像素比例（1 = 全分辨率，2 = 半分辨率）。
    pub resolution_divisor: u32,
}

impl NodeSpec {
    pub fn half_resolution(self) -> bool {
        self.resolution_divisor > 1
    }
}

pub const NODE_TABLE: &[NodeSpec] = &[
    NodeSpec {
        pass: PassKind::Color,
        effect: Some(EffectKind::Color),
        inputs: &[TextureSlot::Source],
        output: TextureSlot::Front,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::Vignette,
        effect: Some(EffectKind::Vignette),
        inputs: &[TextureSlot::Front],
        output: TextureSlot::Back,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::BlurH,
        effect: Some(EffectKind::Blur),
        inputs: &[TextureSlot::Front],
        output: TextureSlot::Back,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::BlurV,
        effect: Some(EffectKind::Blur),
        inputs: &[TextureSlot::Back],
        output: TextureSlot::Front,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::BloomThreshold,
        effect: Some(EffectKind::Bloom),
        inputs: &[TextureSlot::Front],
        output: TextureSlot::HalfA,
        animated: false,
        resolution_divisor: 2,
    },
    NodeSpec {
        pass: PassKind::BloomBlurH,
        effect: Some(EffectKind::Bloom),
        inputs: &[TextureSlot::HalfA],
        output: TextureSlot::HalfB,
        animated: false,
        resolution_divisor: 2,
    },
    NodeSpec {
        pass: PassKind::BloomBlurV,
        effect: Some(EffectKind::Bloom),
        inputs: &[TextureSlot::HalfB],
        output: TextureSlot::HalfA,
        animated: false,
        resolution_divisor: 2,
    },
    NodeSpec {
        pass: PassKind::BloomComposite,
        effect: Some(EffectKind::Bloom),
        inputs: &[TextureSlot::Front, TextureSlot::HalfA],
        output: TextureSlot::Back,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::Particles,
        effect: Some(EffectKind::Particles),
        inputs: &[TextureSlot::Front],
        output: TextureSlot::Back,
        animated: true,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::Gradient,
        effect: Some(EffectKind::Gradient),
        inputs: &[TextureSlot::Front],
        output: TextureSlot::Back,
        animated: false,
        resolution_divisor: 1,
    },
    NodeSpec {
        pass: PassKind::Output,
        effect: None,
        inputs: &[TextureSlot::Front],
        output: TextureSlot::BackBuffer,
        animated: false,
        resolution_divisor: 1,
    },
];

fn node(pass: PassKind) -> NodeSpec {
    *NODE_TABLE
        .iter()
        .find(|node| node.pass == pass)
        .expect("节点表必须包含全部 pass")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectPass {
    pub kind: PassKind,
    pub effect: Option<EffectKind>,
    pub strength: u8,
    pub half_resolution: bool,
    pub animated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPlan {
    pub passes: Vec<EffectPass>,
    pub animated: bool,
    pub degraded: Vec<&'static str>,
    /// 计划键：参数与 pass 组成的稳定哈希；用于帧缓存与诊断。
    pub key: u64,
    /// 临时纹理总字节（不含源纹理与交换链后台缓冲）。
    pub temporary_bytes: u64,
}

impl EffectPlan {
    pub fn pass_count(&self) -> u32 {
        self.passes.len() as u32
    }

    pub fn effect_passes(&self) -> u32 {
        self.passes
            .iter()
            .filter(|pass| pass.kind != PassKind::Output)
            .count() as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanRequest {
    pub width: u32,
    pub height: u32,
    pub effects: ImageEffects,
    /// 显式预算；`None` 使用 `DEFAULT_TEXTURE_BUDGET`。
    pub budget_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    EmptySize,
    BudgetExceeded { required: u64, budget: u64 },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySize => formatter.write_str("渲染尺寸为 0"),
            Self::BudgetExceeded { required, budget } => write!(
                formatter,
                "临时纹理需要 {} MiB，超过预算 {} MiB",
                required / (1024 * 1024),
                budget / (1024 * 1024)
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// 根据效果参数与尺寸裁剪节点，并在预算不足时降级 Bloom。
pub fn build_plan(request: PlanRequest) -> Result<EffectPlan, PlanError> {
    if request.width == 0 || request.height == 0 {
        return Err(PlanError::EmptySize);
    }
    let budget = request.budget_bytes.unwrap_or(DEFAULT_TEXTURE_BUDGET);
    let effects = request.effects.normalized();
    let mut degraded = Vec::new();
    let mut effects = effects;

    // 预算只受 Bloom 的半分辨率附件影响；先降级 Bloom，再拒绝基础缓冲。
    let mut temporary_bytes = base_temporary_bytes(request.width, request.height);
    if effects.bloom > 0 && temporary_bytes + bloom_bytes(request.width, request.height) > budget {
        effects.bloom = 0;
        degraded.push("Bloom（显存预算不足）");
    }
    if temporary_bytes > budget {
        return Err(PlanError::BudgetExceeded {
            required: temporary_bytes,
            budget,
        });
    }
    temporary_bytes = base_temporary_bytes(request.width, request.height);
    if effects.bloom > 0 {
        temporary_bytes += bloom_bytes(request.width, request.height);
    }

    let mut passes = Vec::new();
    let mut push = |kind: PassKind, strength: u8| {
        let spec = node(kind);
        passes.push(EffectPass {
            kind,
            effect: spec.effect,
            strength,
            half_resolution: spec.half_resolution(),
            animated: spec.animated,
        });
    };

    // 顺序与 NODE_TABLE 的输出槽一致：每个 pass 读取上一个 pass 的结果。
    // Color 始终执行：强度为 0 时是纯拷贝，同时负责 cover/contain/tile 的 UV 映射，
    // 避免关闭调色后其它效果直接拉伸源图。
    push(PassKind::Color, effects.color);
    if effects.blur > 0 {
        push(PassKind::BlurH, effects.blur);
        push(PassKind::BlurV, effects.blur);
    }
    if effects.bloom > 0 {
        push(PassKind::BloomThreshold, effects.bloom);
        push(PassKind::BloomBlurH, effects.bloom);
        push(PassKind::BloomBlurV, effects.bloom);
        push(PassKind::BloomComposite, effects.bloom);
    }
    if effects.vignette > 0 {
        push(PassKind::Vignette, effects.vignette);
    }
    if effects.gradient > 0 {
        push(PassKind::Gradient, effects.gradient);
    }
    if effects.particles > 0 {
        push(PassKind::Particles, effects.particles);
    }
    // 输出 pass 始终存在：没有效果时直接复制源纹理到后台缓冲。
    push(PassKind::Output, 0);

    let animated = passes.iter().any(|pass| pass.animated);
    let key = plan_key(&passes);
    Ok(EffectPlan {
        passes,
        animated,
        degraded,
        key,
        temporary_bytes,
    })
}

fn base_temporary_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * BYTES_PER_PIXEL * 2
}

fn bloom_bytes(width: u32, height: u32) -> u64 {
    let half_width = u64::from(width).div_ceil(2);
    let half_height = u64::from(height).div_ceil(2);
    half_width * half_height * BYTES_PER_PIXEL * 2
}

fn plan_key(passes: &[EffectPass]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for pass in passes {
        pass.kind.hash(&mut hasher);
        pass.strength.hash(&mut hasher);
    }
    hasher.finish()
}

/// 帧缓存键：计划、源素材版本、尺寸与动画时间片。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub plan: u64,
    pub image_revision: u64,
    pub width: u32,
    pub height: u32,
    pub time_slot: u64,
}

/// 构造帧键；非动画计划或不允许动画时固定为时间片 0。
pub fn frame_key(
    plan: &EffectPlan,
    image_revision: u64,
    width: u32,
    height: u32,
    elapsed: Duration,
    allow_animation: bool,
) -> FrameKey {
    let time_slot = if allow_animation && plan.animated {
        elapsed.as_millis() as u64 / ANIMATION_INTERVAL.as_millis().max(1) as u64
    } else {
        0
    };
    FrameKey {
        plan: plan.key,
        image_revision,
        width,
        height,
        time_slot,
    }
}

/// 无变化缓存：相同帧键不重复渲染。
#[derive(Debug, Default)]
pub struct FrameCache {
    last: Option<FrameKey>,
    presents: u64,
    skipped: u64,
}

impl FrameCache {
    pub fn should_draw(&mut self, key: FrameKey) -> bool {
        self.last != Some(key)
    }

    pub fn presented(&mut self, key: FrameKey) {
        self.last = Some(key);
        self.presents += 1;
    }

    pub fn skipped(&mut self) {
        self.skipped += 1;
    }

    pub fn presents(&self) -> u64 {
        self.presents
    }

    pub fn skips(&self) -> u64 {
        self.skipped
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// 临时纹理池预算：实际纹理由管线持有，本类型只做可测试的容量与复用记账。
#[derive(Debug)]
pub struct TexturePool {
    budget: u64,
    used: u64,
    slots: BTreeMap<(u32, u32), u32>,
}

impl TexturePool {
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            used: 0,
            slots: BTreeMap::new(),
        }
    }

    pub fn reserve(&mut self, width: u32, height: u32) -> bool {
        let bytes = u64::from(width) * u64::from(height) * BYTES_PER_PIXEL;
        if self.used + bytes > self.budget {
            return false;
        }
        self.used += bytes;
        *self.slots.entry((width, height)).or_insert(0) += 1;
        true
    }

    pub fn release(&mut self, width: u32, height: u32) {
        let bytes = u64::from(width) * u64::from(height) * BYTES_PER_PIXEL;
        if let Some(count) = self.slots.get_mut(&(width, height))
            && *count > 0
        {
            *count -= 1;
            self.used = self.used.saturating_sub(bytes);
            if *count == 0 {
                self.slots.remove(&(width, height));
            }
        }
    }

    pub fn clear(&mut self) {
        self.used = 0;
        self.slots.clear();
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    pub fn live_textures(&self) -> u32 {
        self.slots.values().copied().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(effects: ImageEffects) -> PlanRequest {
        PlanRequest {
            width: 2560,
            height: 1440,
            effects,
            budget_bytes: None,
        }
    }

    #[test]
    fn no_effects_still_maps_source() {
        let plan = build_plan(request(ImageEffects::OFF)).unwrap();
        // Color 强度 0 负责 UV 映射（cover/contain/tile），Output 始终存在。
        assert_eq!(plan.passes.len(), 2);
        assert_eq!(plan.passes[0].kind, PassKind::Color);
        assert_eq!(plan.passes[0].strength, 0);
        assert_eq!(plan.passes[1].kind, PassKind::Output);
        assert!(!plan.animated);
        assert_eq!(plan.effect_passes(), 1);
    }

    #[test]
    fn each_effect_adds_expected_passes_and_can_be_removed() {
        for kind in EffectKind::ALL {
            let mut effects = ImageEffects::OFF;
            effects.set(kind, 50);
            let plan = build_plan(request(effects)).unwrap();
            let effect_count = plan
                .passes
                .iter()
                .filter(|pass| pass.effect == Some(kind))
                .count() as u32;
            assert_eq!(effect_count, kind.pass_count(), "{}", kind.label());
            // Color 始终作为映射 pass；Color 自身关闭时保留强度 0 的拷贝。
            let expected = if kind == EffectKind::Color {
                kind.pass_count() + 1
            } else {
                kind.pass_count() + 2
            };
            assert_eq!(plan.pass_count(), expected);

            // 关闭该效果后，除映射用的 Color(0) 外不应保留该节点。
            let removed = build_plan(request(ImageEffects::OFF)).unwrap();
            assert!(
                removed.passes.iter().all(|pass| {
                    pass.effect != Some(kind) || (kind == EffectKind::Color && pass.strength == 0)
                }),
                "{} 关闭后仍存在节点",
                kind.label()
            );
        }
    }

    #[test]
    fn all_effects_total_expected_pass_count() {
        let mut effects = ImageEffects::OFF;
        for kind in EffectKind::ALL {
            effects.set(kind, 50);
        }
        let plan = build_plan(request(effects)).unwrap();
        let expected: u32 = EffectKind::ALL.iter().map(|kind| kind.pass_count()).sum();
        assert_eq!(plan.pass_count(), expected + 1);
        assert_eq!(plan.effect_passes(), expected);
        assert!(plan.animated, "粒子启用时计划必须标记为动画");
    }

    #[test]
    fn bloom_is_degraded_before_rejecting_budget() {
        let mut effects = ImageEffects::OFF;
        effects.bloom = 40;
        let base = base_temporary_bytes(2560, 1440);
        let plan = build_plan(PlanRequest {
            width: 2560,
            height: 1440,
            effects,
            budget_bytes: Some(base + 1),
        })
        .unwrap();
        assert_eq!(plan.passes.len(), 2);
        assert!(plan.degraded[0].contains("Bloom"));
        assert_eq!(plan.temporary_bytes, base);
    }

    #[test]
    fn impossible_base_budget_is_rejected() {
        let error = build_plan(PlanRequest {
            width: 7680,
            height: 4320,
            effects: ImageEffects::OFF,
            budget_bytes: Some(1024),
        })
        .unwrap_err();
        assert!(matches!(error, PlanError::BudgetExceeded { .. }));
    }

    #[test]
    fn static_plan_skips_unchanged_frames() {
        let effects = ImageEffects::BALANCED;
        let plan = build_plan(request(effects)).unwrap();
        let start = Duration::ZERO;
        let first = frame_key(&plan, 1, 2560, 1440, start, true);
        let later = frame_key(&plan, 1, 2560, 1440, Duration::from_secs(5), true);
        assert_eq!(first, later, "静态效果不得随时间变化");

        let mut cache = FrameCache::default();
        assert!(cache.should_draw(first));
        cache.presented(first);
        assert!(!cache.should_draw(later));
        cache.skipped();
        assert_eq!(cache.presents(), 1);
        assert_eq!(cache.skips(), 1);
    }

    #[test]
    fn animated_plan_produces_new_frame_keys_per_interval() {
        let mut effects = ImageEffects::OFF;
        effects.particles = 30;
        let plan = build_plan(request(effects)).unwrap();
        let first = frame_key(&plan, 1, 2560, 1440, Duration::ZERO, true);
        let same_slot = frame_key(&plan, 1, 2560, 1440, Duration::from_millis(10), true);
        let next_slot = frame_key(&plan, 1, 2560, 1440, Duration::from_millis(34), true);
        let paused = frame_key(&plan, 1, 2560, 1440, Duration::from_millis(34), false);
        assert_eq!(first, same_slot);
        assert_ne!(first, next_slot);
        assert_eq!(paused.time_slot, 0, "静态模式必须固定时间片");
    }

    #[test]
    fn image_or_size_change_invalidates_cache() {
        let plan = build_plan(request(ImageEffects::BALANCED)).unwrap();
        let mut cache = FrameCache::default();
        let key = frame_key(&plan, 1, 2560, 1440, Duration::ZERO, true);
        cache.presented(key);
        assert!(cache.should_draw(frame_key(&plan, 2, 2560, 1440, Duration::ZERO, true)));
        assert!(cache.should_draw(frame_key(&plan, 1, 1920, 1080, Duration::ZERO, true)));
    }

    #[test]
    fn texture_pool_reuses_capacity_and_releases_bytes() {
        let mut pool = TexturePool::new(2 * 100 * 100 * BYTES_PER_PIXEL);
        assert!(pool.reserve(100, 100));
        assert!(pool.reserve(100, 100));
        assert!(!pool.reserve(100, 100));
        assert_eq!(pool.live_textures(), 2);
        pool.release(100, 100);
        assert!(pool.reserve(100, 100));
        pool.clear();
        assert_eq!(pool.used(), 0);
        assert_eq!(pool.live_textures(), 0);
    }

    #[test]
    fn strengths_are_clamped_and_labels_follow_enabled_effects() {
        let mut effects = ImageEffects::OFF;
        effects.set(EffectKind::Bloom, 250);
        assert_eq!(effects.bloom, 100);
        effects.vignette = 20;
        assert_eq!(effects.labels(), vec!["暗角", "Bloom"]);
        assert!(effects.any());
        assert!(!effects.animated());
    }
}
