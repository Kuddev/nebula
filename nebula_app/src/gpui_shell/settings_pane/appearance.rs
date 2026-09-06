use super::appearance_picker::AppearanceColors;
use super::*;

fn appearance_copy(
    title: &'static str,
    description: &'static str,
    colors: AppearanceColors,
) -> gpui::Div {
    v_flex()
        .min_w_0()
        .flex_1()
        .text_left()
        .gap(px(4.0))
        .child(div().text_size(px(14.0)).font_semibold().child(title))
        .child(div().text_size(px(12.0)).text_color(colors.secondary).child(description))
}

impl SettingsPane {
    fn appearance_trigger(
        &self,
        theme: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let name = crate::gpui_shell::theme::effective_theme_name(cx);
        let icon = crate::app_icon::selected();
        let label = if theme {
            chrome_theme(name).short_label()
        } else {
            language.pick(icon.palette().name_zh, icon.palette().name_en)
        };
        let action = if theme {
            language.pick("更换主题", "Change theme")
        } else {
            language.pick("更换图标", "Change icon")
        };
        let focus = if theme { &self.theme_picker_trigger } else { &self.icon_picker_trigger };
        let sample = if theme {
            div()
                .size(px(45.0))
                .flex_shrink_0()
                .child(super::theme_picker::theme_sample(name, true, false))
        } else {
            super::app_icon::icon_image(icon, 45.0, window)
        };
        h_flex()
            .id(if theme { "open-theme-picker" } else { "open-icon-picker" })
            .track_focus(&focus.clone().tab_stop(true))
            .role(gpui::accesskit::Role::Button)
            .aria_label(format!("{action}: {label}"))
            .min_w(px(166.0))
            .max_w(px(250.0))
            .flex_shrink_0()
            .gap(px(11.0))
            .py(px(8.0))
            .pl(px(8.0))
            .pr(px(10.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(if focus.is_focused(window) {
                colors.control
            } else {
                gpui::transparent_black()
            })
            .hover(move |button| button.bg(colors.subtle).border_color(colors.line))
            .cursor_pointer()
            .child(sample)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().text_size(px(12.0)).font_medium().truncate().child(label))
                    .child(div().text_size(px(10.5)).text_color(colors.secondary).child(action)),
            )
            .child(Icon::new(IconName::ChevronRight).size(px(14.0)).text_color(colors.secondary))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_appearance_picker(theme, window, cx)
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    this.open_appearance_picker(theme, window, cx);
                }
            }))
            .into_any_element()
    }

    pub(super) fn terminal_font_size_row(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let size = self.terminal_font_size_px(cx);
        let stepper = h_flex()
            .w(px(142.0))
            .h(px(36.0))
            .items_center()
            .child(
                Button::new("appearance-font-smaller")
                    .icon(IconName::Minus)
                    .ghost()
                    .size(px(34.0))
                    .disabled(size <= 4.0)
                    .tooltip(language.pick("减小字号", "Decrease font size"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_font_size(
                            (this.terminal_font_size_px(cx).ceil() - 1.0).round(),
                            cx,
                        );
                    })),
            )
            .child(div().flex_1().text_center().child(format!("{size:.0} px")))
            .child(
                Button::new("appearance-font-larger")
                    .icon(IconName::Plus)
                    .ghost()
                    .size(px(34.0))
                    .disabled(size >= 96.0)
                    .tooltip(language.pick("增大字号", "Increase font size"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_font_size(
                            (this.terminal_font_size_px(cx).floor() + 1.0).round(),
                            cx,
                        );
                    })),
            );
        self.row(
            language.pick("终端字号（Ctrl+滚轮缩放）", "Terminal font size (Ctrl+wheel)"),
            language.pick("只调整终端文字大小。", "Changes only the terminal text size."),
            stepper,
            cx,
        )
        .into_any_element()
    }

    pub(super) fn section_appearance(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let theme = self.appearance_trigger(true, window, cx);
        let icon = self.appearance_trigger(false, window, cx);
        let selectors = v_flex()
            .w_full()
            .gap(px(24.0))
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(58.0))
                    .gap(px(25.0))
                    .items_center()
                    .justify_between()
                    .child(appearance_copy(
                        language.pick("主题", "Theme"),
                        language.pick(
                            "终端与界面的配色，统一选择。",
                            "One palette for the terminal and interface.",
                        ),
                        colors,
                    ))
                    .child(theme),
            )
            .child(self.switch_row(
                "follow_system_theme",
                language.pick("跟随系统", "Follow system"),
                language.pick(
                    "随系统自动切换深浅色。",
                    "Switches between light and dark with the system.",
                ),
                self.runtime.follow_system_theme,
                cx,
            ))
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(58.0))
                    .gap(px(25.0))
                    .items_center()
                    .justify_between()
                    .child(appearance_copy(
                        language.pick("应用图标", "App icon"),
                        language.pick(
                            "独立于主题，切换配色时保持不变。",
                            "Independent of the theme; changing colors keeps the icon.",
                        ),
                        colors,
                    ))
                    .child(icon),
            );
        let settings = self.appearance_advanced_settings(window, cx);
        v_flex().w_full().gap(px(GROUP_GAP)).child(selectors).child(settings)
    }
}
