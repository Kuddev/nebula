use super::appearance_picker::{AppearanceColors, AppearanceSelection, picker_columns};
use super::*;

pub(super) const THEME_ORDER: [ThemeName; 9] = [
    ThemeName::SilverLight,
    ThemeName::Nebula,
    ThemeName::SteelDark,
    ThemeName::Nord,
    ThemeName::Paper,
    ThemeName::MossDark,
    ThemeName::LimestoneLight,
    ThemeName::CoalDark,
    ThemeName::LinenLight,
];

fn theme_foreground(name: ThemeName) -> [u8; 3] {
    let theme = name.term_theme();
    if let Some(exact) = theme.exact {
        exact.foreground
    } else if theme.is_light {
        nebula_settings::LIGHT_FOREGROUND
    } else {
        let foreground = chrome_theme(name).card_ink().fg;
        [foreground.r, foreground.g, foreground.b]
    }
}

pub(super) fn theme_sample(name: ThemeName, token: bool, compact: bool) -> gpui::Div {
    let theme = chrome_theme(name);
    let palette = theme.palette();
    let background = name.term_theme().background;
    let foreground = theme_foreground(name);
    let accent = theme.accent();
    let chrome = rgb_hsla(palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b);
    let ink = rgb_hsla(foreground[0], foreground[1], foreground[2]);
    let accent = rgb_hsla(accent.r, accent.g, accent.b);
    let line = |width, color| {
        div().w(gpui::relative(width)).h(px(if token { 3.0 } else { 4.0 })).rounded_full().bg(color)
    };
    v_flex()
        .w_full()
        .h(px(if token {
            45.0
        } else if compact {
            60.0
        } else {
            76.0
        }))
        .p(px(if token || compact { 4.0 } else { 6.0 }))
        .rounded(px(8.0))
        .bg(chrome)
        .child(
            v_flex()
                .size_full()
                .justify_center()
                .gap(px(if token { 4.0 } else { 6.0 }))
                .p(px(if token { 5.0 } else { 8.0 }))
                .rounded(px(4.0))
                .bg(rgb_hsla(background[0], background[1], background[2]))
                .child(
                    h_flex()
                        .w_full()
                        .gap(px(5.0))
                        .child(line(0.15, accent))
                        .child(line(0.49, ink.opacity(0.8))),
                )
                .child(line(0.79, ink.opacity(0.55)))
                .child(line(0.43, accent.opacity(0.8))),
        )
}

impl SettingsPane {
    pub(super) fn terminal_appearance_preview(
        &self,
        name: ThemeName,
        typography: bool,
        compact: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let palette = chrome_theme(name).palette();
        let term = name.term_theme();
        let background = if typography && !self.runtime.follow_system_theme {
            self.runtime.background.unwrap_or(term.background)
        } else {
            term.background
        };
        let accent = chrome_theme(name).accent();
        let accent = rgb_hsla(accent.r, accent.g, accent.b);
        let foreground = theme_foreground(name);
        let ink = rgb_hsla(foreground[0], foreground[1], foreground[2]);
        let colors = AppearanceColors::current(cx);
        let family = self.current_font_chain(cx);
        let size = self.terminal_font_size_px(cx);
        v_flex()
            .w_full()
            .min_w_0()
            .rounded(px(9.0))
            .overflow_hidden()
            .border_1()
            .border_color(colors.control)
            .bg(rgb_hsla(background[0], background[1], background[2]))
            .text_color(ink)
            .child(
                h_flex()
                    .w_full()
                    .h(px(34.0))
                    .px(px(12.0))
                    .justify_between()
                    .flex_shrink_0()
                    .bg(rgb_hsla(palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b))
                    .child(
                        h_flex()
                            .gap(px(6.0))
                            .text_size(px(10.0))
                            .child(super::app_icon::icon_image(
                                crate::app_icon::selected(),
                                16.0,
                                window,
                            ))
                            .child(if cfg!(windows) { "PowerShell" } else { "Terminal" }),
                    )
                    .child(div().text_size(px(9.0)).text_color(ink.opacity(0.6)).child("PREVIEW")),
            )
            .child(
                v_flex()
                    .id(if typography {
                        "appearance-font-preview"
                    } else {
                        "appearance-theme-preview"
                    })
                    .w_full()
                    .min_h(px(if compact { 110.0 } else { 181.0 }))
                    .px(px(if typography { 16.0 } else { 13.0 }))
                    .py(px(if compact { 12.0 } else { 19.0 }))
                    .overflow_x_scroll()
                    .font(crate::font_install::gpui_font_with_fallbacks(&family))
                    .text_size(px(size * 0.82))
                    .line_height(gpui::relative(1.75))
                    .child(
                        h_flex()
                            .gap(px(7.0))
                            .child(div().text_color(accent).child("❯"))
                            .child("nebula --version"),
                    )
                    .child(div().mt(px(10.0)).text_size(px(size * 0.75)).child(format!(
                        "{} · {}",
                        crate::brand::NAME,
                        std::env::consts::OS
                    )))
                    .child(
                        h_flex()
                            .gap(px(7.0))
                            .text_size(px(size * 0.73))
                            .child(Icon::new(IconName::Check).size(px(12.0)).text_color(accent))
                            .child("Ready for your next idea."),
                    )
                    .child(
                        h_flex()
                            .mt(px(17.0))
                            .gap(px(7.0))
                            .child(div().text_color(accent).child("❯"))
                            .child(div().w(px(7.0)).h(px(13.0)).bg(ink.opacity(0.85))),
                    ),
            )
    }

    pub(super) fn theme_picker_body(
        &self,
        width: f32,
        compact: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let picker = self.appearance_picker.as_ref().unwrap();
        let AppearanceSelection::Theme(draft) = picker.draft else { unreachable!() };
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let columns = picker_columns(true, f32::from(window.viewport_size().width));
        let grid_width = if compact { width } else { width - 230.0 - 25.0 };
        let gap = if compact { 6.0 } else { 9.0 };
        let option_width = (grid_width - (columns - 1) as f32 * gap) / columns as f32;
        let grid = h_flex()
            .id("appearance-theme-grid")
            .w(px(grid_width))
            .flex_shrink_0()
            .flex_wrap()
            .items_start()
            .gap_x(px(gap))
            .gap_y(px(12.0))
            .role(gpui::accesskit::Role::RadioGroup)
            .aria_label(language.pick("配色主题", "Color themes"))
            .children(picker.draft.choices(picker.filter).into_iter().map(|choice| {
                let AppearanceSelection::Theme(name) = choice else { unreachable!() };
                let selected = draft == name;
                let content = v_flex()
                    .w_full()
                    .p(px(if compact { 6.0 } else { 8.0 }))
                    .child(theme_sample(name, false, compact))
                    .child(
                        h_flex()
                            .mt(px(8.0))
                            .justify_between()
                            .gap(px(3.0))
                            .text_size(px(if compact { 10.0 } else { 11.5 }))
                            .text_color(if selected { colors.ink } else { colors.secondary })
                            .when(selected, |caption| caption.font_semibold())
                            .child(div().truncate().child(choice.label(language)))
                            .child(
                                Icon::new(IconName::Check)
                                    .size(px(12.0))
                                    .when(!selected, |icon| icon.invisible()),
                            ),
                    );
                self.appearance_option(choice, option_width, content, window, cx)
            }));
        let palette = chrome_theme(draft).palette();
        let accent = chrome_theme(draft).accent();
        let term = draft.term_theme();
        let swatches = [
            (term.background, language.pick("终端背景", "Terminal background")),
            (
                [palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b],
                language.pick("界面背景", "Interface background"),
            ),
            (theme_foreground(draft), language.pick("文字", "Text")),
            ([accent.r, accent.g, accent.b], language.pick("强调色", "Accent")),
        ];
        let preview = v_flex().w(px(if compact { width } else { 230.0 })).flex_shrink_0()
            .child(self.terminal_appearance_preview(draft, false, compact, window, cx))
            .child(div().mt(px(if compact { 11.0 } else { 19.0 })).text_size(px(13.0)).font_semibold()
                .when(!compact, |title| title.text_center()).child(chrome_theme(draft).short_label()))
            .child(div().mt(px(5.0)).text_size(px(10.5)).text_color(colors.secondary).when(!compact, |text| text.text_center())
                .child(if palette.is_light { language.pick("浅色主题", "Light theme") } else { language.pick("深色主题", "Dark theme") }))
            .when(!compact, |preview| preview.child(h_flex().mt(px(20.0)).pt(px(17.0)).justify_center().gap(px(9.0))
                .border_t_1().border_color(colors.line).children(swatches.into_iter().enumerate().map(|(index, (color, label))| {
                    let description = format!("{label} {}", format_hex_rgb(color));
                    div().id(("theme-color-swatch", index)).size(px(24.0)).rounded_full().border_1()
                        .border_color(colors.control).bg(rgb_hsla(color[0], color[1], color[2]))
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(description.clone()).build(window, cx))
                }))))
            .child(div().mt(px(if compact { 9.0 } else { 20.0 })).text_size(px(10.5))
                .line_height(gpui::relative(1.7)).text_color(colors.secondary)
                .child(if self.runtime.follow_system_theme {
                    language.pick("手动应用主题后，将关闭「跟随系统」。应用图标保持不变。", "Applying a theme turns off Follow system. Your app icon stays unchanged.")
                } else {
                    language.pick("这里只预览主题，确认后才会生效。应用图标保持不变。", "This is a preview until you confirm. Your app icon stays unchanged.")
                }));
        if compact {
            v_flex().w(px(width)).gap(px(20.0)).child(preview).child(grid)
        } else {
            h_flex().w(px(width)).items_start().gap(px(25.0)).child(grid).child(preview)
        }
    }
}
