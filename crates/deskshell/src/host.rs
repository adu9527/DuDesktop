use std::{
    error::Error,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn check() -> Result<(), Box<dyn Error>> {
    let filename = format!("wallpaper-host{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe()?.with_file_name(filename);
    if !executable.is_file() {
        return Err("未找到宿主，请先执行 cargo build --workspace".into());
    }
    // 仅开发握手：可信宿主输出一条小消息；正式持续通信在 M1 改为有界命名管道。
    let mut child = Command::new(executable)
        .arg("--handshake")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let expected_pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("宿主握手超时，已尝试清理子进程".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!("宿主异常退出：{}", output.status).into());
    }
    let hello: protocol::HostHello = serde_json::from_slice(&output.stdout)?;
    if !hello.is_compatible() || hello.process_id != expected_pid {
        return Err("宿主协议版本或进程身份不匹配".into());
    }
    println!(
        "宿主握手通过：PID={}，协议={}，状态={:?}",
        hello.process_id, hello.protocol_version, hello.state
    );
    Ok(())
}
