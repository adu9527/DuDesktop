mod app;
mod app_check;
mod app_workers;
mod capsule_cmd;
mod config;
mod control;
mod controls;
mod desktop_cmd;
mod diagnostics;
mod display_probe;
mod host;
mod host_check;
mod music;
mod perf_check;
mod policy;
mod shellstate;
mod wallpaper_cmd;
mod weather;

use std::{error::Error, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("DeskShell：{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, Box<dyn Error>> {
    shell_win::enable_per_monitor_dpi_awareness();
    match std::env::args().nth(1).as_deref() {
        Some("doctor") => {
            println!("DeskShell {}", env!("CARGO_PKG_VERSION"));
            println!("Windows x64：{}", shell_win::platform_supported());
            println!("协议版本：{}", protocol::PROTOCOL_VERSION);
            println!("显卡策略：AMD / NVIDIA / Intel 按能力启用；使用 probe 查看当前适配器。");
            println!(
                "当前阶段：M6 长期使用候选；CLI、诊断、受管理素材、安装回滚与本地候选包已交付，24/72 小时与多厂商矩阵未验收。"
            );
        }
        Some("probe") => display_probe::run()?,
        Some(
            "status" | "pause" | "resume" | "set-wallpaper" | "preset" | "restore-desktop" | "quit"
            | "diagnostics" | "assets",
        ) => {
            let name = std::env::args().nth(1).expect("已匹配命令");
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            return Ok(control::run_cli(&name, &arguments));
        }
        Some("desktop-check") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            desktop_cmd::run(&arguments)?;
        }
        Some("capsule") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            capsule_cmd::run(&arguments)?;
        }
        Some("wallpaper") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            wallpaper_cmd::run(&arguments)?;
        }
        Some("host-check") => host_check::run()?,
        Some("app-check") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            app_check::run(&arguments)?;
        }
        Some("perf-check") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            perf_check::run(&arguments)?;
        }
        Some("autostart") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            run_autostart(&arguments)?;
        }
        Some("app") => {
            let arguments: Vec<String> = std::env::args().skip(2).collect();
            app::run(&arguments)?;
        }
        Some("window") => {
            #[cfg(windows)]
            shell_win::run_window()?;
            #[cfg(not(windows))]
            return Err("窗口原型仅支持 Windows".into());
        }
        None | Some("--help" | "-h") => {
            println!(
                "DeskShell 开发框架
  doctor       框架状态
  probe        枚举适配器、显示输出与 DPI
  capsule      M0 透明胶囊原型（--self-test / --input-test / --recovery-test / --seconds N / --monitor N）
  wallpaper    启动宿主播放图片/视频（--file PATH [--kind image|video] [--poster PATH] [--desktop] [--display ID] [--fit cover|contain|stretch|tile] [--paused] [--effects] [--particles] [--seconds N]）
  host-check   M1/M5 壁纸宿主、播放与图片效果验收
  effects-check M5 图片效果参考截图（scripts/dev.ps1 effects-check）
  interop-check M5 mpv D3D11 视频互操作原型（scripts/dev.ps1 interop-check）
  desktop-check M2 桌面附着、完整边界、失效重附着与图标状态自检
  app-check    M3 应用发现/图标/启动、桌面索引与目录监听验收（--launch 执行真实启动）
  perf-check   M4 产品进程树 Eco 稳态 CPU 验收（--seconds N / --max-cpu PCT / --no-desktop）
  autostart    M4 开机启动状态（--status / --enable / --disable）
  app          M4/M5 主程序：托盘、Dock、灵动岛、画质策略与图片效果（--self-test / --seconds N）
  status       查询运行中应用状态（--json）
  pause/resume 暂停/恢复壁纸（--json）
  set-wallpaper 设置壁纸（--file PATH [--kind image|video] [--poster PATH] [--desktop|--preview] [--display ID] [--fit cover|contain|stretch|tile] [--effects off|balanced|ultra] [--managed|--reference] [--json]）
  preset       切换预设 eco|balanced|ultra|custom（--json）
  restore-desktop 停止壁纸并恢复系统桌面（--json）
  quit         请求运行中应用退出（--json）
  diagnostics  export --output FILE：导出应用/宿主/兼容/日志诊断包
  assets       export --output DIR：导出配置快照与当前素材
  window       打开原生 Win32 基础窗口"
            );
        }
        Some(other) => return Err(format!("未知命令：{other}").into()),
    }
    Ok(0)
}

fn run_autostart(args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str).unwrap_or("--status") {
        "--status" => {
            let enabled = shell_win::autostart::is_enabled()?;
            println!("开机启动：{}", if enabled { "已启用" } else { "未启用" });
        }
        "--enable" => {
            shell_win::autostart::set_enabled(true)?;
            println!("开机启动已启用（HKCU Run）");
        }
        "--disable" => {
            shell_win::autostart::set_enabled(false)?;
            println!("开机启动已关闭");
        }
        other => return Err(format!("未知 autostart 参数：{other}").into()),
    }
    Ok(())
}
