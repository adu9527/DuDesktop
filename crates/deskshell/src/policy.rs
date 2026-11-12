//! 用户画质意图与有效渲染策略。
//!
//! 本模块是纯逻辑：不调用 Win32、不访问文件、不持有 UI 句柄，便于用确定性时间测试。
//! 平台信号由 `shell-win::system_state` 提供，配置由 `config` 持久化。
//!
//! 两层模型：
//! - `QualityProfile` 是用户意图（预设、自定义、逐屏覆盖、自动规则开关），不会被自动降档改写；
//! - `EffectivePolicy` 是某一时刻、某一显示器的实际生效值，附带限制原因。
//!
//! 自动规则只做“降档/恢复”，并带进入迟滞、恢复迟滞与冷却，避免全屏切换或窗口遮挡时抖动。

use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use protocol::ImageEffects;

/// 进入自动降档前的持续条件时间。
pub const DEFAULT_REDUCTION_DELAY: Duration = Duration::from_millis(1_500);
/// 条件消失后恢复原画质前的稳定时间。
pub const DEFAULT_RECOVERY_DELAY: Duration = Duration::from_secs(30);
/// 两次自动策略切换之间的最小间隔。
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(10);
/// 自动应用规则上限，避免无界配置。
pub const MAX_AUTO_APPS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    Eco,
    #[default]
    Balanced,
    Ultra,
    Custom,
}

impl QualityPreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eco => "Eco（节能）",
            Self::Balanced => "Balanced（均衡）",
            Self::Ultra => "Ultra（高画质）",
            Self::Custom => "Custom（自定义）",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectSettings {
    pub ui_animations: bool,
    pub shadows: bool,
    pub wallpaper_playback: bool,
}

impl EffectSettings {
    pub const ECO: Self = Self {
        ui_animations: false,
        shadows: false,
        wallpaper_playback: false,
    };
    pub const BALANCED: Self = Self {
        ui_animations: true,
        shadows: true,
        wallpaper_playback: true,
    };
    pub const ULTRA: Self = Self {
        ui_animations: true,
        shadows: true,
        wallpaper_playback: true,
    };
}

impl Default for EffectSettings {
    fn default() -> Self {
        Self::BALANCED
    }
}

impl QualityPreset {
    pub fn settings(self, custom: EffectSettings) -> EffectSettings {
        match self {
            Self::Eco => EffectSettings::ECO,
            Self::Balanced => EffectSettings::BALANCED,
            Self::Ultra => EffectSettings::ULTRA,
            Self::Custom => custom,
        }
    }

    /// 图片效果基线；Custom 使用用户逐项调节的值。
    pub fn effects(self, custom: ImageEffects) -> ImageEffects {
        match self {
            Self::Eco => ImageEffects::OFF,
            Self::Balanced => ImageEffects::BALANCED,
            Self::Ultra => ImageEffects::ULTRA,
            Self::Custom => custom,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoRules {
    pub fullscreen: bool,
    pub obscured: bool,
    pub battery: bool,
    pub remote_session: bool,
    /// 前台进程名（不含路径、大小写不敏感）。
    pub apps: Vec<String>,
}

impl Default for AutoRules {
    fn default() -> Self {
        Self {
            fullscreen: true,
            obscured: true,
            battery: true,
            remote_session: true,
            apps: Vec::new(),
        }
    }
}

impl AutoRules {
    pub fn any_enabled(&self) -> bool {
        self.fullscreen
            || self.obscured
            || self.battery
            || self.remote_session
            || !self.apps.is_empty()
    }

    /// 校验并归一化：去掉空项、重复项与路径分隔符，限制数量。
    pub fn normalize(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.apps = self
            .apps
            .iter()
            .map(|app| app.trim().to_string())
            .filter(|app| !app.is_empty() && !app.contains(['\\', '/']))
            .filter(|app| seen.insert(app.to_ascii_lowercase()))
            .take(MAX_AUTO_APPS)
            .collect();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayOverride {
    pub preset: Option<QualityPreset>,
}

/// 持久化的用户画质意图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityProfile {
    pub preset: QualityPreset,
    pub custom: EffectSettings,
    /// 用户逐项调节的图片效果；仅在 Custom 预设下直接生效。
    pub effects: ImageEffects,
    pub auto: AutoRules,
    /// 稳定显示器身份（如 `\\.\DISPLAY2`）到覆盖预设的映射。
    pub displays: BTreeMap<String, DisplayOverride>,
}

impl Default for QualityProfile {
    fn default() -> Self {
        Self {
            preset: QualityPreset::Balanced,
            custom: EffectSettings::BALANCED,
            effects: ImageEffects::BALANCED,
            auto: AutoRules::default(),
            displays: BTreeMap::new(),
        }
    }
}

impl QualityProfile {
    pub fn preset_for(&self, display: Option<&str>) -> QualityPreset {
        display
            .and_then(|display| self.displays.get(display))
            .and_then(|over| over.preset)
            .unwrap_or(self.preset)
    }

    pub fn settings_for(&self, display: Option<&str>) -> (QualityPreset, EffectSettings) {
        let preset = self.preset_for(display);
        (preset, preset.settings(self.custom))
    }

    pub fn normalize(&mut self) {
        self.effects = self.effects.normalized();
        self.auto.normalize();
        self.displays.retain(|key, _| !key.trim().is_empty());
    }
}

/// 自动降档原因；优先级从高到低。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reduction {
    RemoteSession,
    Fullscreen,
    Obscured,
    App(String),
    Battery,
}

impl Reduction {
    fn priority(&self) -> u8 {
        match self {
            Self::RemoteSession => 5,
            Self::Fullscreen => 4,
            Self::Obscured => 3,
            Self::App(_) => 2,
            Self::Battery => 1,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::RemoteSession => "远程桌面会话".into(),
            Self::Fullscreen => "前台全屏应用".into(),
            Self::Obscured => "壁纸被窗口完全遮挡".into(),
            Self::App(name) => format!("前台应用 {name}"),
            Self::Battery => "电池供电/节能模式".into(),
        }
    }
}

/// 平台输入的只读快照；不包含 UI 对象，便于测试。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySignals {
    pub fullscreen: bool,
    pub obscured: bool,
    pub on_battery: bool,
    pub battery_saver: bool,
    pub remote_session: bool,
    pub foreground_app: Option<String>,
}

pub fn desired_reduction(rules: &AutoRules, signals: &PolicySignals) -> Option<Reduction> {
    let mut reductions = Vec::new();
    if rules.remote_session && signals.remote_session {
        reductions.push(Reduction::RemoteSession);
    }
    if rules.fullscreen && signals.fullscreen {
        reductions.push(Reduction::Fullscreen);
    }
    if rules.obscured && signals.obscured {
        reductions.push(Reduction::Obscured);
    }
    if let Some(app) = &signals.foreground_app
        && rules
            .apps
            .iter()
            .any(|rule| rule.eq_ignore_ascii_case(app.trim()))
    {
        reductions.push(Reduction::App(app.clone()));
    }
    if rules.battery && (signals.on_battery || signals.battery_saver) {
        reductions.push(Reduction::Battery);
    }
    reductions.into_iter().max_by_key(Reduction::priority)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackIntent {
    Animate,
    Static,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    pub display: Option<String>,
    pub preset: QualityPreset,
    pub settings: EffectSettings,
    /// 实际生效的图片效果（自动降档与预设裁剪后）。
    pub effects: ImageEffects,
    pub playback: PlaybackIntent,
    /// 当前已生效的自动降档原因。
    pub active_reduction: Option<String>,
    /// 正在等待迟滞/冷却的条件，供状态面板解释“为什么还没降档”。
    pub pending_reduction: Option<String>,
    pub user_paused: bool,
}

impl EffectivePolicy {
    pub fn reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.user_paused {
            reasons.push("用户暂停".into());
        }
        if self.preset == QualityPreset::Eco {
            reasons.push("Eco 预设：视频停在当前帧，图片效果关闭".into());
        }
        if let Some(reduction) = &self.active_reduction {
            reasons.push(format!("自动规则降档：{reduction}"));
        }
        if let Some(pending) = &self.pending_reduction {
            reasons.push(format!("等待迟滞/冷却：{pending}"));
        }
        if !self.effects.any() {
            reasons.push("图片效果未启用".into());
        }
        reasons
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PolicyTiming {
    pub reduction_delay: Duration,
    pub recovery_delay: Duration,
    pub cooldown: Duration,
}

impl Default for PolicyTiming {
    fn default() -> Self {
        Self {
            reduction_delay: DEFAULT_REDUCTION_DELAY,
            recovery_delay: DEFAULT_RECOVERY_DELAY,
            cooldown: DEFAULT_COOLDOWN,
        }
    }
}

#[derive(Default)]
struct KeyState {
    active: Option<Reduction>,
    /// 当前待提交的目标；`Some(None)` 表示等待恢复。
    pending: Option<(Option<Reduction>, Instant)>,
    last_change: Option<Instant>,
}

pub struct PolicyEngine {
    timing: PolicyTiming,
    states: HashMap<String, KeyState>,
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new(PolicyTiming::default())
    }
}

impl PolicyEngine {
    pub fn new(timing: PolicyTiming) -> Self {
        Self {
            timing,
            states: HashMap::new(),
        }
    }

    /// 对单个显示器求值；同一显示器需用相同 key 连续调用。
    pub fn evaluate(
        &mut self,
        profile: &QualityProfile,
        display: Option<&str>,
        user_paused: bool,
        signals: &PolicySignals,
        now: Instant,
    ) -> EffectivePolicy {
        let key = display.unwrap_or("<primary>").to_owned();
        let desired = desired_reduction(&profile.auto, signals);
        let state = self.states.entry(key).or_default();
        update_state(state, desired, now, self.timing);

        let (preset, mut settings) = profile.settings_for(display);
        let mut effects = preset.effects(profile.effects);
        let active = state.active.clone();
        if active.is_some() {
            // 自动降档只关闭“持续消耗型”效果，保留用户预设的其余选择。
            settings.wallpaper_playback = false;
            effects.bloom = 0;
            effects.blur = 0;
            effects.particles = 0;
            if matches!(active, Some(Reduction::RemoteSession)) {
                settings.ui_animations = false;
                settings.shadows = false;
                effects = ImageEffects::OFF;
            }
        }
        let playback = if user_paused {
            PlaybackIntent::Paused
        } else if !settings.wallpaper_playback {
            PlaybackIntent::Static
        } else {
            PlaybackIntent::Animate
        };
        EffectivePolicy {
            display: display.map(str::to_owned),
            preset,
            settings,
            effects,
            playback,
            active_reduction: active.map(|reduction| reduction.label()),
            pending_reduction: state.pending.as_ref().map(|(pending, _)| {
                pending
                    .clone()
                    .map(|reduction| reduction.label())
                    .unwrap_or_else(|| "恢复播放（等待稳定/冷却）".into())
            }),
            user_paused,
        }
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }
}

fn update_state(
    state: &mut KeyState,
    desired: Option<Reduction>,
    now: Instant,
    timing: PolicyTiming,
) {
    if state.active == desired {
        state.pending = None;
        return;
    }
    let delay = if desired.is_some() {
        timing.reduction_delay
    } else {
        timing.recovery_delay
    };
    let cooled = state
        .last_change
        .is_none_or(|changed| now.saturating_duration_since(changed) >= timing.cooldown);
    match &state.pending {
        Some((pending, since)) if *pending == desired => {
            if now.saturating_duration_since(*since) >= delay && cooled {
                state.active = desired;
                state.pending = None;
                state.last_change = Some(now);
            }
        }
        _ if delay.is_zero() && cooled => {
            state.active = desired;
            state.pending = None;
            state.last_change = Some(now);
        }
        _ => state.pending = Some((desired, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals() -> PolicySignals {
        PolicySignals::default()
    }

    #[test]
    fn presets_produce_expected_effect_baselines() {
        let profile = QualityProfile::default();
        let mut engine = PolicyEngine::default();
        let now = Instant::now();

        let balanced = engine.evaluate(&profile, None, false, &signals(), now);
        assert_eq!(balanced.playback, PlaybackIntent::Animate);
        assert!(balanced.settings.ui_animations);
        assert!(balanced.settings.shadows);
        assert_eq!(balanced.effects, ImageEffects::BALANCED);
        assert!(balanced.effects.color > 0);

        let eco_profile = QualityProfile {
            preset: QualityPreset::Eco,
            ..profile.clone()
        };
        let eco = engine.evaluate(&eco_profile, None, false, &signals(), now);
        assert_eq!(eco.playback, PlaybackIntent::Static);
        assert!(!eco.settings.ui_animations);
        assert!(!eco.settings.shadows);
        assert_eq!(eco.effects, ImageEffects::OFF);

        let ultra_profile = QualityProfile {
            preset: QualityPreset::Ultra,
            ..profile.clone()
        };
        let ultra = engine.evaluate(&ultra_profile, None, false, &signals(), now);
        assert_eq!(ultra.playback, PlaybackIntent::Animate);
        assert_eq!(ultra.effects, ImageEffects::ULTRA);
        assert!(ultra.effects.blur > 0 && ultra.effects.bloom > 0);
    }

    #[test]
    fn custom_preset_honors_individual_toggles() {
        let custom_effects = ImageEffects {
            color: 10,
            vignette: 20,
            blur: 30,
            bloom: 40,
            particles: 50,
            gradient: 60,
        };
        let profile = QualityProfile {
            preset: QualityPreset::Custom,
            custom: EffectSettings {
                ui_animations: false,
                shadows: true,
                wallpaper_playback: false,
            },
            effects: custom_effects,
            ..QualityProfile::default()
        };
        let policy =
            PolicyEngine::default().evaluate(&profile, None, false, &signals(), Instant::now());
        assert!(!policy.settings.ui_animations);
        assert!(policy.settings.shadows);
        assert_eq!(policy.effects, custom_effects);
        assert_eq!(policy.playback, PlaybackIntent::Static);
    }

    #[test]
    fn display_override_only_affects_that_display() {
        let mut displays = BTreeMap::new();
        displays.insert(
            r"\\.\DISPLAY2".into(),
            DisplayOverride {
                preset: Some(QualityPreset::Eco),
            },
        );
        let profile = QualityProfile {
            displays,
            ..QualityProfile::default()
        };
        let engine = &mut PolicyEngine::default();
        let now = Instant::now();
        let primary = engine.evaluate(&profile, Some(r"\\.\DISPLAY1"), false, &signals(), now);
        let secondary = engine.evaluate(&profile, Some(r"\\.\DISPLAY2"), false, &signals(), now);
        assert_eq!(primary.preset, QualityPreset::Balanced);
        assert_eq!(secondary.preset, QualityPreset::Eco);
        assert_eq!(secondary.playback, PlaybackIntent::Static);
    }

    #[test]
    fn fullscreen_reduction_waits_for_hysteresis_then_recovers_after_delay() {
        let timing = PolicyTiming {
            reduction_delay: Duration::from_secs(1),
            recovery_delay: Duration::from_secs(30),
            cooldown: Duration::ZERO,
        };
        let mut engine = PolicyEngine::new(timing);
        let profile = QualityProfile::default();
        let start = Instant::now();
        let mut fullscreen = signals();
        fullscreen.fullscreen = true;

        // 条件刚出现：保持原策略并标记等待。
        let before = engine.evaluate(&profile, None, false, &fullscreen, start);
        assert_eq!(before.playback, PlaybackIntent::Animate);
        assert!(before.pending_reduction.is_some());

        // 超过进入迟滞后降档。
        let reduced = engine.evaluate(
            &profile,
            None,
            false,
            &fullscreen,
            start + Duration::from_millis(1_100),
        );
        assert_eq!(reduced.playback, PlaybackIntent::Static);
        assert!(reduced.active_reduction.is_some());

        // 条件刚消失：仍保持降档。
        let still = engine.evaluate(
            &profile,
            None,
            false,
            &signals(),
            start + Duration::from_millis(2_000),
        );
        assert_eq!(still.playback, PlaybackIntent::Static);
        assert!(still.pending_reduction.is_some());

        // 恢复迟滞到期后回到播放。
        let recovered = engine.evaluate(
            &profile,
            None,
            false,
            &signals(),
            start + Duration::from_millis(2_000) + Duration::from_secs(31),
        );
        assert_eq!(recovered.playback, PlaybackIntent::Animate);
        assert!(recovered.active_reduction.is_none());
    }

    #[test]
    fn cooldown_prevents_rapid_reduction_after_recovery() {
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            recovery_delay: Duration::ZERO,
            cooldown: Duration::from_secs(10),
        };
        let mut engine = PolicyEngine::new(timing);
        let profile = QualityProfile::default();
        let start = Instant::now();
        let mut fullscreen = signals();
        fullscreen.fullscreen = true;

        let reduced = engine.evaluate(&profile, None, false, &fullscreen, start);
        assert_eq!(reduced.playback, PlaybackIntent::Static);

        // 冷却期内即使条件消失也不立刻恢复。
        let cooling = engine.evaluate(
            &profile,
            None,
            false,
            &signals(),
            start + Duration::from_secs(1),
        );
        assert_eq!(cooling.playback, PlaybackIntent::Static);
        let recovered = engine.evaluate(
            &profile,
            None,
            false,
            &signals(),
            start + Duration::from_secs(11),
        );
        assert_eq!(recovered.playback, PlaybackIntent::Animate);

        // 恢复后冷却期内再次全屏：不得立刻降档。
        let blocked = engine.evaluate(
            &profile,
            None,
            false,
            &fullscreen,
            start + Duration::from_secs(12),
        );
        assert_eq!(blocked.playback, PlaybackIntent::Animate);
        let allowed = engine.evaluate(
            &profile,
            None,
            false,
            &fullscreen,
            start + Duration::from_secs(22),
        );
        assert_eq!(allowed.playback, PlaybackIntent::Static);
    }

    #[test]
    fn automatic_reduction_never_mutates_user_profile() {
        let profile = QualityProfile::default();
        let snapshot = profile.clone();
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            ..PolicyTiming::default()
        };
        let mut engine = PolicyEngine::new(timing);
        let mut signals = signals();
        signals.fullscreen = true;
        signals.on_battery = true;
        let policy = engine.evaluate(&profile, None, false, &signals, Instant::now());
        assert_eq!(policy.playback, PlaybackIntent::Static);
        assert_eq!(profile, snapshot, "自动策略不得写回用户意图");
    }

    #[test]
    fn disabled_rules_and_missing_signals_do_not_reduce() {
        let auto = AutoRules {
            battery: false,
            ..AutoRules::default()
        };
        let profile = QualityProfile {
            auto,
            ..QualityProfile::default()
        };
        let mut battery = signals();
        battery.on_battery = true;
        battery.battery_saver = true;
        let policy =
            PolicyEngine::default().evaluate(&profile, None, false, &battery, Instant::now());
        assert_eq!(policy.playback, PlaybackIntent::Animate);
    }

    #[test]
    fn foreground_app_rule_is_case_insensitive_and_trimmed() {
        let mut auto = AutoRules {
            apps: vec![" game.exe ".into(), "GAME.EXE".into(), "".into()],
            ..AutoRules::default()
        };
        auto.normalize();
        let profile = QualityProfile {
            auto,
            ..QualityProfile::default()
        };
        assert_eq!(profile.auto.apps, vec!["game.exe".to_string()]);
        let mut foreground = signals();
        foreground.foreground_app = Some("GAME.EXE".into());
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            ..PolicyTiming::default()
        };
        let policy =
            PolicyEngine::new(timing).evaluate(&profile, None, false, &foreground, Instant::now());
        assert_eq!(policy.playback, PlaybackIntent::Static);
        assert!(policy.active_reduction.unwrap().contains("GAME.EXE"));
    }

    #[test]
    fn user_pause_wins_over_animation_policy() {
        let profile = QualityProfile::default();
        let policy =
            PolicyEngine::default().evaluate(&profile, None, true, &signals(), Instant::now());
        assert_eq!(policy.playback, PlaybackIntent::Paused);
    }

    #[test]
    fn remote_session_disables_ui_effects_too() {
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            ..PolicyTiming::default()
        };
        let profile = QualityProfile::default();
        let mut remote = signals();
        remote.remote_session = true;
        let policy =
            PolicyEngine::new(timing).evaluate(&profile, None, false, &remote, Instant::now());
        assert!(!policy.settings.ui_animations);
        assert!(!policy.settings.shadows);
        assert_eq!(policy.effects, ImageEffects::OFF);
        assert_eq!(policy.playback, PlaybackIntent::Static);
    }

    #[test]
    fn auto_reduction_disables_heavy_image_effects_but_keeps_light_ones() {
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            ..PolicyTiming::default()
        };
        let profile = QualityProfile {
            preset: QualityPreset::Ultra,
            ..QualityProfile::default()
        };
        let mut fullscreen = signals();
        fullscreen.fullscreen = true;
        let policy =
            PolicyEngine::new(timing).evaluate(&profile, None, false, &fullscreen, Instant::now());
        assert_eq!(policy.effects.bloom, 0);
        assert_eq!(policy.effects.blur, 0);
        assert_eq!(policy.effects.particles, 0);
        assert!(policy.effects.color > 0, "轻量调色可保留");
    }

    #[test]
    fn higher_priority_condition_replaces_active_reduction() {
        let timing = PolicyTiming {
            reduction_delay: Duration::ZERO,
            recovery_delay: Duration::ZERO,
            cooldown: Duration::ZERO,
        };
        let mut engine = PolicyEngine::new(timing);
        let profile = QualityProfile::default();
        let start = Instant::now();
        let mut battery = signals();
        battery.on_battery = true;
        let first = engine.evaluate(&profile, None, false, &battery, start);
        assert!(first.active_reduction.unwrap().contains("电池"));
        let mut fullscreen = signals();
        fullscreen.fullscreen = true;
        let second = engine.evaluate(
            &profile,
            None,
            false,
            &fullscreen,
            start + Duration::from_secs(1),
        );
        assert!(second.active_reduction.unwrap().contains("全屏"));
    }
}
