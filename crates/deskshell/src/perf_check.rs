//! M4 性能验收：以固定条件运行产品进程树，测量 Eco 静态稳态的整机归一 CPU。
//!
//! 测量口径：
//! - CPU 执行时间来自 `GetProcessTimes`（kernel+user，100 ns）；
//! - 墙钟来自 `Instant`；
//! - 整机归一 CPU = CPU 执行时间 / 墙钟 / 逻辑处理器数；
//! - 单核百分比 = CPU 执行时间 / 墙钟；
//! - GPU 时间与 DWM 增量分开记录；GPU 时间当前未采集，不伪造。
//!
//! 被测对象是 `deskshell app` 及其壁纸宿主子进程，不是单独宿主。
//! 短测会写入报告并标注“不能替代 5 分钟验收”。

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{config, policy::QualityPreset};

const INSTANCE_NAME: &str = r"Local\DeskShell.Instance";
const WARMUP: Duration = Duration::from_secs(5);

struct Options {
    seconds: u64,
    warmup_seconds: u64,
    max_normalized_cpu: f64,
    image: PathBuf,
    desktop: bool,
    report: Option<PathBuf>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = parse(args)?;
    if !options.image.is_file() {
        return Err(format!(
            "缺少性能测试图片 {}；请执行 scripts/fetch-test-media.ps1",
            options.image.display()
        ));
    }
    if shell_win::shellui::instance_running(INSTANCE_NAME) {
        return Err("已有 DeskShell 实例在运行；性能验收需要独立运行，请先退出托盘中的实例".into());
    }

    let workdir = std::env::temp_dir().join(format!("deskshell-perf-{}", std::process::id()));
    fs::create_dir_all(&workdir).map_err(|error| format!("创建性能测试目录失败：{error}"))?;
    let config_path = workdir.join("config.toml");
    let mut test_config = config::Config::default();
    test_config.wallpaper.source = Some(options.image.display().to_string());
    test_config.wallpaper.kind = Some("image".into());
    test_config.wallpaper.desktop = options.desktop;
    test_config.quality.preset = QualityPreset::Eco;
    test_config.dock.fisheye = false;
    // 性能测试不弹首次引导，也不允许修改桌面图标。
    test_config.onboarding.desktop_icons_asked = true;
    test_config.onboarding.desktop_icons_hidden_by_us = false;
    config::save(&test_config, &config_path)?;

    println!("M4 perf-check");
    println!(
        "  模式：{}",
        if options.desktop {
            "桌面附着"
        } else {
            "预览窗口（冒烟）"
        }
    );
    println!("  素材：{}", options.image.display());
    println!(
        "  时长：{} 秒，预热 {} 秒",
        options.seconds, options.warmup_seconds
    );

    // 被测进程的运行时间必须覆盖预热、测量和清理；测量结束后由本工具显式终止。
    let runtime = options.seconds + options.warmup_seconds + 60;
    let mut app = spawn_app(&config_path, runtime)?;
    let outcome = measure(&mut app, &options);
    let orphans = cleanup(&mut app, &workdir);
    let (mut report, passed) = outcome?;
    report.orphan_hosts = Some(orphans);
    write_report(&report, options.report.as_deref())?;
    if !passed {
        return Err(format!(
            "整机归一 CPU {:.4}% 超过阈值 {:.2}%",
            report.normalized_percent, options.max_normalized_cpu
        ));
    }
    println!("[PASS] Eco 稳态整机归一 CPU 低于阈值");
    Ok(())
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        seconds: 300,
        warmup_seconds: WARMUP.as_secs(),
        max_normalized_cpu: 0.5,
        image: PathBuf::from("testdata").join("4k-pattern.png"),
        desktop: true,
        report: None,
    };
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--seconds" => {
                options.seconds = arguments
                    .next()
                    .ok_or("--seconds 缺少参数")?
                    .parse()
                    .map_err(|_| "--seconds 需要整数")?;
            }
            "--warmup" => {
                options.warmup_seconds = arguments
                    .next()
                    .ok_or("--warmup 缺少参数")?
                    .parse()
                    .map_err(|_| "--warmup 需要整数秒")?;
            }
            "--max-cpu" => {
                options.max_normalized_cpu = arguments
                    .next()
                    .ok_or("--max-cpu 缺少参数")?
                    .parse()
                    .map_err(|_| "--max-cpu 需要百分比数值")?;
            }
            "--image" => {
                options.image = PathBuf::from(arguments.next().ok_or("--image 缺少参数")?);
            }
            "--no-desktop" => options.desktop = false,
            "--report" => {
                options.report = Some(PathBuf::from(arguments.next().ok_or("--report 缺少参数")?));
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    if options.seconds < 5 {
        return Err("--seconds 至少为 5；5 分钟验收使用默认值".into());
    }
    Ok(options)
}

fn spawn_app(config_path: &Path, seconds: u64) -> Result<Child, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("获取当前程序路径失败：{error}"))?;
    Command::new(executable)
        .arg("app")
        .arg("--seconds")
        .arg(seconds.to_string())
        .arg("--config")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("启动被测进程失败：{error}"))
}

fn measure(app: &mut Child, options: &Options) -> Result<(PerfReport, bool), String> {
    let app_pid = app.id();
    let host_pid = wait_for_host(app_pid, Duration::from_secs(30))?;
    println!("[PASS] 产品进程树已启动：app {app_pid}，宿主 {host_pid}");

    std::thread::sleep(Duration::from_secs(options.warmup_seconds));
    if !shell_win::process::process_alive(app_pid) {
        return Err("预热期间被测进程退出".into());
    }

    let tree_before = tree_cpu(app_pid)?;
    let dwm_pid = shell_win::process::find_processes_by_name("dwm.exe")
        .first()
        .map(|entry| entry.pid);
    let dwm_before = dwm_pid.and_then(shell_win::process::process_cpu_times);
    let wall_start = Instant::now();
    std::thread::sleep(Duration::from_secs(options.seconds));
    let wall = wall_start.elapsed();
    let tree_after = tree_cpu(app_pid)?;
    let dwm_after = dwm_pid.and_then(shell_win::process::process_cpu_times);

    if !shell_win::process::process_alive(app_pid) {
        return Err("测量期间被测进程崩溃或提前退出".into());
    }

    let cpu_100ns = tree_after
        .total_100ns
        .saturating_sub(tree_before.total_100ns);
    let wall_100ns = wall.as_nanos() as f64 / 100.0;
    let single_core = cpu_100ns as f64 / wall_100ns.max(1.0) * 100.0;
    let logical = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let normalized = single_core / logical as f64;
    let mut processes = Vec::new();
    for (pid, cpu) in &tree_after.per_process {
        let previous = tree_before.per_process.get(pid).copied().unwrap_or(0);
        let delta = cpu.saturating_sub(previous);
        if delta > 0 {
            processes.push(ProcessUsage {
                pid: *pid,
                cpu_100ns: delta,
            });
        }
    }
    processes.sort_by_key(|process| std::cmp::Reverse(process.cpu_100ns));
    let dwm_delta = match (dwm_before, dwm_after) {
        (Some(before), Some(after)) => {
            Some(after.total_100ns().saturating_sub(before.total_100ns()))
        }
        _ => None,
    };
    let dwm_normalized =
        dwm_delta.map(|delta| delta as f64 / wall_100ns.max(1.0) * 100.0 / logical as f64);

    let mut notes = Vec::new();
    if options.seconds < 300 {
        notes.push("短测：不能替代 5 分钟 M4 验收".to_string());
    }
    if !options.desktop {
        notes.push("预览窗口模式：不验证桌面附着开销".to_string());
    }
    if dwm_delta.is_none() {
        notes.push("DWM CPU 时间不可读（受保护进程或权限不足）；测量环境需单独记录该限制".into());
    } else {
        notes.push("DWM 时间增量单独记录；GPU 时间未采集".into());
    }
    notes.push(
        "测量结束后由验收工具终止被测进程；正常退出路径由 host-check 与安装卸载验收覆盖".into(),
    );
    let passed = normalized <= options.max_normalized_cpu;

    let report = PerfReport {
        measured_unix_seconds: now_seconds(),
        mode: if options.desktop {
            "desktop"
        } else {
            "preview"
        },
        image: options.image.display().to_string(),
        duration_seconds: options.seconds,
        warmup_seconds: options.warmup_seconds,
        logical_cpus: logical,
        tree_processes: tree_after.processes,
        app_pid,
        host_pid,
        per_process: processes,
        tree_cpu_100ns: cpu_100ns,
        wall_millis: wall.as_millis(),
        single_core_percent: single_core,
        normalized_percent: normalized,
        dwm_cpu_100ns: dwm_delta,
        dwm_normalized_percent: dwm_normalized,
        threshold_percent: options.max_normalized_cpu,
        passed,
        orphan_hosts: None,
        gpu_time: "未采集：perf-check 覆盖 mpv 直接播放与 Eco 静态；D3D11 效果路径的 GPU 时间见 host-check/托盘状态（本机驱动时间戳返回 0，报告不可用）".into(),
        notes,
    };

    println!(
        "[INFO] 墙钟 {} ms，进程树 CPU {:.3} s，单核 {:.3}%，整机归一 {:.4}%",
        wall.as_millis(),
        cpu_100ns as f64 / 10_000_000.0,
        single_core,
        normalized
    );
    if let Some(dwm) = dwm_normalized {
        println!("[INFO] DWM 整机归一时间增量：{dwm:.4}%");
    } else {
        println!("[INFO] DWM 时间不可用");
    }

    Ok((report, passed))
}

fn wait_for_host(app_pid: u32, timeout: Duration) -> Result<u32, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if !shell_win::process::process_alive(app_pid) {
            return Err("被测进程在宿主就绪前退出（可能已有实例或配置被拒绝）".into());
        }
        if let Some(entry) = shell_win::process::find_processes_by_name("wallpaper-host.exe")
            .into_iter()
            .find(|entry| entry.parent_pid == app_pid)
        {
            return Ok(entry.pid);
        }
        if Instant::now() >= deadline {
            return Err("等待壁纸宿主超时；请检查 libmpv 与素材".into());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

struct TreeCpu {
    processes: usize,
    total_100ns: u64,
    per_process: BTreeMap<u32, u64>,
}

fn tree_cpu(root: u32) -> Result<TreeCpu, String> {
    let parents = shell_win::process::process_parents();
    let mut members = std::collections::HashSet::new();
    members.insert(root);
    loop {
        let before = members.len();
        for (pid, parent) in &parents {
            if members.contains(parent) {
                members.insert(*pid);
            }
        }
        if members.len() == before {
            break;
        }
    }
    let mut per_process = BTreeMap::new();
    for pid in &members {
        if let Some(times) = shell_win::process::process_cpu_times(*pid) {
            per_process.insert(*pid, times.total_100ns());
        }
    }
    let total = per_process.values().copied().sum();
    Ok(TreeCpu {
        processes: members.len(),
        total_100ns: total,
        per_process,
    })
}

fn cleanup(app: &mut Child, workdir: &Path) -> usize {
    // 测量结束后显式终止被测进程；正常退出路径由 host-check 与安装/卸载验收覆盖。
    if matches!(app.try_wait(), Ok(None)) {
        let _ = shell_win::process::terminate_process(app.id());
        let _ = app.wait();
    }
    // 父进程退出后宿主应自行退出；留下观测时间给 Job/父进程监视收尾。
    std::thread::sleep(Duration::from_secs(2));
    let orphans = shell_win::process::find_processes_by_name("wallpaper-host.exe")
        .into_iter()
        .filter(|entry| entry.parent_pid == app.id())
        .count();
    if orphans > 0 {
        eprintln!("[WARN] 仍观察到 {orphans} 个宿主子进程，可能处于退出收尾");
    }
    let _ = fs::remove_dir_all(workdir);
    orphans
}

#[derive(Serialize)]
struct ProcessUsage {
    pid: u32,
    cpu_100ns: u64,
}

#[derive(Serialize)]
struct PerfReport {
    measured_unix_seconds: u64,
    mode: &'static str,
    image: String,
    duration_seconds: u64,
    warmup_seconds: u64,
    logical_cpus: usize,
    tree_processes: usize,
    app_pid: u32,
    host_pid: u32,
    per_process: Vec<ProcessUsage>,
    tree_cpu_100ns: u64,
    wall_millis: u128,
    single_core_percent: f64,
    normalized_percent: f64,
    dwm_cpu_100ns: Option<u64>,
    dwm_normalized_percent: Option<f64>,
    threshold_percent: f64,
    passed: bool,
    orphan_hosts: Option<usize>,
    gpu_time: String,
    notes: Vec<String>,
}

fn write_report(report: &PerfReport, path: Option<&Path>) -> Result<(), String> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("artifacts").join(format!("m4-perf-{}.json", report.measured_unix_seconds))
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建报告目录失败：{error}"))?;
    }
    let text = serde_json::to_string_pretty(report)
        .map_err(|error| format!("序列化性能报告失败：{error}"))?;
    fs::write(&path, text).map_err(|error| format!("写入性能报告失败：{error}"))?;
    println!("[INFO] 报告：{}", path.display());
    Ok(())
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
