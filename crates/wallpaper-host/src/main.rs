mod effects_check;
mod host;
mod instance;
mod interop;
mod mpv;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wallpaper-host：{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    // 独立效果自检模式：不连接主进程管道，不修改桌面。
    if std::env::args().nth(1).as_deref() == Some("--effects-check") {
        return run_effects_check();
    }
    // M5 视频互操作独立原型：不接入日常播放路径。
    if std::env::args().nth(1).as_deref() == Some("--interop-check") {
        return run_interop_check();
    }

    let mut pipe_name = None;
    let mut parent_pid = None;
    let mut generation = None;
    let mut fault = None;
    let mut fault_generation = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--pipe" => pipe_name = Some(next_value(&mut arguments, "--pipe")?),
            "--parent-pid" => {
                parent_pid = Some(
                    next_value(&mut arguments, "--parent-pid")?
                        .parse()
                        .map_err(|_| "--parent-pid 需要数字".to_string())?,
                );
            }
            "--generation" => {
                generation = Some(
                    next_value(&mut arguments, "--generation")?
                        .parse()
                        .map_err(|_| "--generation 需要数字".to_string())?,
                );
            }
            "--fault" => {
                let value = next_value(&mut arguments, "--fault")?;
                fault = Some(
                    host::FaultMode::parse(&value)
                        .ok_or_else(|| format!("未知故障注入模式：{value}"))?,
                );
            }
            "--fault-generation" => {
                fault_generation = Some(
                    next_value(&mut arguments, "--fault-generation")?
                        .parse()
                        .map_err(|_| "--fault-generation 需要数字".to_string())?,
                );
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }

    let options = host::HostOptions {
        pipe_name: pipe_name.ok_or("缺少 --pipe")?,
        parent_pid: parent_pid.ok_or("缺少 --parent-pid")?,
        generation: generation.ok_or("缺少 --generation")?,
        fault,
        fault_generation,
    };
    host::run(options)
}

fn next_value(arguments: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    arguments.next().ok_or_else(|| format!("{name} 缺少参数"))
}

fn run_interop_check() -> Result<(), String> {
    let mut file = None;
    let mut output = None;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut seconds = 3u64;
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--file" => file = Some(next_value(&mut arguments, "--file")?),
            "--output" => output = Some(next_value(&mut arguments, "--output")?),
            "--width" => {
                width = next_value(&mut arguments, "--width")?
                    .parse()
                    .map_err(|_| "--width 需要数字".to_string())?
            }
            "--height" => {
                height = next_value(&mut arguments, "--height")?
                    .parse()
                    .map_err(|_| "--height 需要数字".to_string())?
            }
            "--seconds" => {
                seconds = next_value(&mut arguments, "--seconds")?
                    .parse()
                    .map_err(|_| "--seconds 需要数字".to_string())?
            }
            other => return Err(format!("未知 interop-check 参数：{other}")),
        }
    }
    interop::run(interop::Options {
        file: file.ok_or("缺少 --file")?,
        output: output.ok_or("缺少 --output")?,
        width,
        height,
        seconds,
    })
}

fn run_effects_check() -> Result<(), String> {
    let mut file = None;
    let mut output = None;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut particles = false;
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--file" => file = Some(next_value(&mut arguments, "--file")?),
            "--output" => output = Some(next_value(&mut arguments, "--output")?),
            "--width" => {
                width = next_value(&mut arguments, "--width")?
                    .parse()
                    .map_err(|_| "--width 需要数字".to_string())?
            }
            "--height" => {
                height = next_value(&mut arguments, "--height")?
                    .parse()
                    .map_err(|_| "--height 需要数字".to_string())?
            }
            "--particles" => particles = true,
            other => return Err(format!("未知 effects-check 参数：{other}")),
        }
    }
    effects_check::run(effects_check::Options {
        file: file.ok_or("缺少 --file")?,
        output: output.ok_or("缺少 --output")?,
        width,
        height,
        particles,
    })
}
