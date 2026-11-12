//! 产品 UI 的语义色与玻璃材质 token。
//!
//! 面板与设置窗共用同一套调色板：默认暗色玻璃，跟随系统亮色主题时切换为
//! 磨砂白玻璃，高对比度下退化为系统实色。色值只在系统主题事件后刷新，
//! 动画帧不读取注册表。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

pub(crate) const fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    /// 玻璃主体：上亮下暗的纵向渐变。
    pub glass_top: D2D1_COLOR_F,
    pub glass_bottom: D2D1_COLOR_F,
    /// 设置窗等需要长期阅读的表面：接近不透明，减少背景干扰。
    pub window_top: D2D1_COLOR_F,
    pub window_bottom: D2D1_COLOR_F,
    /// 1px 外描边：顶部受光、底部收敛。
    pub rim_top: D2D1_COLOR_F,
    pub rim_bottom: D2D1_COLOR_F,
    /// 顶部高光与底部压暗，叠加在主体渐变之上。
    pub sheen_top: D2D1_COLOR_F,
    pub shade_bottom: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub text: D2D1_COLOR_F,
    pub secondary: D2D1_COLOR_F,
    pub tertiary: D2D1_COLOR_F,
    /// 品牌强调色渐变。
    pub accent_top: D2D1_COLOR_F,
    pub accent_bottom: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    pub accent_text: D2D1_COLOR_F,
    pub selected: D2D1_COLOR_F,
    pub hover: D2D1_COLOR_F,
    pub control: D2D1_COLOR_F,
    pub control_hover: D2D1_COLOR_F,
    /// 设置窗内的卡片底色。
    pub card: D2D1_COLOR_F,
    /// 原生编辑框使用的不透明底色，必须与绘制搜索框一致。
    pub input: D2D1_COLOR_F,
    pub danger: D2D1_COLOR_F,
    pub danger_tint: D2D1_COLOR_F,
    pub success: D2D1_COLOR_F,
    pub close_hover: D2D1_COLOR_F,
    pub tooltip_bg: D2D1_COLOR_F,
    pub tooltip_border: D2D1_COLOR_F,
    pub separator: D2D1_COLOR_F,
}

thread_local! {
    static PALETTE: std::cell::Cell<Option<Palette>> = const { std::cell::Cell::new(None) };
    static EPOCH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) fn current() -> Palette {
    PALETTE.with(|palette| {
        if let Some(value) = palette.get() {
            value
        } else {
            let value = load();
            palette.set(Some(value));
            value
        }
    })
}

/// 调色板版本；画笔缓存据此重建渐变。
pub(crate) fn version() -> u32 {
    EPOCH.with(|epoch| epoch.get())
}

pub(crate) fn reload() {
    // 每个 UI 线程独占；系统主题事件才刷新。
    PALETTE.with(|value| value.set(Some(load())));
    EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
}

fn load() -> Palette {
    use windows::{
        Win32::{
            Graphics::Gdi::{
                COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
            },
            System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
            UI::{
                Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
                WindowsAndMessaging::{
                    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
                },
            },
        },
        core::w,
    };
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: 查询系统设置，输出结构尺寸和缓冲区有效。
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some(&mut contrast as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        if contrast.dwFlags & HCF_HIGHCONTRASTON != Default::default() {
            let convert = |index| {
                let value = GetSysColor(index);
                color(value as u8, (value >> 8) as u8, (value >> 16) as u8, 1.0)
            };
            let background = convert(COLOR_WINDOW);
            let foreground = convert(COLOR_WINDOWTEXT);
            let selection = convert(COLOR_HIGHLIGHT);
            return Palette {
                glass_top: background,
                glass_bottom: background,
                window_top: background,
                window_bottom: background,
                rim_top: foreground,
                rim_bottom: foreground,
                sheen_top: background,
                shade_bottom: background,
                border: foreground,
                text: foreground,
                secondary: foreground,
                tertiary: foreground,
                accent_top: selection,
                accent_bottom: selection,
                accent: selection,
                accent_text: convert(COLOR_HIGHLIGHTTEXT),
                selected: selection,
                hover: selection,
                control: background,
                control_hover: selection,
                card: background,
                input: background,
                danger: foreground,
                danger_tint: background,
                success: foreground,
                close_hover: selection,
                tooltip_bg: background,
                tooltip_border: foreground,
                separator: foreground,
            };
        }
        let mut light = 0u32;
        let mut size = 4u32;
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut _ as *mut _),
            Some(&mut size),
        );
        if std::env::var_os("DESKSHELL_UI_THEME")
            .and_then(|value| value.into_string().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("dark"))
        {
            return dark_palette();
        }
        if std::env::var_os("DESKSHELL_UI_THEME")
            .and_then(|value| value.into_string().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("light"))
        {
            return light_palette();
        }
        if light != 0 {
            return light_palette();
        }
    }
    dark_palette()
}

fn dark_palette() -> Palette {
    Palette {
        glass_top: color(36, 41, 56, 0.92),
        glass_bottom: color(15, 17, 25, 0.96),
        window_top: color(24, 27, 38, 1.0),
        window_bottom: color(14, 16, 24, 1.0),
        rim_top: color(255, 255, 255, 0.34),
        rim_bottom: color(255, 255, 255, 0.05),
        sheen_top: color(255, 255, 255, 0.10),
        shade_bottom: color(0, 0, 0, 0.16),
        border: color(255, 255, 255, 0.12),
        text: color(244, 247, 252, 1.0),
        secondary: color(203, 212, 228, 0.72),
        tertiary: color(203, 212, 228, 0.46),
        accent_top: color(138, 180, 255, 1.0),
        accent_bottom: color(167, 139, 250, 1.0),
        accent: color(138, 176, 250, 1.0),
        accent_text: color(18, 22, 34, 1.0),
        selected: color(138, 180, 255, 0.20),
        hover: color(255, 255, 255, 0.09),
        control: color(255, 255, 255, 0.075),
        control_hover: color(255, 255, 255, 0.13),
        card: color(255, 255, 255, 0.055),
        input: color(18, 22, 32, 1.0),
        danger: color(255, 154, 168, 1.0),
        danger_tint: color(255, 120, 140, 0.14),
        success: color(126, 224, 180, 1.0),
        close_hover: color(255, 255, 255, 0.14),
        tooltip_bg: color(26, 30, 41, 0.98),
        tooltip_border: color(255, 255, 255, 0.16),
        separator: color(255, 255, 255, 0.08),
    }
}

fn light_palette() -> Palette {
    Palette {
        glass_top: color(255, 255, 255, 0.90),
        glass_bottom: color(238, 242, 250, 0.96),
        window_top: color(250, 251, 254, 1.0),
        window_bottom: color(241, 244, 250, 1.0),
        rim_top: color(255, 255, 255, 0.96),
        rim_bottom: color(255, 255, 255, 0.24),
        sheen_top: color(255, 255, 255, 0.62),
        shade_bottom: color(23, 34, 54, 0.06),
        border: color(24, 35, 55, 0.12),
        text: color(22, 28, 38, 1.0),
        secondary: color(35, 47, 65, 0.66),
        tertiary: color(35, 47, 65, 0.44),
        accent_top: color(61, 123, 247, 1.0),
        accent_bottom: color(122, 90, 248, 1.0),
        accent: color(56, 112, 240, 1.0),
        accent_text: color(255, 255, 255, 1.0),
        selected: color(61, 123, 247, 0.14),
        hover: color(22, 34, 54, 0.07),
        control: color(22, 34, 54, 0.055),
        control_hover: color(22, 34, 54, 0.10),
        card: color(255, 255, 255, 0.62),
        input: color(255, 255, 255, 1.0),
        danger: color(179, 38, 58, 1.0),
        danger_tint: color(200, 50, 70, 0.10),
        success: color(22, 128, 92, 1.0),
        close_hover: color(22, 34, 54, 0.10),
        tooltip_bg: color(250, 251, 254, 0.98),
        tooltip_border: color(24, 35, 55, 0.14),
        separator: color(22, 34, 54, 0.08),
    }
}
