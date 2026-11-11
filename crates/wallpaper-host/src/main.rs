use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--handshake") => match serde_json::to_string(&protocol::HostHello::current()) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("宿主握手序列化失败：{error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("当前为宿主框架，仅支持 --handshake；持续 IPC 和播放器将在 M1 接入。");
            ExitCode::FAILURE
        }
    }
}
