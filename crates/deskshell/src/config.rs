//! 版本化用户配置：迁移备份、原子保存、未知字段保留与损坏/上一版本回滚。
//!
//! 存储位置：`%APPDATA%\DeskShell\config.toml` 为用户配置；缓存与可再生成数据放
//! `%LOCALAPPDATA%\DeskShell\cache`。配置写入前会把旧文件复制为同名 `.prev`，
//! 主文件损坏时优先从 `.prev` 恢复。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::policy::QualityProfile;

/// 配置 schema 版本。
///
/// v3：加入图片效果参数（`quality.effects`），v2 配置按默认值迁移并保留备份。
/// v4：加入壁纸缩放方式（`wallpaper.fit`），v3 配置按 cover 迁移。
/// v5：加入桌面小部件配置（`widgets`）。
/// v6：加入快捷控制；旧配置的小部件迁移为仅快捷控制默认开启。
pub const CONFIG_VERSION: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub dock: DockConfig,
    pub wallpaper: WallpaperConfig,
    pub onboarding: OnboardingConfig,
    pub quality: QualityProfile,
    pub widgets: WidgetsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            dock: DockConfig::default(),
            wallpaper: WallpaperConfig::default(),
            onboarding: OnboardingConfig::default(),
            quality: QualityProfile::default(),
            widgets: WidgetsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    /// 固定项的应用稳定身份；与应用库分离。
    pub pinned: Vec<String>,
    /// 鱼眼/悬停放大动画；关闭后静态停帧。
    pub fisheye: bool,
    pub manual_apps: Vec<ManualAppEntry>,
    /// Dock 最大条目数（含固定、运行中与系统入口）。
    pub max_items: usize,
    /// 图标样式。
    pub style: DockStyle,
    /// 是否自动追加运行中应用。
    pub show_running: bool,
    /// 运行中应用最多占用几个位置。
    pub running_limit: usize,
    /// 是否显示应用库/资源管理器/回收站入口。
    pub show_system: bool,
}

/// Dock 图标样式；默认使用参考图的大图标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockStyle {
    #[default]
    Large,
    Standard,
    Labels,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            pinned: Vec::new(),
            fisheye: true,
            manual_apps: Vec::new(),
            max_items: 10,
            style: DockStyle::Large,
            show_running: true,
            running_limit: 6,
            show_system: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualAppEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WallpaperConfig {
    /// 素材路径；空表示不启动壁纸宿主。
    pub source: Option<String>,
    /// `image` 或 `video`；空表示按扩展名判断。
    pub kind: Option<String>,
    pub poster: Option<String>,
    /// 是否附着桌面；false 时使用预览窗口，仅用于开发。
    pub desktop: bool,
    /// 稳定显示器身份（如 `\\.\DISPLAY2`）；空表示主显示器。
    pub display: Option<String>,
    pub paused: bool,
    /// `source` 是否位于受管理素材目录（由本程序复制/维护）。
    pub managed: bool,
    /// 导入受管理目录前的原始路径；用于用户切回引用模式或人工恢复。
    pub original_source: Option<String>,
    /// 最近一次成功应用的素材路径；当前素材缺失时用于回退。
    pub last_good_source: Option<String>,
    /// 壁纸缩放方式；默认铺满裁切，避免上下黑边。
    pub fit: protocol::FitMode,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            source: None,
            kind: None,
            poster: None,
            // 日常运行时默认附着桌面；预览窗口只用于显式开发命令。
            desktop: true,
            display: None,
            paused: false,
            managed: false,
            original_source: None,
            last_good_source: None,
            fit: protocol::FitMode::Cover,
        }
    }
}

/// 桌面小部件配置；每个小部件独立显隐、位置与宽度。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetsConfig {
    pub organizer: OrganizerWidgetConfig,
    pub clock: WidgetConfig,
    pub weather: WeatherWidgetConfig,
    pub music: WidgetConfig,
    pub controls: WidgetConfig,
}

impl Default for WidgetsConfig {
    fn default() -> Self {
        Self {
            organizer: OrganizerWidgetConfig {
                base: WidgetConfig {
                    enabled: false,
                    x: 24,
                    y: 80,
                    width: 340.0,
                },
                columns: 3,
                items: Vec::new(),
            },
            clock: WidgetConfig {
                enabled: false,
                x: 40,
                y: 240,
                width: 300.0,
            },
            weather: WeatherWidgetConfig {
                base: WidgetConfig {
                    enabled: false,
                    x: 1860,
                    y: 60,
                    width: 300.0,
                },
                city: "上海".into(),
            },
            music: WidgetConfig {
                enabled: false,
                x: 40,
                y: 900,
                width: 340.0,
            },
            controls: WidgetConfig {
                enabled: true,
                x: 40,
                y: 420,
                width: 360.0,
            },
        }
    }
}

/// 通用小部件位置：物理屏幕坐标，单位为像素。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetConfig {
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    /// 宽度（DIP），实际像素按窗口 DPI 缩放。
    pub width: f32,
}

impl Default for WidgetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            x: 40,
            y: 240,
            width: 320.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WeatherWidgetConfig {
    #[serde(flatten)]
    pub base: WidgetConfig,
    pub city: String,
}

impl Default for WeatherWidgetConfig {
    fn default() -> Self {
        Self {
            base: WidgetConfig::default(),
            city: "上海".into(),
        }
    }
}

/// 桌面收纳：应用/文件夹/路径快捷方式网格。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrganizerWidgetConfig {
    #[serde(flatten)]
    pub base: WidgetConfig,
    /// 每行列数（2..=6）。
    pub columns: usize,
    /// `app:<id>`、`path:<绝对路径>` 或 `folder:<绝对路径>`。
    pub items: Vec<String>,
}

impl Default for OrganizerWidgetConfig {
    fn default() -> Self {
        Self {
            base: WidgetConfig::default(),
            columns: 3,
            items: Vec::new(),
        }
    }
}

/// 受管理素材目录：`%LOCALAPPDATA%\DeskShell\assets`。
pub fn managed_assets_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("DeskShell").join("assets"))
}

/// 把素材复制到受管理目录，返回新路径。文件名使用时间戳 + 原文件名，避免覆盖。
pub fn import_managed_asset(source: &Path) -> Result<PathBuf, String> {
    let directory = managed_assets_dir().ok_or("无法解析 LOCALAPPDATA")?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建素材目录 {} 失败：{error}", directory.display()))?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wallpaper".into());
    let stamp = unix_millis();
    let target = directory.join(format!("{stamp}-{}-{name}", std::process::id()));
    fs::copy(source, &target).map_err(|error| {
        format!(
            "复制素材到受管理目录失败（{} -> {}）：{error}",
            source.display(),
            target.display()
        )
    })?;
    Ok(target)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingConfig {
    /// 是否已经询问过隐藏桌面图标；只询问一次。
    pub desktop_icons_asked: bool,
    /// 本程序是否修改过桌面图标可见性，用于退出时只恢复自己的修改。
    pub desktop_icons_hidden_by_us: bool,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
    /// 配置版本高于当前程序时禁止保存，避免覆盖用户新版本配置。
    pub read_only: bool,
    pub path: PathBuf,
    /// 原始根表，保存时用于保留本程序不认识的字段。
    pub raw: Option<toml::Value>,
    pub migrated_from: Option<u32>,
}

pub fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("DeskShell").join("config.toml"))
}

pub fn previous_path(path: &Path) -> PathBuf {
    path.with_extension("toml.prev")
}

/// 读取配置；损坏文件先尝试 `.prev`，再改名保留并从默认配置继续。
pub fn load() -> Result<LoadedConfig, String> {
    let path = config_path().ok_or("无法读取 APPDATA 环境变量")?;
    load_from(&path)
}

pub fn load_from(path: &Path) -> Result<LoadedConfig, String> {
    let mut warnings = Vec::new();
    let mut migrated_from = None;
    let mut read_only = false;

    let mut text = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("读取配置失败 {}：{error}", path.display())),
    };
    let mut value = match text.as_deref().map(toml::from_str::<toml::Value>) {
        None => None,
        Some(Ok(value)) => Some(value),
        Some(Err(error)) => {
            let fallback = recover_previous(path, &mut warnings);
            match fallback {
                Some((previous_text, previous_value)) => {
                    warnings.push(format!("配置损坏，已从上一版本恢复：{error}"));
                    text = Some(previous_text);
                    Some(previous_value)
                }
                None => {
                    warnings.push(format!("配置损坏且无可用备份，本次使用默认配置：{error}"));
                    None
                }
            }
        }
    };

    if let Some(value) = value.take() {
        let version = value
            .get("version")
            .and_then(toml::Value::as_integer)
            .unwrap_or(1)
            .max(0) as u32;
        if version > CONFIG_VERSION {
            read_only = true;
            warnings.push(format!(
                "配置版本 {} 高于当前程序支持的 {}；将只读使用，不做覆盖保存",
                version, CONFIG_VERSION
            ));
        } else if version < CONFIG_VERSION {
            let backed_up = text
                .as_ref()
                .map(|text| backup_before_migration(path, version, text, &mut warnings))
                .unwrap_or(false);
            migrated_from = Some(version);
            warnings.push(format!(
                "配置已从版本 {} 迁移到 {}{}",
                version,
                CONFIG_VERSION,
                if backed_up {
                    "；迁移前备份已保留"
                } else {
                    "；迁移前备份未写入，见上一条警告"
                }
            ));
        }
        match value.clone().try_into::<Config>() {
            Ok(mut config) => {
                config.version = if read_only { version } else { CONFIG_VERSION };
                if migrated_from.is_some_and(|version| version < 6) {
                    // v6：默认只保留快捷控制；其余小部件保留配置但关闭。
                    config.widgets.clock.enabled = false;
                    config.widgets.weather.base.enabled = false;
                    config.widgets.music.enabled = false;
                    config.widgets.organizer.base.enabled = false;
                    config.widgets.controls.enabled = true;
                }
                config.quality.normalize();
                return Ok(LoadedConfig {
                    config,
                    warnings,
                    read_only,
                    path: path.to_path_buf(),
                    raw: Some(value),
                    migrated_from,
                });
            }
            Err(error) => {
                warnings.push(format!("配置字段无法解析：{error}"));
                if let Some((mut previous, previous_raw)) =
                    load_previous_config(path, &mut warnings)
                {
                    previous.version = CONFIG_VERSION;
                    previous.quality.normalize();
                    warnings.push("已从上一配置 .prev 恢复".into());
                    return Ok(LoadedConfig {
                        config: previous,
                        warnings,
                        read_only,
                        path: path.to_path_buf(),
                        raw: Some(previous_raw),
                        migrated_from,
                    });
                }
                warnings.push("没有可用的上一配置，本次使用默认配置".into());
            }
        }
    }

    Ok(LoadedConfig {
        config: Config::default(),
        warnings,
        read_only,
        path: path.to_path_buf(),
        raw: None,
        migrated_from,
    })
}

fn load_previous_config(path: &Path, warnings: &mut Vec<String>) -> Option<(Config, toml::Value)> {
    let previous = previous_path(path);
    let text = fs::read_to_string(&previous).ok()?;
    let value = toml::from_str::<toml::Value>(&text).ok()?;
    let config = value.clone().try_into::<Config>().ok()?;
    if let Some(backup) = rename_corrupt(path, warnings) {
        warnings.push(format!("损坏配置已保留为 {}", backup.display()));
    }
    Some((config, value))
}

fn recover_previous(path: &Path, warnings: &mut Vec<String>) -> Option<(String, toml::Value)> {
    let previous = previous_path(path);
    let text = fs::read_to_string(&previous).ok()?;
    let value = toml::from_str::<toml::Value>(&text).ok()?;
    // 损坏主文件改名保留，便于事后诊断；失败不阻止从备份恢复。
    if let Some(backup) = rename_corrupt(path, warnings) {
        warnings.push(format!("损坏配置已保留为 {}", backup.display()));
    }
    Some((text, value))
}

fn rename_corrupt(path: &Path, warnings: &mut Vec<String>) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let backup = corrupt_path(path);
    match fs::rename(path, &backup) {
        Ok(()) => Some(backup),
        Err(error) => {
            warnings.push(format!("配置损坏且无法备份（{error}），继续使用回退配置"));
            None
        }
    }
}

fn backup_before_migration(
    path: &Path,
    version: u32,
    text: &str,
    warnings: &mut Vec<String>,
) -> bool {
    let backup = path.with_file_name(format!(
        "{}.v{}-{}.bak",
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".into()),
        version,
        unix_seconds()
    ));
    match fs::write(&backup, text) {
        Ok(()) => true,
        Err(error) => {
            warnings.push(format!(
                "迁移前备份失败（{error}），本次仍继续迁移；请先手动备份 {}",
                path.display()
            ));
            false
        }
    }
}

/// 保存配置：备份旧文件为 `.prev`，合并未知字段，再原子替换主文件。
pub fn save(config: &Config, path: &Path) -> Result<(), String> {
    save_with_raw(config, path, None)
}

pub fn save_with_raw(
    config: &Config,
    path: &Path,
    raw: Option<&toml::Value>,
) -> Result<(), String> {
    if config.version > CONFIG_VERSION {
        return Err(format!("拒绝保存高于当前程序的配置版本 {}", config.version));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建配置目录 {} 失败：{error}", parent.display()))?;
    }
    let known =
        toml::Value::try_from(config).map_err(|error| format!("序列化配置失败：{error}"))?;
    let merged = merge_preserving_unknown(known, raw);
    let text = toml::to_string_pretty(&merged).map_err(|error| format!("输出配置失败：{error}"))?;

    if path.is_file() {
        let previous = previous_path(path);
        fs::copy(path, &previous)
            .map_err(|error| format!("保存前备份上一配置失败 {}：{error}", previous.display()))?;
    }

    let temporary = path.with_extension("toml.tmp");
    {
        let mut file = fs::File::create(&temporary)
            .map_err(|error| format!("创建临时配置失败 {}：{error}", temporary.display()))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("写入临时配置失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("刷新临时配置失败：{error}"))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("替换配置失败 {}：{error}", path.display()))?;
    Ok(())
}

/// 已知字段覆盖旧值，未知字段原样保留；表类型递归合并。
fn merge_preserving_unknown(known: toml::Value, raw: Option<&toml::Value>) -> toml::Value {
    let mut known_table = match known {
        toml::Value::Table(table) => table,
        other => return other,
    };
    let Some(toml::Value::Table(raw_table)) = raw else {
        return toml::Value::Table(known_table);
    };
    for (key, raw_value) in raw_table {
        match known_table.get_mut(key) {
            Some(known_value) if known_value.is_table() && raw_value.is_table() => {
                *known_value = merge_preserving_unknown(known_value.clone(), Some(raw_value));
            }
            Some(_) => {}
            None => {
                known_table.insert(key.clone(), raw_value.clone());
            }
        }
    }
    toml::Value::Table(known_table)
}

fn corrupt_path(path: &Path) -> PathBuf {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    path.with_file_name(format!("{name}.corrupt-{}.toml", unix_seconds()))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::QualityPreset;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir()
            .join(format!(
                "deskshell-config-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ))
            .join("config.toml")
    }

    #[test]
    fn round_trip_preserves_dock_wallpaper_and_quality() {
        let path = temp_path("roundtrip");
        let mut config = Config::default();
        config.dock.pinned.push("apps:test!App".into());
        config.dock.fisheye = false;
        config.wallpaper.source = Some(r"C:\wallpapers\a.png".into());
        config.wallpaper.desktop = true;
        config.onboarding.desktop_icons_asked = true;
        config.quality.preset = QualityPreset::Ultra;
        config.quality.auto.battery = false;
        config.quality.effects.color = 42;
        config.quality.effects.particles = 0;
        save(&config, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(!loaded.read_only);
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert_eq!(loaded.config.dock.pinned, config.dock.pinned);
        assert!(!loaded.config.dock.fisheye);
        assert_eq!(loaded.config.wallpaper.source, config.wallpaper.source);
        assert!(loaded.config.onboarding.desktop_icons_asked);
        assert_eq!(loaded.config.quality.preset, QualityPreset::Ultra);
        assert!(!loaded.config.quality.auto.battery);
        assert_eq!(loaded.config.quality.effects.color, 42);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_file_recovers_from_previous_and_keeps_evidence() {
        let path = temp_path("corrupt");
        let dir = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "version = 2\n[quality]\npreset = \"eco\"\n").unwrap();
        fs::write(
            previous_path(&path),
            "version = 2\n[wallpaper]\nsource = \"prev.png\"\n",
        )
        .unwrap();
        fs::write(&path, "this is not toml = [").unwrap();
        let loaded = load_from(&path).unwrap();
        assert!(!loaded.warnings.is_empty());
        assert_eq!(
            loaded.config.wallpaper.source.as_deref(),
            Some("prev.png"),
            "应从 .prev 恢复"
        );
        assert!(!path.exists(), "损坏主文件应改名");
        let corrupt = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
            .count();
        assert_eq!(corrupt, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn semantic_error_recovers_previous_config() {
        let path = temp_path("semantic");
        let dir = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            previous_path(&path),
            "version = 2\n[wallpaper]\nsource = \"prev.png\"\n",
        )
        .unwrap();
        // 合法 TOML，但枚举值无法解析，应视为配置损坏并回滚。
        fs::write(&path, "version = 2\n[quality]\npreset = \"turbo\"\n").unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.config.wallpaper.source.as_deref(), Some("prev.png"));
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains(".prev"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_without_backup_uses_defaults() {
        let path = temp_path("corrupt-default");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not toml = [").unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert!(loaded.config.wallpaper.source.is_none());
        assert!(!loaded.warnings.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn v1_config_is_migrated_with_backup() {
        let path = temp_path("migrate");
        let dir = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"version = 1
[dock]
pinned = ["apps:a"]
[dock.menu]
custom = true
"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.migrated_from, Some(1));
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert_eq!(loaded.config.quality, QualityProfile::default());
        let backups: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".v1-"))
            .collect();
        assert_eq!(backups.len(), 1, "迁移前必须保留备份");

        save_with_raw(&loaded.config, &path, loaded.raw.as_ref()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("version = {CONFIG_VERSION}")));
        assert!(text.contains("custom = true"), "未知字段必须保留：{text}");
        let reloaded = load_from(&path).unwrap();
        assert_eq!(reloaded.config.dock.pinned, vec!["apps:a".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_keeps_previous_and_unknown_fields_recursively() {
        let path = temp_path("unknown");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"version = 2
[future]
enabled = true
[quality]
preset = "balanced"
future_quality_key = 7
"#,
        )
        .unwrap();
        let mut loaded = load_from(&path).unwrap();
        loaded.config.dock.fisheye = false;
        save_with_raw(&loaded.config, &path, loaded.raw.as_ref()).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[future]"));
        assert!(text.contains("enabled = true"));
        assert!(text.contains("future_quality_key = 7"));
        assert!(text.contains("fisheye = false"));
        assert!(previous_path(&path).is_file(), "保存前必须保留上一配置");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn newer_version_is_read_only_and_not_overwritten() {
        let path = temp_path("newer");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "version = 99\n").unwrap();
        let loaded = load_from(&path).unwrap();
        assert!(loaded.read_only);
        assert!(save(&loaded.config, &path).is_err());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("99"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn v2_config_migrates_effects_defaults_with_backup() {
        let path = temp_path("migrate-v2");
        let dir = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"version = 2
[quality]
preset = "balanced"
"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.migrated_from, Some(2));
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert_eq!(
            loaded.config.quality.effects,
            protocol::ImageEffects::BALANCED
        );
        let backups: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".v2-"))
            .collect();
        assert_eq!(backups.len(), 1, "迁移前必须保留 v2 备份");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = temp_path("missing");
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert!(loaded.warnings.is_empty());
    }
}
