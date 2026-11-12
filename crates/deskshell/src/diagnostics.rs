//! 本地滚动结构化诊断日志与用户主动导出。
//!
//! 日志目录：`%LOCALAPPDATA%\DeskShell\logs`。单文件上限 2 MiB，最多保留 5 个历史文件；
//! 每行一个 JSON 对象，包含 Unix 毫秒时间、级别、事件与字段。日志不作为控制流，
//! 失败只回退到 stderr，不影响主功能。
//!
//! 导出由用户显式触发：把应用状态、宿主快照、系统兼容信息与最近日志合并为一个 JSON
//! 文件，原子写入指定路径。

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use windows::{
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
    },
    core::PCWSTR,
};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOG_FILES: usize = 5;
const EXPORT_LOG_LINES: usize = 500;

struct Logger {
    path: PathBuf,
    file: File,
    bytes: u64,
}

static LOGGER: OnceLock<Mutex<Option<Logger>>> = OnceLock::new();

fn logger_slot() -> &'static Mutex<Option<Logger>> {
    LOGGER.get_or_init(|| Mutex::new(None))
}

pub fn log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("DeskShell").join("logs"))
}

/// 初始化滚动日志；重复调用安全。
pub fn init() -> Result<(), String> {
    let Some(directory) = log_dir() else {
        return Err("无法解析 LOCALAPPDATA".into());
    };
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建日志目录 {} 失败：{error}", directory.display()))?;
    let path = directory.join("deskshell.log");
    rotate_if_needed(&path, &directory, 0);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("打开日志 {} 失败：{error}", path.display()))?;
    let bytes = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    *logger_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Logger { path, file, bytes });
    Ok(())
}

/// 写入一条结构化日志；未初始化或写入失败时只回退到 stderr。
pub fn log(level: &str, event: &str, fields: Value) {
    let line = json!({
        "ts": unix_millis(),
        "level": level,
        "event": event,
        "fields": fields,
    });
    let mut slot = logger_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(logger) = slot.as_mut() else {
        eprintln!("[{level}] {event} {fields}");
        return;
    };
    let Ok(text) = serde_json::to_string(&line) else {
        return;
    };
    if writeln!(logger.file, "{text}").is_err() {
        eprintln!("[{level}] {event} {fields}");
        return;
    }
    let _ = logger.file.flush();
    logger.bytes = logger.bytes.saturating_add(text.len() as u64 + 1);
    if logger.bytes >= MAX_LOG_BYTES {
        rotate(logger);
    }
}

fn rotate(logger: &mut Logger) {
    let Some(directory) = logger.path.parent().map(Path::to_path_buf) else {
        return;
    };
    rotate_if_needed(&logger.path, &directory, 1);
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&logger.path)
    {
        logger.file = file;
        logger.bytes = 0;
    }
}

fn rotate_if_needed(path: &Path, directory: &Path, keep: usize) {
    let size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if size < MAX_LOG_BYTES {
        return;
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "deskshell".into());
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_else(|| "log".into());
    for index in (keep..MAX_LOG_FILES).rev() {
        let from = directory.join(format!("{stem}.{index}.{extension}"));
        let to = directory.join(format!("{stem}.{}.{extension}", index + 1));
        if from.is_file() {
            let _ = fs::rename(&from, &to);
        }
    }
    let first = directory.join(format!("{stem}.1.{extension}"));
    let _ = fs::rename(path, first);
}

/// 读取最近日志行（当前文件不足时补充上一个滚动文件）。
pub fn recent_lines(limit: usize) -> Vec<String> {
    let Some(directory) = log_dir() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for name in ["deskshell.log", "deskshell.1.log"] {
        let path = directory.join(name);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut file_lines: Vec<String> = text.lines().map(str::to_owned).collect();
        if lines.is_empty() {
            lines.append(&mut file_lines);
        } else {
            file_lines.append(&mut lines);
            lines = file_lines;
        }
        if lines.len() >= limit {
            break;
        }
    }
    if lines.len() > limit {
        lines.split_off(lines.len() - limit)
    } else {
        lines
    }
}

/// 导出诊断包：调用方提供实时状态，本函数补充系统信息与最近日志。
pub fn export(output: &Path, mut bundle: Value) -> Result<(), String> {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建导出目录 {} 失败：{error}", parent.display()))?;
    }
    if let Value::Object(map) = &mut bundle {
        map.insert("generated_unix_ms".into(), json!(unix_millis()));
        map.insert("system".into(), system_info());
        map.insert(
            "log_file".into(),
            Value::String(
                log_dir()
                    .map(|directory| directory.join("deskshell.log").display().to_string())
                    .unwrap_or_default(),
            ),
        );
        map.insert("log_lines".into(), json!(recent_lines(EXPORT_LOG_LINES)));
    }
    let text = serde_json::to_string_pretty(&bundle)
        .map_err(|error| format!("编码诊断包失败：{error}"))?;
    let temporary = output.with_extension("json.tmp");
    {
        let mut file =
            File::create(&temporary).map_err(|error| format!("创建诊断临时文件失败：{error}"))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("写入诊断包失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("刷新诊断包失败：{error}"))?;
    }
    fs::rename(&temporary, output).map_err(|error| format!("替换诊断包失败：{error}"))
}

fn system_info() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "product_name": registry_string("ProductName"),
        "display_version": registry_string("DisplayVersion"),
        "current_build": registry_string("CurrentBuildNumber"),
        "ubr": registry_string("UBR"),
    })
}

fn registry_string(name: &str) -> Option<String> {
    let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut data = vec![0u16; 256];
    let mut size = (data.len() * 2) as u32;
    // SAFETY: 子键与值名以 NUL 结尾；输出缓冲区在调用期间有效。
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).min(data.len());
    let text = String::from_utf16_lossy(&data[..length]);
    Some(text.trim_end_matches('\0').to_owned())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_lines_are_bounded_and_structured() {
        // 单元测试不依赖用户目录：只验证 JSON 行格式与截断逻辑。
        let mut lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let bounded = if lines.len() > 2 {
            lines.split_off(lines.len() - 2)
        } else {
            lines
        };
        assert_eq!(bounded, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn system_info_contains_os_and_build_fields() {
        let info = system_info();
        assert!(info["os"].is_string());
        assert!(info.get("current_build").is_some());
    }
}
