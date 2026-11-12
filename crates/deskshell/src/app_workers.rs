//! M3 后台工作线程：应用发现/图标、桌面索引与目录监听。
//!
//! 所有结果通过 `Arc<Mutex<Option<..>>>` 交接，并用 `PostMessage` 唤醒 UI 线程；
//! UI 线程不等待 Shell 枚举、图标提取或目录扫描。

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use render::IconPixels;
use shell_win::{
    apps::{AppEntry, IconImage, app_icon, discover_apps, file_image, merge_manual_apps},
    deskfiles::{DesktopFile, FileCategory, index_desktop},
    shellui::post_message_raw,
    watcher::DesktopWatcher,
};

/// 超时的 Shell 图标线程无法安全取消；达到上限后停止继续请求，避免线程无界增长。
static ABANDONED_ICON_WORKERS: AtomicUsize = AtomicUsize::new(0);

pub struct AppsReady {
    pub entries: Vec<AppEntry>,
    pub icons: HashMap<String, IconPixels>,
    pub warnings: Vec<String>,
}

pub struct FilesReady {
    pub files: Vec<DesktopFile>,
    pub warnings: Vec<String>,
    /// 仅当前可见前 8 行的图标/缩略图；不会为全部文件预生成。
    pub icons: HashMap<String, IconPixels>,
}

/// 启动应用发现线程；`pinned` 与首批应用库条目会预取 48px 图标。
pub fn spawn_apps(
    manual: Vec<(String, PathBuf)>,
    pinned: Vec<String>,
    notify: isize,
    message: u32,
) -> Arc<Mutex<Option<AppsReady>>> {
    let store = Arc::new(Mutex::new(None));
    let result = Arc::clone(&store);
    let _ = thread::Builder::new()
        .name("apps-index".into())
        .spawn(move || {
            let report = discover_apps();
            let mut warnings = report.warnings;
            let mut entries = report.entries;
            warnings.extend(merge_manual_apps(&mut entries, manual));
            let mut wanted: Vec<String> = pinned;
            wanted.extend(entries.iter().take(12).map(|entry| entry.id.clone()));
            let mut seen = HashSet::new();
            let mut icons = HashMap::new();
            let mut failures = 0usize;
            for id in wanted {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if icons.len() >= 24 {
                    break;
                }
                let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                    continue;
                };
                match app_icon(entry, 96) {
                    Ok(icon) => {
                        icons.insert(
                            id,
                            IconPixels {
                                width: icon.width,
                                height: icon.height,
                                bgra: icon.bgra,
                            },
                        );
                    }
                    Err(error) => {
                        failures += 1;
                        if failures <= 4 {
                            warnings.push(format!("{} 图标不可用：{error}", entry.name));
                        }
                    }
                }
            }
            *result.lock().unwrap_or_else(|error| error.into_inner()) = Some(AppsReady {
                entries,
                icons,
                warnings,
            });
            post_message_raw(notify, message);
        });
    store
}

/// 启动桌面索引与监听线程；每次扫描结果通过 `store` 交接并通知 UI。
pub fn spawn_files(
    notify: isize,
    message: u32,
    stop: Arc<AtomicBool>,
) -> Arc<Mutex<Option<FilesReady>>> {
    let store = Arc::new(Mutex::new(None));
    let result = Arc::clone(&store);
    let _ = thread::Builder::new()
        .name("desktop-files".into())
        .spawn(move || {
            let mut last_signature = None;
            let mut icons = HashMap::new();
            loop {
                let index = index_desktop();
                let signature = signature(&index.files);
                if last_signature.as_ref() != Some(&signature) {
                    last_signature = Some(signature);
                    let mut warnings = index.warnings;
                    load_visible_icons(&index.files, &mut icons, &mut warnings);
                    *result.lock().unwrap_or_else(|error| error.into_inner()) = Some(FilesReady {
                        files: index.files,
                        warnings,
                        icons: icons.clone(),
                    });
                    post_message_raw(notify, message);
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if last_signature.is_some() {
                    // 首次扫描完成后进入监听循环；监听失败时退化为低频快照校验。
                    if watched_loop(
                        &stop,
                        &notify,
                        message,
                        &result,
                        &mut last_signature,
                        &mut icons,
                    ) {
                        continue;
                    }
                }
                // 没有可用监听或监听退出：有限等待后重扫，避免忙轮询。
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    store
}

/// 返回 true 表示监听正常结束且需要重新扫描；false 表示无法监听。
fn watched_loop(
    stop: &AtomicBool,
    notify: &isize,
    message: u32,
    store: &Arc<Mutex<Option<FilesReady>>>,
    last_signature: &mut Option<Vec<(String, u64, u64)>>,
    icons: &mut HashMap<String, IconPixels>,
) -> bool {
    let (roots, _) = shell_win::deskfiles::desktop_roots();
    let paths: Vec<PathBuf> = roots.into_iter().map(|root| root.path).collect();
    let Ok(watcher) = DesktopWatcher::start(&paths) else {
        return false;
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let Some(batch) = watcher.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        // 去抖：监听层已合并 150 ms；这里再等待一次写入稳定窗口。
        thread::sleep(Duration::from_millis(120));
        let index = index_desktop();
        let signature = signature(&index.files);
        if last_signature.as_ref() != Some(&signature) {
            *last_signature = Some(signature);
            let mut all_warnings = index.warnings;
            all_warnings.extend(batch.errors);
            load_visible_icons(&index.files, icons, &mut all_warnings);
            *store.lock().unwrap_or_else(|error| error.into_inner()) = Some(FilesReady {
                files: index.files,
                warnings: all_warnings,
                icons: icons.clone(),
            });
            post_message_raw(*notify, message);
        }
    }
}

/// 只为当前前 8 行加载图标/缩略图；单个请求超过 1.5 秒即隔离并在有限次数后降级。
fn load_visible_icons(
    files: &[DesktopFile],
    icons: &mut HashMap<String, IconPixels>,
    warnings: &mut Vec<String>,
) {
    let visible: Vec<&DesktopFile> = files
        .iter()
        .take(8)
        .filter(|file| !file.cloud_placeholder)
        .collect();
    let keep: HashSet<&str> = visible.iter().map(|file| file.id.as_str()).collect();
    icons.retain(|id, _| keep.contains(id.as_str()));
    let mut failures = 0usize;
    for file in visible {
        if icons.contains_key(&file.id) {
            continue;
        }
        if ABANDONED_ICON_WORKERS.load(Ordering::Relaxed) >= 2 {
            warnings.push("图标/缩略图请求多次超时，已停止继续请求".into());
            break;
        }
        let thumbnail = file.category == FileCategory::Image && !file.is_dir;
        match icon_with_timeout(file.path.clone(), 32, thumbnail) {
            Ok(icon) => {
                icons.insert(
                    file.id.clone(),
                    IconPixels {
                        width: icon.width,
                        height: icon.height,
                        bgra: icon.bgra,
                    },
                );
            }
            Err(error) => {
                failures += 1;
                if failures <= 2 {
                    warnings.push(format!("{}：{error}", file.name));
                }
            }
        }
    }
}

pub(crate) fn icon_with_timeout(
    path: PathBuf,
    size: u32,
    thumbnail: bool,
) -> Result<IconImage, String> {
    if ABANDONED_ICON_WORKERS.load(Ordering::Relaxed) >= 2 {
        return Err("Shell 图标服务已降级".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("file-icon".into())
        .spawn(move || {
            let result = file_image(&path, size, thumbnail);
            let _ = sender.send(result);
        })
        .map_err(|error| format!("启动图标线程失败：{error}"))?;
    match receiver.recv_timeout(Duration::from_millis(1500)) {
        Ok(result) => result,
        Err(_) => {
            ABANDONED_ICON_WORKERS.fetch_add(1, Ordering::Relaxed);
            Err("图标/缩略图请求超时（已隔离）".into())
        }
    }
}

pub(crate) fn app_icon_with_timeout(entry: AppEntry, size: u32) -> Result<IconImage, String> {
    if ABANDONED_ICON_WORKERS.load(Ordering::Relaxed) >= 2 {
        return Err("Shell 图标服务已降级".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("app-icon-request".into())
        .spawn(move || {
            let _ = sender.send(app_icon(&entry, size));
        })
        .map_err(|e| e.to_string())?;
    match receiver.recv_timeout(Duration::from_millis(1500)) {
        Ok(result) => result,
        Err(_) => {
            ABANDONED_ICON_WORKERS.fetch_add(1, Ordering::Relaxed);
            Err("应用图标请求超时".into())
        }
    }
}

fn signature(files: &[DesktopFile]) -> Vec<(String, u64, u64)> {
    let mut signature: Vec<(String, u64, u64)> = files
        .iter()
        .map(|file| {
            let modified = file
                .modified
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            (file.id.clone(), file.size, modified)
        })
        .collect();
    signature.sort();
    signature
}
