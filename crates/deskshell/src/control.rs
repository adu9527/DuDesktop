//! 版本化 CLI 控制通道：单实例应用内的命名管道服务端与命令行客户端。
//!
//! 与壁纸宿主 IPC 分离：该通道只处理用户/自动化控制（状态、暂停/恢复、设置壁纸、
//! 切换预设、恢复桌面、导出诊断），不承载播放数据。响应为 JSON，退出码稳定：
//! 0 成功、2 参数错误、3 应用未运行、4 命令被拒绝、5 超时或传输失败。
//!
//! 服务端由 `deskshell app` 启动；请求转发到 UI 线程处理，保证所有状态修改与
//! 托盘/窗口操作在同一线程。连接为当前用户专用，拒绝远程客户端。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use protocol::{FitMode, ImageEffects, InstanceStatus};
use serde::{Deserialize, Serialize};
use shell_win::{
    event::WaitEvent,
    pipe::{PipeConnection, PipeError, PipeServer},
};

/// 控制通道固定名称；单实例互斥保证同一会话只有一个服务端。
pub const CONTROL_PIPE: &str = r"\\.\pipe\DeskShell.Control";
const MAX_CONTROL_BYTES: usize = 256 * 1024;
const ACCEPT_SLICE: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const UI_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlCommand {
    Status,
    Pause,
    Resume,
    SetWallpaper {
        path: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        poster: Option<String>,
        /// `Some(true)` 附着桌面；`Some(false)` 预览；`None` 保持当前配置。
        #[serde(default)]
        desktop: Option<bool>,
        #[serde(default)]
        display: Option<String>,
        #[serde(default)]
        effects: Option<ImageEffects>,
        /// 缩放方式；`None` 保持当前配置。
        #[serde(default)]
        fit: Option<FitMode>,
        /// 复制到受管理素材目录；false 时引用原文件。
        #[serde(default)]
        managed: bool,
    },
    SetPreset {
        preset: String,
    },
    RestoreDesktop,
    Quit,
    ExportAssets {
        output: String,
    },
    ExportDiagnostics {
        output: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    /// 稳定机器码：`ok` / `rejected` / `invalid` / `not_running` / `timeout` / `internal`。
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateReport>,
}

impl ControlResponse {
    pub fn ok(message: impl Into<String>, state: Option<StateReport>) -> Self {
        Self {
            ok: true,
            code: "ok".into(),
            message: message.into(),
            state,
        }
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            code: "rejected".into(),
            message: message.into(),
            state: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateReport {
    pub app_running: bool,
    pub version: String,
    pub protocol_version: u32,
    pub config_version: u32,
    pub config_path: String,
    pub config_read_only: bool,
    pub wallpaper_source: Option<String>,
    pub wallpaper_kind: Option<String>,
    pub wallpaper_managed: bool,
    pub wallpaper_original: Option<String>,
    pub desktop: bool,
    pub display: Option<String>,
    pub paused: bool,
    pub wallpaper_stopped: bool,
    /// `cover` / `contain` / `stretch` / `tile`。
    pub fit: String,
    pub preset: String,
    /// `animate` / `static` / `paused`。
    pub playback: String,
    pub effects: ImageEffects,
    pub effects_active: Vec<String>,
    pub reasons: Vec<String>,
    pub host: Option<HostReport>,
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReport {
    pub status: String,
    pub host_pid: Option<u32>,
    pub generation: Option<u64>,
    pub applied_config_version: u64,
    pub mpv_available: bool,
    pub mpv_version: Option<String>,
    pub mpv_client_api: Option<String>,
    pub instances: Vec<InstanceStatus>,
}

/// UI 线程与管道线程共享的请求/响应桥。
pub struct ControlBridge {
    request: Mutex<Option<(u64, ControlCommand)>>,
    response: Mutex<Option<(u64, ControlResponse)>>,
    request_event: WaitEvent,
    response_event: WaitEvent,
    stop: AtomicBool,
    next_id: AtomicU64,
}

impl ControlBridge {
    pub fn new() -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            request: Mutex::new(None),
            response: Mutex::new(None),
            request_event: WaitEvent::new()
                .map_err(|error| format!("创建控制请求事件失败：{error}"))?,
            response_event: WaitEvent::new()
                .map_err(|error| format!("创建控制响应事件失败：{error}"))?,
            stop: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }))
    }

    /// 启动控制服务线程；`notify` 是主窗口句柄数值，用于唤醒 UI 线程。
    pub fn start(self: &Arc<Self>, notify: isize) -> Result<JoinHandle<()>, String> {
        let bridge = Arc::clone(self);
        thread::Builder::new()
            .name("deskshell-control".into())
            .spawn(move || control_loop(bridge, notify))
            .map_err(|error| format!("创建控制服务线程失败：{error}"))
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn take_request(&self) -> Option<(u64, ControlCommand)> {
        self.request
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub fn respond(&self, id: u64, response: ControlResponse) {
        *self
            .response
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((id, response));
        self.response_event.set();
    }
}

fn control_loop(bridge: Arc<ControlBridge>, notify: isize) {
    let server = match PipeServer::new(CONTROL_PIPE) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("控制通道：创建服务端失败：{error}");
            return;
        }
    };
    while !bridge.stop.load(Ordering::Acquire) {
        let connection = match server.accept(ACCEPT_SLICE) {
            Ok(connection) => connection,
            Err(PipeError::Timeout) => continue,
            Err(error) => {
                eprintln!("控制通道：等待连接失败：{error}");
                continue;
            }
        };
        let _ = serve_connection(&bridge, &connection, notify);
    }
}

fn serve_connection(
    bridge: &Arc<ControlBridge>,
    connection: &PipeConnection,
    notify: isize,
) -> Result<(), String> {
    let payload = connection
        .read_frame(MAX_CONTROL_BYTES, Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let command: ControlCommand = match serde_json::from_slice(&payload) {
        Ok(command) => command,
        Err(error) => {
            let response = ControlResponse {
                ok: false,
                code: "invalid".into(),
                message: format!("控制请求 JSON 无效：{error}"),
                state: None,
            };
            send_response(connection, &response)?;
            return Ok(());
        }
    };
    let id = bridge.next_id.fetch_add(1, Ordering::Relaxed);
    *bridge
        .request
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some((id, command));
    bridge.request_event.set();
    shell_win::shellui::post_message_raw(notify, crate::app::WM_APP_CONTROL);

    if !bridge.response_event.wait(Some(UI_TIMEOUT)) {
        let response = ControlResponse {
            ok: false,
            code: "timeout".into(),
            message: "应用未在期限内处理控制命令".into(),
            state: None,
        };
        return send_response(connection, &response);
    }
    let response = bridge
        .response
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .filter(|(response_id, _)| *response_id == id)
        .map(|(_, response)| response);
    let response = response.unwrap_or_else(|| ControlResponse {
        ok: false,
        code: "internal".into(),
        message: "控制响应丢失".into(),
        state: None,
    });
    send_response(connection, &response)
}

fn send_response(connection: &PipeConnection, response: &ControlResponse) -> Result<(), String> {
    let payload =
        serde_json::to_vec(response).map_err(|error| format!("编码控制响应失败：{error}"))?;
    connection
        .write_frame(&payload, Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())
}

/// 命令行入口；返回进程退出码。
pub fn run_cli(name: &str, args: &[String]) -> u8 {
    let json = args.iter().any(|argument| argument == "--json");
    let command = match parse_command(name, args) {
        Ok(command) => command,
        Err(message) => {
            print_failure(&message, "invalid", json, 2);
            return 2;
        }
    };
    let payload = match serde_json::to_vec(&command) {
        Ok(payload) => payload,
        Err(error) => {
            print_failure(&format!("编码命令失败：{error}"), "internal", json, 5);
            return 5;
        }
    };
    let connection = match PipeConnection::connect(CONTROL_PIPE, Duration::from_millis(500)) {
        Ok(connection) => connection,
        Err(PipeError::Io { code: 2 | 3, .. }) | Err(PipeError::Timeout) => {
            // 超时也可能是应用正忙；用主窗口存在性区分“未运行”和“暂时不可用”。
            if shell_win::shellui::window_exists(crate::app::MAIN_CLASS) {
                print_failure(
                    "DeskShell 控制通道暂时不可用（应用可能正在处理其他命令）",
                    "timeout",
                    json,
                    5,
                );
                return 5;
            }
            print_failure(
                "DeskShell 未运行；请先启动 `deskshell app`",
                "not_running",
                json,
                3,
            );
            return 3;
        }
        Err(error) => {
            print_failure(&format!("连接控制通道失败：{error}"), "timeout", json, 5);
            return 5;
        }
    };
    if let Err(error) = connection.write_frame(&payload, Some(IO_TIMEOUT)) {
        print_failure(&format!("发送控制命令失败：{error}"), "timeout", json, 5);
        return 5;
    }
    let response_payload = match connection.read_frame(MAX_CONTROL_BYTES, Some(UI_TIMEOUT)) {
        Ok(payload) => payload,
        Err(error) => {
            print_failure(&format!("读取控制响应失败：{error}"), "timeout", json, 5);
            return 5;
        }
    };
    let response: ControlResponse = match serde_json::from_slice(&response_payload) {
        Ok(response) => response,
        Err(error) => {
            print_failure(&format!("控制响应 JSON 无效：{error}"), "internal", json, 5);
            return 5;
        }
    };
    if json {
        match serde_json::to_string_pretty(&response) {
            Ok(text) => println!("{text}"),
            Err(error) => eprintln!("DeskShell：输出 JSON 失败：{error}"),
        }
    } else {
        println!("{}", response.message);
        if let Some(state) = &response.state {
            print_state(state);
        }
    }
    match response.code.as_str() {
        "ok" => 0,
        "rejected" => 4,
        "not_running" => 3,
        "invalid" => 2,
        _ => 5,
    }
}

fn parse_command(name: &str, args: &[String]) -> Result<ControlCommand, String> {
    let mut filtered = args.iter().filter(|argument| *argument != "--json");
    match name {
        "status" => Ok(ControlCommand::Status),
        "pause" => Ok(ControlCommand::Pause),
        "resume" => Ok(ControlCommand::Resume),
        "restore-desktop" => Ok(ControlCommand::RestoreDesktop),
        "quit" => Ok(ControlCommand::Quit),
        "preset" => {
            let preset = filtered.next().ok_or("preset 缺少名称")?.clone();
            validate_preset(&preset)?;
            Ok(ControlCommand::SetPreset { preset })
        }
        "set-wallpaper" => {
            let mut path = None;
            let mut kind = None;
            let mut poster = None;
            let mut desktop = None;
            let mut display = None;
            let mut effects = None;
            let mut fit = None;
            let mut managed = false;
            while let Some(argument) = filtered.next() {
                match argument.as_str() {
                    "--file" => path = Some(next_value(&mut filtered, "--file")?),
                    "--kind" => {
                        let value = next_value(&mut filtered, "--kind")?;
                        if value != "image" && value != "video" {
                            return Err("--kind 只能是 image 或 video".into());
                        }
                        kind = Some(value);
                    }
                    "--poster" => poster = Some(next_value(&mut filtered, "--poster")?),
                    "--desktop" => desktop = Some(true),
                    "--preview" => desktop = Some(false),
                    "--display" => display = Some(next_value(&mut filtered, "--display")?),
                    "--managed" => managed = true,
                    "--reference" => managed = false,
                    "--effects" => {
                        effects = Some(match next_value(&mut filtered, "--effects")?.as_str() {
                            "off" => ImageEffects::OFF,
                            "balanced" => ImageEffects::BALANCED,
                            "ultra" => ImageEffects::ULTRA,
                            other => {
                                return Err(format!(
                                    "--effects 只能是 off、balanced 或 ultra：{other}"
                                ));
                            }
                        });
                    }
                    "--fit" => {
                        let value = next_value(&mut filtered, "--fit")?;
                        fit = Some(FitMode::parse(&value).ok_or_else(|| {
                            format!("--fit 只能是 cover、contain、stretch 或 tile：{value}")
                        })?);
                    }
                    other => return Err(format!("未知 set-wallpaper 参数：{other}")),
                }
            }
            Ok(ControlCommand::SetWallpaper {
                path: path.ok_or("set-wallpaper 缺少 --file")?,
                kind,
                poster,
                desktop,
                display,
                effects,
                fit,
                managed,
            })
        }
        "diagnostics" => {
            let sub = filtered.next().ok_or("diagnostics 缺少 export")?;
            if sub != "export" {
                return Err(format!("未知 diagnostics 子命令：{sub}"));
            }
            let mut output = None;
            while let Some(argument) = filtered.next() {
                match argument.as_str() {
                    "--output" => output = Some(next_value(&mut filtered, "--output")?),
                    other => return Err(format!("未知 diagnostics export 参数：{other}")),
                }
            }
            Ok(ControlCommand::ExportDiagnostics {
                output: output.ok_or("diagnostics export 缺少 --output")?,
            })
        }
        "assets" => {
            let sub = filtered.next().ok_or("assets 缺少 export")?;
            if sub != "export" {
                return Err(format!("未知 assets 子命令：{sub}"));
            }
            let mut output = None;
            while let Some(argument) = filtered.next() {
                match argument.as_str() {
                    "--output" => output = Some(next_value(&mut filtered, "--output")?),
                    other => return Err(format!("未知 assets export 参数：{other}")),
                }
            }
            Ok(ControlCommand::ExportAssets {
                output: output.ok_or("assets export 缺少 --output")?,
            })
        }
        other => Err(format!("未知控制命令：{other}")),
    }
}

fn validate_preset(preset: &str) -> Result<(), String> {
    match preset {
        "eco" | "balanced" | "ultra" | "custom" => Ok(()),
        other => Err(format!(
            "预设只能是 eco、balanced、ultra 或 custom：{other}"
        )),
    }
}

fn next_value<'a>(
    arguments: &mut impl Iterator<Item = &'a String>,
    name: &str,
) -> Result<String, String> {
    arguments
        .next()
        .cloned()
        .ok_or_else(|| format!("{name} 缺少参数"))
}

fn print_failure(message: &str, code: &str, json: bool, exit: u8) -> u8 {
    if json {
        let response = ControlResponse {
            ok: false,
            code: code.into(),
            message: message.into(),
            state: None,
        };
        if let Ok(text) = serde_json::to_string_pretty(&response) {
            println!("{text}");
        }
    } else {
        eprintln!("DeskShell：{message}");
    }
    exit
}

fn print_state(state: &StateReport) {
    println!("  version: {}", state.version);
    println!("  config: v{} {}", state.config_version, state.config_path);
    println!(
        "  wallpaper: {} ({}) desktop={} paused={} stopped={} managed={} original={}",
        state.wallpaper_source.as_deref().unwrap_or("未设置"),
        state.wallpaper_kind.as_deref().unwrap_or("-"),
        state.desktop,
        state.paused,
        state.wallpaper_stopped,
        state.wallpaper_managed,
        state.wallpaper_original.as_deref().unwrap_or("-")
    );
    println!(
        "  preset: {} playback: {} fit: {} effects: {}",
        state.preset,
        state.playback,
        state.fit,
        if state.effects_active.is_empty() {
            "关闭".into()
        } else {
            state.effects_active.join("、")
        }
    );
    if let Some(host) = &state.host {
        println!(
            "  host: {} pid={:?} generation={:?} applied_config={}",
            host.status, host.host_pid, host.generation, host.applied_config_version
        );
        for instance in &host.instances {
            println!(
                "    instance {}: {:?} displayed={:?} path={:?} player={} passes={} presents={} gpu={:?}",
                instance.id,
                instance.state,
                instance.displayed,
                instance.render_path,
                instance.player_active,
                instance.effect_passes,
                instance.present_count,
                instance.gpu_time_us
            );
        }
    } else {
        println!("  host: 未运行");
    }
    for notice in &state.notices {
        println!("  notice: {notice}");
    }
}
