//! M3 行为验收：应用发现/图标/启动、桌面索引与目录监听。
//!
//! 图形界面与真实交互由 `deskshell app --self-test` 另行验证；
//! 本命令只覆盖不依赖窗口的后台能力，失败返回非零退出码。

use std::time::{Duration, Instant};

use shell_win::{
    apps::{AppEntry, AppSource, discover_apps, launch_app, manual_app},
    deskfiles::{DesktopRootKind, index_desktop},
    process::{all_process_ids, process_alive, terminate_process},
    watcher::DesktopWatcher,
};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut launch = false;
    let mut probes = false;
    for argument in args {
        match argument.as_str() {
            "--launch" => launch = true,
            "--probes" => probes = true,
            other => return Err(format!("未知参数：{other}")),
        }
    }
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    println!("== 应用发现 ==");
    let report = discover_apps();
    for warning in &report.warnings {
        println!("警告：{warning}");
        warnings.push(warning.clone());
    }
    if report.entries.is_empty() {
        failures.push("应用发现结果为空".to_string());
    }
    let mut ids = std::collections::HashSet::new();
    for entry in &report.entries {
        if !ids.insert(entry.id.clone()) {
            failures.push(format!("应用身份重复：{}", entry.id));
        }
    }
    let apps_folder = report
        .entries
        .iter()
        .filter(|entry| entry.source == AppSource::AppsFolder)
        .count();
    let start_menu = report
        .entries
        .iter()
        .filter(|entry| entry.source == AppSource::StartMenu)
        .count();
    let aumid = report
        .entries
        .iter()
        .filter(|entry| matches!(entry.launch, shell_win::apps::AppLaunch::Aumid(_)))
        .count();
    println!(
        "共 {} 项：AppsFolder {apps_folder}，开始菜单 {start_menu}，AUMID {aumid}",
        report.entries.len()
    );
    for entry in report.entries.iter().take(8) {
        println!(
            "  [{}] {}（{}）",
            entry.source.label(),
            entry.name,
            entry.id
        );
    }
    if aumid == 0 {
        println!("提示：本机没有枚举到 AUMID 条目，打包应用启动验证将跳过");
    }

    println!("== 应用图标 ==");
    let mut icon_success = 0usize;
    for entry in report.entries.iter().take(5) {
        match shell_win::apps::app_icon(entry, 48) {
            Ok(icon) if !icon.bgra.is_empty() => {
                icon_success += 1;
                println!(
                    "  {}：{}x{}，{} 字节",
                    entry.name,
                    icon.width,
                    icon.height,
                    icon.bgra.len()
                );
            }
            Ok(_) => warnings.push(format!("{} 的图标为空", entry.name)),
            Err(error) => warnings.push(format!("{} 图标失败：{error}", entry.name)),
        }
    }
    if icon_success == 0 {
        failures.push("没有成功提取任何应用图标".to_string());
    }

    println!("== 桌面索引 ==");
    let index = index_desktop();
    for warning in &index.warnings {
        println!("警告：{warning}");
        warnings.push(warning.clone());
    }
    let user = index
        .files
        .iter()
        .filter(|file| file.root == DesktopRootKind::User)
        .count();
    let public = index
        .files
        .iter()
        .filter(|file| file.root == DesktopRootKind::Public)
        .count();
    println!(
        "共 {} 项：用户桌面 {user}，公共桌面 {public}",
        index.files.len()
    );
    for file in index.files.iter().take(8) {
        println!(
            "  [{}] {}（{}，{}，占位={}）",
            file.root.label(),
            file.name,
            file.category.label(),
            if file.is_dir {
                "目录".to_string()
            } else {
                format!("{} 字节", file.size)
            },
            file.cloud_placeholder
        );
    }
    if index.files.is_empty() {
        warnings.push("桌面索引为空（可能所有条目被隐藏或桌面为空）".into());
    }

    println!("== 目录监听 ==");
    check_watcher(&mut failures, &mut warnings)?;

    if launch {
        println!("== 应用启动 ==");
        check_launch(&report.entries, &mut failures, &mut warnings);
    } else {
        println!("== 应用启动（未执行）==");
        println!("  使用 --launch 执行传统应用与打包应用的真实启动/关闭验证");
    }

    if probes {
        println!("== 文件操作探针 ==");
        check_file_probes(&mut failures, &mut warnings);
    } else {
        println!("== 文件操作探针（未执行）==");
        println!("  使用 --probes 在用户桌面创建并回收一个唯一临时文件，同时验证复制路径");
    }

    println!();
    if !warnings.is_empty() {
        println!("警告 {} 条：", warnings.len());
        for warning in &warnings {
            println!("  - {warning}");
        }
    }
    if failures.is_empty() {
        println!("app-check PASS");
        Ok(())
    } else {
        println!("app-check FAIL");
        for failure in &failures {
            println!("  - {failure}");
        }
        Err(format!("{} 项失败", failures.len()))
    }
}

fn check_watcher(failures: &mut Vec<String>, warnings: &mut Vec<String>) -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "deskshell-app-check-watch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&root).map_err(|error| format!("创建监听测试目录失败：{error}"))?;
    let watcher = match DesktopWatcher::start(std::slice::from_ref(&root)) {
        Ok(watcher) => watcher,
        Err(error) => {
            failures.push(format!("启动目录监听失败：{error}"));
            let _ = std::fs::remove_dir_all(&root);
            return Ok(());
        }
    };
    // 给重叠监听一个短就绪窗口。
    std::thread::sleep(Duration::from_millis(100));
    std::fs::write(root.join("probe.txt"), b"probe")
        .map_err(|error| format!("写入监听测试文件失败：{error}"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = false;
    while Instant::now() < deadline {
        if let Some(batch) = watcher.recv_timeout(Duration::from_millis(200)) {
            if !batch.errors.is_empty() {
                warnings.push(format!("监听警告：{}", batch.errors.join("；")));
            }
            if batch
                .paths
                .iter()
                .any(|path| path.file_name().is_some_and(|name| name == "probe.txt"))
            {
                seen = true;
                break;
            }
        }
    }
    if seen {
        println!("  PASS：收到 probe.txt 变化通知");
    } else {
        failures.push("3 秒内未收到目录变化通知".to_string());
    }
    drop(watcher);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

fn check_launch(entries: &[AppEntry], failures: &mut Vec<String>, warnings: &mut Vec<String>) {
    let classic = entries
        .iter()
        .find(|entry| {
            entry.source == AppSource::StartMenu
                && (entry.name.contains("记事本") || entry.name.eq_ignore_ascii_case("Notepad"))
        })
        .cloned()
        .unwrap_or_else(|| manual_app("Notepad", r"C:\Windows\System32\notepad.exe"));
    println!("  传统应用：{}（{}）", classic.name, classic.id);
    let before = all_process_ids();
    match launch_app(&classic) {
        Ok(Some(pid)) => {
            let executable = classic_executable(&classic);
            match wait_for_new_process(pid, executable.as_deref(), &before) {
                Some(observed) => {
                    println!("    PASS：进程 {observed} 已启动");
                    if !before.contains(&observed) {
                        let _ = terminate_process(observed);
                    }
                }
                None => failures.push(format!(
                    "传统应用启动后未观察到有效进程（Shell 返回 {pid}）"
                )),
            }
        }
        Ok(None) => warnings.push("传统应用启动成功但 Shell 未返回进程 ID（无法自动关闭）".into()),
        Err(error) => failures.push(format!("传统应用启动失败：{error}")),
    }

    let packaged = entries
        .iter()
        .find(|entry| match &entry.launch {
            shell_win::apps::AppLaunch::Aumid(aumid) => {
                aumid.to_ascii_lowercase().contains("calculator")
            }
            _ => false,
        })
        .or_else(|| {
            entries
                .iter()
                .find(|entry| matches!(entry.launch, shell_win::apps::AppLaunch::Aumid(_)))
        })
        .cloned();
    match packaged {
        Some(entry) => {
            println!("  打包应用：{}（{}）", entry.name, entry.id);
            let before = all_process_ids();
            match launch_app(&entry) {
                Ok(Some(pid)) => {
                    std::thread::sleep(Duration::from_millis(1200));
                    if process_alive(pid) {
                        let created = !before.contains(&pid);
                        println!("    PASS：激活进程 {pid}（本次新建：{created}）");
                        if created {
                            let _ = terminate_process(pid);
                        }
                    } else {
                        failures.push(format!("打包应用激活后进程 {pid} 不存在"));
                    }
                }
                Ok(None) => warnings.push("打包应用激活成功但未返回进程 ID".into()),
                Err(error) => failures.push(format!("打包应用激活失败：{error}")),
            }
        }
        None => {
            println!("  打包应用：本机未枚举到 AUMID 条目，跳过");
            warnings.push("没有可用的打包应用样本，UWP 启动未验证".into());
        }
    }
}

/// 在用户桌面创建唯一探针文件，验证复制路径与移入回收站，然后确认文件消失。
/// 若系统安全策略拒绝未签名进程写入桌面，则改用临时根目录验证同一套操作实现。
fn check_file_probes(failures: &mut Vec<String>, warnings: &mut Vec<String>) {
    let (roots, _) = shell_win::deskfiles::desktop_roots();
    let Some(root) = roots.iter().find(|root| root.kind == DesktopRootKind::User) else {
        failures.push("没有可用的用户桌面根目录".to_string());
        return;
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let path = root.path.join(format!(
        "DeskShell-probe-{}-{stamp}.txt",
        std::process::id()
    ));
    match std::fs::write(&path, b"DeskShell M3 file probe") {
        Ok(()) => run_probe_in_root(&root.path, &path, true, failures, warnings),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            warnings.push(format!(
                "用户桌面写入被安全策略拒绝（{error}）；改用临时根目录验证回收站实现"
            ));
            let temp = std::env::temp_dir().join(format!(
                "deskshell-probe-root-{}-{stamp}",
                std::process::id()
            ));
            if let Err(error) = std::fs::create_dir_all(&temp) {
                failures.push(format!("创建临时探针根目录失败：{error}"));
                return;
            }
            let probe = temp.join("DeskShell-probe.txt");
            match std::fs::write(&probe, b"DeskShell M3 file probe") {
                Ok(()) => run_probe_in_root(&temp, &probe, false, failures, warnings),
                Err(error) => failures.push(format!("创建临时探针文件失败：{error}")),
            }
            let _ = std::fs::remove_dir_all(&temp);
        }
        Err(error) => failures.push(format!("无法在用户桌面创建探针文件：{error}")),
    }
}

fn run_probe_in_root(
    root: &std::path::Path,
    path: &std::path::Path,
    desktop_guard: bool,
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let (files, _) = shell_win::deskfiles::index_root(&shell_win::deskfiles::DesktopRoot {
        kind: DesktopRootKind::User,
        path: root.to_path_buf(),
    });
    if !files.iter().any(|file| file.path == path) {
        failures.push("新探针文件未出现在桌面索引中".to_string());
    } else {
        println!("  PASS：桌面索引包含新文件");
    }
    let mut copied = false;
    for attempt in 0..3 {
        match shell_win::deskfiles::copy_path(path) {
            Ok(()) => match shell_win::deskfiles::read_clipboard_text() {
                Ok(text) if text.contains(&path.to_string_lossy().to_string()) => {
                    copied = true;
                    break;
                }
                Ok(_) => warnings.push("剪贴板内容与探针路径不一致".into()),
                Err(error) => warnings.push(format!("读取剪贴板失败：{error}")),
            },
            Err(error) => warnings.push(format!("第 {} 次复制路径失败：{error}", attempt + 1)),
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if copied {
        println!("  PASS：复制路径写入剪贴板并可读回");
    } else {
        failures.push("复制路径验证失败".to_string());
    }
    let result = if desktop_guard {
        shell_win::deskfiles::recycle_path(path)
    } else {
        shell_win::deskfiles::recycle_path_in_root(path, root)
    };
    match result {
        Ok(()) if !path.exists() => println!("  PASS：探针文件已移入回收站"),
        Ok(()) => failures.push("回收站操作返回成功但文件仍存在".to_string()),
        Err(error) => failures.push(format!("移入回收站失败：{error}")),
    }
    if path.exists() {
        // 失败时清理，避免在用户桌面留下测试文件。
        let _ = std::fs::remove_file(path);
        warnings.push("探针清理：已直接删除残留文件（未进入回收站）".into());
    }
}

/// 启动后最多等待 2 秒：优先确认返回的 PID，部分系统应用会由短命启动器转交真实进程。
fn wait_for_new_process(
    pid: u32,
    executable: Option<&str>,
    before: &std::collections::HashSet<u32>,
) -> Option<u32> {
    for attempt in 0..10 {
        if process_alive(pid) {
            return Some(pid);
        }
        if attempt > 0
            && let Some(executable) = executable
        {
            let candidate = shell_win::process::find_processes_by_name(executable)
                .into_iter()
                .find(|entry| !before.contains(&entry.pid) && process_alive(entry.pid));
            if let Some(entry) = candidate {
                return Some(entry.pid);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

fn classic_executable(entry: &AppEntry) -> Option<String> {
    let path = match &entry.launch {
        shell_win::apps::AppLaunch::ShellPath(path) => path,
        _ => return None,
    };
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.contains('.'))
}
