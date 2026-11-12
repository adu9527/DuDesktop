//! 设置窗窗口：无边框玻璃窗口、滚动、键盘导航与命令事件。

use std::{mem::size_of, rc::Rc};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{BeginPaint, CreateRoundRectRgn, EndPaint, ScreenToClient, SetWindowRgn},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END,
                VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
                VK_UP,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, HTCAPTION, HTCLIENT,
                IsWindow, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
                SetForegroundWindow, SetWindowPos, ShowWindow, SystemParametersInfoW, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WNDCLASSW,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::PCWSTR,
};

use super::{
    SettingsEvent, SettingsView, draw,
    layout::{self, DESIRED_HEIGHT, HEADER_HEIGHT, Layout, MIN_HEIGHT, RADIUS, WINDOW_WIDTH},
};
use crate::{
    gpu::Gpu,
    panel::shadow::ShadowWindow,
    surface::{CompositionTarget, RenderError},
};

const CLASS_NAME: &str = "DeskShell.Settings";
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_KEYDOWN: u32 = 0x0100;
const WM_CLOSE: u32 = 0x0010;
const WM_MOVE: u32 = 0x0003;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_NCHITTEST: u32 = 0x0084;

pub struct SettingsWindow {
    hwnd: HWND,
    gpu: Option<Rc<Gpu>>,
    target: Option<CompositionTarget>,
    view: SettingsView,
    layout: Layout,
    dpi: u32,
    scroll: f32,
    hover: Option<usize>,
    close_hover: bool,
    focus: Option<usize>,
    shadow: Option<ShadowWindow>,
    visible: bool,
    pending_error: Option<String>,
}

impl SettingsWindow {
    pub fn create(view: SettingsView) -> Result<Self, String> {
        register_class()?;
        let class: Vec<u16> = CLASS_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 窗口类已注册；先创建 1×1 隐藏窗口，再按实际 DPI 定位。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                PCWSTR(class.as_ptr()),
                PCWSTR(class.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                GetModuleHandleW(None)
                    .ok()
                    .map(|module| HINSTANCE(module.0)),
                None,
            )
        }
        .map_err(|error| format!("创建设置窗口失败：{error}"))?;
        // SAFETY: 查询新建窗口所在显示器的 DPI。
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let (width_dip, height_dip) = desired_size(dpi);
        let scale = dpi as f32 / 96.0;
        let width_px = (width_dip * scale).round() as i32;
        let height_px = (height_dip * scale).round() as i32;
        let work = work_area();
        let x = work.left + ((work.right - work.left) - width_px).max(0) / 2;
        let y = work.top + ((work.bottom - work.top) - height_px).max(0) / 2;
        // SAFETY: 句柄有效；设置最终尺寸与居中位置。
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width_px,
                height_px,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let layout = layout::compute(&view, width_dip, height_dip);
        let mut window = Self {
            hwnd,
            gpu: None,
            target: None,
            view,
            layout,
            dpi,
            scroll: 0.0,
            hover: None,
            close_hover: false,
            focus: None,
            shadow: None,
            visible: false,
            pending_error: None,
        };
        window.adopt_window_size()?;
        window.ensure_gpu()?;
        window.show();
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_view(&mut self, view: SettingsView) {
        self.view = view;
        let _ = self.adopt_window_size();
        let _ = self.render_frame();
    }

    pub fn show(&mut self) {
        // SAFETY: 句柄由本对象持有；显示并激活普通设置窗口。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.visible = true;
        self.ensure_shadow();
        let _ = self.render_frame();
    }

    pub fn hide(&mut self) {
        // SAFETY: 句柄由本对象持有。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
        if let Some(shadow) = &mut self.shadow {
            shadow.hide();
        }
    }

    pub fn window_screen_rect(&self) -> Option<(i32, i32, u32, u32)> {
        let mut rect = RECT::default();
        // SAFETY: 只读取窗口矩形。
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut rect) }
            .ok()?;
        Some((
            rect.left,
            rect.top,
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    pub fn handle_message(
        &mut self,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Vec<SettingsEvent> {
        match message {
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                self.update_hover(x, y);
                self.track_leave();
                Vec::new()
            }
            WM_MOUSELEAVE => {
                if self.hover.take().is_some() || self.close_hover {
                    self.close_hover = false;
                    let _ = self.render_frame();
                }
                Vec::new()
            }
            WM_LBUTTONUP => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                if self.layout.close.contains(x, y) {
                    return vec![SettingsEvent::Dismissed];
                }
                if let Some(index) = layout::hit(&self.layout, x, y, self.scroll) {
                    self.focus = Some(index);
                    let _ = self.render_frame();
                    return self.command_for(index).into_iter().collect();
                }
                Vec::new()
            }
            WM_MOUSEWHEEL => {
                let notches = ((wparam.0 >> 16) as u16 as i16) as f32 / 120.0;
                self.scroll_by(-notches * 72.0);
                Vec::new()
            }
            WM_PAINT => {
                // DComp 直接呈现；仍然验证绘制区避免消息堆积。
                // SAFETY: 标准 BeginPaint/EndPaint 配对。
                unsafe {
                    let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                    let _ = BeginPaint(self.hwnd, &mut paint);
                    let _ = EndPaint(self.hwnd, &paint);
                }
                Vec::new()
            }
            WM_SIZE => {
                let _ = self.adopt_window_size();
                let _ = self.render_frame();
                self.ensure_shadow();
                Vec::new()
            }
            WM_MOVE => {
                self.ensure_shadow();
                Vec::new()
            }
            WM_DPICHANGED => {
                self.on_dpi_changed(lparam);
                Vec::new()
            }
            0x007E => {
                // 显示器拓扑变化：重新按主工作区居中，避免窗口落在已移除的屏幕。
                let dpi = self.dpi.max(96);
                let (width_dip, height_dip) = desired_size(dpi);
                let width_px = (width_dip * self.scale()).round() as i32;
                let height_px = (height_dip * self.scale()).round() as i32;
                let work = work_area();
                let x = work.left + ((work.right - work.left) - width_px).max(0) / 2;
                let y = work.top + ((work.bottom - work.top) - height_px).max(0) / 2;
                // SAFETY: 句柄有效；只调整位置与尺寸。
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        x,
                        y,
                        width_px,
                        height_px,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                let _ = self.adopt_window_size();
                let _ = self.render_frame();
                self.ensure_shadow();
                Vec::new()
            }
            WM_KEYDOWN => self.key(wparam.0 as u32),
            WM_CLOSE => vec![SettingsEvent::Dismissed],
            0x001A | 0x0015 | 0x031A => {
                crate::theme::reload();
                let _ = self.render_frame();
                Vec::new()
            }
            _ => {
                // SAFETY: 非业务消息交回系统默认处理。
                unsafe {
                    let _ = DefWindowProcW(self.hwnd, message, wparam, lparam);
                }
                Vec::new()
            }
        }
    }

    fn on_dpi_changed(&mut self, lparam: LPARAM) {
        // SAFETY: 查询新 DPI 与系统提供的建议矩形。
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        self.dpi = dpi;
        let (width_dip, height_dip) = desired_size(dpi);
        let width_px = (width_dip * self.scale()).round() as i32;
        let height_px = (height_dip * self.scale()).round() as i32;
        let (left, top) = if lparam.0 != 0 {
            // SAFETY: WM_DPICHANGED 的 lParam 指向有效建议矩形。
            let rect = unsafe { &*(lparam.0 as *const RECT) };
            (rect.left, rect.top)
        } else {
            (0, 0)
        };
        // SAFETY: 句柄有效；只调整位置与尺寸。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                left,
                top,
                width_px,
                height_px,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let _ = self.adopt_window_size();
        let _ = self.render_frame();
        self.ensure_shadow();
    }

    fn key(&mut self, key: u32) -> Vec<SettingsEvent> {
        match key {
            value if value == VK_ESCAPE.0 as u32 => {
                return vec![SettingsEvent::Dismissed];
            }
            value if value == VK_TAB.0 as u32 => {
                // SAFETY: 只读取当前键盘状态。
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.move_focus(!shift);
            }
            value if value == VK_DOWN.0 as u32 => self.move_focus(true),
            value if value == VK_UP.0 as u32 => self.move_focus(false),
            value if value == (VK_RETURN.0 as u32) || value == VK_SPACE.0 as u32 => {
                if let Some(index) = self.focus {
                    return self.command_for(index).into_iter().collect();
                }
            }
            value if value == VK_NEXT.0 as u32 => {
                self.scroll_by(self.layout.viewport.height() * 0.8);
            }
            value if value == VK_PRIOR.0 as u32 => {
                self.scroll_by(-self.layout.viewport.height() * 0.8);
            }
            value if value == VK_HOME.0 as u32 => self.scroll_by(-self.layout.content_height),
            value if value == VK_END.0 as u32 => self.scroll_by(self.layout.content_height),
            _ => {}
        }
        Vec::new()
    }

    fn command_for(&self, index: usize) -> Option<SettingsEvent> {
        let entry = self.layout.items.get(index)?;
        let item = self.view.groups.get(entry.group)?.items.get(entry.item)?;
        if !item.enabled {
            return None;
        }
        Some(SettingsEvent::Command(item.command))
    }

    fn move_focus(&mut self, forward: bool) {
        let count = self.layout.items.len();
        if count == 0 {
            return;
        }
        let next = match self.focus {
            None => 0,
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
        };
        self.focus = Some(next);
        self.reveal(next);
        let _ = self.render_frame();
    }

    fn reveal(&mut self, index: usize) {
        let Some(entry) = self.layout.items.get(index) else {
            return;
        };
        let viewport_top = self.layout.viewport.top + self.scroll;
        let viewport_bottom = viewport_top + self.layout.viewport.height();
        if entry.rect.top < viewport_top {
            self.scroll = (entry.rect.top - self.layout.viewport.top - 8.0).max(0.0);
        } else if entry.rect.bottom > viewport_bottom {
            self.scroll =
                (entry.rect.bottom - self.layout.viewport.top - self.layout.viewport.height()
                    + 8.0)
                    .clamp(0.0, self.layout.scroll_max());
        }
    }

    fn scroll_by(&mut self, delta: f32) {
        self.scroll = (self.scroll + delta).clamp(0.0, self.layout.scroll_max());
        let _ = self.render_frame();
    }

    fn update_hover(&mut self, x: f32, y: f32) {
        let hover = layout::hit(&self.layout, x, y, self.scroll);
        let close_hover = self.layout.close.contains(x, y);
        if hover == self.hover && close_hover == self.close_hover {
            return;
        }
        self.hover = hover;
        self.close_hover = close_hover;
        let _ = self.render_frame();
    }

    fn track_leave(&self) {
        let mut event = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: 结构体有效；窗口句柄属于本线程。
        unsafe {
            let _ = TrackMouseEvent(&mut event);
        }
    }

    fn adopt_window_size(&mut self) -> Result<(), String> {
        self.dpi = {
            // SAFETY: 查询本窗口 DPI，失败时回退 96。
            let value = unsafe { GetDpiForWindow(self.hwnd) };
            if value == 0 { 96 } else { value }
        };
        let mut rect = RECT::default();
        // SAFETY: 只读取客户区尺寸。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        let width = (rect.right - rect.left) as f32 / self.scale();
        let height = (rect.bottom - rect.top) as f32 / self.scale();
        self.layout = layout::compute(&self.view, width.max(320.0), height.max(MIN_HEIGHT));
        self.scroll = self.scroll.min(self.layout.scroll_max());
        self.apply_region()?;
        Ok(())
    }

    fn apply_region(&self) -> Result<(), String> {
        let Some((_, _, width, height)) = self.window_screen_rect() else {
            return Ok(());
        };
        let diameter = (2.0 * RADIUS * self.scale()).round() as i32;
        // SAFETY: 创建区域后交给 SetWindowRgn；成功时系统接管所有权。
        let region = unsafe {
            CreateRoundRectRgn(
                0,
                0,
                width as i32 + 1,
                height as i32 + 1,
                diameter,
                diameter,
            )
        };
        if region.is_invalid() {
            return Err("创建设置窗口区域失败".into());
        }
        // SAFETY: 区域句柄有效；失败时仍需自行删除。
        let result = unsafe { SetWindowRgn(self.hwnd, Some(region), true) };
        if result == 0 {
            // SAFETY: SetWindowRgn 失败时所有权未转移。
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                    windows::Win32::Graphics::Gdi::HGDIOBJ(region.0),
                );
            }
            return Err("设置窗口区域失败".into());
        }
        Ok(())
    }

    fn ensure_gpu(&mut self) -> Result<(), String> {
        if self.gpu.is_none() {
            self.gpu = Some(Gpu::new().map_err(String::from)?);
        }
        if self.target.is_none() {
            let Some((_, _, width, height)) = self.window_screen_rect() else {
                return Err("设置窗口尺寸不可用".into());
            };
            let gpu = Rc::clone(self.gpu.as_ref().expect("GPU 已创建"));
            self.target = Some(
                CompositionTarget::new(gpu, self.hwnd, width, height, self.dpi)
                    .map_err(String::from)?,
            );
        }
        Ok(())
    }

    fn render_frame(&mut self) -> Result<(), String> {
        self.render_frame_once(true)
    }

    fn render_frame_once(&mut self, retry: bool) -> Result<(), String> {
        self.ensure_gpu()?;
        let Some((_, _, width, height)) = self.window_screen_rect() else {
            return Ok(());
        };
        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };
        target
            .resize(width.max(1), height.max(1), self.dpi)
            .map_err(String::from)?;
        let view = &self.view;
        let layout = &self.layout;
        let state = draw::DrawState {
            scroll: self.scroll,
            hover: self.hover,
            close_hover: self.close_hover,
            focus: self.focus,
        };
        let result = target.render(|gpu, surface| draw::draw(gpu, surface, view, layout, state));
        match result {
            Ok(()) => Ok(()),
            Err(RenderError::TargetLost) => {
                target.recreate_surface().map_err(String::from)?;
                target
                    .render(|gpu, surface| draw::draw(gpu, surface, view, layout, state))
                    .map_err(String::from)
            }
            Err(RenderError::DeviceLost) if retry => {
                self.target = None;
                self.gpu = None;
                self.render_frame_once(false)
            }
            Err(RenderError::DeviceLost) => Err("图形设备丢失".into()),
            Err(RenderError::Other(message)) => {
                self.pending_error = Some(message.clone());
                Err(message)
            }
        }
    }

    fn ensure_shadow(&mut self) {
        if self.shadow.is_none() {
            match ShadowWindow::create() {
                Ok(shadow) => self.shadow = Some(shadow),
                Err(error) => {
                    self.pending_error = Some(error);
                    return;
                }
            }
        }
        let Some((left, top, width, height)) = self.window_screen_rect() else {
            return;
        };
        let radius = (RADIUS * self.scale()).round() as i32;
        if let Some(shadow) = &mut self.shadow
            && let Err(error) =
                shadow.show_below(self.hwnd, (left, top, width, height), radius, self.dpi)
        {
            self.pending_error = Some(error);
            shadow.hide();
        }
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // SAFETY: 窗口由本对象创建；阴影随字段释放。
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn register_class() -> Result<(), String> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            let class: Vec<u16> = CLASS_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: 只注册本进程的设置窗口类。
            let module = unsafe { GetModuleHandleW(None) }
                .map_err(|error| format!("获取模块句柄失败：{error}"))?;
            // SAFETY: 类名在注册调用期间有效。
            let atom = unsafe {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(settings_proc),
                    hInstance: HINSTANCE(module.0),
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                })
            };
            if atom == 0 {
                return Err(format!(
                    "注册设置窗口类失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })
        .clone()
}

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCHITTEST {
        // 页头可拖动，关闭按钮与内容区保持正常点击。
        // SAFETY: 查询窗口 DPI。
        let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
        let mut point = POINT {
            x: (lparam.0 & 0xFFFF) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
        };
        // SAFETY: 转换屏幕坐标到客户区。
        unsafe {
            let _ = ScreenToClient(hwnd, &mut point);
        }
        let mut rect = RECT::default();
        // SAFETY: 只读取客户区。
        unsafe {
            let _ = GetClientRect(hwnd, &mut rect);
        }
        let header = (HEADER_HEIGHT * scale) as i32;
        let close_left = rect.right - (56.0 * scale) as i32;
        if point.y < header && point.x < close_left {
            return LRESULT(HTCAPTION as isize);
        }
        return LRESULT(HTCLIENT as isize);
    }
    // SAFETY: 默认窗口过程由系统调用。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn desired_size(dpi: u32) -> (f32, f32) {
    let work = work_area();
    let scale = dpi.max(96) as f32 / 96.0;
    let max_width = (work.right - work.left) as f32 / scale - 32.0;
    let max_height = (work.bottom - work.top) as f32 / scale - 48.0;
    let width = WINDOW_WIDTH.min(max_width.max(520.0));
    let height = DESIRED_HEIGHT
        .min(max_height)
        .max(MIN_HEIGHT.min(max_height));
    (width, height)
}

fn work_area() -> RECT {
    let mut rect = RECT::default();
    // SAFETY: 输出结构有效；查询主显示器工作区。
    unsafe {
        let _ = SystemParametersInfoW(
            windows::Win32::UI::WindowsAndMessaging::SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut core::ffi::c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 800,
        };
    }
    rect
}
