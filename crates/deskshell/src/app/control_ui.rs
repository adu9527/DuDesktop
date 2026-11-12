//! UI 线程中的控制命令处理与状态快照。
//!
//! 所有控制命令都在主窗口消息循环内执行，复用托盘/设置相同的状态修改路径；
//! 管道线程只负责收发 JSON，不直接触碰配置、窗口或宿主监督器。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use protocol::ImageEffects;
use serde_json::json;

use crate::{
    config,
    control::{ControlResponse, HostReport, StateReport},
    diagnostics,
    policy::QualityPreset,
};

use super::{AppState, fit_label};

impl AppState {
    pub fn handle_control(&mut self) {
        let Some((id, command)) = self.control.take_request() else {
            return;
        };
        let name = match &command {
            crate::control::ControlCommand::Status => "status",
            crate::control::ControlCommand::Pause => "pause",
            crate::control::ControlCommand::Resume => "resume",
            crate::control::ControlCommand::SetWallpaper { .. } => "set_wallpaper",
            crate::control::ControlCommand::SetPreset { .. } => "set_preset",
            crate::control::ControlCommand::RestoreDesktop => "restore_desktop",
            crate::control::ControlCommand::Quit => "quit",
            crate::control::ControlCommand::ExportAssets { .. } => "export_assets",
            crate::control::ControlCommand::ExportDiagnostics { .. } => "export_diagnostics",
        };
        diagnostics::log("info", "control_command", json!({ "command": name }));
        let response = self.execute_control(command);
        diagnostics::log(
            if response.ok { "info" } else { "warn" },
            "control_response",
            json!({ "command": name, "code": response.code, "message": response.message }),
        );
        self.control.respond(id, response);
    }

    fn execute_control(&mut self, command: crate::control::ControlCommand) -> ControlResponse {
        use crate::control::ControlCommand;
        match command {
            ControlCommand::Status => {
                ControlResponse::ok("DeskShell 运行中", Some(self.control_state()))
            }
            ControlCommand::Pause => {
                self.config.wallpaper.paused = true;
                self.save_config();
                self.evaluate_policy(true);
                ControlResponse::ok("壁纸已暂停", Some(self.control_state()))
            }
            ControlCommand::Resume => {
                self.config.wallpaper.paused = false;
                self.wallpaper_stopped = false;
                self.save_config();
                self.evaluate_policy(true);
                if self.supervisor.is_none() && !self.host_bootstrapping {
                    self.start_wallpaper();
                }
                ControlResponse::ok("壁纸已恢复", Some(self.control_state()))
            }
            ControlCommand::SetPreset { preset } => {
                let Some(preset) = parse_preset(&preset) else {
                    return ControlResponse::rejected(format!("未知预设：{preset}"));
                };
                self.config.quality.preset = preset;
                self.config.quality.normalize();
                self.save_config();
                self.policy_engine.clear();
                self.evaluate_policy(true);
                ControlResponse::ok(
                    format!("画质预设：{}", preset.label()),
                    Some(self.control_state()),
                )
            }
            ControlCommand::SetWallpaper {
                path,
                kind,
                poster,
                desktop,
                display,
                effects,
                fit,
                managed,
            } => self
                .control_set_wallpaper(path, kind, poster, desktop, display, effects, fit, managed),
            ControlCommand::RestoreDesktop => {
                self.wallpaper_stopped = true;
                let result = self
                    .supervisor
                    .as_ref()
                    .map(|supervisor| supervisor.set_instances(Vec::new()));
                self.supervisor_instances.clear();
                match result {
                    Some(Ok(_)) | None => {
                        ControlResponse::ok("已恢复系统桌面", Some(self.control_state()))
                    }
                    Some(Err(error)) => {
                        ControlResponse::rejected(format!("恢复桌面失败：{}", error.message))
                    }
                }
            }
            ControlCommand::Quit => {
                // SAFETY: 只结束本线程消息循环；响应在本函数返回后发送。
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                }
                ControlResponse::ok("DeskShell 正在退出", None)
            }
            ControlCommand::ExportAssets { output } => self.control_export_assets(output),
            ControlCommand::ExportDiagnostics { output } => self.control_export_diagnostics(output),
        }
    }

    // 控制命令字段较多，保持与 JSON 请求一一对应，避免额外 DTO 转换。
    #[allow(clippy::too_many_arguments)]
    fn control_set_wallpaper(
        &mut self,
        path: String,
        kind: Option<String>,
        poster: Option<String>,
        desktop: Option<bool>,
        display: Option<String>,
        effects: Option<ImageEffects>,
        fit: Option<protocol::FitMode>,
        managed: bool,
    ) -> ControlResponse {
        let path = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) => {
                return ControlResponse::rejected(format!("素材不可用：{path}（{error}）"));
            }
        };
        if !path.is_file() {
            return ControlResponse::rejected(format!("素材不是文件：{}", path.display()));
        }
        let kind = match kind.as_deref() {
            Some("image") => "image",
            Some("video") => "video",
            Some(other) => return ControlResponse::rejected(format!("未知素材类型：{other}")),
            None => {
                if is_video_path(&path) {
                    "video"
                } else {
                    "image"
                }
            }
        };
        if poster.is_some() && kind != "video" {
            return ControlResponse::rejected("静态回退图只用于视频素材");
        }
        let original = path.display().to_string();
        let already_managed = is_managed_path(&path);
        let source_path = if managed && !already_managed {
            match config::import_managed_asset(&path) {
                Ok(target) => target,
                Err(error) => return ControlResponse::rejected(error),
            }
        } else {
            path
        };
        let managed_source = managed || already_managed;
        self.config.wallpaper.source = Some(source_path.display().to_string());
        self.config.wallpaper.kind = Some(kind.into());
        self.config.wallpaper.poster = poster;
        self.config.wallpaper.managed = managed_source;
        self.config.wallpaper.original_source = if managed_source { Some(original) } else { None };
        self.config.wallpaper.last_good_source = Some(source_path.display().to_string());
        if let Some(desktop) = desktop {
            self.config.wallpaper.desktop = desktop;
        }
        if let Some(display) = display {
            self.config.wallpaper.display = Some(display);
        }
        if let Some(effects) = effects {
            self.config.quality.effects = effects.normalized();
            self.config.quality.preset = QualityPreset::Custom;
        }
        if let Some(fit) = fit {
            self.config.wallpaper.fit = fit;
        }
        self.config.wallpaper.paused = false;
        self.wallpaper_stopped = false;
        self.save_config();
        self.policy_engine.clear();
        self.evaluate_policy(true);
        if self.supervisor.is_none() && !self.host_bootstrapping {
            self.start_wallpaper();
        }
        ControlResponse::ok(
            format!(
                "壁纸素材：{}",
                self.config.wallpaper.source.as_deref().unwrap_or("")
            ),
            Some(self.control_state()),
        )
    }

    /// 导出配置快照与当前素材；导入时可用 `set-wallpaper --file <asset> --managed`。
    fn control_export_assets(&self, output: String) -> ControlResponse {
        let output = absolute_path(&output);
        if let Err(error) = fs::create_dir_all(&output) {
            return ControlResponse {
                ok: false,
                code: "internal".into(),
                message: format!("创建素材导出目录失败：{error}"),
                state: None,
            };
        }
        let config_file = output.join("config.toml");
        if let Err(error) = fs::copy(&self.config_path, &config_file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return ControlResponse {
                ok: false,
                code: "internal".into(),
                message: format!("导出配置失败：{error}"),
                state: None,
            };
        }
        let mut asset_file = None;
        if let Some(source) = &self.config.wallpaper.source
            && Path::new(source).is_file()
        {
            let name = Path::new(source)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "wallpaper-asset".into());
            let target = output.join(&name);
            if let Err(error) = fs::copy(source, &target) {
                return ControlResponse {
                    ok: false,
                    code: "internal".into(),
                    message: format!("导出素材失败：{error}"),
                    state: None,
                };
            }
            asset_file = Some(name);
        }
        let manifest = json!({
            "version": 1,
            "exported_unix_ms": unix_millis(),
            "source": self.config.wallpaper.source,
            "original_source": self.config.wallpaper.original_source,
            "kind": self.config.wallpaper.kind,
            "desktop": self.config.wallpaper.desktop,
            "display": self.config.wallpaper.display,
            "managed": self.config.wallpaper.managed,
            "effects": self.config.quality.effects,
            "preset": format!("{:?}", self.config.quality.preset),
            "config_file": config_file.file_name().map(|name| name.to_string_lossy().into_owned()),
            "asset_file": asset_file,
            "restore_hint": "用 deskshell set-wallpaper --file <导出目录>/<asset_file> --kind <kind> --managed 重新导入",
        });
        if let Err(error) = serde_json::to_string_pretty(&manifest)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                fs::write(output.join("manifest.json"), text).map_err(|error| error.to_string())
            })
        {
            return ControlResponse {
                ok: false,
                code: "internal".into(),
                message: format!("写入导出清单失败：{error}"),
                state: None,
            };
        }
        ControlResponse::ok(
            format!("素材与配置已导出：{}", output.display()),
            Some(self.control_state()),
        )
    }

    fn control_export_diagnostics(&self, output: String) -> ControlResponse {
        let output = PathBuf::from(output);
        let output = if output.is_relative() {
            std::env::current_dir()
                .map(|directory| directory.join(&output))
                .unwrap_or(output)
        } else {
            output
        };
        let state = self.control_state();
        let bundle = json!({
            "app": state,
            "compat": compat_report(),
            "config": {
                "path": self.config_path.display().to_string(),
                "version": self.config.version,
                "read_only": self.config_read_only,
            },
        });
        match diagnostics::export(&output, bundle) {
            Ok(()) => ControlResponse::ok(
                format!("诊断已导出：{}", output.display()),
                Some(self.control_state()),
            ),
            Err(error) => ControlResponse {
                ok: false,
                code: "internal".into(),
                message: error,
                state: None,
            },
        }
    }

    pub fn control_state(&self) -> StateReport {
        let policy = self.effective_policy.as_ref();
        let playback = policy
            .map(|policy| match policy.playback {
                crate::policy::PlaybackIntent::Animate => "animate",
                crate::policy::PlaybackIntent::Static => "static",
                crate::policy::PlaybackIntent::Paused => "paused",
            })
            .unwrap_or("unknown");
        let effects = policy.map(|policy| policy.effects).unwrap_or_default();
        let mut notices = Vec::new();
        if let Some(status) = &self.island_status {
            notices.push(status.clone());
        }
        for warning in self.file_warnings.iter().take(4) {
            notices.push(warning.clone());
        }
        StateReport {
            app_running: true,
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: protocol::PROTOCOL_VERSION,
            config_version: self.config.version,
            config_path: self.config_path.display().to_string(),
            config_read_only: self.config_read_only,
            wallpaper_source: self.config.wallpaper.source.clone(),
            wallpaper_kind: self.config.wallpaper.kind.clone(),
            wallpaper_managed: self.config.wallpaper.managed,
            wallpaper_original: self.config.wallpaper.original_source.clone(),
            desktop: self.config.wallpaper.desktop,
            display: self.config.wallpaper.display.clone(),
            paused: self.config.wallpaper.paused,
            wallpaper_stopped: self.wallpaper_stopped,
            fit: fit_label(self.config.wallpaper.fit).into(),
            preset: policy
                .map(|policy| policy.preset.label().to_string())
                .unwrap_or_else(|| self.config.quality.preset.label().to_string()),
            playback: playback.into(),
            effects,
            effects_active: effects.labels().into_iter().map(str::to_owned).collect(),
            reasons: policy.map(|policy| policy.reasons()).unwrap_or_default(),
            host: self.host_report(),
            notices,
        }
    }

    fn host_report(&self) -> Option<HostReport> {
        let supervisor = self.supervisor.as_ref()?;
        let status = supervisor.status();
        let snapshot = status.snapshot();
        let generation = match &status {
            crate::host::SupervisorStatus::Connected { generation, .. } => Some(*generation),
            _ => None,
        };
        Some(HostReport {
            status: status.describe(),
            host_pid: status.host_pid(),
            generation,
            applied_config_version: snapshot
                .map(|snapshot| snapshot.applied_config_version)
                .unwrap_or(0),
            mpv_available: snapshot
                .map(|snapshot| snapshot.mpv_available)
                .unwrap_or(false),
            mpv_version: snapshot.and_then(|snapshot| snapshot.mpv_version.clone()),
            mpv_client_api: snapshot.and_then(|snapshot| snapshot.mpv_client_api.clone()),
            instances: snapshot
                .map(|snapshot| snapshot.instances.clone())
                .unwrap_or_default(),
        })
    }
}

fn absolute_path(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_relative() {
        std::env::current_dir()
            .map(|directory| directory.join(&path))
            .unwrap_or(path)
    } else {
        path
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_preset(value: &str) -> Option<QualityPreset> {
    match value {
        "eco" => Some(QualityPreset::Eco),
        "balanced" => Some(QualityPreset::Balanced),
        "ultra" => Some(QualityPreset::Ultra),
        "custom" => Some(QualityPreset::Custom),
        _ => None,
    }
}

fn is_video_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "ts")
    )
}

fn is_managed_path(path: &Path) -> bool {
    config::managed_assets_dir().is_some_and(|directory| {
        path.starts_with(&directory)
            || path
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("\\deskshell\\assets\\")
    })
}

fn compat_report() -> serde_json::Value {
    match render::probe_displays() {
        Ok(topology) => json!({
            "adapters": topology.adapters.iter().map(|adapter| json!({
                "index": adapter.index,
                "name": adapter.name,
                "vendor": format!("{:?}", adapter.vendor),
                "vendor_id": adapter.vendor_id,
                "device_id": adapter.device_id,
                "luid": adapter.luid,
                "dedicated_video_memory": adapter.dedicated_video_memory,
                "shared_system_memory": adapter.shared_system_memory,
                "software": adapter.software,
                "outputs": adapter.outputs.iter().map(|output| json!({
                    "device_name": output.device_name,
                    "attached": output.attached,
                    "width": output.width,
                    "height": output.height,
                    "refresh_hz": output.refresh_hz,
                    "rotation": output.rotation,
                    "dpi_x": output.dpi_x,
                    "dpi_y": output.dpi_y,
                    "scale_percent": output.scale_percent(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
        Err(error) => json!({ "error": error.to_string() }),
    }
}
