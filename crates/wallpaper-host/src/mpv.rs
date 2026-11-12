//! libmpv 动态加载与窗口嵌入播放封装。
//!
//! 构建期不链接 libmpv：运行时按固定目录查找 `libmpv-2.dll`，并用 C API 版本检查拒绝不兼容二进制。
//! mpv client API 允许任意线程调用命令；本模块仍把事件等待集中在单一事件线程，
//! 命令由宿主主线程发出，避免在回调中重入。

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use protocol::FitMode;
use shell_win::event::WaitEvent;
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    },
    core::{PCSTR, PCWSTR},
};

const MPV_CLIENT_API_MAJOR: u64 = 2;
const MPV_CLIENT_API_MINOR: u64 = 1;

const MPV_FORMAT_STRING: c_int = 1;
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;
const MPV_FORMAT_FLAG: c_int = 3;

const MPV_EVENT_NONE: c_int = 0;
const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_LOG_MESSAGE: c_int = 2;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_FILE_LOADED: c_int = 8;
const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

pub const END_FILE_REASON_EOF: c_int = 0;
const END_FILE_REASON_ERROR: c_int = 4;

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: i64,
    playlist_insert_id: i64,
    playlist_insert_num_entries: c_int,
}

type CreateFn = unsafe extern "C" fn() -> *mut c_void;
type InitializeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type TerminateDestroyFn = unsafe extern "C" fn(*mut c_void);
type SetOptionStringFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type SetPropertyStringFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type GetPropertyStringFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type CommandFn = unsafe extern "C" fn(*mut c_void, *const *const c_char) -> c_int;
type WaitEventFn = unsafe extern "C" fn(*mut c_void, f64) -> *const MpvEvent;
type WakeupFn = unsafe extern "C" fn(*mut c_void);
type ObservedPropertyFn = unsafe extern "C" fn(*mut c_void, u64, *const c_char, c_int) -> c_int;
type RequestLogFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type ErrorStringFn = unsafe extern "C" fn(c_int) -> *const c_char;
type ClientApiVersionFn = unsafe extern "C" fn() -> u64;

/// mpv render API 参数（`mpv_render_param`）。
#[repr(C)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}

/// `mpv_d3d11_init_params`：只含设备指针，mpv 自行增加引用。
#[repr(C)]
struct MpvD3d11InitParams {
    device: *mut c_void,
}

/// `mpv_d3d11_fbo`：只含纹理指针，mpv 不保留引用。
#[repr(C)]
struct MpvD3d11Fbo {
    tex: *mut c_void,
}

const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_D3D11_INIT_PARAMS: c_int = 21;
const MPV_RENDER_PARAM_D3D11_FBO: c_int = 22;
const MPV_RENDER_UPDATE_FRAME: u64 = 1;

type RenderContextCreateFn =
    unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const MpvRenderParam) -> c_int;
type RenderContextRenderFn = unsafe extern "C" fn(*mut c_void, *const MpvRenderParam) -> c_int;
type RenderContextUpdateFn = unsafe extern "C" fn(*mut c_void) -> u64;
type RenderContextCallbackFn = unsafe extern "C" fn(*mut c_void);
type RenderContextSetCallbackFn =
    unsafe extern "C" fn(*mut c_void, Option<RenderContextCallbackFn>, *mut c_void);
type RenderContextReportSwapFn = unsafe extern "C" fn(*mut c_void);
type RenderContextFreeFn = unsafe extern "C" fn(*mut c_void);

#[derive(Debug, Clone)]
pub struct MpvError {
    pub message: String,
}

impl MpvError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MpvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MpvError {}

pub struct MpvLibrary {
    /// 保持模块加载到进程退出，避免 mpv 线程仍在使用时卸载。
    _module: HMODULE,
    create: CreateFn,
    initialize: InitializeFn,
    terminate_destroy: TerminateDestroyFn,
    set_option_string: SetOptionStringFn,
    set_property_string: SetPropertyStringFn,
    get_property_string: GetPropertyStringFn,
    free: FreeFn,
    command: CommandFn,
    wait_event: WaitEventFn,
    wakeup: WakeupFn,
    observe_property: ObservedPropertyFn,
    request_log: RequestLogFn,
    error_string: ErrorStringFn,
    client_api_version: u64,
    // D3D11 render API 为可选能力：缺失时普通播放仍可用，互操作原型明确报不可用。
    render_context_create: Option<RenderContextCreateFn>,
    render_context_render: Option<RenderContextRenderFn>,
    render_context_update: Option<RenderContextUpdateFn>,
    render_context_set_callback: Option<RenderContextSetCallbackFn>,
    render_context_report_swap: Option<RenderContextReportSwapFn>,
    render_context_free: Option<RenderContextFreeFn>,
}

// SAFETY: mpv client API 明确允许从多线程调用命令与属性接口；模块句柄在进程生命周期内保持加载。
unsafe impl Send for MpvLibrary {}
// SAFETY: 共享引用只调用 mpv client API 的线程安全函数，所有函数指针在进程生命周期内有效。
unsafe impl Sync for MpvLibrary {}

fn load_symbol<T: Copy>(module: HMODULE, name: &CStr) -> Result<T, MpvError> {
    // SAFETY: 模块句柄有效，符号名以 NUL 结尾。
    let symbol = unsafe { GetProcAddress(module, PCSTR(name.as_ptr() as *const u8)) }
        .ok_or_else(|| MpvError::new(format!("libmpv 缺少导出：{}", name.to_string_lossy())))?;
    // SAFETY: 调用方以正确的 C 函数签名实例化 T；这是动态绑定的固有前提。
    Ok(unsafe { std::mem::transmute_copy::<_, T>(&symbol) })
}

fn load_optional_symbol<T: Copy>(module: HMODULE, name: &CStr) -> Option<T> {
    // SAFETY: 模块句柄有效，符号名以 NUL 结尾。
    let symbol = unsafe { GetProcAddress(module, PCSTR(name.as_ptr() as *const u8)) }?;
    // SAFETY: 调用方以正确的 C 函数签名实例化 T；这是动态绑定的固有前提。
    Some(unsafe { std::mem::transmute_copy::<_, T>(&symbol) })
}

fn c_string(name: &str) -> Result<CString, MpvError> {
    CString::new(name).map_err(|_| MpvError::new(format!("字符串包含 NUL：{name}")))
}

impl MpvLibrary {
    /// 按固定顺序查找并加载 `libmpv-2.dll`。
    pub fn load() -> Result<Arc<Self>, MpvError> {
        let mut candidates = Vec::new();
        if let Ok(directory) = std::env::var("DESKSHELL_MPV_DIR") {
            let path = PathBuf::from(directory);
            candidates.push(if path.is_dir() {
                path.join("libmpv-2.dll")
            } else {
                path
            });
        }
        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            candidates.push(directory.join("libmpv-2.dll"));
            candidates.push(directory.join("mpv").join("libmpv-2.dll"));
            for ancestor in directory.ancestors().skip(1).take(3) {
                candidates.push(
                    ancestor
                        .join("third_party")
                        .join("mpv")
                        .join("libmpv-2.dll"),
                );
            }
        }
        if let Ok(current) = std::env::current_dir() {
            candidates.push(current.join("third_party").join("mpv").join("libmpv-2.dll"));
        }

        let mut last_error = None;
        for candidate in candidates {
            if !candidate.is_file() {
                continue;
            }
            match Self::load_path(&candidate) {
                Ok(library) => return Ok(Arc::new(library)),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            MpvError::new(
                "未找到 libmpv-2.dll；请执行 scripts/fetch-mpv.ps1 或设置 DESKSHELL_MPV_DIR",
            )
        }))
    }

    fn load_path(path: &std::path::Path) -> Result<Self, MpvError> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 路径以 NUL 结尾；模块在进程生命周期内保持加载（不调用 FreeLibrary）。
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
            .map_err(|error| MpvError::new(format!("加载 {} 失败：{error}", path.display())))?;

        let result = (|| -> Result<Self, MpvError> {
            let create = load_symbol(module, c"mpv_create")?;
            let initialize = load_symbol(module, c"mpv_initialize")?;
            let terminate_destroy = load_symbol(module, c"mpv_terminate_destroy")?;
            let set_option_string = load_symbol(module, c"mpv_set_option_string")?;
            let set_property_string = load_symbol(module, c"mpv_set_property_string")?;
            let get_property_string = load_symbol(module, c"mpv_get_property_string")?;
            let free = load_symbol(module, c"mpv_free")?;
            let command = load_symbol(module, c"mpv_command")?;
            let wait_event = load_symbol(module, c"mpv_wait_event")?;
            let wakeup = load_symbol(module, c"mpv_wakeup")?;
            let observe_property = load_symbol(module, c"mpv_observe_property")?;
            let request_log = load_symbol(module, c"mpv_request_log_messages")?;
            let error_string = load_symbol(module, c"mpv_error_string")?;
            let client_api_version: ClientApiVersionFn =
                load_symbol(module, c"mpv_client_api_version")?;
            // SAFETY: 已按 client.h 的签名解析导出函数。
            let version = unsafe { client_api_version() };
            let major = version >> 16;
            let minor = version & 0xFFFF;
            if major != MPV_CLIENT_API_MAJOR || minor < MPV_CLIENT_API_MINOR {
                return Err(MpvError::new(format!(
                    "libmpv client API 版本 {major}.{minor} 不兼容；需要 {MPV_CLIENT_API_MAJOR}.{MPV_CLIENT_API_MINOR}+"
                )));
            }
            Ok(Self {
                _module: module,
                create,
                initialize,
                terminate_destroy,
                set_option_string,
                set_property_string,
                get_property_string,
                free,
                command,
                wait_event,
                wakeup,
                observe_property,
                request_log,
                error_string,
                client_api_version: version,
                render_context_create: load_optional_symbol(module, c"mpv_render_context_create"),
                render_context_render: load_optional_symbol(module, c"mpv_render_context_render"),
                render_context_update: load_optional_symbol(module, c"mpv_render_context_update"),
                render_context_set_callback: load_optional_symbol(
                    module,
                    c"mpv_render_context_set_update_callback",
                ),
                render_context_report_swap: load_optional_symbol(
                    module,
                    c"mpv_render_context_report_swap",
                ),
                render_context_free: load_optional_symbol(module, c"mpv_render_context_free"),
            })
        })();

        match result {
            Ok(library) => Ok(library),
            Err(error) => {
                // SAFETY: 加载失败且没有 mpv 线程引用该模块。
                unsafe {
                    let _ = FreeLibrary(module);
                }
                Err(error)
            }
        }
    }

    pub fn client_api_string(&self) -> String {
        format!(
            "{}.{}",
            self.client_api_version >> 16,
            self.client_api_version & 0xFFFF
        )
    }

    /// 创建一个只用于读取版本号的临时上下文。
    pub fn probe_version(self: &Arc<Self>) -> Option<String> {
        let context = Context::new(self).ok()?;
        context
            .set_option("vo", "null")
            .and_then(|_| context.set_option("ao", "null"))
            .and_then(|_| context.set_option("audio", "no"))
            .and_then(|_| context.set_option("idle", "yes"))
            .and_then(|_| context.set_option("config", "no"))
            .and_then(|_| context.initialize())
            .ok()?;
        context.get_string("mpv-version")
    }
}

struct Context {
    library: Arc<MpvLibrary>,
    raw: *mut c_void,
    /// 事件线程使用借用视图，不能释放上下文。
    owns: bool,
}

// SAFETY: client API 允许跨线程调用；Context 的释放只在 Player 的 Drop 中、事件线程结束后执行。
unsafe impl Send for Context {}
// SAFETY: 共享引用只调用线程安全的 mpv 属性/命令接口；原始上下文在事件线程退出后才释放。
unsafe impl Sync for Context {}

impl Context {
    fn new(library: &Arc<MpvLibrary>) -> Result<Self, MpvError> {
        // SAFETY: 创建独立 mpv 上下文；失败返回空指针。
        let raw = unsafe { (library.create)() };
        if raw.is_null() {
            return Err(MpvError::new("mpv_create 返回空上下文"));
        }
        Ok(Self {
            library: Arc::clone(library),
            raw,
            owns: true,
        })
    }

    fn borrowed(&self) -> Self {
        Self {
            library: Arc::clone(&self.library),
            raw: self.raw,
            owns: false,
        }
    }

    fn initialize(&self) -> Result<(), MpvError> {
        // SAFETY: 上下文有效。
        let code = unsafe { (self.library.initialize)(self.raw) };
        self.check(code, "初始化 mpv")
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let name = c_string(name)?;
        let value = c_string(value)?;
        // SAFETY: 上下文与 C 字符串在调用期间有效。
        let code =
            unsafe { (self.library.set_option_string)(self.raw, name.as_ptr(), value.as_ptr()) };
        self.check(code, "设置 mpv 选项")
    }

    fn set_property_string(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let name = c_string(name)?;
        let value = c_string(value)?;
        // SAFETY: 上下文与 C 字符串在调用期间有效。
        let code =
            unsafe { (self.library.set_property_string)(self.raw, name.as_ptr(), value.as_ptr()) };
        self.check(code, "设置 mpv 属性")
    }

    fn get_string(&self, name: &str) -> Option<String> {
        let name = c_string(name).ok()?;
        // SAFETY: 返回字符串由 mpv 分配，需用 mpv_free 释放。
        let pointer = unsafe { (self.library.get_property_string)(self.raw, name.as_ptr()) };
        if pointer.is_null() {
            return None;
        }
        // SAFETY: mpv 保证返回以 NUL 结尾的字符串。
        let value = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: 指针由 mpv_get_property_string 分配。
        unsafe {
            (self.library.free)(pointer as *mut c_void);
        }
        Some(value)
    }

    fn command(&self, arguments: &[&str]) -> Result<(), MpvError> {
        let mut owned = Vec::with_capacity(arguments.len());
        for argument in arguments {
            owned.push(c_string(argument)?);
        }
        let mut pointers: Vec<*const c_char> = owned.iter().map(|value| value.as_ptr()).collect();
        pointers.push(ptr::null());
        // SAFETY: 参数数组以空指针结尾，字符串在调用期间有效。
        let code = unsafe { (self.library.command)(self.raw, pointers.as_ptr()) };
        self.check(code, "执行 mpv 命令")
    }

    fn observe(&self, name: &str, format: c_int) -> Result<(), MpvError> {
        let name = c_string(name)?;
        // SAFETY: 上下文有效；属性名以 NUL 结尾。
        let code = unsafe { (self.library.observe_property)(self.raw, 0, name.as_ptr(), format) };
        self.check(code, "观察 mpv 属性")
    }

    fn request_log(&self, level: &str) -> Result<(), MpvError> {
        let level = c_string(level)?;
        // SAFETY: 上下文有效。
        let code = unsafe { (self.library.request_log)(self.raw, level.as_ptr()) };
        self.check(code, "订阅 mpv 日志")
    }

    fn wait_event(&self, timeout: f64) -> *const MpvEvent {
        // SAFETY: 上下文有效；返回指针在下一次 wait_event 前有效。
        unsafe { (self.library.wait_event)(self.raw, timeout) }
    }

    fn wakeup(&self) {
        // SAFETY: 上下文有效。
        unsafe {
            (self.library.wakeup)(self.raw);
        }
    }

    fn error_text(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string 返回静态字符串。
        let pointer = unsafe { (self.library.error_string)(code) };
        if pointer.is_null() {
            return format!("mpv 错误码 {code}");
        }
        // SAFETY: 返回静态 NUL 结尾字符串。
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }

    fn check(&self, code: c_int, operation: &str) -> Result<(), MpvError> {
        if code < 0 {
            Err(MpvError::new(format!(
                "{operation}失败：{}（{code}）",
                self.error_text(code)
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: 调用方保证事件线程已经退出；上下文由本对象独占。
            unsafe {
                (self.library.terminate_destroy)(self.raw);
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObservedState {
    pub file_loaded: bool,
    pub end_reason: Option<c_int>,
    pub end_error: Option<String>,
    pub hwdec_current: Option<String>,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_milli: Option<u32>,
    pub pixel_format: Option<String>,
    pub paused: Option<bool>,
    pub idle_active: Option<bool>,
    pub last_log_error: Option<String>,
}

impl ObservedState {
    fn clear_for_load(&mut self) {
        self.file_loaded = false;
        self.end_reason = None;
        self.end_error = None;
        self.hwdec_current = None;
        self.codec = None;
        self.width = None;
        self.height = None;
        self.fps_milli = None;
        self.pixel_format = None;
        self.last_log_error = None;
        self.idle_active = Some(false);
    }

    fn read_properties(&mut self, context: &Context) {
        self.hwdec_current = context.get_string("hwdec-current");
        self.codec = context.get_string("video-codec");
        self.width = context
            .get_string("width")
            .and_then(|value| value.parse().ok());
        self.height = context
            .get_string("height")
            .and_then(|value| value.parse().ok());
        self.fps_milli = context
            .get_string("container-fps")
            .and_then(|value| value.parse::<f64>().ok())
            .map(|fps| (fps * 1000.0).round() as u32);
        self.pixel_format = context.get_string("video-params/pixelformat");
        self.paused = context.get_string("pause").map(|value| value == "yes");
        self.idle_active = context
            .get_string("idle-active")
            .map(|value| value == "yes");
    }
}

pub struct Player {
    context: Context,
    observed: Arc<Mutex<ObservedState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn new(
        library: &Arc<MpvLibrary>,
        window_id: isize,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, MpvError> {
        let context = Context::new(library)?;
        context.set_option("wid", &window_id.to_string())?;
        context.set_option("vo", "gpu")?;
        context.set_option("gpu-context", "d3d11")?;
        context.set_option("gpu-api", "d3d11")?;
        context.set_option("hwdec", "d3d11va")?;
        // 对所有编码尝试硬解，并由 software-fallback=no 阻止静默软件解码。
        context.set_option("hwdec-codecs", "all")?;
        context.set_option("hwdec-software-fallback", "no")?;
        context.set_option("audio", "no")?;
        context.set_option("ao", "null")?;
        context.set_option("idle", "yes")?;
        context.set_option("pause", "yes")?;
        // 不使用 mpv 的 loop-file=inf：该模式在解码失败时可能无限重试而不产生 END_FILE。
        // 循环由宿主在收到 EOF 事件后重新加载文件实现。
        context.set_option("loop-file", "no")?;
        context.set_option("image-display-duration", "inf")?;
        context.set_option("force-window", "yes")?;
        context.set_option("keep-open", "no")?;
        context.set_option("osc", "no")?;
        context.set_option("osd-level", "0")?;
        context.set_option("input-default-bindings", "no")?;
        context.set_option("config", "no")?;
        context.set_option("terminal", "no")?;
        context.set_option("msg-level", "all=warn")?;
        context.initialize()?;
        context.request_log("warn")?;
        for (name, format) in [
            ("hwdec-current", MPV_FORMAT_STRING),
            ("video-codec", MPV_FORMAT_STRING),
            ("width", MPV_FORMAT_INT64),
            ("height", MPV_FORMAT_INT64),
            ("container-fps", MPV_FORMAT_DOUBLE),
            ("video-params/pixelformat", MPV_FORMAT_STRING),
            ("pause", MPV_FORMAT_FLAG),
            ("idle-active", MPV_FORMAT_FLAG),
        ] {
            context.observe(name, format)?;
        }

        let observed = Arc::new(Mutex::new(ObservedState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_observed = Arc::clone(&observed);
        let thread_stop = Arc::clone(&stop);
        let notify = Arc::clone(&on_change);
        let thread_context = context.borrowed();
        let thread = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || {
                let context = thread_context;
                loop {
                    let event = context.wait_event(-1.0);
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if event.is_null() {
                        continue;
                    }
                    let mut changed = false;
                    // SAFETY: 事件指针在下一次 wait_event 前有效；只读取当前事件类型对应字段。
                    let event = unsafe { &*event };
                    match event.event_id {
                        MPV_EVENT_NONE => continue,
                        MPV_EVENT_SHUTDOWN => break,
                        MPV_EVENT_FILE_LOADED => {
                            let mut state = thread_observed
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            state.clear_for_load();
                            state.file_loaded = true;
                            state.end_reason = None;
                            state.read_properties(&context);
                            changed = true;
                        }
                        MPV_EVENT_END_FILE => {
                            let data = event.data as *const MpvEventEndFile;
                            if !data.is_null() {
                                // SAFETY: END_FILE 事件的 data 指向该结构。
                                let data = unsafe { &*data };
                                let mut state = thread_observed
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner());
                                state.file_loaded = false;
                                state.end_reason = Some(data.reason);
                                state.end_error =
                                    if data.reason == END_FILE_REASON_ERROR || data.error < 0 {
                                        Some(context.error_text(data.error))
                                    } else {
                                        None
                                    };
                                changed = true;
                            }
                        }
                        MPV_EVENT_PROPERTY_CHANGE => {
                            let data = event.data as *const MpvEventProperty;
                            if !data.is_null() {
                                // SAFETY: PROPERTY_CHANGE 事件的 data 指向该结构。
                                let data = unsafe { &*data };
                                // SAFETY: 属性名由 mpv 提供且以 NUL 结尾，只在当前事件内使用。
                                if let Some(name) = unsafe { cstr_opt(data.name) } {
                                    let mut state = thread_observed
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner());
                                    apply_property(&mut state, &name, data);
                                    changed = true;
                                }
                            }
                        }
                        MPV_EVENT_VIDEO_RECONFIG => {
                            let mut state = thread_observed
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            state.read_properties(&context);
                            changed = true;
                        }
                        MPV_EVENT_LOG_MESSAGE => {
                            let data = event.data as *const MpvEventLogMessage;
                            if !data.is_null() {
                                // SAFETY: LOG_MESSAGE 事件的 data 指向该结构。
                                let data = unsafe { &*data };
                                // SAFETY: 日志级别与文本由 mpv 提供且以 NUL 结尾，只在当前事件内使用。
                                let level = unsafe { cstr_opt(data.level) }.unwrap_or_default();
                                // SAFETY: 同上，复制到 String 后不再引用事件数据。
                                let text = unsafe { cstr_opt(data.text) }.unwrap_or_default();
                                if level.contains("error") || level.contains("fatal") {
                                    eprintln!("mpv[{level}]：{}", text.trim_end());
                                    let mut state = thread_observed
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner());
                                    state.last_log_error = Some(text);
                                }
                            }
                        }
                        _ => {}
                    }
                    if changed {
                        notify();
                    }
                }
            })
            .map_err(|error| MpvError::new(format!("创建 mpv 事件线程失败：{error}")))?;

        Ok(Self {
            context,
            observed,
            stop,
            thread: Some(thread),
        })
    }

    pub fn load(&self, path: &str, paused: bool) -> Result<(), MpvError> {
        {
            let mut state = self
                .observed
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.clear_for_load();
            state.paused = Some(paused);
        }
        self.context
            .set_property_string("pause", if paused { "yes" } else { "no" })?;
        self.context.set_property_string("loop-file", "no")?;
        self.context.command(&["loadfile", path, "replace"])
    }

    pub fn set_pause(&self, paused: bool) -> Result<(), MpvError> {
        self.context
            .set_property_string("pause", if paused { "yes" } else { "no" })
    }

    /// 设置视频/图片回退路径的缩放方式；在 `load` 前调用。
    pub fn set_fit(&self, fit: FitMode) -> Result<(), MpvError> {
        let (panscan, keepaspect) = match fit {
            // 平铺对视频没有等价语义，退化为铺满。
            FitMode::Cover | FitMode::Tile => ("1.0", "yes"),
            FitMode::Contain => ("0.0", "yes"),
            FitMode::Stretch => ("0.0", "no"),
        };
        self.context.set_property_string("panscan", panscan)?;
        self.context.set_property_string("keepaspect", keepaspect)
    }

    pub fn observed(&self) -> ObservedState {
        self.observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn get_number(&self, name: &str) -> Option<u64> {
        self.context
            .get_string(name)
            .and_then(|value| value.parse().ok())
    }

    /// 视频播放位置（毫秒）；图片、空闲或未加载时返回 `None`。
    pub fn time_position_ms(&self) -> Option<u64> {
        self.context
            .get_string("time-pos")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| (seconds * 1000.0).round() as u64)
    }

    pub fn last_log_error(&self) -> Option<String> {
        self.observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last_log_error
            .clone()
    }
}

#[repr(C)]
struct MpvEventLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

unsafe fn cstr_opt(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        None
    } else {
        // SAFETY: mpv 事件中的字符串以 NUL 结尾且只在事件有效期内使用。
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn apply_property(state: &mut ObservedState, name: &str, data: &MpvEventProperty) {
    match name {
        "hwdec-current" => state.hwdec_current = property_string(data),
        "video-codec" => state.codec = property_string(data),
        "width" => state.width = property_int(data).map(|value| value.max(0) as u32),
        "height" => state.height = property_int(data).map(|value| value.max(0) as u32),
        "container-fps" => {
            state.fps_milli = property_double(data).map(|fps| (fps * 1000.0).round() as u32);
        }
        "video-params/pixelformat" => state.pixel_format = property_string(data),
        "pause" => state.paused = property_flag(data),
        "idle-active" => state.idle_active = property_flag(data),
        _ => {}
    }
}

fn property_string(data: &MpvEventProperty) -> Option<String> {
    if data.format != MPV_FORMAT_STRING || data.data.is_null() {
        return None;
    }
    // SAFETY: STRING 属性的 data 指向 char* 指针。
    let pointer = unsafe { *(data.data as *const *const c_char) };
    // SAFETY: 复制的 char* 指向 mpv 提供的 NUL 结尾字符串。
    unsafe { cstr_opt(pointer) }
}

fn property_int(data: &MpvEventProperty) -> Option<i64> {
    if data.format != MPV_FORMAT_INT64 || data.data.is_null() {
        return None;
    }
    // SAFETY: INT64 属性的 data 指向 int64。
    Some(unsafe { *(data.data as *const i64) })
}

fn property_double(data: &MpvEventProperty) -> Option<f64> {
    if data.format != MPV_FORMAT_DOUBLE || data.data.is_null() {
        return None;
    }
    // SAFETY: DOUBLE 属性的 data 指向 double。
    Some(unsafe { *(data.data as *const f64) })
}

fn property_flag(data: &MpvEventProperty) -> Option<bool> {
    if data.format != MPV_FORMAT_FLAG || data.data.is_null() {
        return None;
    }
    // SAFETY: FLAG 属性的 data 指向 int。
    Some(unsafe { *(data.data as *const c_int) } != 0)
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.context.wakeup();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Context 随后释放底层 mpv 上下文，关闭 VO 子窗口并释放解码资源。
    }
}

/// D3D11 render API 会话（M5 互操作原型）。
///
/// mpv 在创建时只借用传入的设备；渲染时把一帧直接写入调用方提供的
/// `ID3D11Texture2D`，不经过 GPU→CPU→GPU 往返。纹理所有权始终在调用方，
/// mpv 在 `render` 返回前把工作提交到立即上下文，不保留纹理引用。
pub struct RenderSession {
    context: Context,
    render: *mut c_void,
    update: Arc<WaitEvent>,
    callback_data: *mut Arc<WaitEvent>,
    library: Arc<MpvLibrary>,
}

// SAFETY: render API 允许从任意线程调用，但本原型只在宿主主线程使用；
// 事件与回调数据在 Drop 前保持有效。
unsafe impl Send for RenderSession {}

impl RenderSession {
    pub fn new(library: &Arc<MpvLibrary>, device: *mut c_void) -> Result<Self, MpvError> {
        let create = library.render_context_create.ok_or_else(|| {
            MpvError::new("libmpv 缺少 D3D11 render API：mpv_render_context_create")
        })?;
        let _render_fn = library
            .render_context_render
            .ok_or_else(|| MpvError::new("libmpv 缺少 mpv_render_context_render"))?;
        let _update_fn = library
            .render_context_update
            .ok_or_else(|| MpvError::new("libmpv 缺少 mpv_render_context_update"))?;
        let _free_fn = library
            .render_context_free
            .ok_or_else(|| MpvError::new("libmpv 缺少 mpv_render_context_free"))?;

        let context = Context::new(library)?;
        context.set_option("vo", "libmpv")?;
        context.set_option("hwdec", "d3d11va")?;
        context.set_option("hwdec-codecs", "all")?;
        context.set_option("hwdec-software-fallback", "no")?;
        context.set_option("audio", "no")?;
        context.set_option("ao", "null")?;
        context.set_option("idle", "yes")?;
        context.set_option("pause", "no")?;
        context.set_option("loop-file", "no")?;
        context.set_option("config", "no")?;
        context.set_option("terminal", "no")?;
        context.set_option("msg-level", "all=warn")?;
        context.initialize()?;

        let api = c"d3d11".as_ptr();
        let init = MpvD3d11InitParams { device };
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: api as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_D3D11_INIT_PARAMS,
                data: &init as *const _ as *mut c_void,
            },
            MpvRenderParam {
                type_: 0,
                data: ptr::null_mut(),
            },
        ];
        let mut render = ptr::null_mut();
        // SAFETY: 参数数组以 type=0 结束；设备指针在创建期间有效，mpv 会自行增加引用。
        let code = unsafe { create(&mut render, context.raw, params.as_mut_ptr()) };
        if code < 0 {
            return Err(MpvError::new(format!(
                "创建 mpv D3D11 render 上下文失败：{}（{code}）",
                context.error_text(code)
            )));
        }
        if render.is_null() {
            return Err(MpvError::new("mpv_render_context_create 返回空上下文"));
        }

        let update = Arc::new(
            WaitEvent::new()
                .map_err(|error| MpvError::new(format!("创建渲染事件失败：{error}")))?,
        );
        let callback_data = Box::into_raw(Box::new(Arc::clone(&update)));
        if let Some(set_callback) = library.render_context_set_callback {
            // SAFETY: render 上下文有效；回调数据在 Drop 中释放。
            unsafe {
                set_callback(
                    render,
                    Some(render_update_callback),
                    callback_data as *mut c_void,
                );
            }
        }
        Ok(Self {
            context,
            render,
            update,
            callback_data,
            library: Arc::clone(library),
        })
    }

    pub fn load(&self, path: &str) -> Result<(), MpvError> {
        self.context.command(&["loadfile", path, "replace"])
    }

    /// 等待 mpv 的更新回调；事件为自动重置，超时返回 `false`。
    pub fn wait_update(&self, timeout: Duration) -> bool {
        self.update.wait(Some(timeout))
    }

    pub fn update_flags(&self) -> u64 {
        let Some(update) = self.library.render_context_update else {
            return 0;
        };
        // SAFETY: 上下文有效；只读取待处理更新标志。
        unsafe { update(self.render) }
    }

    pub fn has_frame(&self) -> bool {
        self.update_flags() & MPV_RENDER_UPDATE_FRAME != 0
    }

    /// 把当前视频帧渲染到调用方纹理；调用方负责随后 Present 或复制。
    pub fn render_frame(&self, texture: *mut c_void) -> Result<(), MpvError> {
        let render_fn = self
            .library
            .render_context_render
            .ok_or_else(|| MpvError::new("libmpv 缺少 mpv_render_context_render"))?;
        let fbo = MpvD3d11Fbo { tex: texture };
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_D3D11_FBO,
                data: &fbo as *const _ as *mut c_void,
            },
            MpvRenderParam {
                type_: 0,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: 纹理在同一设备创建，调用期间有效；mpv 不保留引用。
        let code = unsafe { render_fn(self.render, params.as_mut_ptr()) };
        if code < 0 {
            return Err(MpvError::new(format!(
                "mpv D3D11 渲染失败：{}（{code}）",
                self.context.error_text(code)
            )));
        }
        if let Some(report_swap) = self.library.render_context_report_swap {
            // SAFETY: 渲染成功后告知 mpv 目标已交换。
            unsafe { report_swap(self.render) };
        }
        Ok(())
    }

    pub fn property(&self, name: &str) -> Option<String> {
        self.context.get_string(name)
    }
}

impl Drop for RenderSession {
    fn drop(&mut self) {
        if let Some(set_callback) = self.library.render_context_set_callback {
            // SAFETY: 先撤销回调，再释放上下文与回调数据。
            unsafe { set_callback(self.render, None, ptr::null_mut()) };
        }
        if let Some(free) = self.library.render_context_free {
            // SAFETY: 上下文由本对象独占；free 会等待 mpv 完成。
            unsafe { free(self.render) };
        }
        if !self.callback_data.is_null() {
            // SAFETY: callback_data 由 new 中的 Box::into_raw 创建。
            unsafe { drop(Box::from_raw(self.callback_data)) };
            self.callback_data = ptr::null_mut();
        }
    }
}

extern "C" fn render_update_callback(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: data 指向 RenderSession 持有的 Box<Arc<WaitEvent>>，生命周期覆盖回调。
    let update = unsafe { &*(data as *const Arc<WaitEvent>) };
    update.set();
}
