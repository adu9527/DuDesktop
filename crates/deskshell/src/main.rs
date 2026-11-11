mod host;

use std::{error::Error, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("DeskShell：{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("doctor") => {
            println!("DeskShell {}", env!("CARGO_PKG_VERSION"));
            println!("Windows x64：{}", shell_win::platform_supported());
            println!("协议版本：{}", protocol::PROTOCOL_VERSION);
            println!("显卡策略：AMD / NVIDIA / Intel 按能力启用；尚未探测 GPU。");
            println!("当前阶段：框架；透明合成、视频与持久 IPC 尚未实现。");
        }
        Some("host-check") => host::check()?,
        Some("window") => {
            #[cfg(windows)]
            shell_win::run_window()?;
            #[cfg(not(windows))]
            return Err("窗口原型仅支持 Windows".into());
        }
        None | Some("--help" | "-h") => {
            println!(
                "DeskShell 开发框架\n  doctor      框架状态\n  host-check  启动宿主并验证握手\n  window      打开原生 Win32 基础窗口"
            );
        }
        Some(other) => return Err(format!("未知命令：{other}").into()),
    }
    Ok(())
}
