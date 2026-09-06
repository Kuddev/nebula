use super::*;

impl SettingsPane {
    pub(super) fn appearance_advanced_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let font_picker = self.font_picker_dropdown(window, cx);
        let opacity: SharedString = format!("{:.0}%", self.runtime.opacity * 100.0).into();
        let wallpaper_opacity: SharedString =
            format!("{:.0}%", self.runtime.background_image_opacity * 100.0).into();
        let custom_background = self
            .group(language.pick("自定义背景", "Custom background"), cx)
            .child(self.background_color_row(cx))
            .child(self.background_image_row(cx))
            .child(self.select_row(
                "background_image_fit",
                language.pick("背景图像拉伸模式", "Background image fit"),
                help("background_image_fit", language),
                cx,
            ))
            .child(self.select_row(
                "background_image_alignment",
                language.pick("背景图像对齐", "Background image alignment"),
                help("background_image_alignment", language),
                cx,
            ))
            .child(self.slider_row(
                language.pick("背景图像不透明度", "Background image opacity"),
                help("background_image_opacity", language),
                &self.wallpaper_opacity_slider,
                wallpaper_opacity,
                cx,
            ))
            .child(self.switch_row(
                "background_image_cover_chrome",
                language.pick(
                    "将背景图扩展到标题栏和侧边栏",
                    "Extend the background image into the title bar and sidebar",
                ),
                help("background_image_cover_chrome", language),
                self.runtime.background_image_cover_chrome,
                cx,
            ));
        let cursor = self
            .group(language.pick("光标", "Cursor"), cx)
            .child(self.select_row(
                "cursor_shape",
                language.pick("光标形状", "Cursor shape"),
                help("cursor_shape", language),
                cx,
            ))
            .child(self.switch_row(
                "cursor_blink",
                language.pick("光标闪烁", "Blink cursor"),
                help("cursor_blink", language),
                self.runtime.cursor_blink.unwrap_or(DEFAULT_CURSOR_BLINK),
                cx,
            ));
        let interface = self
            .group(language.pick("界面", "Interface"), cx)
            .child(self.switch_row(
                "tab_close_visible",
                language.pick("显示标签关闭按钮", "Show tab close buttons"),
                help("tab_close_visible", language),
                self.runtime.tab_close_visible,
                cx,
            ))
            .child(self.select_row(
                "language",
                language.text(crate::i18n::Message::CommonLanguage),
                help("language", language),
                cx,
            ))
            .child(self.select_row(
                "density",
                language.pick("界面外观", "Interface density"),
                help("density", language),
                cx,
            ))
            .child(self.slider_row(
                language.pick("终端正文不透明度", "Terminal content opacity"),
                help("opacity", language),
                &self.opacity_slider,
                opacity,
                cx,
            ))
            .child(self.select_row(
                "blur",
                language.pick("背景模糊", "Background blur"),
                help("blur", language),
                cx,
            ));
        let terminal = self
            .group(language.pick("终端外观", "Terminal appearance"), cx)
            .child(self.row(
                language.pick("终端字体", "Terminal font"),
                help("font_family", language),
                font_picker,
                cx,
            ))
            .child(self.terminal_font_size_row(cx))
            .child(self.select_row(
                "cell_width_mode",
                language.pick("字体间距", "Character spacing"),
                help("cell_width_mode", language),
                cx,
            ))
            .child(self.switch_row(
                "fetch",
                language.pick("启动欢迎信息", "Startup system information"),
                help("fetch", language),
                self.runtime.fetch,
                cx,
            ))
            .child(self.switch_row(
                "powerline",
                language.pick("Powerline 提示符", "Powerline prompt"),
                help("powerline", language),
                self.runtime.powerline,
                cx,
            ));

        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(terminal)
            .child(cursor)
            .child(interface)
            .child(custom_background)
    }
}
