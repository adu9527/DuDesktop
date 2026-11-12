//! 小部件窗口：无边框玻璃层、拖动、右边缘调宽与 DPI。

use std::{mem::size_of, rc::Rc};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, ScreenToClient, SetWindowRgn,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
                GetWindowRect, HTCAPTION, HTCLIENT, IsWindow, RegisterClassW, SW_HIDE,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
                SystemParametersInfoW, WM_CLOSE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WNDCLASSW,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::PCWSTR,
};

use super::{
    WidgetEvent, WidgetSpec, WidgetView,
    draw::{self, WidgetIcons},
    layout::{self, Hit, Layout, RADIUS},
};
use crate::{
    gpu::Gpu,
    panel::shadow::ShadowWindow,
    surface::{CompositionTarget, RenderError},
};

const CLASS_NAME: &str = "DeskShell.Widgets";
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_MOVE: u32 = 0x0003;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_DISPLAYCHANGE: u32 = 0x007E;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const RESIZE_EDGE: f32 = 8.0;

pub struct WidgetWindow {
    hwnd: HWND,
    gpu: Option<Rc<Gpu>>,
    target: Option<CompositionTarget>,
    view: WidgetView,
    width: f32,
    layout: Layout,
    dpi: u32,
    hover: Option<Hit>,
    resize: Option<(i32, f32)>,
    slider_drag: bool,
    icons: WidgetIcons,
    shadow: Option<ShadowWindow>,
    visible: bool,
    pending_error: Option<String>,
}

impl WidgetWindow {
    pub fn create(spec: WidgetSpec, view: WidgetView) -> Result<Self, String> {
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
        .map_err(|error| format!("创建小部件窗口失败：{error}"))?;
        // SAFETY: 查询新建窗口 DPI。
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let layout = layout::compute(&view, spec.width);
        let mut window = Self {
            hwnd,
            gpu: None,
            target: None,
            view,
            width: layout.width,
            layout,
            dpi,
            hover: None,
            resize: None,
            slider_drag: false,
            icons: WidgetIcons::new(),
            shadow: None,
            visible: false,
            pending_error: None,
        };
        window.place(spec.x, spec.y);
        window.apply_region()?;
        window.ensure_gpu()?;
        window.render_frame()?;
        window.show();
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn kind(&self) -> super::WidgetKind {
        self.view.kind
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    /// 更新数据；高度随内容变化，位置保持不变。
    pub fn set_view(&mut self, view: WidgetView) {
        if view.kind != self.view.kind {
            return;
        }
        self.view = view;
        self.relayout();
        let _ = self.render_frame();
    }

    pub fn show(&mut self) {
        // SAFETY: 句柄由本对象持有；显示但不激活。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
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

    pub fn handle_message(
        &mut self,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Vec<WidgetEvent> {
        let scale = self.scale();
        match message {
            WM_MOUSEMOVE => {
                if self.resize.is_some() {
                    self.resize_to_cursor();
                    return Vec::new();
                }
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                if self.slider_drag {
                    let volume = self.layout.volume_at(x);
                    if volume != self.view.controls.volume {
                        self.view.controls.volume = volume;
                        self.view.controls.muted = false;
                        let _ = self.render_frame();
                        return vec![WidgetEvent::VolumeChanged(volume)];
                    }
                    return Vec::new();
                }
                self.update_hover(x, y);
                self.track_leave();
                Vec::new()
            }
            WM_MOUSELEAVE => {
                if self.hover.take().is_some() {
                    let _ = self.render_frame();
                }
                Vec::new()
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                if x >= layout::width_of(&self.view, self.width) - RESIZE_EDGE {
                    let mut point = POINT::default();
                    // SAFETY: 只读取光标位置。
                    unsafe {
                        let _ = GetCursorPos(&mut point);
                    }
                    self.resize = Some((point.x, self.width));
                    // SAFETY: 捕获鼠标，直到松开左键。
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                    return Vec::new();
                }
                if self.layout.hit(x, y) == Some(Hit::Slider) {
                    self.slider_drag = true;
                    // SAFETY: 捕获鼠标，拖动期间持续更新音量。
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                    let volume = self.layout.volume_at(x);
                    self.view.controls.volume = volume;
                    self.view.controls.muted = false;
                    let _ = self.render_frame();
                    return vec![WidgetEvent::VolumeChanged(volume)];
                }
                Vec::new()
            }
            WM_LBUTTONUP => {
                if self.resize.take().is_some() {
                    // SAFETY: 与 SetCapture 配对。
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return vec![WidgetEvent::Resized { width: self.width }];
                }
                if self.slider_drag {
                    self.slider_drag = false;
                    // SAFETY: 与 SetCapture 配对。
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return vec![WidgetEvent::VolumeChanged(self.view.controls.volume)];
                }
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                self.click(x, y)
            }
            WM_RBUTTONUP => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
                if let Some(Hit::Item(index)) = self.layout.hit(x, y)
                    && let Some(item) = self.view.organizer.get(index)
                {
                    return vec![WidgetEvent::ItemMenu(item.id.clone())];
                }
                Vec::new()
            }
            WM_EXITSIZEMOVE => {
                let mut rect = RECT::default();
                // SAFETY: 只读取窗口位置。
                unsafe {
                    let _ = GetWindowRect(self.hwnd, &mut rect);
                }
                vec![WidgetEvent::Moved {
                    x: rect.left,
                    y: rect.top,
                }]
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
                let _ = self.adopt_dpi();
                let _ = self.render_frame();
                self.ensure_shadow();
                Vec::new()
            }
            WM_MOVE => {
                self.ensure_shadow();
                Vec::new()
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                // SAFETY: 只查询本窗口 DPI。
                self.dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
                self.relayout();
                let _ = self.apply_region();
                let _ = self.render_frame();
                self.ensure_shadow();
                Vec::new()
            }
            WM_CLOSE => vec![WidgetEvent::Dismissed],
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

    fn click(&mut self, x: f32, y: f32) -> Vec<WidgetEvent> {
        match self.layout.hit(x, y) {
            Some(Hit::Close) => vec![WidgetEvent::Dismissed],
            Some(Hit::Previous) => vec![WidgetEvent::PreviousMusic],
            Some(Hit::Play) => vec![WidgetEvent::ToggleMusic],
            Some(Hit::Next) => vec![WidgetEvent::NextMusic],
            Some(Hit::Mute) => vec![WidgetEvent::ToggleMute],
            Some(Hit::Wifi) => vec![WidgetEvent::Wifi],
            Some(Hit::Bluetooth) => vec![WidgetEvent::Bluetooth],
            Some(Hit::Button(index)) => match index {
                0 => vec![WidgetEvent::OpenSettings],
                1 => vec![WidgetEvent::OpenDisplay],
                2 => vec![WidgetEvent::OpenSound],
                _ => vec![WidgetEvent::OpenNetwork],
            },
            Some(Hit::Item(index)) => self
                .view
                .organizer
                .get(index)
                .map(|item| vec![WidgetEvent::OpenItem(item.id.clone())])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn resize_to_cursor(&mut self) {
        let Some((start_x, start_width)) = self.resize else {
            return;
        };
        let mut point = POINT::default();
        // SAFETY: 只读取光标位置。
        unsafe {
            let _ = GetCursorPos(&mut point);
        }
        let width = start_width + (point.x - start_x) as f32 / self.scale();
        let width = width.clamp(220.0, 640.0);
        if (width - self.width).abs() < 0.5 {
            return;
        }
        self.width = width;
        self.relayout();
        let _ = self.render_frame();
    }

    fn update_hover(&mut self, x: f32, y: f32) {
        let hover = self.layout.hit(x, y);
        if hover == self.hover {
            return;
        }
        self.hover = hover;
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

    fn place(&mut self, x: i32, y: i32) {
        let (width, height) = self.pixel_size();
        let work = work_area();
        let max_x = (work.right - width as i32).max(work.left);
        let max_y = (work.bottom - height as i32).max(work.top);
        let x = x.clamp(work.left, max_x);
        let y = y.clamp(work.top, max_y);
        // SAFETY: 句柄有效；只调整位置与尺寸。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width as i32,
                height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// 布局变化后保持左上角不变，只调整宽高。
    fn relayout(&mut self) {
        self.layout = layout::compute(&self.view, self.width);
        let (width, height) = self.pixel_size();
        let mut rect = RECT::default();
        // SAFETY: 只读取窗口位置。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        // SAFETY: 句柄有效；只调整尺寸。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                rect.left,
                rect.top,
                width as i32,
                height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let _ = self.apply_region();
        let _ = self.adopt_dpi();
        self.ensure_shadow();
    }

    fn pixel_size(&self) -> (u32, u32) {
        let scale = self.scale();
        (
            (self.layout.width * scale).round().max(1.0) as u32,
            (self.layout.height * scale).round().max(1.0) as u32,
        )
    }

    fn adopt_dpi(&mut self) -> Result<(), String> {
        // SAFETY: 查询本窗口 DPI，失败时回退 96。
        let value = unsafe { GetDpiForWindow(self.hwnd) };
        self.dpi = if value == 0 { 96 } else { value };
        Ok(())
    }

    fn apply_region(&self) -> Result<(), String> {
        let mut rect = RECT::default();
        // SAFETY: 只读取窗口矩形。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let width = (rect.right - rect.left).max(1);
        let height = (rect.bottom - rect.top).max(1);
        let diameter = (2.0 * RADIUS * self.scale()).round() as i32;
        // SAFETY: 创建区域后交给 SetWindowRgn；成功时系统接管所有权。
        let region = unsafe { CreateRoundRectRgn(0, 0, width + 1, height + 1, diameter, diameter) };
        if region.is_invalid() {
            return Err("创建小部件区域失败".into());
        }
        // SAFETY: 区域句柄有效；失败时仍需自行删除。
        let result = unsafe { SetWindowRgn(self.hwnd, Some(region), true) };
        if result == 0 {
            // SAFETY: SetWindowRgn 失败时所有权未转移。
            unsafe {
                let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(region.0));
            }
            return Err("设置小部件区域失败".into());
        }
        Ok(())
    }

    fn ensure_gpu(&mut self) -> Result<(), String> {
        if self.gpu.is_none() {
            self.gpu = Some(Gpu::new().map_err(String::from)?);
        }
        if self.target.is_none() {
            let (width, height) = self.pixel_size();
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
        let (width, height) = self.pixel_size();
        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };
        target
            .resize(width, height, self.dpi)
            .map_err(String::from)?;
        let view = &self.view;
        let layout = &self.layout;
        let icons = &mut self.icons;
        let hover = self.hover;
        let result = target.render(|gpu, surface| {
            icons.sync(surface, view)?;
            draw::draw(gpu, surface, view, layout, icons, hover)
        });
        match result {
            Ok(()) => Ok(()),
            Err(RenderError::TargetLost) => {
                target.recreate_surface().map_err(String::from)?;
                target
                    .render(|gpu, surface| {
                        icons.sync(surface, view)?;
                        draw::draw(gpu, surface, view, layout, icons, hover)
                    })
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
        let mut rect = RECT::default();
        // SAFETY: 只读取窗口矩形。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let width = (rect.right - rect.left).max(1) as u32;
        let height = (rect.bottom - rect.top).max(1) as u32;
        let radius = (RADIUS * self.scale()).round() as i32;
        if let Some(shadow) = &mut self.shadow
            && let Err(error) = shadow.show_below(
                self.hwnd,
                (rect.left, rect.top, width, height),
                radius,
                self.dpi,
            )
        {
            self.pending_error = Some(error);
            shadow.hide();
        }
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

impl Drop for WidgetWindow {
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
            // SAFETY: 只注册本进程的小部件窗口类。
            let module = unsafe { GetModuleHandleW(None) }
                .map_err(|error| format!("获取模块句柄失败：{error}"))?;
            // SAFETY: 类名在注册调用期间有效。
            let atom = unsafe {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(widget_proc),
                    hInstance: HINSTANCE(module.0),
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                })
            };
            if atom == 0 {
                return Err(format!(
                    "注册小部件窗口类失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })
        .clone()
}

unsafe extern "system" fn widget_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCHITTEST {
        // SAFETY: 查询窗口 DPI 与客户区坐标。
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
        let width = rect.right as f32 / scale;
        let x = point.x as f32 / scale;
        let y = point.y as f32 / scale;
        // 右边缘留给调宽；页头左侧可拖动，关闭按钮与内容区保持点击。
        if x >= width - RESIZE_EDGE {
            return LRESULT(HTCLIENT as isize);
        }
        if y < layout::HEADER && x < width - 40.0 {
            return LRESULT(HTCAPTION as isize);
        }
        return LRESULT(HTCLIENT as isize);
    }
    // SAFETY: 默认窗口过程由系统调用。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
