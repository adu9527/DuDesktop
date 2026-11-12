//! 原生搜索框：由系统负责 IME、选择、剪贴板与无障碍语义。
//!
//! 编辑框不画边框，嵌在 Direct2D 绘制的圆角搜索框内；父窗口在
//! `WM_CTLCOLOREDIT` 中返回与面板一致的不透明底色。

use std::cell::Cell;

use super::PanelView;
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, HBRUSH, HDC, HGDIOBJ, SetBkColor, SetTextColor,
        },
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

const WM_CTLCOLOREDIT: u32 = 0x0133;
const EM_SETCUEBANNER: u32 = 0x1501;
const EM_SETMARGINS: u32 = 0x00D3;

pub(super) struct SearchBox {
    edit: HWND,
    font: Option<windows::Win32::Graphics::Gdi::HFONT>,
    font_dpi: u32,
}

impl SearchBox {
    pub fn new(parent: HWND) -> Result<Self, String> {
        // SAFETY: 子控件由父窗口拥有，当前 UI 线程创建和使用。
        unsafe {
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_TABSTOP | WINDOW_STYLE(0x0080),
                0,
                0,
                1,
                1,
                Some(parent),
                Some(HMENU(101usize as *mut _)),
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
            let cue: Vec<u16> = "搜索名称或类型…".encode_utf16().chain(Some(0)).collect();
            SendMessageW(
                edit,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
            SendMessageW(edit, EM_SETMARGINS, Some(WPARAM(0)), Some(LPARAM(0)));
            Ok(Self {
                edit,
                font: None,
                font_dpi: 0,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.edit
    }

    pub fn layout(&mut self, view: &PanelView, dpi: u32) {
        let scale = dpi as f32 / 96.0;
        let visible = view.expanded && view.island_tab == crate::panel::IslandTab::Files;
        // SAFETY: 原生编辑控件属于本线程；只切换可见性。
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                self.edit,
                if visible {
                    windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE
                } else {
                    windows::Win32::UI::WindowsAndMessaging::SW_HIDE
                },
            );
        }
        if !visible {
            return;
        }
        let field = view.search_rect();
        // SAFETY: HWND 是本线程有效子控件；SendMessage 同步复制字符串。
        unsafe {
            if self.font_dpi != dpi {
                use windows::Win32::Graphics::Gdi::*;
                let mut descriptor = LOGFONTW {
                    lfHeight: -(13.0 * scale).round() as i32,
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
                let font = CreateFontIndirectW(&descriptor);
                if !font.is_invalid() {
                    SendMessageW(
                        self.edit,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                    if let Some(old) = self.font.replace(font) {
                        let _ = DeleteObject(old.into());
                    }
                    self.font_dpi = dpi;
                }
            }
            let _ = SetWindowPos(
                self.edit,
                None,
                ((field.left + 36.0) * scale) as i32,
                ((field.top + 6.0) * scale) as i32,
                ((field.width() - 48.0).max(40.0) * scale) as i32,
                (24.0 * scale) as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.edit, if view.expanded { SW_SHOWNA } else { SW_HIDE });
        }
    }

    pub fn query(&self) -> String {
        // SAFETY: 读取本线程控件文字；缓冲区预留 NUL 空间。
        unsafe {
            let length = GetWindowTextLengthW(self.edit).clamp(0, 4096) as usize;
            let mut text = vec![0u16; length + 1];
            let copied = GetWindowTextW(self.edit, &mut text).max(0) as usize;
            String::from_utf16_lossy(&text[..copied])
        }
    }

    pub fn set_text(&self, value: &str) {
        let text: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: 同线程控件同步复制文字。
        unsafe {
            let _ = SetWindowTextW(self.edit, windows::core::PCWSTR(text.as_ptr()));
        }
    }
}

impl Drop for SearchBox {
    fn drop(&mut self) {
        // SAFETY: 面板所有者在销毁父窗口后释放本字段，不再有控件使用字体。
        unsafe {
            if let Some(font) = self.font.take() {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

thread_local! {
    static EDIT_BRUSH: Cell<Option<(COLORREF, HBRUSH)>> = const { Cell::new(None) };
}

/// 处理父窗口收到的 `WM_CTLCOLOREDIT`；返回画刷句柄。
pub(super) fn ctl_color(message: u32, wparam: WPARAM) -> Option<LRESULT> {
    if message != WM_CTLCOLOREDIT {
        return None;
    }
    let palette = crate::theme::current();
    let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
    let foreground = colorref(palette.text);
    let background = colorref(palette.input);
    // SAFETY: HDC 由系统提供且仅在本次消息内有效；文本与背景色只影响本次绘制。
    unsafe {
        SetTextColor(hdc, foreground);
        SetBkColor(hdc, background);
    }
    let brush = EDIT_BRUSH.with(|slot| {
        if let Some((color, brush)) = slot.get()
            && color == background
        {
            return brush;
        }
        // SAFETY: 旧画刷由本线程创建且不再被系统持有；创建失败时返回空画刷。
        unsafe {
            if let Some((_, old)) = slot.get() {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
            let brush = CreateSolidBrush(background);
            slot.set(Some((background, brush)));
            brush
        }
    });
    Some(LRESULT(brush.0 as isize))
}

fn colorref(color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u32;
    COLORREF(r | (g << 8) | (b << 16))
}
