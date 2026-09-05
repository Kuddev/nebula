use super::*;

impl SettingsPane {
    pub(super) fn appearance_advanced_settings(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let opacity: SharedString = format!("{:.0}%", self.runtime.opacity * 100.0).into();
        let wallpaper_opacity: SharedString =
            format!("{:.0}%", self.runtime.background_image_opacity * 100.0).into();
        let custom_background = self
            .group(language.pick("自定义背景", "Custom background"), cx)
            .child(self.background_color_row(cx))
            .child(self.background_image_row(cx))
            .child(self.select_row("background_image_fit", language.pick("背景图像拉伸模式", "Background image fit"), language.pick("拉伸会把图压变形；适应保持比例、四周留边；填充保持比例但裁掉溢出的部分；原始尺寸按图片自己的像素铺。", "Fill distorts the image; Uniform preserves its aspect ratio and may leave margins; Uniform to fill preserves the ratio but crops overflow; Original size uses the image's own pixels."), cx))
            .child(self.select_row("background_image_alignment", language.pick("背景图像对齐", "Background image alignment"), language.pick("图片没铺满或被裁掉时，保留哪一侧。壁纸类的图通常选顶部或居中，主体才不会被切走。", "Choose which edge to preserve when the image does not fill the area or is cropped. Top or center usually keeps a wallpaper's subject visible."), cx))
            .child(self.slider_row(
                language.pick("背景图像不透明度", "Background image opacity"),
                language.pick("压得越低文字越清楚。图片不参与配色，字色始终由主题决定。", "Lower values make text easier to read. The image does not affect colors; text colors always come from the theme."),
                &self.wallpaper_opacity_slider,
                wallpaper_opacity,
                cx,
            ))
            .child(self.switch_row(
                "background_image_cover_chrome",
                language.pick("将背景图扩展到标题栏和侧边栏", "Extend the background image into the title bar and sidebar"),
                language.pick("开着时整窗一张图，界面和终端连成一片；关掉则图片只铺终端区域，侧栏与标题栏保持纯色、文字更稳。", "When enabled, one image covers the entire window. When disabled, it covers only the terminal while the sidebar and title bar keep a solid background."),
                self.runtime.background_image_cover_chrome,
                cx,
            ));
        let cursor = self
            .group(language.pick("光标", "Cursor"), cx)
            .child(self.select_row(
                "cursor_shape",
                language.pick("光标形状", "Cursor shape"),
                language.pick("条形贴近编辑器的手感，实心框在满屏输出里最容易一眼找到。", "A bar feels closer to an editor; a filled box is easiest to spot in dense terminal output."),
                cx,
            ))
            .child(self.switch_row(
                "cursor_blink",
                language.pick("光标闪烁", "Blink cursor"),
                language.pick("关掉后光标常亮。长时间盯屏时不闪更省心，代价是光标在密集输出里没那么显眼。", "When disabled, the cursor stays lit. A steady cursor is calmer during long sessions but less visible in dense output."),
                self.runtime.cursor_blink.unwrap_or(DEFAULT_CURSOR_BLINK),
                cx,
            ));
        let interface = self
            .group(language.pick("界面", "Interface"), cx)
            .child(self.switch_row(
                "tab_close_visible",
                language.pick("显示标签关闭按钮", "Show tab close buttons"),
                language.pick(
                    "关闭后侧栏与顶栏的标签都不再显示关闭按钮；中键点击标签仍可关闭。",
                    "Hides close buttons in both sidebar and top tabs. Middle-click still closes a tab.",
                ),
                self.runtime.tab_close_visible,
                cx,
            ))
            .child(self.select_row(
                "language",
                language.pick("语言", "Language"),
                language.pick("只改 Pebrel 自己的界面。终端里程序输出什么语言由它们自己的环境变量决定，不受这里影响。", "Changes only Pebrel's interface. Programs inside the terminal choose their language from their own environment and are not affected."),
                cx,
            ))
            .child(self.select_row("density", language.pick("界面外观", "Interface density"), language.pick("紧凑会收窄标签行高与设置页行距这些界面留白。终端内容的行距不归它管，那是字体的事。", "Compact reduces interface spacing such as tab height and Settings row gaps. It does not change terminal line spacing, which is controlled by the font."), cx))
            .child(self.slider_row(
                language.pick("终端正文不透明度", "Terminal content opacity"),
                language.pick("1 = 完全不透明。调低会透出后方窗口，配合下面的窗口模糊才不至于让文字压在杂乱内容上。", "1 is fully opaque. Lower values reveal windows behind Pebrel; use window blur to keep terminal text readable over busy content."),
                &self.opacity_slider,
                opacity,
                cx,
            ))
            .child(self.select_row("blur", language.pick("背景模糊", "Background blur"), language.pick("五者是五套成本模型，不是越靠后越好：Mica 只取系统壁纸的色调、最省；Aero 与 Acrylic 每帧实时模糊窗口后方的真实内容，Acrylic 还多一层着色与噪点。", "These are different performance models, not quality levels. Mica only samples the wallpaper tint and costs least; Aero and Acrylic blur live content behind the window every frame, and Acrylic adds tint and noise."), cx));
        let terminal = self
            .group(language.pick("终端外观", "Terminal appearance"), cx)
            .child(self.select_row("cell_width_mode", language.pick("字体间距", "Character spacing"), language.pick("列宽的取整方式。紧凑向下取整、字更密；宽松向上补一像素，专治 `Maple Mono` 这类平均字宽带小数的字体把字形挤扁。只作用于终端网格。", "Controls how terminal cell width is rounded. Compact rounds down for denser text; Relaxed adds a pixel to prevent fonts with fractional widths, such as `Maple Mono`, from looking squeezed. This affects only the terminal grid."), cx))
            .child(self.switch_row(
                "fetch",
                language.pick("启动欢迎信息", "Startup system information"),
                language.pick("新会话开头跑一次 `fastfetch` 打印系统信息。默认关，因为开新标签会因此慢一拍。", "Runs `fastfetch` at the start of a new session. It is off by default because it adds a delay when opening a tab."),
                self.runtime.fetch,
                cx,
            ))
            .child(self.switch_row(
                "powerline",
                language.pick("Powerline 提示符", "Powerline prompt"),
                language.pick("给 Pebrel 注入的提示符加箭头分段。需要终端字体带 Powerline 字形，否则那些箭头会显示成方框。", "Adds arrow segments to Pebrel's injected prompt. The terminal font must include Powerline glyphs or the arrows will render as boxes."),
                self.runtime.powerline,
                cx,
            ));

        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(custom_background)
            .child(cursor)
            .child(interface)
            .child(terminal)
    }
}
