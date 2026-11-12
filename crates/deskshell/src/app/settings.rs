//! 设置窗的视图构建与命令转发；业务状态和执行路径全部复用主程序。
use render::{SettingsEvent, SettingsGroup, SettingsItem, SettingsView};

use super::AppState;

impl AppState {
    pub(super) fn show_settings(&mut self) {
        let view = self.settings_view();
        if let Some(window) = &mut self.settings {
            window.set_view(view);
            window.show();
            return;
        }
        match render::SettingsWindow::create(view) {
            Ok(window) => self.settings = Some(window),
            Err(error) => self.push_notice(format!("打开设置失败：{error}")),
        }
    }

    pub(super) fn refresh_settings(&mut self) {
        let view = self.settings_view();
        if let Some(window) = &mut self.settings {
            window.set_view(view);
            if let Some(error) = window.take_error() {
                self.push_notice(format!("设置窗绘制失败：{error}"));
            }
        }
    }

    pub(super) fn handle_settings_events(&mut self, events: Vec<SettingsEvent>) {
        for event in events {
            match event {
                SettingsEvent::Command(command) => {
                    self.execute_ui_command(command);
                    self.refresh_settings();
                }
                SettingsEvent::Dismissed => {
                    self.settings = None;
                }
            }
        }
    }

    fn settings_view(&self) -> SettingsView {
        let effects = self
            .config
            .quality
            .preset
            .settings(self.config.quality.custom);
        let auto = &self.config.quality.auto;
        let preset = match self.config.quality.preset {
            crate::policy::QualityPreset::Eco => 100,
            crate::policy::QualityPreset::Balanced => 101,
            crate::policy::QualityPreset::Ultra => 102,
            crate::policy::QualityPreset::Custom => 103,
        };
        let autostart = shell_win::autostart::is_enabled().unwrap_or(false);
        let fit = self.config.wallpaper.fit;
        let dock = &self.config.dock;
        let widgets = &self.config.widgets;
        let groups = vec![
            SettingsGroup::new(
                "壁纸与素材",
                vec![
                    SettingsItem::action(300, "选择图片…"),
                    SettingsItem::action(301, "选择视频…"),
                    SettingsItem::action(302, "选择视频静态回退图…"),
                    SettingsItem::action(402, "应用壁纸"),
                    SettingsItem::radio(410, "铺满裁切（无黑边）", fit == protocol::FitMode::Cover),
                    SettingsItem::radio(
                        411,
                        "适应（保留完整画面）",
                        fit == protocol::FitMode::Contain,
                    ),
                    SettingsItem::radio(412, "拉伸铺满", fit == protocol::FitMode::Stretch),
                    SettingsItem::radio(413, "平铺", fit == protocol::FitMode::Tile),
                ],
            ),
            SettingsGroup::new(
                "性能预设",
                vec![
                    SettingsItem::radio(100, "极简 Eco", preset == 100),
                    SettingsItem::radio(101, "均衡 Balanced", preset == 101),
                    SettingsItem::radio(102, "极致 Ultra", preset == 102),
                    SettingsItem::radio(103, "自定义 Custom", preset == 103),
                ],
            ),
            SettingsGroup::new(
                "外观与播放",
                vec![
                    SettingsItem::toggle(110, "UI 动画", effects.ui_animations),
                    SettingsItem::toggle(111, "面板阴影", effects.shadows),
                    SettingsItem::toggle(112, "壁纸播放", effects.wallpaper_playback),
                    SettingsItem::action(4, "暂停 / 恢复壁纸"),
                ],
            ),
            SettingsGroup::new(
                "Dock 与任务栏",
                vec![
                    SettingsItem::toggle(420, "显示运行中应用", dock.show_running),
                    SettingsItem::toggle(421, "显示系统入口", dock.show_system),
                    SettingsItem::radio(
                        422,
                        "大图标",
                        dock.style == crate::config::DockStyle::Large,
                    ),
                    SettingsItem::radio(
                        423,
                        "标准图标",
                        dock.style == crate::config::DockStyle::Standard,
                    ),
                    SettingsItem::radio(
                        424,
                        "图标 + 名称",
                        dock.style == crate::config::DockStyle::Labels,
                    ),
                ],
            ),
            SettingsGroup::new(
                "小部件",
                vec![
                    SettingsItem::toggle(
                        456,
                        "快捷控制（音量 / 网络 / 蓝牙）",
                        widgets.controls.enabled,
                    ),
                    SettingsItem::toggle(450, "桌面收纳", widgets.organizer.base.enabled),
                    SettingsItem::toggle(451, "时钟", widgets.clock.enabled),
                    SettingsItem::toggle(452, "天气", widgets.weather.base.enabled),
                    SettingsItem::toggle(453, "音乐（系统媒体）", widgets.music.enabled),
                ],
            ),
            SettingsGroup::new(
                "自动策略",
                vec![
                    SettingsItem::toggle(120, "全屏应用时暂停", auto.fullscreen),
                    SettingsItem::toggle(121, "完全遮挡时暂停", auto.obscured),
                    SettingsItem::toggle(122, "电池 / 节能时暂停", auto.battery),
                    SettingsItem::toggle(123, "远程会话时降档", auto.remote_session),
                ],
            ),
            SettingsGroup::new(
                "系统与恢复",
                vec![
                    SettingsItem::toggle(401, "开机启动", autostart),
                    SettingsItem::action(6, "恢复桌面图标"),
                    SettingsItem::action(5, "恢复系统壁纸"),
                    SettingsItem::action(400, "状态与故障…"),
                ],
            )
            .with_columns(2),
        ];
        let subtitle = format!(
            "动态壁纸与桌面增强 · {}{}",
            self.config.quality.preset.label(),
            if self.config_read_only {
                " · 配置只读"
            } else {
                " · 更改即时保存"
            }
        );
        SettingsView::new(
            "DeskShell 设置",
            subtitle,
            groups,
            "Esc 关闭 · Tab 切换 · 空格/回车确认 · 详细效果参数在托盘菜单",
        )
    }
}
