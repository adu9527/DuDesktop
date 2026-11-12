//! M0 胶囊原型的窗口、命中区域、阴影层与事件驱动帧调度。

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    mem::size_of,
    rc::Rc,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateRoundRectRgn, EndPaint, EnumDisplayMonitors, GetMonitorInfoW, HBRUSH,
            HDC, HMONITOR, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
            PAINTSTRUCT, ScreenToClient, SetWindowRgn,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetFocus, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, TME_LEAVE, TRACKMOUSEEVENT,
                TrackMouseEvent, VK_ESCAPE, mouse_event,
            },
            WindowsAndMessaging::{
                CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GWLP_USERDATA, GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW,
                GetWindowLongPtrW, GetWindowRect, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW,
                IsWindow, KillTimer, LoadCursorW, MSG, PostQuitMessage, RegisterClassW,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                SetCursorPos, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                TranslateMessage, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
                WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SIZE, WM_TIMER,
                WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WindowFromPoint,
            },
        },
    },
    core::{BOOL, w},
};

use crate::capsule::{
    CAPSULE_HEIGHT, CAPSULE_WIDTH_MAX, CapsuleOptions, CompositionError, SHADOW_MARGIN_X,
    SHADOW_MARGIN_Y, SHADOW_WINDOW_HEIGHT, SHADOW_WINDOW_WIDTH, capsule_region,
    composition::{Composition, GpuContext, LayerRole},
};

const CLASS_NAME: windows::core::PCWSTR = w!("DeskShell.M0.Capsule");
const SHADOW_CLASS_NAME: windows::core::PCWSTR = w!("DeskShell.M0.Shadow");
const WINDOW_TITLE: windows::core::PCWSTR = w!("DeskShell M0 胶囊原型");
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_ANIMATION: usize = 1;
const TIMER_SELF_TEST: usize = 2;
const TIMER_EXIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfTestPhase {
    None,
    HoverAnimation,
    IdleStart,
    IdleVerify,
    ShadowClick,
    ShadowClickCheck,
    CapsuleClick,
    CapsuleClickCheck,
    Finish,
}

pub fn run(options: CapsuleOptions) -> Result<(), String> {
    let fatal = Rc::new(RefCell::new(None::<String>));
    // 队列地址稳定且由 run 独占所有权；回调仅短暂借用队列，不接触业务状态。
    let events = Box::new(RefCell::new(VecDeque::new()));
    let hwnd = create_window(events.as_ref())?;
    let mut state = WindowState::new(options, Rc::clone(&fatal));
    let result = state
        .initialize(hwnd)
        .and_then(|()| state.run_message_loop(&events));
    if options.input_test && state.cursor_saved {
        state.restore_cursor();
    }
    // SAFETY: 队列和状态保持存活直到所有窗口销毁；回调不拥有或释放它们。
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        if IsWindow(Some(state.shadow_hwnd)).as_bool() {
            let _ = DestroyWindow(state.shadow_hwnd);
        }
    }
    if let Some(error) = fatal.borrow_mut().take() {
        return Err(error);
    }
    result
}

enum WindowEvent {
    Paint,
    Size,
    Dpi(u32, RECT),
    MouseMove,
    MouseLeave,
    Timer(usize),
    Closed,
}

type EventQueue = RefCell<VecDeque<WindowEvent>>;

struct WindowState {
    hwnd: HWND,
    shadow_hwnd: HWND,
    dpi: u32,
    gpu: Option<Rc<GpuContext>>,
    capsule: Option<Composition>,
    shadow: Option<Composition>,
    progress: f32,
    target_progress: f32,
    animating: bool,
    last_tick: Instant,
    tracking_leave: bool,
    stats: FrameStats,
    total_presents: u64,
    region_size: (u32, u32, u32, i32),
    options: CapsuleOptions,
    self_test_phase: SelfTestPhase,
    self_test_presents: u64,
    cursor_before: POINT,
    cursor_saved: bool,
    self_test_finished: bool,
    fatal: Rc<RefCell<Option<String>>>,
}

#[derive(Default)]
struct FrameStats {
    frames: u32,
    total: Duration,
    peak: Duration,
}

impl WindowState {
    fn new(options: CapsuleOptions, fatal: Rc<RefCell<Option<String>>>) -> Self {
        Self {
            hwnd: HWND::default(),
            shadow_hwnd: HWND::default(),
            dpi: 96,
            gpu: None,
            capsule: None,
            shadow: None,
            progress: 0.0,
            target_progress: 0.0,
            animating: false,
            last_tick: Instant::now(),
            tracking_leave: false,
            stats: FrameStats::default(),
            total_presents: 0,
            region_size: (0, 0, 0, 0),
            options,
            self_test_phase: SelfTestPhase::None,
            self_test_presents: 0,
            cursor_before: POINT::default(),
            cursor_saved: false,
            self_test_finished: false,
            fatal,
        }
    }

    fn initialize(&mut self, hwnd: HWND) -> Result<(), String> {
        self.hwnd = hwnd;
        // SAFETY: 窗口由当前线程创建且句柄有效。
        self.dpi = unsafe { GetDpiForWindow(hwnd) };
        self.shadow_hwnd = create_shadow_window()?;
        self.layout_windows()?;

        let gpu = GpuContext::new().map_err(String::from)?;
        let (capsule_width, capsule_height) = self.client_size()?;
        let (shadow_width, shadow_height) = self.shadow_client_size()?;
        self.capsule = Some(
            Composition::new(
                Rc::clone(&gpu),
                hwnd,
                capsule_width,
                capsule_height,
                self.dpi,
                LayerRole::Capsule,
            )
            .map_err(String::from)?,
        );
        self.shadow = Some(
            Composition::new(
                Rc::clone(&gpu),
                self.shadow_hwnd,
                shadow_width,
                shadow_height,
                self.dpi,
                LayerRole::Shadow,
            )
            .map_err(String::from)?,
        );
        self.gpu = Some(gpu);

        self.apply_region();
        self.render_once()?;
        // SAFETY: 两层窗口句柄有效；首帧已提交，避免显示时出现空窗口。
        unsafe {
            let _ = ShowWindow(self.shadow_hwnd, SW_SHOWNOACTIVATE);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        if self.options.self_test {
            self.set_timer(TIMER_SELF_TEST, 700);
        }
        if let Some(seconds) = self.options.seconds {
            self.set_timer(
                TIMER_EXIT,
                (seconds.saturating_mul(1000)).min(u32::MAX as u64) as u32,
            );
        }
        Ok(())
    }

    fn layout_windows(&mut self) -> Result<(), String> {
        let scale = self.dpi as f32 / 96.0;
        let width = (CAPSULE_WIDTH_MAX * scale).round() as i32;
        let height = (CAPSULE_HEIGHT * scale).round() as i32;
        let work = self.work_area()?;
        let x = work.left + ((work.right - work.left) - width) / 2;
        let y = work.top + (12.0 * scale).round() as i32;
        // SAFETY: 只在当前窗口上调整位置与尺寸。
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| format!("定位胶囊窗口失败：{error}"))?;
        self.layout_shadow_window()
    }

    fn work_area(&self) -> Result<RECT, String> {
        let monitor = match self.options.monitor {
            Some(index) => monitor_handle(index).ok_or_else(|| {
                format!("未找到显示器索引 {index}，请使用 --monitor 检查可用数量")
            })?,
            // SAFETY: 坐标固定为原点，只查询主显示器句柄。
            None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
        };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: 显示器句柄来自系统查询；结构体已设置 cbSize。
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            Ok(info.rcWork)
        } else {
            Err("读取显示器工作区失败".into())
        }
    }

    fn layout_shadow_window(&self) -> Result<(), String> {
        if self.shadow_hwnd.is_invalid() || self.hwnd.is_invalid() {
            return Ok(());
        }
        let mut rect = RECT::default();
        // SAFETY: 胶囊窗口句柄有效。
        unsafe { GetWindowRect(self.hwnd, &mut rect) }
            .map_err(|error| format!("读取胶囊窗口位置失败：{error}"))?;
        let scale = self.dpi as f32 / 96.0;
        let width = (SHADOW_WINDOW_WIDTH * scale).round() as i32;
        let height = (SHADOW_WINDOW_HEIGHT * scale).round() as i32;
        let x = rect.left - (SHADOW_MARGIN_X * scale).round() as i32;
        let y = rect.top - (SHADOW_MARGIN_Y * scale).round() as i32;
        // SAFETY: 阴影窗口为独立顶层窗口，位置跟随胶囊。
        unsafe {
            SetWindowPos(
                self.shadow_hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| format!("定位阴影窗口失败：{error}"))
    }

    fn client_size(&self) -> Result<(u32, u32), String> {
        let mut rect = RECT::default();
        // SAFETY: 窗口句柄有效。
        unsafe { GetClientRect(self.hwnd, &mut rect) }
            .map_err(|error| format!("读取客户区失败：{error}"))?;
        Ok((
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        ))
    }

    fn shadow_client_size(&self) -> Result<(u32, u32), String> {
        let mut rect = RECT::default();
        // SAFETY: 阴影窗口句柄有效。
        unsafe { GetClientRect(self.shadow_hwnd, &mut rect) }
            .map_err(|error| format!("读取阴影客户区失败：{error}"))?;
        Ok((
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        ))
    }

    fn apply_region(&mut self) {
        let Ok((window_width, height)) = self.client_size() else {
            return;
        };
        let width = crate::capsule::capsule_width_px(self.progress, self.dpi);
        let key = (window_width, height, self.dpi, width);
        if self.region_size == key {
            return;
        }
        let (left, top, right, bottom) =
            capsule_region(window_width, height, self.progress, self.dpi);
        let diameter = bottom - top;
        // SAFETY: 创建并移交窗口区域；成功时由系统拥有，失败时由本函数删除。
        unsafe {
            let region = CreateRoundRectRgn(left, top, right + 1, bottom + 1, diameter, diameter);
            if region.is_invalid() {
                return;
            }
            if SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
                return;
            }
        }
        self.region_size = key;
    }

    fn on_size(&mut self) -> Result<(), String> {
        if self.hwnd.is_invalid() {
            return Ok(());
        }
        let (width, height) = self.client_size()?;
        self.apply_region();
        if let Some(capsule) = self.capsule.as_mut() {
            let result = capsule.resize(width, height, self.dpi);
            self.recover_graphics(result)?;
        }
        self.layout_shadow_window()?;
        let (shadow_width, shadow_height) = self.shadow_client_size()?;
        if let Some(shadow) = self.shadow.as_mut() {
            let result = shadow.resize(shadow_width, shadow_height, self.dpi);
            self.recover_graphics(result)?;
        }
        // SAFETY: 窗口句柄有效；请求重绘由 WM_PAINT 统一渲染。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
    }

    fn set_target(&mut self, target: f32) {
        if (self.target_progress - target).abs() < f32::EPSILON {
            return;
        }
        self.target_progress = target;
        self.last_tick = Instant::now();
        self.stats = FrameStats::default();
        self.animating = true;
        self.set_timer(TIMER_ANIMATION, 16);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().clamp(0.001, 0.05);
        self.last_tick = now;
        let delta = self.target_progress - self.progress;
        self.progress += delta * (1.0 - (-18.0 * dt).exp());
        if (self.target_progress - self.progress).abs() < 0.001 {
            self.progress = self.target_progress;
            self.animating = false;
            self.kill_timer(TIMER_ANIMATION);
            self.print_frame_stats();
            self.on_animation_settled();
        }
        self.apply_region();
        // SAFETY: 窗口句柄有效；重绘合并由系统完成。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn on_animation_settled(&mut self) {
        if self.self_test_phase == SelfTestPhase::HoverAnimation {
            // 收尾帧仍会触发一次 WM_PAINT；先等它完成再开始空闲窗口测量。
            self.self_test_phase = SelfTestPhase::IdleStart;
            self.set_timer(TIMER_SELF_TEST, 600);
        }
    }

    fn render_once(&mut self) -> Result<(), String> {
        let start = Instant::now();
        let result = self.render_layers();
        if result.is_err() {
            self.recover_graphics(result)?;
            // 每次事件只重试一次；重建后再次失败直接上报，避免恢复忙循环。
            self.render_layers().map_err(String::from)?;
        }
        self.record_frame(start.elapsed());
        Ok(())
    }

    fn recover_graphics(&mut self, result: Result<(), CompositionError>) -> Result<(), String> {
        match result {
            Ok(()) => Ok(()),
            Err(CompositionError::DeviceLost) => self.rebuild_gpu(),
            Err(CompositionError::TargetLost) => {
                let result = self.recreate_surfaces();
                match result {
                    Err(CompositionError::DeviceLost) => self.rebuild_gpu(),
                    other => other.map_err(String::from),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    fn recreate_surfaces(&mut self) -> Result<(), CompositionError> {
        if let Some(capsule) = self.capsule.as_mut() {
            capsule.recreate_surface()?;
        }
        if let Some(shadow) = self.shadow.as_mut() {
            shadow.recreate_surface()?;
        }
        Ok(())
    }

    fn render_layers(&mut self) -> Result<(), CompositionError> {
        let capsule = self
            .capsule
            .as_ref()
            .ok_or_else(|| CompositionError::Other("胶囊层缺失".into()))?;
        let shadow = self
            .shadow
            .as_ref()
            .ok_or_else(|| CompositionError::Other("阴影层缺失".into()))?;
        capsule.render(self.progress)?;
        self.total_presents += 1;
        shadow.render(self.progress)?;
        self.total_presents += 1;
        Ok(())
    }

    fn rebuild_gpu(&mut self) -> Result<(), String> {
        println!("重建共享 GPU 上下文与两层交换链");
        self.capsule = None;
        self.shadow = None;
        self.gpu = None;
        let gpu = GpuContext::new().map_err(String::from)?;
        let (capsule_width, capsule_height) = self.client_size()?;
        let (shadow_width, shadow_height) = self.shadow_client_size()?;
        self.capsule = Some(
            Composition::new(
                Rc::clone(&gpu),
                self.hwnd,
                capsule_width,
                capsule_height,
                self.dpi,
                LayerRole::Capsule,
            )
            .map_err(String::from)?,
        );
        self.shadow = Some(
            Composition::new(
                Rc::clone(&gpu),
                self.shadow_hwnd,
                shadow_width,
                shadow_height,
                self.dpi,
                LayerRole::Shadow,
            )
            .map_err(String::from)?,
        );
        self.gpu = Some(gpu);
        Ok(())
    }

    fn record_frame(&mut self, elapsed: Duration) {
        self.stats.frames += 1;
        self.stats.total += elapsed;
        if elapsed > self.stats.peak {
            self.stats.peak = elapsed;
        }
    }

    fn print_frame_stats(&self) {
        if self.stats.frames == 0 {
            return;
        }
        let average = self.stats.total.as_secs_f64() * 1000.0 / f64::from(self.stats.frames);
        println!(
            "动画结束：{} 帧，绘制/提交墙钟平均 {average:.2} ms，峰值 {:.2} ms，两层累计成功 Present {} 次",
            self.stats.frames,
            self.stats.peak.as_secs_f64() * 1000.0,
            self.total_presents
        );
    }

    fn handle_mouse_move(&mut self) {
        if !self.tracking_leave {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: 跟踪当前窗口的鼠标离开事件。
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            self.tracking_leave = true;
        }
        self.set_target(1.0);
    }

    fn handle_mouse_leave(&mut self) {
        self.tracking_leave = false;
        self.set_target(0.0);
    }

    fn handle_timer(&mut self, id: usize) {
        match id {
            TIMER_ANIMATION => self.tick(),
            TIMER_SELF_TEST => self.self_test_step(),
            TIMER_EXIT => {
                // SAFETY: 计时器到期，关闭当前原型窗口。
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                }
            }
            _ => {}
        }
    }

    fn self_test_step(&mut self) {
        match self.self_test_phase {
            SelfTestPhase::None => {
                if self.options.recovery_test {
                    let result = (|| {
                        self.recover_graphics(Err(CompositionError::TargetLost))?;
                        self.render_once()?;
                        self.recover_graphics(Err(CompositionError::DeviceLost))?;
                        self.render_once()
                    })();
                    if let Err(error) = result {
                        self.require_check(false, &format!("恢复路径自检失败：{error}"));
                        self.self_test_phase = SelfTestPhase::Finish;
                        return;
                    }
                    println!("恢复路径自检：目标重建、设备重建及后续双层呈现 PASS（合成错误注入）");
                }
                self.run_hit_test();
                self.self_test_phase = SelfTestPhase::HoverAnimation;
                self.set_target(1.0);
            }
            SelfTestPhase::IdleStart => {
                self.self_test_presents = self.total_presents;
                self.self_test_phase = SelfTestPhase::IdleVerify;
                self.set_timer(TIMER_SELF_TEST, 600);
            }
            SelfTestPhase::IdleVerify => {
                let idle_presents = self.total_presents - self.self_test_presents;
                let verdict = if idle_presents == 0 { "PASS" } else { "FAIL" };
                println!(
                    "空闲帧检查：动画结束并稳定后 600 ms 内 Present 增加 {idle_presents} 次 {verdict}"
                );
                self.require_check(idle_presents == 0, "空闲期间仍有 Present");
                if self.options.input_test {
                    self.begin_input_test();
                } else {
                    self.self_test_phase = SelfTestPhase::Finish;
                    self.set_timer(TIMER_SELF_TEST, 100);
                }
            }
            SelfTestPhase::ShadowClick => {
                let point = self.shadow_test_point();
                self.click_at(point);
                self.self_test_phase = SelfTestPhase::ShadowClickCheck;
                self.set_timer(TIMER_SELF_TEST, 400);
            }
            SelfTestPhase::ShadowClickCheck => {
                // SAFETY: 只读取系统前台窗口。
                let foreground = unsafe { GetForegroundWindow() };
                let pass = foreground != self.hwnd && foreground != self.shadow_hwnd;
                println!(
                    "真实点击穿透：阴影/留白区域点击后前台窗口未变为本程序 {}",
                    verdict(pass)
                );
                self.require_check(pass, "阴影点击激活了本程序");
                self.self_test_phase = SelfTestPhase::CapsuleClick;
                self.set_timer(TIMER_SELF_TEST, 100);
            }
            SelfTestPhase::CapsuleClick => {
                let point = self.center_point();
                self.click_at(point);
                self.self_test_phase = SelfTestPhase::CapsuleClickCheck;
                self.set_timer(TIMER_SELF_TEST, 400);
            }
            SelfTestPhase::CapsuleClickCheck => {
                // SAFETY: 只读取系统前台窗口。
                let foreground = unsafe { GetForegroundWindow() };
                // SAFETY: 只读取当前线程的焦点窗口。
                let focus = unsafe { GetFocus() };
                let pass = foreground == self.hwnd;
                println!(
                    "胶囊点击焦点：前台窗口为本程序 {}，键盘焦点为本程序 {}",
                    verdict(pass),
                    verdict(focus == self.hwnd)
                );
                self.require_check(pass && focus == self.hwnd, "胶囊点击未获得前台或键盘焦点");
                self.restore_cursor();
                self.self_test_phase = SelfTestPhase::Finish;
                self.set_timer(TIMER_SELF_TEST, 200);
            }
            SelfTestPhase::Finish => {
                self.self_test_finished = true;
                self.self_test_phase = SelfTestPhase::None;
                // SAFETY: 自检结束，关闭原型窗口。
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                }
            }
            SelfTestPhase::HoverAnimation => {}
        }
    }

    fn begin_input_test(&mut self) {
        // SAFETY: 保存用户鼠标位置，测试结束后恢复。
        unsafe {
            self.cursor_saved = GetCursorPos(&mut self.cursor_before).is_ok();
        }
        self.require_check(self.cursor_saved, "无法保存测试前鼠标位置");
        self.self_test_phase = SelfTestPhase::ShadowClick;
        self.set_timer(TIMER_SELF_TEST, 100);
    }

    fn center_point(&self) -> POINT {
        let mut rect = RECT::default();
        // SAFETY: 窗口句柄有效。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        POINT {
            x: (rect.left + rect.right) / 2,
            y: (rect.top + rect.bottom) / 2,
        }
    }

    fn shadow_test_point(&self) -> POINT {
        let mut rect = RECT::default();
        // SAFETY: 窗口句柄有效。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        POINT {
            x: (rect.left + rect.right) / 2,
            y: rect.bottom + 6,
        }
    }

    fn click_at(&self, point: POINT) {
        // SAFETY: 显式测试模式才移动真实鼠标；点击后立即抬起左键。
        unsafe {
            let _ = SetCursorPos(point.x, point.y);
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        }
    }

    fn restore_cursor(&self) {
        if !self.cursor_saved {
            return;
        }
        // SAFETY: 恢复测试前保存的鼠标位置。
        unsafe {
            let _ = SetCursorPos(self.cursor_before.x, self.cursor_before.y);
        }
    }

    fn run_hit_test(&self) {
        let mut rect = RECT::default();
        // SAFETY: 窗口句柄有效。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let center = POINT {
            x: (rect.left + rect.right) / 2,
            y: (rect.top + rect.bottom) / 2,
        };
        let outside = POINT {
            x: rect.left - 20,
            y: center.y,
        };
        let corner = POINT {
            x: rect.left + 2,
            y: rect.top + 2,
        };
        // 窗口按最大宽度创建；未展开时两侧留白应不拦截鼠标。
        let margin = POINT {
            x: rect.left + 4,
            y: center.y,
        };
        let shadow_point = self.shadow_test_point();
        // SAFETY: WindowFromPoint 是只读系统查询，不会激活或移动窗口。
        let center_hit = unsafe { WindowFromPoint(center) } == self.hwnd;
        // SAFETY: 同上。
        let outside_hit = unsafe { WindowFromPoint(outside) } != self.hwnd;
        // SAFETY: 同上。
        let corner_hit = unsafe { WindowFromPoint(corner) } != self.hwnd;
        // SAFETY: 同上。
        let margin_hit = unsafe { WindowFromPoint(margin) } != self.hwnd;
        // SAFETY: 同上。
        let shadow_hit = unsafe { WindowFromPoint(shadow_point) };
        let shadow_pass = shadow_hit != self.hwnd && shadow_hit != self.shadow_hwnd;
        println!(
            "命中测试：胶囊中心 -> {}，窗口外 -> {}，圆角外 -> {}，未展开留白 -> {}，阴影层不拦截 -> {}",
            verdict(center_hit),
            verdict(outside_hit),
            verdict(corner_hit),
            verdict(margin_hit),
            verdict(shadow_pass)
        );
        self.require_check(
            center_hit && outside_hit && corner_hit && margin_hit && shadow_pass,
            "窗口命中测试失败",
        );
    }

    fn on_dpi_changed(&mut self, dpi: u32, suggested: &RECT) -> Result<(), String> {
        self.dpi = dpi;
        let scale = dpi as f32 / 96.0;
        let width = (CAPSULE_WIDTH_MAX * scale).round() as i32;
        let height = (CAPSULE_HEIGHT * scale).round() as i32;
        // SAFETY: 采用系统建议的位置，尺寸按新 DPI 重新计算，保持胶囊比例。
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                suggested.left,
                suggested.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| format!("应用 DPI 变化失败：{error}"))?;
        self.on_size()
    }

    fn set_timer(&self, id: usize, milliseconds: u32) {
        // SAFETY: 计时器绑定当前窗口，消息由消息循环处理。
        unsafe {
            if SetTimer(Some(self.hwnd), id, milliseconds, None) == 0 {
                self.fatal
                    .borrow_mut()
                    .get_or_insert_with(|| "创建窗口计时器失败".into());
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }

    fn kill_timer(&self, id: usize) {
        // SAFETY: 与 set_timer 成对调用。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    fn require_check(&self, pass: bool, message: &str) {
        if !pass {
            self.fatal
                .borrow_mut()
                .get_or_insert_with(|| message.into());
        }
    }

    fn run_message_loop(&mut self, events: &EventQueue) -> Result<(), String> {
        let mut message = MSG::default();
        loop {
            loop {
                // 在处理事件前释放队列借用，允许 Win32 同步回调继续入队。
                let event = events.borrow_mut().pop_front();
                let Some(event) = event else { break };
                match event {
                    WindowEvent::Paint => self.render_once()?,
                    WindowEvent::Size => self.on_size()?,
                    WindowEvent::Dpi(dpi, rect) => self.on_dpi_changed(dpi, &rect)?,
                    WindowEvent::MouseMove => self.handle_mouse_move(),
                    WindowEvent::MouseLeave => self.handle_mouse_leave(),
                    WindowEvent::Timer(id) => self.handle_timer(id),
                    WindowEvent::Closed => return self.check_self_test_complete(),
                }
                // DestroyWindow 后不再处理此前排队的绘制/尺寸事件。
                // SAFETY: 只查询句柄是否仍有效。
                if !unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
                    return self.check_self_test_complete();
                }
            }
            // SAFETY: 标准 Win32 消息循环；窗口与计时器均由本线程创建。
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            if status == -1 {
                return Err(format!(
                    "读取窗口消息失败：{}",
                    windows::core::Error::from_win32()
                ));
            }
            if status == 0 {
                return self.check_self_test_complete();
            }
            // SAFETY: 消息结构由 GetMessageW 填充。
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    fn check_self_test_complete(&self) -> Result<(), String> {
        if self.options.self_test && !self.self_test_finished {
            Err("自检未完成，窗口已提前关闭".into())
        } else {
            Ok(())
        }
    }
}

fn monitor_handle(index: usize) -> Option<HMONITOR> {
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: 回调把显示器句柄追加到栈上向量，调用期间指针有效。
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<HMONITOR> as isize),
        );
    }
    monitors.get(index).copied()
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: 指针来自 monitor_handle 的栈上向量，枚举回调同步执行。
    unsafe {
        let monitors = &mut *(data.0 as *mut Vec<HMONITOR>);
        monitors.push(monitor);
    }
    TRUE
}

fn create_window(events: &EventQueue) -> Result<HWND, String> {
    // SAFETY: 模块句柄只读；类名与回调在进程生命周期内有效。
    unsafe {
        let instance =
            GetModuleHandleW(None).map_err(|error| format!("获取模块句柄失败：{error}"))?;
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW)
                .map_err(|error| format!("加载光标失败：{error}"))?,
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("注册胶囊窗口类失败".into());
        }
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            0,
            0,
            CAPSULE_WIDTH_MAX as i32,
            CAPSULE_HEIGHT as i32,
            None,
            None,
            Some(instance.into()),
            Some(events as *const EventQueue as *const c_void),
        )
        .map_err(|error| format!("创建胶囊窗口失败：{error}"))
    }
}

fn create_shadow_window() -> Result<HWND, String> {
    // SAFETY: 阴影窗口无用户数据；类名与回调在进程生命周期内有效。
    unsafe {
        let instance =
            GetModuleHandleW(None).map_err(|error| format!("获取模块句柄失败：{error}"))?;
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: SHADOW_CLASS_NAME,
            lpfnWndProc: Some(shadow_proc),
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("注册阴影窗口类失败".into());
        }
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP
                | WS_EX_TOOLWINDOW
                | WS_EX_TOPMOST
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE,
            SHADOW_CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            0,
            0,
            SHADOW_WINDOW_WIDTH as i32,
            SHADOW_WINDOW_HEIGHT as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|error| format!("创建阴影窗口失败：{error}"))
    }
}

unsafe fn enqueue(hwnd: HWND, event: WindowEvent) {
    // SAFETY: 指针由 WM_NCCREATE 写入，队列由 run 持有至窗口销毁后。
    // 回调同步执行，主循环不会持有跨 Win32 调用的队列借用。
    unsafe {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const EventQueue;
        if let Some(queue) = pointer.as_ref() {
            queue.borrow_mut().push_back(event);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 参数来自系统消息分发；队列在窗口销毁后才释放。
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = lparam.0 as *const CREATESTRUCTW;
                let pointer = (*create).lpCreateParams as *const EventQueue;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, pointer as isize);
                LRESULT(1)
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
                enqueue(hwnd, WindowEvent::Paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                enqueue(hwnd, WindowEvent::MouseMove);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                enqueue(hwnd, WindowEvent::MouseLeave);
                LRESULT(0)
            }
            WM_TIMER => {
                enqueue(hwnd, WindowEvent::Timer(wparam.0));
                LRESULT(0)
            }
            WM_SIZE => {
                enqueue(hwnd, WindowEvent::Size);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                let suggested = &*(lparam.0 as *const RECT);
                enqueue(hwnd, WindowEvent::Dpi(dpi, *suggested));
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let x = (lparam.0 as u32 & 0xffff) as u16 as i16 as i32;
                let y = ((lparam.0 as u32 >> 16) & 0xffff) as u16 as i16 as i32;
                let mut point = POINT { x, y };
                let _ = ScreenToClient(hwnd, &mut point);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let inside = point.x >= rect.left
                    && point.x < rect.right
                    && point.y >= rect.top
                    && point.y < rect.bottom;
                LRESULT(if inside {
                    HTCLIENT as isize
                } else {
                    HTTRANSPARENT as isize
                })
            }
            WM_KEYDOWN => {
                if wparam.0 == usize::from(VK_ESCAPE.0) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_ANIMATION);
                let _ = KillTimer(Some(hwnd), TIMER_SELF_TEST);
                let _ = KillTimer(Some(hwnd), TIMER_EXIT);
                enqueue(hwnd, WindowEvent::Closed);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe extern "system" fn shadow_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 阴影窗口无用户数据，仅响应基础绘制与命中消息。
    unsafe {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

fn verdict(pass: bool) -> &'static str {
    if pass { "PASS" } else { "FAIL" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_and_failed_self_tests_are_errors() {
        let fatal = Rc::new(RefCell::new(None));
        let mut state = WindowState::new(
            CapsuleOptions {
                self_test: true,
                ..Default::default()
            },
            Rc::clone(&fatal),
        );
        assert!(state.check_self_test_complete().is_err());
        state.self_test_finished = true;
        assert!(state.check_self_test_complete().is_ok());
        state.require_check(false, "first failure");
        state.require_check(false, "second failure");
        assert_eq!(fatal.borrow().as_deref(), Some("first failure"));
    }
}
