//! 应用发现与启动：`shell:AppsFolder` 为主，合并开始菜单快捷方式。
//!
//! 身份策略：
//! - 打包应用/Shell 条目使用 `shell:AppsFolder\<AUMID>` 的父相对解析名；
//! - 开始菜单快捷方式使用规范化的 `.lnk` 路径；
//! - 不以“是否存在 exe 路径”作为条目有效性的前提。
//!
//! COM 说明：发现与图标提取会自行进入 STA；若调用线程已初始化 COM 且模式不同，
//! 会复用现有 apartment 而不是失败。所有枚举结果均为纯 Rust 数据，不跨线程传裸 COM 指针。

use std::{
    collections::HashSet,
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, SIZE},
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
        },
        System::{
            Com::{
                CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize, IBindCtx,
            },
            Threading::GetProcessId,
        },
        UI::{
            Shell::{
                AO_NONE, ApplicationActivationManager, BHID_EnumItems, FOLDERID_AppsFolder,
                FOLDERID_CommonPrograms, FOLDERID_Programs, IApplicationActivationManager,
                IEnumShellItems, IShellItem, IShellItemImageFactory, KF_FLAG_DEFAULT,
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHCreateItemFromParsingName, SHELLEXECUTEINFOW, SHFILEINFOW, SHGFI_ICON,
                SHGFI_LARGEICON, SHGetFileInfoW, SHGetKnownFolderItem, SHGetKnownFolderPath,
                SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SIIGBF_BIGGERSIZEOK,
                SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY, ShellExecuteExW,
            },
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, SW_SHOWNORMAL},
        },
    },
    core::{Interface, PCWSTR, PWSTR, w},
};

/// 应用条目来源；仅用于展示与诊断，不改变启动语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSource {
    AppsFolder,
    StartMenu,
    Manual,
}

impl AppSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::AppsFolder => "应用",
            Self::StartMenu => "开始菜单",
            Self::Manual => "手动添加",
        }
    }
}

/// 启动方式：打包应用优先走激活管理器，其余使用 Shell 默认动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppLaunch {
    /// 应用用户模型 ID（AUMID），例如 `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`。
    Aumid(String),
    /// Shell 可解析路径：AppsFolder 解析名、`.lnk` 或用户手动添加的文件路径。
    ShellPath(String),
}

impl AppLaunch {
    /// 返回可用于 `SHCreateItemFromParsingName`/`ShellExecuteEx` 的解析名。
    pub fn parsing_name(&self) -> String {
        match self {
            Self::Aumid(aumid) => format!("shell:AppsFolder\\{aumid}"),
            Self::ShellPath(path) => path.clone(),
        }
    }
}

/// 一个可启动的应用条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// 稳定身份；重启后不变，用作固定项与图标缓存键。
    pub id: String,
    pub name: String,
    pub source: AppSource,
    pub launch: AppLaunch,
}

/// 发现结果：警告用于界面提示，不静默吞掉权限或枚举失败。
#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub entries: Vec<AppEntry>,
    pub warnings: Vec<String>,
}

/// RGBA/BGRA 图标像素，已预乘 alpha，可直接交给 D2D 预乘位图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// 发现所有应用。会自行初始化 COM apartment；可在后台线程安全调用。
pub fn discover_apps() -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let _apartment = match ComApartment::enter() {
        Ok(apartment) => apartment,
        Err(error) => {
            report.warnings.push(error);
            return report;
        }
    };

    let mut names = HashSet::new();
    match discover_apps_folder(&mut report.warnings) {
        Some(entries) => {
            for entry in entries {
                names.insert(normalize_name(&entry.name));
                report.entries.push(entry);
            }
        }
        None => report
            .warnings
            .push("无法枚举 shell:AppsFolder，仅使用开始菜单条目".into()),
    }
    discover_start_menu(&names, &mut report);
    report
        .entries
        .sort_by_key(|entry| normalize_name(&entry.name));
    report.entries.dedup_by(|left, right| left.id == right.id);
    report
}

/// 添加一个用户手动指定的应用（exe/lnk/任何 Shell 可打开文件）。
pub fn manual_app(name: impl Into<String>, path: impl Into<PathBuf>) -> AppEntry {
    let path = path.into();
    let name = {
        let name = name.into();
        if name.trim().is_empty() {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        } else {
            name
        }
    };
    AppEntry {
        id: format!("manual:{}", normalize_path(&path)),
        name,
        source: AppSource::Manual,
        launch: AppLaunch::ShellPath(path.to_string_lossy().into_owned()),
    }
}

/// 启动应用。返回可观测的进程 ID（Shell 未提供时为空）。
pub fn launch_app(entry: &AppEntry) -> Result<Option<u32>, String> {
    let _apartment = ComApartment::enter()?;
    match &entry.launch {
        AppLaunch::Aumid(aumid) => match activate_aumid(aumid) {
            Ok(pid) => Ok(Some(pid)),
            Err(activation_error) => {
                // 打包条目也可能注册了文件关联式默认动作，激活失败时回退到 Shell。
                shell_execute(&entry.launch.parsing_name()).map_err(|shell_error| {
                    format!("激活失败（{activation_error}）；Shell 启动失败（{shell_error}）")
                })
            }
        },
        AppLaunch::ShellPath(path) => shell_execute(path),
    }
}

/// 提取应用图标；优先使用 Shell 图像工厂，失败时回退到系统图标。
pub fn app_icon(entry: &AppEntry, size: u32) -> Result<IconImage, String> {
    let _apartment = ComApartment::enter()?;
    let size = size.clamp(16, 256);
    let parsing = entry.launch.parsing_name();
    if let Ok(image) = shell_item_icon(&parsing, size) {
        return Ok(image);
    }
    file_icon(&parsing, size).or_else(|_| file_icon(&entry.id, size))
}

/// 提取普通文件/文件夹的图标或缩略图；`thumbnail` 只应传给图片等可预览类型。
///
/// 该调用可能进入第三方 Shell 扩展，调用方必须在后台线程执行并设置超时边界。
pub fn file_image(path: &Path, size: u32, thumbnail: bool) -> Result<IconImage, String> {
    let _apartment = ComApartment::enter()?;
    let size = size.clamp(16, 256);
    let parsing = path.to_string_lossy();
    let wide: Vec<u16> = parsing.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 字符串在调用期间有效；返回的 Shell 条目由本函数持有。
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }
        .map_err(|error| format!("创建 Shell 文件条目失败：{error}"))?;
    let factory: IShellItemImageFactory = item
        .cast()
        .map_err(|error| format!("获取图像工厂失败：{error}"))?;
    let flags = if thumbnail {
        SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK
    } else {
        SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK
    };
    // SAFETY: 尺寸与标志固定；返回位图由本函数释放。
    let bitmap = match unsafe {
        factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            flags,
        )
    } {
        Ok(bitmap) => bitmap,
        Err(_) if thumbnail => {
            // 缩略图不可用时回退系统图标，不影响文件条目显示。
            return file_icon(&parsing, size);
        }
        Err(error) => return Err(format!("提取文件图标失败：{error}")),
    };
    let result = hbitmap_pixels(bitmap);
    // SAFETY: 位图由 GetImage 返回，所有权已转移给本函数。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
    result
}

fn shell_item_icon(parsing: &str, size: u32) -> Result<IconImage, String> {
    let wide: Vec<u16> = parsing.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 字符串在调用期间有效；返回的 Shell 条目由本函数持有。
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }
        .map_err(|error| format!("创建 Shell 条目失败：{error}"))?;
    let factory: IShellItemImageFactory = item
        .cast()
        .map_err(|error| format!("获取图像工厂失败：{error}"))?;
    // SAFETY: 尺寸与标志为固定值；返回位图由本函数释放。
    let bitmap = unsafe {
        factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
        )
    }
    .map_err(|error| format!("提取 Shell 图标失败：{error}"))?;
    let result = hbitmap_pixels(bitmap);
    // SAFETY: 位图由 GetImage 返回，所有权已转移给本函数。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
    result
}

fn file_icon(path: &str, size: u32) -> Result<IconImage, String> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: 输出结构与路径均在调用期间有效；返回图标由本函数销毁。
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || info.hIcon.is_invalid() {
        return Err(format!("系统未返回图标：{path}（请求 {size}px）"));
    }
    let image = hicon_pixels(info.hIcon);
    // SAFETY: 图标句柄由 SHGetFileInfoW 返回，调用方负责销毁。
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    image
}

fn hbitmap_pixels(bitmap: HBITMAP) -> Result<IconImage, String> {
    let mut object = BITMAP::default();
    // SAFETY: 输出结构有效；只读取位图元数据。
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut object as *mut BITMAP as *mut c_void),
        )
    };
    if got == 0 || object.bmWidth <= 0 || object.bmHeight <= 0 {
        return Err("位图信息不可用".into());
    }
    let width = object.bmWidth as u32;
    let height = object.bmHeight.unsigned_abs();
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width as i32;
    // 负高度请求自上而下的像素，避免 D2D 位图上下颠倒。
    info.bmiHeader.biHeight = -(height as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: 设备上下文与位图有效；缓冲区大小与位图尺寸一致。
    let dc = unsafe { CreateCompatibleDC(None) };
    // SAFETY: 上面的设备上下文有效，缓冲区与位图尺寸一致；只读取像素。
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut c_void),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: 设备上下文由本函数创建。
    unsafe {
        let _ = DeleteDC(dc);
    }
    if lines == 0 {
        return Err("读取位图像素失败".into());
    }
    premultiply(&mut pixels);
    Ok(IconImage {
        width,
        height,
        bgra: pixels,
    })
}

fn hicon_pixels(icon: HICON) -> Result<IconImage, String> {
    let mut info = ICONINFO::default();
    // SAFETY: 输出结构有效；查询图标内部位图。
    unsafe { GetIconInfo(icon, &mut info) }
        .map_err(|error| format!("读取图标信息失败：{error}"))?;
    let color = if info.hbmColor.is_invalid() {
        None
    } else {
        hbitmap_pixels(info.hbmColor).ok()
    };
    let mask = if info.hbmMask.is_invalid() {
        None
    } else {
        hbitmap_pixels(info.hbmMask).ok()
    };
    // SAFETY: 两个位图由 GetIconInfo 返回，调用方负责删除。
    unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
    }
    match (color, mask) {
        (Some(mut color), mask) => {
            // 图标颜色位图通常不含 alpha；用掩码补齐透明区域。
            if color.bgra.chunks_exact(4).all(|pixel| pixel[3] == 0) {
                if let Some(mask) = &mask {
                    for (pixel, mask_pixel) in color
                        .bgra
                        .chunks_exact_mut(4)
                        .zip(mask.bgra.chunks_exact(4))
                    {
                        let transparent =
                            mask_pixel[0] as u16 + mask_pixel[1] as u16 + mask_pixel[2] as u16 > 0;
                        pixel[3] = if transparent { 0 } else { 255 };
                    }
                } else {
                    for pixel in color.bgra.chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                }
            }
            premultiply(&mut color.bgra);
            Ok(color)
        }
        (None, Some(mask)) => {
            // 单色图标：把掩码当作黑白图标，透明区域保持 0。
            let mut image = mask;
            for pixel in image.bgra.chunks_exact_mut(4) {
                let luminance = !pixel[0];
                pixel[0] = luminance;
                pixel[1] = luminance;
                pixel[2] = luminance;
                pixel[3] = 255;
            }
            premultiply(&mut image.bgra);
            Ok(image)
        }
        (None, None) => Err("图标没有可用位图".into()),
    }
}

fn premultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        pixel[0] = (u16::from(pixel[0]) * alpha / 255) as u8;
        pixel[1] = (u16::from(pixel[1]) * alpha / 255) as u8;
        pixel[2] = (u16::from(pixel[2]) * alpha / 255) as u8;
    }
}

fn discover_apps_folder(warnings: &mut Vec<String>) -> Option<Vec<AppEntry>> {
    // SAFETY: 已知文件夹标识为常量；返回条目由本函数持有。
    let folder: IShellItem =
        unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: 只请求枚举处理器；返回的枚举器独立持有引用。
    let enumerator: IEnumShellItems =
        unsafe { folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems) }.ok()?;
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    loop {
        let mut items = [None];
        let mut fetched = 0u32;
        // SAFETY: 输出数组与计数指针有效；S_FALSE 表示枚举结束。
        let result = unsafe { enumerator.Next(&mut items, Some(&mut fetched)) };
        if fetched == 0 {
            if let Err(error) = result
                && error.code().0 as u32 != 0x0000_0001
            {
                warnings.push(format!("枚举 AppsFolder 中断：{error}"));
            }
            break;
        }
        let Some(item) = items[0].take() else {
            break;
        };
        match describe_shell_item(&item) {
            Some(entry) => entries.push(entry),
            None => skipped += 1,
        }
        if entries.len() >= 4096 {
            warnings.push("AppsFolder 条目超过 4096，已截断".into());
            break;
        }
    }
    if skipped > 0 {
        warnings.push(format!("AppsFolder 有 {skipped} 个条目缺少名称或解析名"));
    }
    Some(entries)
}

fn describe_shell_item(item: &IShellItem) -> Option<AppEntry> {
    let name = shell_display_name(item, SIGDN_NORMALDISPLAY)?;
    let name = name.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let relative = shell_display_name(item, SIGDN_PARENTRELATIVEPARSING)?;
    if relative.is_empty() {
        return None;
    }
    // AppsFolder 条目的解析名是相对父容器的（例如 `{GUID}\Vendor\app.exe`），
    // 必须补回 `shell:AppsFolder\` 前缀才能再次定位；AUMID 则直接交给激活接口。
    let launch = if looks_like_aumid(&relative) {
        AppLaunch::Aumid(relative.clone())
    } else {
        AppLaunch::ShellPath(format!("shell:AppsFolder\\{relative}"))
    };
    Some(AppEntry {
        id: format!("apps:{relative}"),
        name,
        source: AppSource::AppsFolder,
        launch,
    })
}

fn shell_display_name(item: &IShellItem, kind: windows::Win32::UI::Shell::SIGDN) -> Option<String> {
    // SAFETY: 只读取名称；返回字符串由 COM 分配。
    let raw: PWSTR = unsafe { item.GetDisplayName(kind) }.ok()?;
    let text = pwstr_to_string(raw);
    // SAFETY: 指针由 SHGetKnownFolderPath/GetDisplayName 分配，必须由调用方释放。
    unsafe {
        CoTaskMemFree(Some(raw.0 as *const c_void));
    }
    Some(text)
}

fn discover_start_menu(seen_names: &HashSet<String>, report: &mut DiscoveryReport) {
    let mut roots = Vec::new();
    for folder in [&FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        // SAFETY: 已知文件夹标识为常量；返回路径由调用方释放。
        match unsafe { SHGetKnownFolderPath(folder, KF_FLAG_DEFAULT, None) } {
            Ok(raw) => {
                let path = PathBuf::from(pwstr_to_string(raw));
                // SAFETY: 路径字符串由 SHGetKnownFolderPath 分配。
                unsafe {
                    CoTaskMemFree(Some(raw.0 as *const c_void));
                }
                if path.is_dir() {
                    roots.push(path);
                }
            }
            Err(error) => report
                .warnings
                .push(format!("无法解析开始菜单目录：{error}")),
        }
    }
    let mut links = Vec::new();
    for root in &roots {
        collect_links(root, 0, &mut links, &mut report.warnings);
    }
    links.sort();
    let mut seen_targets = HashSet::new();
    for link in links {
        let name = link
            .file_stem()
            .map(|stem| stem.to_string_lossy().trim().to_owned())
            .unwrap_or_default();
        if name.is_empty() || is_uninstall_entry(&name) {
            continue;
        }
        let normalized = normalize_name(&name);
        if seen_names.contains(&normalized) || !seen_targets.insert(normalized) {
            continue;
        }
        report.entries.push(AppEntry {
            id: format!("lnk:{}", normalize_path(&link)),
            name,
            source: AppSource::StartMenu,
            launch: AppLaunch::ShellPath(link.to_string_lossy().into_owned()),
        });
    }
}

fn collect_links(
    directory: &Path,
    depth: usize,
    links: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) {
    if depth > 4 || links.len() >= 2048 {
        return;
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "无法读取开始菜单目录 {}：{error}",
                directory.display()
            ));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_links(&path, depth + 1, links, warnings);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        {
            links.push(path);
        }
    }
}

fn activate_aumid(aumid: &str) -> Result<u32, String> {
    // SAFETY: 创建系统激活管理器；接口引用计数由返回值管理。
    let manager: IApplicationActivationManager =
        unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER) }
            .map_err(|error| format!("创建应用激活管理器失败：{error}"))?;
    let aumid: Vec<u16> = aumid.encode_utf16().chain(std::iter::once(0)).collect();
    let arguments: Vec<u16> = [0u16].to_vec();
    // SAFETY: 两个字符串在调用期间有效；激活管理器负责启动应用。
    let pid = unsafe {
        manager.ActivateApplication(PCWSTR(aumid.as_ptr()), PCWSTR(arguments.as_ptr()), AO_NONE)
    }
    .map_err(|error| format!("激活应用失败：{error}"))?;
    if pid == 0 {
        return Err("激活成功但系统未返回进程 ID".into());
    }
    Ok(pid)
}

fn shell_execute(target: &str) -> Result<Option<u32>, String> {
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: 结构体按文档初始化；字符串在调用期间有效。
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| format!("Shell 启动失败：{error}（{target}）"))?;
    if info.hProcess.is_invalid() {
        return Ok(None);
    }
    // SAFETY: 进程句柄由 ShellExecuteEx 返回并归本函数所有。
    let pid = unsafe { GetProcessId(info.hProcess) };
    // SAFETY: 只关闭 Shell 返回的进程句柄。
    unsafe {
        let _ = CloseHandle(HANDLE(info.hProcess.0));
    }
    Ok((pid != 0).then_some(pid))
}

fn looks_like_aumid(value: &str) -> bool {
    value.contains('!') && !value.contains('\\') && !value.contains('/')
}

fn is_uninstall_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("uninstall") || name.contains("卸载")
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
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

/// 以 STA 进入 COM；如果当前线程已有其他 apartment 模式，只复用不重复初始化。
struct ComApartment {
    owned: bool,
}

impl ComApartment {
    fn enter() -> Result<Self, String> {
        // SAFETY: 只初始化当前线程的 COM apartment。
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_ok() {
            return Ok(Self { owned: true });
        }
        // RPC_E_CHANGED_MODE：线程已用其他模式初始化，继续使用即可。
        if result.0 as u32 == 0x8001_0106 {
            return Ok(Self { owned: false });
        }
        Err(format!("初始化 COM 失败：{result:?}"))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: 与本类型中的 CoInitializeEx 一一配对。
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// 用户配置中的手动应用条目转换为统一模型。
pub fn merge_manual_apps(
    entries: &mut Vec<AppEntry>,
    manual: impl IntoIterator<Item = (String, PathBuf)>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut ids: HashSet<String> = entries.iter().map(|entry| entry.id.clone()).collect();
    for (name, path) in manual {
        if !path.exists() {
            warnings.push(format!("手动添加的应用不存在：{}", path.display()));
            continue;
        }
        let entry = manual_app(name, path);
        if ids.insert(entry.id.clone()) {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| normalize_name(&entry.name));
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aumid_detection_rejects_shell_paths() {
        assert!(looks_like_aumid(
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        ));
        assert!(!looks_like_aumid(
            "shell:AppsFolder\\Microsoft.WindowsCalculator!App"
        ));
        assert!(!looks_like_aumid(r"C:\Windows\notepad.exe"));
        assert!(!looks_like_aumid("Microsoft.Windows.Explorer"));
    }

    #[test]
    fn uninstall_entries_are_filtered() {
        assert!(is_uninstall_entry("Uninstall Foo"));
        assert!(is_uninstall_entry("卸载 Foo"));
        assert!(!is_uninstall_entry("Foo"));
    }

    #[test]
    fn manual_app_uses_path_when_name_is_empty() {
        let entry = manual_app("", r"C:\Windows\notepad.exe");
        assert_eq!(entry.name, "notepad");
        assert!(entry.id.starts_with("manual:"));
        assert_eq!(entry.source, AppSource::Manual);
    }

    #[test]
    fn icon_pixels_are_premultiplied() {
        let mut pixels = vec![200u8, 100, 50, 128, 10, 20, 30, 255];
        premultiply(&mut pixels);
        assert_eq!(pixels[0], 100);
        assert_eq!(pixels[3], 128);
        assert_eq!(pixels[4..], [10, 20, 30, 255]);
    }

    #[test]
    fn merge_manual_skips_missing_paths_and_duplicates() {
        let mut entries = vec![manual_app("Notepad", r"C:\Windows\notepad.exe")];
        let warnings = merge_manual_apps(
            &mut entries,
            [
                (
                    "Notepad".to_owned(),
                    PathBuf::from(r"C:\Windows\notepad.exe"),
                ),
                (
                    "Missing".to_owned(),
                    PathBuf::from(r"C:\definitely-missing-deskshell-app.exe"),
                ),
            ],
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(warnings.len(), 1);
    }
}
