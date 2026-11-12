//! 面板窗口：位置、区域裁剪、DPI、命中输入与事件驱动绘制。
//!
//! 消息模型：窗口过程只做系统默认处理；主程序消息循环把所有面板消息交给
//! `handle_message`，因此不存在跨线程窗口状态或回调重入。

use std::{mem::size_of, rc::Rc, time::Instant};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, EnumDisplayMonitors,
            GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
            MonitorFromWindow, PAINTSTRUCT, SetWindowRgn,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_LEFT,
                VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
            },
            WindowsAndMessaging::{
                CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow,
                GetWindowRect, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, IsWindow, KillTimer,
                LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
                SetTimer, SetWindowPos, ShowWindow, WM_ACTIVATE, WM_CLOSE, WM_DPICHANGED,
                WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::{BOOL, w},
};

use crate::{
    gpu::Gpu,
    panel::{
        IslandTab, PanelEvent, PanelHit, PanelKind, PanelOptions, PanelView,
        composition::PanelComposition,
        controls::{self, SearchBox},
        hit_test, panel_radius, panel_size,
        shadow::ShadowWindow,
        tooltip::TooltipWindow,
    },
    surface::RenderError,
};

const WM_MOUSELEAVE: u32 = 0x02A3;
/// 窗口过程把系统直接发送的 `WM_ACTIVATE` 转成自定义消息，交给消息循环处理。
const WM_PANEL_ACTIVATE: u32 = 0x8052;
/// 同理转发 `WM_DPICHANGED`；转发时丢弃建议矩形指针。
const WM_PANEL_DPICHANGED: u32 = 0x8053;
const TIMER_ANIMATION: usize = 1;
/// 悬停放大动画时长，仅在动画期间使用 16 ms 定时器。
const HOVER_DURATION_MS: f32 = 120.0;

pub struct PanelWindow {
    hwnd: HWND,
    kind: PanelKind,
    view: PanelView,
    options: PanelOptions,
    gpu: Option<Rc<Gpu>>,
    composition: Option<PanelComposition>,
    dpi: u32,
    visible: bool,
    elevated: bool,
    previous_foreground: HWND,
    hover: Option<usize>,
    hover_t: f32,
    hover_target: f32,
    animating: bool,
    last_tick: Instant,
    presents: u64,
    monitor_rect: RECT,
    work_rect: RECT,
    pending_error: Option<String>,
    shadow: Option<ShadowWindow>,
    shadows: bool,
    shadow_error: Option<String>,
    browse: Option<SearchBox>,
    search_focused: bool,
    tooltip: Option<TooltipWindow>,
}

impl PanelWindow {
    pub fn create(kind: PanelKind, view: PanelView, options: PanelOptions) -> Result<Self, String> {
        let class = register_class(kind)?;
        let (client_width, client_height) = pixel_size(&view, 96);
        // SAFETY: 窗口类已注册；创建隐藏的弹出窗口，稍后按 DPI 重新布局。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | if kind == PanelKind::Dock {
                        WS_EX_NOREDIRECTIONBITMAP
                    } else {
                        Default::default()
                    },
                class,
                w!("DeskShell"),
                WS_POPUP | windows::Win32::UI::WindowsAndMessaging::WS_CLIPCHILDREN,
                0,
                0,
                client_width as i32,
                client_height as i32,
                None,
                None,
                GetModuleHandleW(None)
                    .ok()
                    .map(|module| HINSTANCE(module.0)),
                None,
            )
        }
        .map_err(|error| format!("创建面板窗口失败：{error}"))?;
        let mut window = Self {
            hwnd,
            kind,
            view,
            options,
            gpu: None,
            composition: None,
            dpi: 96,
            visible: false,
            elevated: false,
            previous_foreground: HWND::default(),
            hover: None,
            hover_t: 0.0,
            hover_target: 0.0,
            animating: false,
            last_tick: Instant::now(),
            presents: 0,
            monitor_rect: monitor_rect(hwnd, options.monitor),
            work_rect: work_rect(hwnd, options.monitor),
            pending_error: None,
            shadow: None,
            shadows: false,
            shadow_error: None,
            browse: None,
            search_focused: false,
            tooltip: None,
        };
        if kind == PanelKind::Island {
            window.browse = Some(SearchBox::new(hwnd)?);
        }
        if kind == PanelKind::Dock {
            // 自绘提示窗点击穿透，不使用系统 STATIC 边框。
            window.tooltip = Some(TooltipWindow::create()?);
        }
        window.dpi = window.current_dpi();
        window.layout()?;
        window.ensure_gpu()?;
        window.render_frame()?;
        if options.visible {
            window.set_visible(true);
        }
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn kind(&self) -> PanelKind {
        self.kind
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn presents(&self) -> u64 {
        self.presents
    }

    pub fn view(&self) -> &PanelView {
        &self.view
    }

    /// 搜索、类型筛选与滚动共用的视图入口；索引始终对应原始业务列表。
    pub fn set_filter(&mut self, query: String, category: String) -> Result<(), String> {
        self.view.query = query;
        self.view.category = category;
        self.view.offset = 0;
        self.view.selected = None;
        if let Some(browse) = &self.browse {
            browse.set_text(&self.view.query);
        }
        self.layout()?;
        self.render_frame()
    }

    pub fn scroll_rows(&mut self, delta: i32) {
        self.scroll(delta);
    }

    pub fn pending_error(&self) -> Option<&str> {
        self.pending_error.as_deref()
    }

    pub fn set_view(&mut self, view: PanelView) -> Result<(), String> {
        if view.kind != self.kind {
            return Err("面板视图类型与窗口类型不一致".into());
        }
        let size_changed = panel_size(&self.view) != panel_size(&view);
        let was_expanded = self.view.expanded;
        let mut view = view;
        view.query = self.view.query.clone();
        view.category = self.view.category.clone();
        view.offset = self.view.offset;
        view.island_tab = self.view.island_tab;
        self.view = view;
        if let Some(selected) = self.view.selected
            && selected >= self.view.items.len()
        {
            self.view.selected = self.view.items.len().checked_sub(1);
        }
        if self.kind == PanelKind::Dock && !self.view.fisheye {
            // 关闭动画时立即归位并停止 16 ms 定时器，不等插值自然结束。
            self.animating = false;
            self.hover = None;
            self.hover_t = 0.0;
            self.hover_target = 0.0;
            // SAFETY: 只停止本窗口的动画定时器。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATION);
            }
        }
        if size_changed || was_expanded != self.view.expanded || self.kind == PanelKind::Island {
            self.layout()?;
        }
        self.render_frame()
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        // SAFETY: 窗口句柄由本对象持有。
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        self.visible = visible;
        if !visible {
            // SAFETY: 自有提示窗随面板隐藏。
            if let Some(tooltip) = &mut self.tooltip {
                tooltip.hide();
            }
            self.animating = false;
            self.hover = None;
            self.hover_t = 0.0;
            self.hover_target = 0.0;
            // SAFETY: 窗口隐藏时停止专属动画唤醒。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATION);
            }
        } else if let Err(error) = self.render_frame() {
            self.pending_error = Some(error);
        }
        self.sync_shadow();
    }

    /// 开关面板阴影；关闭时销毁分层窗口与位图，释放专属资源。
    pub fn set_shadows(&mut self, enabled: bool) {
        if !enabled {
            self.shadows = false;
            self.shadow = None;
            return;
        }
        self.shadows = true;
        if self.shadow.is_none() {
            match ShadowWindow::create() {
                Ok(shadow) => {
                    self.shadow = Some(shadow);
                    self.shadow_error = None;
                }
                Err(error) => {
                    self.shadow_error = Some(error);
                    self.shadows = false;
                }
            }
        }
        self.sync_shadow();
    }

    pub fn shadow_ready(&self) -> bool {
        self.shadows && self.shadow.is_some()
    }

    pub fn shadow_error(&self) -> Option<&str> {
        self.shadow_error.as_deref()
    }

    pub fn shadow_hwnd(&self) -> Option<HWND> {
        self.shadow
            .as_ref()
            .filter(|shadow| shadow.is_alive())
            .map(|shadow| shadow.hwnd())
    }

    fn sync_shadow(&mut self) {
        if !self.shadows {
            if let Some(shadow) = &mut self.shadow {
                shadow.hide();
            }
            return;
        }
        let Some(rect) = self.window_screen_rect() else {
            if let Some(shadow) = &mut self.shadow {
                shadow.hide();
            }
            return;
        };
        let (width, height) = pixel_size(&self.view, self.dpi);
        let radius = (panel_radius(&self.view) * self.scale()).round() as i32;
        let shadow = match self.shadow.as_mut() {
            Some(shadow) if self.visible => shadow,
            _ => {
                if let Some(shadow) = &mut self.shadow {
                    shadow.hide();
                }
                return;
            }
        };
        if let Err(error) =
            shadow.show_below(self.hwnd, (rect.0, rect.1, width, height), radius, self.dpi)
        {
            self.shadow_error = Some(error);
            shadow.hide();
        }
    }

    /// 显式唤起：临时置于最前并获取焦点，退出交互后恢复，不永久 TOPMOST。
    pub fn summon(&mut self, expand: bool) -> Result<(), String> {
        if !self.visible {
            self.set_visible(true);
        }
        if expand && !self.view.expanded {
            self.view.expanded = true;
            self.layout()?;
            self.render_frame()?;
        }
        self.previous_foreground = {
            // SAFETY: 只读取当前前台窗口。
            unsafe { GetForegroundWindow() }
        };
        if self.previous_foreground == self.hwnd {
            self.previous_foreground = HWND::default();
        }
        // SAFETY: 窗口句柄有效；临时提升层级。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.elevated = true;
        self.sync_shadow();
        Ok(())
    }

    /// 交互结束：恢复焦点并撤销临时提升。
    pub fn release_elevation(&mut self) {
        if !self.elevated {
            return;
        }
        self.elevated = false;
        self.release_topmost();
        // SAFETY: 窗口句柄有效；恢复之前的前台窗口。
        unsafe {
            if !self.previous_foreground.is_invalid()
                && IsWindow(Some(self.previous_foreground)).as_bool()
            {
                let _ = SetForegroundWindow(self.previous_foreground);
            }
        }
        self.previous_foreground = HWND::default();
        self.sync_shadow();
    }

    /// 撤销临时置顶，并把面板插到当前前台窗口之后，避免继续压住普通窗口。
    fn release_topmost(&mut self) {
        // SAFETY: 只读取前台窗口、检查句柄有效性并调整层级；句柄均来自本进程记录。
        unsafe {
            let current = GetForegroundWindow();
            let fallback_ok = !self.previous_foreground.is_invalid()
                && IsWindow(Some(self.previous_foreground)).as_bool()
                && self.previous_foreground != self.hwnd;
            // 优先跟随真正接管前台的窗口，其次回退到呼出时记录的前台窗口。
            let anchor = if !current.is_invalid() && current != self.hwnd {
                current
            } else if fallback_ok {
                self.previous_foreground
            } else {
                HWND_NOTOPMOST
            };
            let _ = SetWindowPos(
                self.hwnd,
                Some(anchor),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn hide(&mut self) {
        self.release_elevation();
        self.set_visible(false);
    }

    /// 自检/截图专用：直接设置临时置顶，不触发激活或焦点变化。
    pub fn set_temporary_topmost(&mut self, topmost: bool) {
        // SAFETY: 窗口句柄有效；只调整层级。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(if topmost {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                }),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        self.sync_shadow();
    }

    /// 处理窗口消息；返回需要主程序执行的业务事件。
    pub fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Vec<PanelEvent> {
        match msg {
            0x0111 => {
                // 兼容直接分发的控件通知；正常情况下窗口过程会转成自定义消息。
                Vec::new()
            }
            0x8051 => {
                // EN_SETFOCUS / EN_KILLFOCUS：搜索框获得焦点时显示强调描边。
                self.search_focused = wparam.0 == 0x0100;
                let _ = self.render_frame();
                Vec::new()
            }
            0x0015 | 0x001A | 0x031A | 0x007E => {
                crate::theme::reload();
                let _ = self.layout();
                let _ = self.render_frame();
                Vec::new()
            }
            0x8050 => {
                if let Some(browse) = &self.browse {
                    self.view.query = browse.query();
                    self.view.offset = 0;
                    self.view.selected = None;
                    let _ = self.layout();
                    let _ = self.render_frame();
                }
                Vec::new()
            }
            0x020A => {
                let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
                self.scroll(if delta > 0 { -3 } else { 3 });
                Vec::new()
            }
            WM_MOUSEMOVE => {
                // SAFETY: 鼠标坐标由系统的 lparam 提供。
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                self.update_hover(x, y);
                self.track_leave();
                Vec::new()
            }
            WM_MOUSELEAVE => {
                self.update_hover(-1.0, -1.0);
                Vec::new()
            }
            WM_LBUTTONDOWN => Vec::new(),
            WM_LBUTTONUP => {
                // SAFETY: 鼠标坐标由系统的 lparam 提供。
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                self.click_at(x, y)
            }
            WM_LBUTTONDBLCLK => {
                // SAFETY: 鼠标坐标由系统的 lparam 提供。
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                self.double_click_at(x, y)
            }
            WM_RBUTTONUP => {
                // SAFETY: 鼠标坐标由系统的 lparam 提供。
                let x = (lparam.0 & 0xFFFF) as i16 as f32 / self.scale();
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale();
                self.context_at(x, y)
            }
            WM_KEYDOWN => self.key(wparam.0 as u32),
            WM_TIMER => {
                if wparam.0 == TIMER_ANIMATION {
                    self.tick();
                }
                Vec::new()
            }
            WM_ACTIVATE | WM_PANEL_ACTIVATE => {
                if (wparam.0 & 0xFFFF) == 0 && self.elevated {
                    // 失焦即撤销临时提升；灵动岛是热键呼出的一次性面板，直接收起。
                    self.elevated = false;
                    self.release_topmost();
                    self.previous_foreground = HWND::default();
                    if self.kind == PanelKind::Island {
                        self.view.expanded = false;
                        self.hide();
                        self.sync_shadow();
                        return vec![PanelEvent::DismissRequested];
                    }
                    self.sync_shadow();
                }
                Vec::new()
            }
            WM_DPICHANGED | WM_PANEL_DPICHANGED => {
                self.dpi = self.current_dpi();
                if lparam.0 != 0 {
                    // SAFETY: WM_DPICHANGED 的 lParam 由系统提供有效建议矩形。
                    let rect = unsafe { &*(lparam.0 as *const RECT) };
                    // SAFETY: 先将窗口移动至建议显示器，再按该显示器工作区重新布局。
                    unsafe {
                        let _ = SetWindowPos(
                            self.hwnd,
                            None,
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
                let _ = self.layout();
                let _ = self.render_frame();
                Vec::new()
            }
            WM_PAINT => {
                // DComp 直接呈现，不走 WM_PAINT；仍然验证绘制区避免消息堆积。
                // SAFETY: 标准 BeginPaint/EndPaint 配对。
                unsafe {
                    let mut paint = PAINTSTRUCT::default();
                    let _ = BeginPaint(self.hwnd, &mut paint);
                    let _ = EndPaint(self.hwnd, &paint);
                }
                Vec::new()
            }
            WM_ERASEBKGND => Vec::new(),
            WM_CLOSE => {
                self.hide();
                vec![PanelEvent::DismissRequested]
            }
            _ => Vec::new(),
        }
    }

    /// 测试与键盘导航共用的点击入口，坐标为 DIP。
    pub fn click_at(&mut self, x: f32, y: f32) -> Vec<PanelEvent> {
        if std::env::var_os("DESKSHELL_UI_DEBUG").is_some() {
            eprintln!(
                "panel {} click dip=({x:.1},{y:.1}) dpi={} hit={:?}",
                self.kind.class_name(),
                self.dpi,
                hit_test(&self.view, x, y)
            );
        }
        match hit_test(&self.view, x, y) {
            PanelHit::Item(index) => {
                if self.view.kind == PanelKind::Island {
                    self.select(index);
                    return vec![PanelEvent::SelectionChanged(Some(index))];
                }
                let Some(item) = self.view.items.get(index) else {
                    return Vec::new();
                };
                vec![PanelEvent::ItemActivated {
                    id: item.id.clone(),
                    index,
                }]
            }
            PanelHit::Tab(index) => {
                if let Some(tab) = IslandTab::from_index(index)
                    && tab != self.view.island_tab
                {
                    self.view.island_tab = tab;
                    self.view.selected = None;
                    self.view.offset = 0;
                    let _ = self.layout();
                    let _ = self.render_frame();
                }
                vec![PanelEvent::TabChanged(index)]
            }
            PanelHit::Category(index) => {
                let values = self.view.categories();
                if let Some(label) = values.get(index) {
                    self.view.category = if index == 0 {
                        String::new()
                    } else {
                        label.clone()
                    };
                    self.view.offset = 0;
                    self.view.selected = None;
                    let _ = self.layout();
                    let _ = self.render_frame();
                    if let Some(browse) = &self.browse {
                        browse.set_text(&self.view.query);
                    }
                }
                Vec::new()
            }
            PanelHit::Search => {
                self.focus_search();
                Vec::new()
            }
            PanelHit::Action(index) => {
                let Some(action) = self.view.actions.get(index).copied() else {
                    return Vec::new();
                };
                let Some(selected) = self
                    .view
                    .selected
                    .and_then(|index| self.view.items.get(index))
                else {
                    return Vec::new();
                };
                vec![PanelEvent::Action {
                    action,
                    id: selected.id.clone(),
                }]
            }
            PanelHit::Header => {
                if self.view.kind == PanelKind::Island {
                    self.view.expanded = !self.view.expanded;
                    let _ = self.layout();
                    let _ = self.render_frame();
                    vec![PanelEvent::ExpandedChanged(self.view.expanded)]
                } else {
                    Vec::new()
                }
            }
            PanelHit::Close => {
                self.view.expanded = false;
                let _ = self.layout();
                let _ = self.render_frame();
                vec![PanelEvent::DismissRequested]
            }
            PanelHit::Empty => Vec::new(),
        }
    }

    pub fn double_click_at(&mut self, x: f32, y: f32) -> Vec<PanelEvent> {
        if self.view.kind != PanelKind::Island {
            return Vec::new();
        }
        if let PanelHit::Item(index) = hit_test(&self.view, x, y)
            && let Some(id) = self.view.items.get(index).map(|item| item.id.clone())
        {
            self.select(index);
            return vec![PanelEvent::ItemActivated { id, index }];
        }
        Vec::new()
    }

    pub fn context_at(&mut self, x: f32, y: f32) -> Vec<PanelEvent> {
        if let PanelHit::Item(index) = hit_test(&self.view, x, y)
            && let Some(id) = self.view.items.get(index).map(|item| item.id.clone())
        {
            return vec![PanelEvent::ContextMenu { id, index }];
        }
        Vec::new()
    }

    pub fn key(&mut self, key: u32) -> Vec<PanelEvent> {
        match key {
            0x09 if self.kind == PanelKind::Island && self.view.expanded => {
                self.focus_search();
                Vec::new()
            }
            0x21 | 0x22 => {
                let delta = self.view.visible_rows() as i32 * if key == 0x21 { -1 } else { 1 };
                self.move_selection(delta)
                    .map(|index| vec![PanelEvent::SelectionChanged(Some(index))])
                    .unwrap_or_default()
            }
            value if value == VK_ESCAPE.0 as u32 => {
                if self.view.kind == PanelKind::Island && self.view.expanded {
                    self.view.expanded = false;
                    let _ = self.layout();
                    let _ = self.render_frame();
                    self.release_elevation();
                    self.hide();
                    return vec![PanelEvent::DismissRequested];
                }
                self.release_elevation();
                vec![PanelEvent::DismissRequested]
            }
            value if value == VK_LEFT.0 as u32 => self
                .move_selection(-1)
                .map(|index| vec![PanelEvent::SelectionChanged(Some(index))])
                .unwrap_or_default(),
            value if value == VK_RIGHT.0 as u32 => self
                .move_selection(1)
                .map(|index| vec![PanelEvent::SelectionChanged(Some(index))])
                .unwrap_or_default(),
            value if value == VK_UP.0 as u32 => {
                if self.view.kind == PanelKind::Island {
                    self.move_selection(-1)
                        .map(|index| vec![PanelEvent::SelectionChanged(Some(index))])
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            value if value == VK_DOWN.0 as u32 => {
                if self.view.kind == PanelKind::Island {
                    self.move_selection(1)
                        .map(|index| vec![PanelEvent::SelectionChanged(Some(index))])
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            value if value == VK_RETURN.0 as u32 => {
                let Some(index) = self.view.selected else {
                    return Vec::new();
                };
                let Some(item) = self.view.items.get(index) else {
                    return Vec::new();
                };
                vec![PanelEvent::ItemActivated {
                    id: item.id.clone(),
                    index,
                }]
            }
            value if value == VK_SPACE.0 as u32 => {
                if self.view.kind == PanelKind::Island {
                    self.view.expanded = !self.view.expanded;
                    let _ = self.layout();
                    let _ = self.render_frame();
                    return vec![PanelEvent::ExpandedChanged(self.view.expanded)];
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// 动画定时器回调；只在悬停动画未结束时持续。
    pub fn tick(&mut self) {
        let elapsed = self.last_tick.elapsed().as_secs_f32() * 1000.0;
        self.last_tick = Instant::now();
        let step = (elapsed / HOVER_DURATION_MS).clamp(0.02, 0.5);
        if self.hover_t < self.hover_target {
            self.hover_t = (self.hover_t + step).min(self.hover_target);
        } else if self.hover_t > self.hover_target {
            self.hover_t = (self.hover_t - step).max(self.hover_target);
        }
        let _ = self.render_frame();
        if (self.hover_t - self.hover_target).abs() < f32::EPSILON {
            self.animating = false;
            // SAFETY: 只停止本窗口的动画定时器。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATION);
            }
        }
    }

    pub fn layout(&mut self) -> Result<(), String> {
        self.monitor_rect = monitor_rect(self.hwnd, self.options.monitor);
        self.work_rect = work_rect(self.hwnd, self.options.monitor);
        if self.kind == PanelKind::Island {
            self.view.max_width =
                ((self.work_rect.right - self.work_rect.left) as f32 / self.scale() - 16.0)
                    .max(180.0);
            let available =
                (self.work_rect.bottom - self.work_rect.top) as f32 / self.scale() - 40.0;
            let fixed = crate::panel::ISLAND_HEADER
                + crate::panel::ISLAND_SEARCH_ROW
                + if self.view.chips_visible() {
                    crate::panel::ISLAND_CHIP_ROW
                } else {
                    0.0
                }
                + crate::panel::ISLAND_PADDING * 2.0
                + crate::panel::ISLAND_ACTION_BAR
                + crate::panel::ISLAND_STATUS_HEIGHT;
            self.view.row_limit = (((available - fixed) / crate::panel::ISLAND_ROW_HEIGHT).floor()
                as usize)
                .clamp(1, crate::panel::MAX_ISLAND_ROWS);
            self.view.offset = self.view.offset.min(
                self.view
                    .filtered_indices()
                    .len()
                    .saturating_sub(self.view.visible_rows()),
            );
        }
        let (width_px, height_px) = pixel_size(&self.view, self.dpi);
        let scale = self.scale();
        let (x, y) = match self.kind {
            PanelKind::Dock => (
                self.work_rect.left
                    + ((self.work_rect.right - self.work_rect.left - width_px as i32) / 2),
                self.work_rect.bottom - height_px as i32 - (8.0 * scale) as i32,
            ),
            PanelKind::Island => (
                self.monitor_rect.left
                    + ((self.monitor_rect.right - self.monitor_rect.left - width_px as i32) / 2),
                self.monitor_rect.top + (8.0 * scale) as i32,
            ),
        };
        // SAFETY: 句柄有效；只调整位置与尺寸，不改变层级。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width_px as i32,
                height_px as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.apply_region()?;
        if let Some(browse) = &mut self.browse {
            browse.layout(&self.view, self.dpi);
        }
        if let Some(composition) = self.composition.as_mut() {
            composition
                .resize(width_px, height_px, self.dpi)
                .map_err(String::from)?;
        }
        self.sync_shadow();
        Ok(())
    }

    fn apply_region(&self) -> Result<(), String> {
        let (width_px, height_px) = pixel_size(&self.view, self.dpi);
        // GDI 接收圆角椭圆的直径，D2D 接收半径，不能混用。
        let diameter = (2.0 * panel_radius(&self.view) * self.scale()).round() as i32;
        // SAFETY: 创建区域后交给 SetWindowRgn；成功时系统接管所有权。
        let region = unsafe {
            CreateRoundRectRgn(
                0,
                0,
                width_px as i32 + 1,
                height_px as i32 + 1,
                diameter,
                diameter,
            )
        };
        if region.is_invalid() {
            return Err("创建窗口区域失败".into());
        }
        // SAFETY: 区域句柄有效；失败时仍需自行删除。
        let result = unsafe { SetWindowRgn(self.hwnd, Some(region), true) };
        if result == 0 {
            // SAFETY: SetWindowRgn 失败时所有权未转移。
            unsafe {
                let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(region.0));
            }
            return Err("设置窗口区域失败".into());
        }
        Ok(())
    }

    fn ensure_gpu(&mut self) -> Result<(), String> {
        if self.gpu.is_none() {
            self.gpu = Some(Gpu::new().map_err(String::from)?);
        }
        if self.composition.is_none() {
            let (width_px, height_px) = pixel_size(&self.view, self.dpi);
            let gpu = Rc::clone(self.gpu.as_ref().expect("GPU 已创建"));
            self.composition = Some(
                PanelComposition::new(gpu, self.hwnd, width_px, height_px, self.dpi)
                    .map_err(String::from)?,
            );
        }
        Ok(())
    }

    fn render_frame(&mut self) -> Result<(), String> {
        self.ensure_gpu()?;
        let composition = self
            .composition
            .as_mut()
            .ok_or_else(|| "面板合成对象缺失".to_string())?;
        let result = composition.render(&self.view, self.hover, self.hover_t, self.search_focused);
        match result {
            Ok(()) => {
                self.presents += 1;
                Ok(())
            }
            Err(RenderError::TargetLost) => {
                composition.recreate_surface().map_err(String::from)?;
                composition
                    .render(&self.view, self.hover, self.hover_t, self.search_focused)
                    .map_err(String::from)?;
                self.presents += 1;
                Ok(())
            }
            Err(RenderError::DeviceLost) => {
                // 设备移除：重建共享 GPU 上下文与合成目标，本帧重试一次。
                self.composition = None;
                self.gpu = None;
                self.ensure_gpu()?;
                self.composition
                    .as_mut()
                    .ok_or_else(|| "重建面板合成对象失败".to_string())?
                    .render(&self.view, self.hover, self.hover_t, self.search_focused)
                    .map_err(String::from)?;
                self.presents += 1;
                Ok(())
            }
            Err(RenderError::Other(message)) => {
                self.pending_error = Some(message.clone());
                Err(message)
            }
        }
    }

    fn update_hover(&mut self, x: f32, y: f32) {
        let hover = match hit_test(&self.view, x, y) {
            PanelHit::Item(index) => Some(index),
            PanelHit::Action(index) => Some(index + 1000),
            PanelHit::Category(index) => Some(index + 2000),
            _ => None,
        };
        if hover == self.hover {
            return;
        }
        self.hover = hover;
        if self.kind == PanelKind::Dock {
            self.update_tooltip();
        }
        if self.kind == PanelKind::Dock && self.view.fisheye {
            self.hover_target = if hover.is_some() { 1.0 } else { 0.0 };
            self.last_tick = Instant::now();
            if !self.animating {
                self.animating = true;
                // SAFETY: 只为本窗口安装 16 ms 动画定时器。
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_ANIMATION, 16, None);
                }
            }
        } else {
            self.hover_t = if hover.is_some() { 1.0 } else { 0.0 };
            self.hover_target = self.hover_t;
            let _ = self.render_frame();
        }
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

    fn select(&mut self, index: usize) {
        if self.view.selected == Some(index) {
            return;
        }
        self.view.selected = Some(index);
        if self.kind == PanelKind::Dock {
            self.update_tooltip();
        }
        let _ = self.render_frame();
    }

    fn move_selection(&mut self, delta: i32) -> Option<usize> {
        let indices = self.view.filtered_indices();
        if indices.is_empty() {
            return None;
        }
        let current = self
            .view
            .selected
            .and_then(|selected| indices.iter().position(|index| *index == selected));
        let row = current
            .map(|position| (position as i32 + delta).clamp(0, indices.len() as i32 - 1) as usize)
            .unwrap_or(0);
        if row < self.view.offset {
            self.view.offset = row;
        }
        if row >= self.view.offset + self.view.visible_rows() {
            self.view.offset = row + 1 - self.view.visible_rows();
        }
        self.select(indices[row]);
        Some(indices[row])
    }

    fn scroll(&mut self, delta: i32) {
        let max = self
            .view
            .filtered_indices()
            .len()
            .saturating_sub(self.view.visible_rows());
        self.view.offset = (self.view.offset as i64 + delta as i64).clamp(0, max as i64) as usize;
        let _ = self.render_frame();
    }

    fn update_tooltip(&mut self) {
        let Some(index) = self.hover.or(self.view.selected) else {
            if let Some(tooltip) = &mut self.tooltip {
                tooltip.hide();
            }
            return;
        };
        let Some(item) = self.view.items.get(index) else {
            if let Some(tooltip) = &mut self.tooltip {
                tooltip.hide();
            }
            return;
        };
        let label = item.label.clone();
        let Some((left, top, _, _)) = self.window_screen_rect() else {
            if let Some(tooltip) = &mut self.tooltip {
                tooltip.hide();
            }
            return;
        };
        let scale = self.scale();
        let style = self.view.dock_style;
        let center = left
            + ((crate::panel::DOCK_PADDING + index as f32 * style.slot() + style.slot() / 2.0)
                * scale) as i32;
        let icon_top = top + (style.icon_top() * scale) as i32;
        if let Some(tooltip) = &mut self.tooltip {
            tooltip.show(
                &label,
                center,
                icon_top,
                self.dpi,
                self.work_rect.left,
                self.work_rect.right,
            );
        }
    }

    pub fn owns_input(&self, hwnd: HWND) -> bool {
        self.browse
            .as_ref()
            .is_some_and(|browse| hwnd == browse.hwnd())
    }

    pub fn cycle_input(&self, _hwnd: HWND) {
        if self.browse.is_some() {
            // SAFETY: 面板窗口由本线程持有。
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
            }
        }
    }

    pub fn focus_list(&self) {
        // SAFETY: 自有顶层窗口在 UI 线程获取焦点。
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
        }
    }

    pub fn focus_search(&self) {
        if let Some(browse) = &self.browse {
            // SAFETY: 原生搜索控件属于本面板，由当前 UI 线程持有。
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(browse.hwnd()));
            }
        }
    }

    fn current_dpi(&self) -> u32 {
        // SAFETY: 窗口句柄由本对象持有；查询失败时回退 96。
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 96 } else { dpi }
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// 条目在屏幕坐标系中的矩形，供自检与真实鼠标测试使用。
    pub fn item_screen_rects(&self) -> Vec<(String, (i32, i32, i32, i32))> {
        let mut origin = POINT::default();
        // SAFETY: 只读取窗口位置。
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut origin);
        }
        let scale = self.scale();
        let mut rects = Vec::new();
        match self.kind {
            PanelKind::Dock => {
                let style = self.view.dock_style;
                for (index, item) in self
                    .view
                    .items
                    .iter()
                    .take(crate::panel::DOCK_MAX_ITEMS)
                    .enumerate()
                {
                    let left = crate::panel::DOCK_PADDING
                        + index as f32 * style.slot()
                        + (style.slot() - style.icon_size()) / 2.0;
                    let top = style.icon_top();
                    rects.push((
                        item.id.clone(),
                        (
                            origin.x + (left * scale) as i32,
                            origin.y + (top * scale) as i32,
                            (style.icon_size() * scale) as i32,
                            (style.icon_size() * scale) as i32,
                        ),
                    ));
                }
            }
            PanelKind::Island => {
                if self.view.expanded {
                    let (width, _) = panel_size(&self.view);
                    for (row, index) in self.view.visible_indices().into_iter().enumerate() {
                        let item = &self.view.items[index];
                        let top =
                            self.view.rows_top() + row as f32 * crate::panel::ISLAND_ROW_HEIGHT;
                        rects.push((
                            item.id.clone(),
                            (
                                origin.x,
                                origin.y + (top * scale) as i32,
                                (width * scale) as i32,
                                (crate::panel::ISLAND_ROW_HEIGHT * scale) as i32,
                            ),
                        ));
                    }
                }
            }
        }
        rects
    }

    pub fn window_screen_rect(&self) -> Option<(i32, i32, u32, u32)> {
        let mut rect = RECT::default();
        // SAFETY: 只读取窗口矩形。
        unsafe { GetWindowRect(self.hwnd, &mut rect) }.ok()?;
        Some((
            rect.left,
            rect.top,
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
    }

    pub fn monitor_screen_rect(&self) -> (i32, i32, u32, u32) {
        (
            self.monitor_rect.left,
            self.monitor_rect.top,
            (self.monitor_rect.right - self.monitor_rect.left).max(0) as u32,
            (self.monitor_rect.bottom - self.monitor_rect.top).max(0) as u32,
        )
    }

    /// 检查窗口区域是否使四角不可命中；返回是否被裁掉。
    pub fn corner_clipped(&self) -> bool {
        let Some((left, top, _, _)) = self.window_screen_rect() else {
            return false;
        };
        // SAFETY: 只做只读命中测试。
        let corner = unsafe {
            windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(POINT { x: left, y: top })
        };
        corner != self.hwnd
    }
}

impl Drop for PanelWindow {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象创建；销毁前先停止定时器。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATION);
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn register_class(kind: PanelKind) -> Result<windows::core::PCWSTR, String> {
    static DOCK: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    static ISLAND: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let slot = match kind {
        PanelKind::Dock => &DOCK,
        PanelKind::Island => &ISLAND,
    };
    let result = slot.get_or_init(|| {
        let class_name = wide(kind.class_name());
        // SAFETY: 只查询当前进程模块句柄。
        let module = unsafe { GetModuleHandleW(None) }.map_err(|error| error.to_string())?;
        // SAFETY: 只注册窗口类；类名在调用期间有效。
        let atom = unsafe {
            RegisterClassW(&WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(module.0),
                hCursor: LoadCursorW(None, IDC_ARROW).map_err(|error| error.to_string())?,
                lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
                ..Default::default()
            })
        };
        if atom == 0 {
            return Err(format!(
                "注册面板窗口类失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    });
    result.clone()?;
    // 类名必须在注册后继续有效；使用静态宽字符串。
    Ok(match kind {
        PanelKind::Dock => w!("DeskShell.Panel.Dock"),
        PanelKind::Island => w!("DeskShell.Panel.Island"),
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 搜索框颜色只读取主题并返回本线程创建的画刷。
    if let Some(result) = controls::ctl_color(msg, wparam) {
        return result;
    }
    // SAFETY: 控件同步通知仅投递消息，不在回调中访问面板状态。
    unsafe {
        let notification = (wparam.0 >> 16) & 0xffff;
        if msg == 0x0111 && notification == 0x0300 {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                0x8050,
                WPARAM(0),
                LPARAM(0),
            );
        } else if msg == 0x0111 && (notification == 0x0100 || notification == 0x0200) {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                0x8051,
                WPARAM(notification),
                LPARAM(0),
            );
        } else if msg == WM_ACTIVATE {
            // 系统直接发送，不经过消息队列；转发后由 PanelWindow 统一处理失焦收起。
            let _ = PostMessageW(Some(hwnd), WM_PANEL_ACTIVATE, wparam, lparam);
        } else if msg == WM_DPICHANGED {
            // 建议矩形由 DefWindowProc 应用；转发消息只触发面板重新布局。
            let _ = PostMessageW(Some(hwnd), WM_PANEL_DPICHANGED, wparam, LPARAM(0));
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn pixel_size(view: &PanelView, dpi: u32) -> (u32, u32) {
    let scale = dpi as f32 / 96.0;
    let (width, height) = panel_size(view);
    (
        (width * scale).round().max(1.0) as u32,
        (height * scale).round().max(1.0) as u32,
    )
}

fn monitor_rect(hwnd: HWND, index: Option<usize>) -> RECT {
    monitor_rects(hwnd, index).0
}

fn work_rect(hwnd: HWND, index: Option<usize>) -> RECT {
    monitor_rects(hwnd, index).1
}

fn monitor_rects(hwnd: HWND, index: Option<usize>) -> (RECT, RECT) {
    // 枚举所有显示器并排序，选择稳定索引；默认使用窗口所在显示器或主显示器。
    let mut monitors: Vec<(RECT, RECT)> = Vec::new();
    // SAFETY: 回调只在调用期间运行，向本地 Vec 追加。
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<(RECT, RECT)> as isize),
        );
    }
    monitors.sort_by_key(|(rect, _)| (rect.left, rect.top));
    if monitors.is_empty() {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: 输出结构有效。
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
        // SAFETY: 显示器句柄有效。
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return (info.rcMonitor, info.rcWork);
        }
        return (RECT::default(), RECT::default());
    }
    match index {
        Some(index) if index < monitors.len() => monitors[index],
        _ => {
            // 默认跟随窗口所在显示器；窗口尚未定位时使用主显示器。
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: 输出结构有效。
            let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
            // SAFETY: 显示器句柄来自系统，输出结构按文档初始化。
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                (info.rcMonitor, info.rcWork)
            } else {
                monitors[0]
            }
        }
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: 调用方保证 data 指向有效的 Vec；显示器信息输出到本地结构。
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let monitors = &mut *(data.0 as *mut Vec<(RECT, RECT)>);
            monitors.push((info.rcMonitor, info.rcWork));
        }
    }
    BOOL(1)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
