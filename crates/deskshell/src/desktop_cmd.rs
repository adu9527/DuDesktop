//! M2 桌面附着自检：WorkerW 探测、完整显示器边界、失效重附着、图标状态与幂等。
//!
//! 该命令只创建隐藏探针窗口，不长期覆盖桌面；验证完成后立即解除附着并销毁窗口。
//! Explorer 重启、锁屏/解锁、休眠恢复的真实路径需要手工步骤（见 docs/DEVELOPMENT_PLAN.md）。

use std::path::PathBuf;

use shell_win::desktop::{self, DesktopAttachment};
use shell_win::preview::{PreviewPlacement, PreviewWindow};

use crate::shellstate::ShellStateJournal;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut restore_icons = false;
    let mut journal_path: Option<PathBuf> = None;
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--restore-icons" => restore_icons = true,
            "--journal" => {
                journal_path = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--journal 缺少路径".to_string())?,
                ))
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }

    println!("M2 desktop-check");
    let displays = desktop::displays();
    if displays.is_empty() {
        return Err("未枚举到任何显示器".into());
    }
    for (index, display) in displays.iter().enumerate() {
        println!(
            "[INFO] 显示器 {index}：{}，{}x{}，位置 ({}, {}){}",
            display.id,
            display.width,
            display.height,
            display.left,
            display.top,
            if display.primary {
                "，主显示器"
            } else {
                ""
            }
        );
    }

    // 1. 稳定身份优先，索引回退；两者当前必须指向同一显示器。
    for (index, display) in displays.iter().enumerate() {
        if desktop::display_by_id(&display.id).as_ref() != Some(display) {
            return Err(format!("按设备名解析显示器 {} 结果不一致", display.id));
        }
        if desktop::display_by_index(Some(index as u32)).as_ref() != Some(display) {
            return Err(format!("按索引 {index} 解析显示器结果不一致"));
        }
    }
    if !display_matches_resolve(displays.first().unwrap()) {
        return Err("resolve_display 未按稳定身份返回结果".into());
    }
    println!("[PASS] 显示器稳定身份（设备名）与索引回退一致");

    let target = displays
        .iter()
        .find(|display| display.primary)
        .cloned()
        .unwrap_or_else(|| displays[0].clone());

    // 2. 探测壁纸宿主。失败是明确回退原因，不是静默跳过。
    match desktop::desktop_host() {
        Ok(worker) => println!("[PASS] 壁纸宿主 WorkerW 探测：{worker:?}"),
        Err(error) => {
            println!("[BLOCKED] 壁纸宿主探测失败：{error}；系统静态壁纸保持可用");
            return Err(format!("WorkerW 探测失败：{error}"));
        }
    }

    // 3. 附着并使用显示器完整边界（rcMonitor），而不是工作区。
    let placement = PreviewPlacement {
        x: target.left,
        y: target.top,
        width: target.width,
        height: target.height,
        visible: false,
    };
    let window = PreviewWindow::create("DeskShell 桌面附着自检", placement)
        .map_err(|error| format!("创建探针窗口失败：{error}"))?;
    let mut attachment = match DesktopAttachment::attach(window.hwnd(), target.clone()) {
        Ok(attachment) => attachment,
        Err(error) => {
            println!("[BLOCKED] 桌面附着失败：{error}；应按静态回退处理");
            return Err(format!("桌面附着失败：{error}"));
        }
    };
    if !attachment.is_valid() {
        return Err("附着后父子关系校验失败".into());
    }
    let rect = desktop::window_screen_rect(window.hwnd())
        .ok_or_else(|| "无法读取探针窗口屏幕矩形".to_string())?;
    if rect != (target.left, target.top, target.width, target.height) {
        return Err(format!(
            "壁纸窗口未覆盖显示器完整边界：窗口 {rect:?}，显示器 ({}, {}, {}, {})",
            target.left, target.top, target.width, target.height
        ));
    }
    println!(
        "[PASS] 附着成功且覆盖完整显示器边界 {}x{} @ ({}, {})",
        target.width, target.height, target.left, target.top
    );

    // 4. 模拟 Explorer 重建造成的附着失效：解除父窗口后必须检测到并重新附着。
    desktop::detach_window(window.hwnd());
    if attachment.is_valid() {
        return Err("解除附着后仍报告有效".into());
    }
    attachment
        .reattach(target.clone())
        .map_err(|error| format!("重新附着失败：{error}"))?;
    if !attachment.is_valid() {
        return Err("重新附着后父子关系校验失败".into());
    }
    let rect = desktop::window_screen_rect(window.hwnd())
        .ok_or_else(|| "重新附着后无法读取窗口矩形".to_string())?;
    if rect != (target.left, target.top, target.width, target.height) {
        return Err(format!("重新附着后几何不一致：{rect:?}"));
    }
    println!("[PASS] 附着失效检测与重新附着");

    // 5. 图标状态查询与幂等设置（不改变当前状态，不默认隐藏）。
    let visible =
        desktop::desktop_icons_visible().map_err(|error| format!("查询图标状态失败：{error}"))?;
    let again = desktop::desktop_icons_visible()
        .map_err(|error| format!("重复查询图标状态失败：{error}"))?;
    if visible != again {
        return Err("两次查询桌面图标状态不一致".into());
    }
    let changed = desktop::set_desktop_icons_visible(visible)
        .map_err(|error| format!("幂等设置图标状态失败：{error}"))?;
    if changed {
        return Err("设置当前已生效的图标状态不应发送切换命令".into());
    }
    println!(
        "[PASS] 桌面图标状态查询与幂等（当前{}）",
        if visible { "可见" } else { "隐藏" }
    );

    // 6. 可选：仅执行“恢复为可见”，修改前先写日志。
    if restore_icons {
        let path = journal_path
            .or_else(ShellStateJournal::default_path)
            .ok_or_else(|| "无法确定 Shell 状态日志路径".to_string())?;
        let mut journal = ShellStateJournal::open(&path)?;
        let pending: Vec<String> = journal
            .pending_recovery()
            .map(|entry| entry.key.clone())
            .collect();
        if !pending.is_empty() {
            println!("[INFO] 待恢复的 Shell 修改：{}", pending.join("，"));
        }
        // 重复执行时保留首次捕获的原值。
        let original = journal
            .get("desktop-icons")
            .map(|entry| entry.original.clone())
            .unwrap_or_else(|| visible.to_string());
        journal.begin("desktop-icons", original, "true".into())?;
        match desktop::set_desktop_icons_visible(true) {
            Ok(changed) => {
                journal.mark_applied("desktop-icons")?;
                if changed {
                    println!(
                        "[PASS] 已恢复桌面图标显示（日志 {}）",
                        journal.path().display()
                    );
                } else {
                    println!(
                        "[PASS] 桌面图标原本可见，无需修改（日志 {}）",
                        journal.path().display()
                    );
                }
                journal.mark_recovered("desktop-icons")?;
            }
            Err(error) => {
                journal.mark_failed("desktop-icons", error.clone())?;
                return Err(format!("恢复桌面图标失败：{error}"));
            }
        }
    }

    attachment.detach();
    drop(window);
    println!(
        "M2 desktop-check 完成。真实 Explorer 重启、锁屏/解锁、休眠恢复需按文档手工执行并记录。"
    );
    Ok(())
}

fn display_matches_resolve(display: &desktop::DisplayInfo) -> bool {
    desktop::resolve_display(Some(&display.id), Some(u32::MAX)) == Some(display.clone())
}
