//! 桌面文件索引与基础操作。
//!
//! 规则：
//! - 聚合用户桌面（含重定向）和公共桌面；分类只影响展示，绝不移动文件。
//! - 身份使用完整路径而不是文件名，允许不同目录存在同名文件。
//! - OneDrive 等云端占位文件只列出元数据，不触发下载或缩略图请求。
//! - 删除前校验目标位于已知桌面根目录下，并统一移入回收站。

use std::{
    ffi::c_void,
    mem::size_of,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use windows::{
    Win32::{
        Foundation::{HANDLE, HGLOBAL, HWND},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_SYSTEM, FILE_FLAGS_AND_ATTRIBUTES,
        },
        System::{
            Com::CoTaskMemFree,
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
        UI::Shell::{
            FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            FOLDERID_Desktop, FOLDERID_PublicDesktop, KF_FLAG_DEFAULT, SEE_MASK_FLAG_NO_UI,
            SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHFILEOPSTRUCTW, SHFileOperationW,
            SHGetKnownFolderPath, ShellExecuteExW,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

/// 展示分类；不改变文件实际位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileCategory {
    Folder,
    Image,
    Document,
    Archive,
    Installer,
    Other,
}

impl FileCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "文件夹",
            Self::Image => "图片",
            Self::Document => "文档",
            Self::Archive => "压缩包",
            Self::Installer => "安装包",
            Self::Other => "其他",
        }
    }

    pub fn from_path(path: &Path, is_dir: bool) -> Self {
        if is_dir {
            return Self::Folder;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "avif" | "tif" | "tiff"
            | "svg" | "ico" => Self::Image,
            "txt" | "md" | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "csv"
            | "rtf" | "odt" | "ods" => Self::Document,
            "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "zst" => Self::Archive,
            "exe" | "msi" | "msix" | "appx" => Self::Installer,
            _ => Self::Other,
        }
    }
}

/// 桌面根目录来源；仅用于展示与诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRootKind {
    User,
    Public,
}

impl DesktopRootKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "我的桌面",
            Self::Public => "公共桌面",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRoot {
    pub kind: DesktopRootKind,
    pub path: PathBuf,
}

/// 一个桌面条目。`id` 是规范化完整路径，跨扫描稳定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopFile {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub category: FileCategory,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub root: DesktopRootKind,
    /// OneDrive 等占位文件：允许列出但禁止主动触发下载。
    pub cloud_placeholder: bool,
}

#[derive(Debug, Default)]
pub struct DesktopIndex {
    pub files: Vec<DesktopFile>,
    pub warnings: Vec<String>,
}

/// 解析用户桌面（含重定向）与公共桌面；同一路径只保留一次。
pub fn desktop_roots() -> (Vec<DesktopRoot>, Vec<String>) {
    let mut roots = Vec::new();
    let mut warnings = Vec::new();
    for (kind, folder) in [
        (DesktopRootKind::User, &FOLDERID_Desktop),
        (DesktopRootKind::Public, &FOLDERID_PublicDesktop),
    ] {
        // SAFETY: 已知文件夹标识为常量；返回字符串由调用方释放。
        match unsafe { SHGetKnownFolderPath(folder, KF_FLAG_DEFAULT, None) } {
            Ok(raw) => {
                let path = PathBuf::from(pwstr_to_string(raw));
                // SAFETY: 与 SHGetKnownFolderPath 配对释放。
                unsafe {
                    CoTaskMemFree(Some(raw.0 as *const c_void));
                }
                if !roots
                    .iter()
                    .any(|root: &DesktopRoot| normalize_path(&root.path) == normalize_path(&path))
                {
                    roots.push(DesktopRoot { kind, path });
                }
            }
            Err(error) => warnings.push(format!("无法解析{}：{error}", kind.label())),
        }
    }
    (roots, warnings)
}

/// 聚合扫描所有桌面；单个根目录失败不阻断其他根目录。
pub fn index_desktop() -> DesktopIndex {
    let (roots, warnings) = desktop_roots();
    let mut index = DesktopIndex {
        files: Vec::new(),
        warnings,
    };
    for root in &roots {
        let (files, root_warnings) = index_root(root);
        index.files.extend(files);
        index.warnings.extend(root_warnings);
    }
    index.files.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    index
}

/// 扫描单个根目录的直属子项；供测试与手动刷新使用。
pub fn index_root(root: &DesktopRoot) -> (Vec<DesktopFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let entries = match std::fs::read_dir(&root.path) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "无法读取{} {}：{error}",
                root.kind.label(),
                root.path.display()
            ));
            return (files, warnings);
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("读取桌面条目失败：{error}"));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!("读取元数据失败 {}：{error}", path.display()));
                continue;
            }
        };
        let attributes = metadata.file_attributes();
        if attributes & (FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0) != 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "desktop.ini" {
            continue;
        }
        let is_dir = metadata.is_dir();
        files.push(DesktopFile {
            id: normalize_path(&path),
            name,
            category: FileCategory::from_path(&path, is_dir),
            path,
            is_dir,
            size: metadata.len(),
            modified: metadata.modified().ok(),
            root: root.kind,
            cloud_placeholder: is_cloud_placeholder(attributes),
        });
    }
    files.sort_by_key(|file| file.name.to_lowercase());
    (files, warnings)
}

fn is_cloud_placeholder(attributes: u32) -> bool {
    attributes
        & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0
            | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
            | FILE_ATTRIBUTE_OFFLINE.0)
        != 0
}

/// 用 Shell 默认动作打开文件或文件夹。
pub fn open_path(path: &Path) -> Result<(), String> {
    shell_execute(path, None)
}

/// 在资源管理器中定位并选中条目。
pub fn reveal_path(path: &Path) -> Result<(), String> {
    let parameter = format!("/select,\"{}\"", path.display());
    shell_execute(Path::new("explorer.exe"), Some(&parameter))
}

/// 复制完整路径到剪贴板；调用线程需要已有消息队列（UI 线程）。
pub fn copy_path(path: &Path) -> Result<(), String> {
    let text: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes = text.len() * size_of::<u16>();
    // SAFETY: 只操作当前线程的剪贴板；所有句柄在作用域内释放或转移。
    unsafe {
        OpenClipboard(None).map_err(|error| format!("打开剪贴板失败：{error}"))?;
        let result = (|| {
            EmptyClipboard().map_err(|error| format!("清空剪贴板失败：{error}"))?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|error| format!("分配剪贴板内存失败：{error}"))?;
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                return Err("锁定剪贴板内存失败".to_string());
            }
            std::ptr::copy_nonoverlapping(text.as_ptr(), pointer as *mut u16, text.len());
            let _ = GlobalUnlock(memory);
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0)))
                .map_err(|error| format!("写入剪贴板失败：{error}"))?;
            // SetClipboardData 成功后所有权归系统，不能再释放 memory。
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// 读取剪贴板文本；仅用于验收探针与诊断。
pub fn read_clipboard_text() -> Result<String, String> {
    // SAFETY: 只操作当前线程的剪贴板；句柄由系统持有，不关闭。
    unsafe {
        OpenClipboard(None).map_err(|error| format!("打开剪贴板失败：{error}"))?;
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT.0 as u32)
                .map_err(|error| format!("剪贴板没有文本：{error}"))?;
            let pointer = GlobalLock(HGLOBAL(handle.0));
            if pointer.is_null() {
                return Err("锁定剪贴板数据失败".to_string());
            }
            let mut length = 0usize;
            let units = pointer as *const u16;
            while *units.add(length) != 0 && length < 1_048_576 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(units, length));
            let _ = GlobalUnlock(HGLOBAL(handle.0));
            Ok(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// 移入回收站，而不是永久删除。只允许已知桌面根目录的直属子项。
pub fn recycle_path(path: &Path) -> Result<(), String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("无法解析待删除路径 {}：{error}", path.display()))?;
    if !is_within_desktop_root(&canonical) {
        return Err(format!(
            "拒绝删除桌面根目录之外或根目录本身：{}",
            canonical.display()
        ));
    }
    recycle_canonical(&canonical)
}

/// 与 `recycle_path` 相同，但显式限定根目录；用于验收探针与未来的受管理素材目录。
pub fn recycle_path_in_root(path: &Path, root: &Path) -> Result<(), String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("无法解析待删除路径 {}：{error}", path.display()))?;
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("无法解析根目录 {}：{error}", root.display()))?;
    let parent_ok = canonical
        .parent()
        .is_some_and(|parent| paths_equal(parent, &root));
    if !parent_ok {
        return Err(format!(
            "拒绝删除根目录之外或根目录本身：{}",
            canonical.display()
        ));
    }
    recycle_canonical(&canonical)
}

fn recycle_canonical(canonical: &Path) -> Result<(), String> {
    // `canonicalize` 返回 `\\?\` 逐字路径；旧版 SHFileOperation 不接受该前缀。
    let operation_path = strip_verbatim_prefix(canonical);
    let mut from: Vec<u16> = operation_path
        .to_string_lossy()
        .encode_utf16()
        .chain([0u16, 0u16])
        .collect();
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_mut_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    // SAFETY: pFrom 以双 NUL 结尾且在调用期间有效；结构体按文档初始化。
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 {
        return Err(format!(
            "移入回收站失败（代码 {code}）：{}",
            operation_path.display()
        ));
    }
    if operation.fAnyOperationsAborted.as_bool() {
        return Err("移入回收站被取消".into());
    }
    Ok(())
}

/// 判断路径是否位于已知桌面根目录下（不包含根目录本身）。
pub fn is_within_desktop_root(path: &Path) -> bool {
    let (roots, _) = desktop_roots();
    let Some(parent) = path.parent() else {
        return false;
    };
    roots.iter().any(|root| {
        let Ok(root) = std::fs::canonicalize(&root.path) else {
            return false;
        };
        paths_equal(parent, &root)
    })
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn shell_execute(path: &Path, parameters: Option<&str>) -> Result<(), String> {
    let file: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let parameters: Vec<u16> = parameters
        .map(|value| value.encode_utf16().chain(std::iter::once(0)).collect())
        .unwrap_or_default();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if parameters.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(parameters.as_ptr())
        },
        nShow: 1,
        ..Default::default()
    };
    // SAFETY: 结构体按文档初始化；文件名与参数在调用期间有效。
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| format!("打开失败：{error}（{}）", path.display()))
}

fn pwstr_to_string(value: PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    // SAFETY: 调用方保证指针指向以 NUL 结尾的 UTF-16 字符串。
    unsafe {
        let mut length = 0usize;
        while *value.0.add(length) != 0 {
            length += 1;
            if length > 32768 {
                break;
            }
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(value.0, length))
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// 属性标志的便捷包装，避免调用方依赖具体位值。
pub fn file_attributes(path: &Path) -> Option<FILE_FLAGS_AND_ATTRIBUTES> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| FILE_FLAGS_AND_ATTRIBUTES(metadata.file_attributes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "deskshell-files-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn categories_follow_extension_and_directory() {
        assert_eq!(
            FileCategory::from_path(Path::new("a.PNG"), false),
            FileCategory::Image
        );
        assert_eq!(
            FileCategory::from_path(Path::new("a.docx"), false),
            FileCategory::Document
        );
        assert_eq!(
            FileCategory::from_path(Path::new("a.zip"), false),
            FileCategory::Archive
        );
        assert_eq!(
            FileCategory::from_path(Path::new("a.msi"), false),
            FileCategory::Installer
        );
        assert_eq!(
            FileCategory::from_path(Path::new("a.unknown"), false),
            FileCategory::Other
        );
        assert_eq!(
            FileCategory::from_path(Path::new("folder"), true),
            FileCategory::Folder
        );
    }

    #[test]
    fn index_keeps_same_name_in_different_roots() {
        let first = temp_dir("user");
        let second = temp_dir("public");
        std::fs::write(first.join("同名.txt"), b"a").unwrap();
        std::fs::write(second.join("同名.txt"), b"b").unwrap();
        let (first_files, first_warnings) = index_root(&DesktopRoot {
            kind: DesktopRootKind::User,
            path: first.clone(),
        });
        let (second_files, second_warnings) = index_root(&DesktopRoot {
            kind: DesktopRootKind::Public,
            path: second.clone(),
        });
        assert!(first_warnings.is_empty());
        assert!(second_warnings.is_empty());
        assert_eq!(first_files.len(), 1);
        assert_eq!(second_files.len(), 1);
        assert_ne!(first_files[0].id, second_files[0].id);
        assert_eq!(first_files[0].category, FileCategory::Document);
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    #[test]
    fn hidden_files_are_skipped() {
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, SetFileAttributesW};

        let root = temp_dir("hidden");
        let visible = root.join("visible.txt");
        let hidden = root.join("hidden.txt");
        std::fs::write(&visible, b"x").unwrap();
        std::fs::write(&hidden, b"x").unwrap();
        let wide: Vec<u16> = hidden
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 路径以 NUL 结尾且在调用期间有效。
        unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_HIDDEN) }.unwrap();
        let (files, warnings) = index_root(&DesktopRoot {
            kind: DesktopRootKind::User,
            path: root.clone(),
        });
        assert!(warnings.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "visible.txt");
        // 恢复属性，避免临时目录清理受隐藏属性影响。
        // SAFETY: 同上，路径有效。
        unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_NORMAL) }.unwrap();
        assert!(is_cloud_placeholder(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0));
        assert!(is_cloud_placeholder(FILE_ATTRIBUTE_RECALL_ON_OPEN.0));
        assert!(!is_cloud_placeholder(0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recycle_guard_rejects_root_and_outside_paths() {
        assert!(!is_within_desktop_root(Path::new("C:\\")));
        assert!(!is_within_desktop_root(Path::new(
            "C:\\definitely-not-desktop\\a.txt"
        )));
    }
}
