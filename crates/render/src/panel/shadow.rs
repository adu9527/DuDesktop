//! 面板软阴影：独立分层窗口，不参与 DComp 交换链，不接收鼠标输入。
//!
//! 阴影只在尺寸/半径变化时重新生成像素，平时仅随面板位置移动；
//! 关闭阴影时窗口隐藏，不保留绘制工作。点击由 `WM_NCHITTEST` 与
//! `WS_EX_TRANSPARENT` 透传给下层窗口。

use std::{ffi::c_void, mem::size_of, ptr::null_mut, sync::OnceLock};

use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ,
            ReleaseDC, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, IsWindow,
            RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_NCHITTEST, WNDCLASSW,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
    core::PCWSTR,
};

const CLASS_NAME: &str = "DeskShell.Panel.Shadow";
/// 阴影最大不透明度（0..255）；约 27% 更接近系统面板。
const MAX_ALPHA: f32 = 70.0;
/// 阴影扩展边距（DIP）。
const MARGIN_DIP: f32 = 14.0;

pub struct ShadowWindow {
    hwnd: HWND,
    surface: Option<Surface>,
    key: Option<(i32, i32, i32, u32)>,
    visible: bool,
}

impl ShadowWindow {
    pub fn create() -> Result<Self, String> {
        static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
        REGISTERED
            .get_or_init(|| {
                let class: Vec<u16> = CLASS_NAME
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                // SAFETY: 只注册本进程的阴影窗口类。
                let module = unsafe { GetModuleHandleW(None) }
                    .map_err(|error| format!("获取模块句柄失败：{error}"))?;
                // SAFETY: 类名在注册期间有效。
                let atom = unsafe {
                    RegisterClassW(&WNDCLASSW {
                        lpfnWndProc: Some(shadow_proc),
                        hInstance: HINSTANCE(module.0),
                        lpszClassName: PCWSTR(class.as_ptr()),
                        ..Default::default()
                    })
                };
                if atom == 0 {
                    return Err(format!(
                        "注册面板阴影窗口类失败：{}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(())
            })
            .clone()?;
        let class: Vec<u16> = CLASS_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: 创建不激活、点击穿透的分层窗口。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
        .map_err(|error| format!("创建面板阴影窗口失败：{error}"))?;
        Ok(Self {
            hwnd,
            surface: None,
            key: None,
            visible: false,
        })
    }

    pub fn is_alive(&self) -> bool {
        // SAFETY: 只查询句柄。
        unsafe { IsWindow(Some(self.hwnd)).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
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

    /// 把阴影放到面板正下方；尺寸变化时才重建像素。
    pub fn show_below(
        &mut self,
        panel: HWND,
        panel_rect: (i32, i32, u32, u32),
        radius: i32,
        dpi: u32,
    ) -> Result<(), String> {
        let margin = ((MARGIN_DIP * dpi as f32 / 96.0).round() as i32).max(8);
        let width = panel_rect.2 as i32 + margin * 2;
        let height = panel_rect.3 as i32 + margin * 2;
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        let key = (width, height, radius, dpi);
        if self.key != Some(key) || self.surface.is_none() {
            self.surface = Some(Surface::build(width, height, radius, margin)?);
            self.key = Some(key);
        }
        let surface = self.surface.as_ref().expect("阴影表面已创建");
        let position = POINT {
            x: panel_rect.0 - margin,
            y: panel_rect.1 - margin,
        };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: 屏幕 DC 用于分层窗口更新，使用后立即释放。
        let screen = unsafe { GetDC(None) };
        // SAFETY: 位图与 DC 由 ShadowWindow 持有，尺寸与位置与表面一致。
        let result = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                Some(screen),
                Some(&position),
                Some(&size),
                Some(surface.dc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
        // SAFETY: 与 GetDC 配对。
        unsafe {
            let _ = ReleaseDC(None, screen);
        }
        result.map_err(|error| format!("更新面板阴影失败：{error}"))?;
        if !self.visible {
            // SAFETY: 句柄有效；只显示不激活。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
        }
        // SAFETY: 把阴影插到面板之后（z 序更低），面板提升到 TOPMOST 时阴影同层跟随。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(panel),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(())
    }
}

impl Drop for ShadowWindow {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象创建。
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Surface {
    fn build(width: i32, height: i32, radius: i32, margin: i32) -> Result<Self, String> {
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        // 负高度：自上而下，便于按行填充 alpha。
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        // SAFETY: 兼容 DC 由本函数持有；像素指针在 DeleteObject 前有效。
        let dc = unsafe { CreateCompatibleDC(None) };
        let mut bits: *mut c_void = null_mut();
        // SAFETY: BITMAPINFO 与输出指针有效。
        let bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|error| format!("创建阴影位图失败：{error}"))?;
        if bits.is_null() {
            // SAFETY: 位图与 DC 由本函数创建。
            unsafe {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
            }
            return Err("阴影位图没有像素缓冲区".into());
        }
        // SAFETY: 位图与 DC 有效；选择对象后旧对象由本结构恢复。
        let previous = unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };
        fill_shadow(bits as *mut u8, width, height, margin, radius);
        Ok(Self {
            dc,
            bitmap,
            previous,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: 恢复旧对象后再删除位图与 DC，避免删除仍被选中的对象。
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

fn fill_shadow(bits: *mut u8, width: i32, height: i32, margin: i32, radius: i32) {
    let count = width as usize * height as usize * 4;
    // SAFETY: 调用方保证 bits 指向 width*height*4 字节的 DIB。
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits, count) };
    let left = margin as f32;
    let top = margin as f32;
    let right = width as f32 - margin as f32;
    let bottom = height as f32 - margin as f32;
    let radius = radius.max(0) as f32;
    for y in 0..height {
        for x in 0..width {
            let distance = rounded_rect_signed_distance(
                x as f32 + 0.5,
                y as f32 + 0.5,
                left,
                top,
                right,
                bottom,
                radius,
            );
            let alpha = shadow_alpha(distance, margin as f32);
            let index = (y as usize * width as usize + x as usize) * 4;
            // 预乘黑色：RGB 为 0，仅 alpha 变化。
            pixels[index] = 0;
            pixels[index + 1] = 0;
            pixels[index + 2] = 0;
            pixels[index + 3] = alpha;
        }
    }
}

pub(crate) fn rounded_rect_signed_distance(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> f32 {
    let center_x = (left + right) / 2.0;
    let center_y = (top + bottom) / 2.0;
    let half_width = (right - left) / 2.0;
    let half_height = (bottom - top) / 2.0;
    let radius = radius.clamp(0.0, half_width.min(half_height));
    let qx = (x - center_x).abs() - (half_width - radius);
    let qy = (y - center_y).abs() - (half_height - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

pub(crate) fn shadow_alpha(distance: f32, margin: f32) -> u8 {
    if margin <= 0.0 {
        return 0;
    }
    if distance <= 0.0 {
        return MAX_ALPHA as u8;
    }
    if distance >= margin {
        return 0;
    }
    let falloff = 1.0 - distance / margin;
    (MAX_ALPHA * falloff * falloff).round() as u8
}

unsafe extern "system" fn shadow_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCHITTEST {
        // 点击穿透：阴影不参与命中，事件交给下层窗口。
        return LRESULT(HTTRANSPARENT as isize);
    }
    // SAFETY: 默认窗口过程由系统调用。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_distance_is_negative_inside_and_positive_outside() {
        let inside = rounded_rect_signed_distance(50.0, 20.0, 0.0, 0.0, 100.0, 40.0, 12.0);
        assert!(inside < 0.0);
        let outside = rounded_rect_signed_distance(-5.0, 20.0, 0.0, 0.0, 100.0, 40.0, 12.0);
        assert!(outside > 0.0);
        // 圆角外但在包围盒内的点也应在外部。
        let corner = rounded_rect_signed_distance(1.0, 1.0, 0.0, 0.0, 100.0, 40.0, 12.0);
        assert!(corner > 0.0);
    }

    #[test]
    fn alpha_fades_from_edge_to_margin() {
        let margin = 14.0;
        assert_eq!(shadow_alpha(-1.0, margin), MAX_ALPHA as u8);
        assert!(shadow_alpha(0.0, margin) > 0);
        assert!(shadow_alpha(3.0, margin) > shadow_alpha(10.0, margin));
        assert_eq!(shadow_alpha(margin, margin), 0);
        assert_eq!(shadow_alpha(20.0, margin), 0);
    }
}
