//! Dock 悬停提示：自绘圆角小窗，替代系统边框样式，且不拦截点击。

use std::sync::OnceLock;

use windows::{
    Win32::{
        Foundation::{COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateFontIndirectW, CreatePen, CreateRoundRectRgn, CreateSolidBrush,
            DeleteObject, DrawTextW, EndPaint, GetDC, GetTextExtentPoint32W, HGDIOBJ, LOGFONTW,
            ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, w},
};

const CLASS_NAME: &str = "DeskShell.Panel.Tooltip";
const FONT_PROP: PCWSTR = w!("DeskShell.Panel.Tooltip.Font");

pub(super) struct TooltipWindow {
    hwnd: HWND,
    font: Option<windows::Win32::Graphics::Gdi::HFONT>,
    font_dpi: u32,
    visible: bool,
}

impl TooltipWindow {
    pub fn create() -> Result<Self, String> {
        register_class()?;
        let class: Vec<u16> = CLASS_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 窗口类已注册；创建不激活、点击穿透的提示窗口。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
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
        .map_err(|error| format!("创建提示窗口失败：{error}"))?;
        Ok(Self {
            hwnd,
            font: None,
            font_dpi: 0,
            visible: false,
        })
    }

    pub fn hide(&mut self) {
        if self.visible {
            // SAFETY: 句柄由本对象持有。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }

    /// 在图标上方显示名称；坐标单位为像素。
    pub fn show(
        &mut self,
        text: &str,
        center_x: i32,
        icon_top: i32,
        dpi: u32,
        work_left: i32,
        work_right: i32,
    ) {
        if text.is_empty() {
            self.hide();
            return;
        }
        self.ensure_font(dpi);
        let scale = dpi as f32 / 96.0;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut measured = SIZE::default();
        // SAFETY: 屏幕 DC 与字体由本对象持有；测量调用同步完成。
        unsafe {
            let screen = GetDC(None);
            let previous = SelectObject(
                screen,
                self.font
                    .map_or(HGDIOBJ(std::ptr::null_mut()), |font| HGDIOBJ(font.0)),
            );
            let _ = GetTextExtentPoint32W(screen, &wide, &mut measured);
            let _ = SelectObject(screen, previous);
            let _ = ReleaseDC(None, screen);
        }
        let width = ((measured.cx as f32 + 28.0 * scale).round() as i32)
            .clamp((72.0 * scale) as i32, (360.0 * scale) as i32);
        let height = (30.0 * scale).round() as i32;
        let x = (center_x - width / 2).clamp(work_left, (work_right - width).max(work_left));
        let y = icon_top - height - (8.0 * scale).round() as i32;
        // SAFETY: 句柄有效；字符串在同步调用期间有效。
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
            let diameter = (14.0 * scale).round() as i32;
            let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, diameter, diameter);
            if !region.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::SetWindowRgn(self.hwnd, Some(region), true);
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.visible = true;
    }

    fn ensure_font(&mut self, dpi: u32) {
        if self.font_dpi == dpi && self.font.is_some() {
            return;
        }
        let scale = dpi as f32 / 96.0;
        let mut descriptor = LOGFONTW {
            lfHeight: -(12.0 * scale).round() as i32,
            lfWeight: 400,
            ..Default::default()
        };
        for (target, value) in descriptor
            .lfFaceName
            .iter_mut()
            .zip("Microsoft YaHei UI".encode_utf16())
        {
            *target = value;
        }
        // SAFETY: 字体描述符有效；创建的字体由本对象持有并释放。
        let font = unsafe { CreateFontIndirectW(&descriptor) };
        if !font.is_invalid() {
            // SAFETY: 属性只保存本对象拥有的字体句柄。
            unsafe {
                let _ = SetPropW(self.hwnd, FONT_PROP, Some(HANDLE(font.0)));
            }
            if let Some(old) = self.font.replace(font) {
                // SAFETY: 旧字体已从窗口属性替换，且不再被绘制使用。
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(old.0));
                }
            }
            self.font_dpi = dpi;
        }
    }
}

impl Drop for TooltipWindow {
    fn drop(&mut self) {
        // SAFETY: 窗口与字体由本对象创建。
        unsafe {
            if let Some(font) = self.font.take() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            if !self.hwnd.is_invalid() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn register_class() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            let class: Vec<u16> = CLASS_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: 只注册当前进程的提示窗口类。
            let module = unsafe { GetModuleHandleW(None) }
                .map_err(|error| format!("获取模块句柄失败：{error}"))?;
            // SAFETY: 类名在注册调用期间有效。
            let atom = unsafe {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(tooltip_proc),
                    hInstance: HINSTANCE(module.0),
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                })
            };
            if atom == 0 {
                return Err(format!(
                    "注册提示窗口类失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })
        .clone()
}

unsafe extern "system" fn tooltip_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: 默认窗口过程由系统调用。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn paint(hwnd: HWND) {
    let palette = crate::theme::current();
    let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    // SAFETY: 标准 BeginPaint/EndPaint 配对；句柄由窗口过程提供。
    unsafe {
        let dc = BeginPaint(hwnd, &mut paint);
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let brush = CreateSolidBrush(colorref(palette.tooltip_bg));
        let pen = CreatePen(
            windows::Win32::Graphics::Gdi::PS_SOLID,
            1,
            colorref(palette.tooltip_border),
        );
        let old_brush = SelectObject(dc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(dc, HGDIOBJ(pen.0));
        let _ = RoundRect(dc, 0, 0, rect.right, rect.bottom, 14, 14);
        let _ = SelectObject(dc, old_brush);
        let _ = SelectObject(dc, old_pen);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let font = GetPropW(hwnd, FONT_PROP);
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, colorref(palette.text));
        let length = GetWindowTextLengthW(hwnd).clamp(0, 512) as usize;
        let mut text = vec![0u16; length + 1];
        let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        text.truncate(copied);
        let mut text_rect = rect;
        text_rect.left += 10;
        text_rect.right -= 10;
        let _ = DrawTextW(
            dc,
            &mut text,
            &mut text_rect,
            windows::Win32::Graphics::Gdi::DT_CENTER
                | windows::Win32::Graphics::Gdi::DT_VCENTER
                | windows::Win32::Graphics::Gdi::DT_SINGLELINE
                | windows::Win32::Graphics::Gdi::DT_NOPREFIX
                | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS,
        );
        let _ = SelectObject(dc, old_font);
        let _ = EndPaint(hwnd, &paint);
    }
}

fn colorref(color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u32;
    COLORREF(r | (g << 8) | (b << 16))
}
