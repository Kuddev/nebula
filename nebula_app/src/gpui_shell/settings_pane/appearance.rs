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

    fn appearance_typography(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let size = self.terminal_font_size_px(cx);
        let family = self.current_font_chain(cx);
        let summary = format!(
            "{} · {size:.0} px",
            font_display_name(family.split(',').next().unwrap_or(&family))
        );
        let expanded = self.typography_expanded;
        let header = Button::new("appearance-typography-toggle")
            .ghost()
            .w_full()
            .h(px(88.0))
            .px_0()
            .child(
                h_flex()
                    .w_full()
                    .gap(px(18.0))
                    .justify_between()
                    .child(appearance_copy(
                        language.pick("终端字体", "Terminal font"),
                        language.pick(
                            "字体与字号，按需调整。",
                            "Adjust the font and size when needed.",
                        ),
                        colors,
                    ))
                    .child(
                        h_flex()
                            .max_w(px(280.0))
                            .gap(px(19.0))
                            .text_color(colors.secondary)
                            .text_size(px(12.0))
                            .child(div().truncate().child(summary))
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(px(14.0)),
                            ),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.typography_expanded = !this.typography_expanded;
                if !this.typography_expanded {
                    this.close_font_picker(window, true, cx);
                }
                cx.notify();
            }));
        let section = v_flex().w_full().border_b_1().border_color(colors.line).child(header);
        if !expanded {
            return section;
        }
        let font_picker = self.font_picker_dropdown(window, cx);
        let stepper = h_flex()
            .w(px(142.0))
            .h(px(36.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(colors.control)
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
            .child(div().flex_1().text_center().text_size(px(12.0)).child(format!("{size:.0} px")))
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
        let theme = crate::gpui_shell::theme::effective_theme_name(cx);
        section
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .flex_wrap()
                    .gap(px(24.0))
                    .pb(px(24.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(220.0))
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(colors.secondary)
                                    .child(language.pick("字体", "Font")),
                            )
                            .child(font_picker),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(colors.secondary)
                                    .child(language.pick("字号", "Size")),
                            )
                            .child(stepper),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(520.0))
                    .pb(px(24.0))
                    .child(self.terminal_appearance_preview(theme, true, false, window, cx))
                    .child(
                        h_flex()
                            .mt(px(9.0))
                            .justify_between()
                            .text_size(px(10.0))
                            .text_color(colors.muted)
                            .child(
                                h_flex()
                                    .gap(px(5.0))
                                    .child(Icon::new(IconName::Eye).size(px(11.0)))
                                    .child(language.pick("字体预览", "Font preview")),
                            )
                            .child(chrome_theme(theme).short_label()),
                    ),
            )
    }

    pub(super) fn appearance_page_header(&self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        h_flex()
            .w_full()
            .text_color(colors.ink)
            .min_h(px(110.0))
            .flex_shrink_0()
            .px(px(40.0))
            .py(px(25.0))
            .gap(px(20.0))
            .border_b_1()
            .border_color(colors.line)
            .justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(px(25.0))
                            .font_semibold()
                            .child(language.pick("外观", "Appearance")),
                    )
                    .child(div().text_size(px(12.0)).text_color(colors.secondary).child(
                        language.pick(
                            "主题、应用图标与显示偏好。",
                            "Themes, app icons and display preferences.",
                        ),
                    )),
            )
            .child(
                Button::new("appearance-reset")
                    .icon(IconName::Undo2)
                    .label(language.pick("恢复默认", "Restore defaults"))
                    .ghost()
                    .h(px(32.0))
                    .px(px(10.0))
                    .text_size(px(12.0))
                    .text_color(colors.secondary)
                    .on_click(cx.listener(|this, _, window, cx| this.reset_appearance(window, cx))),
            )
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
        let typography = self.appearance_typography(window, cx);
        let platform_note = if cfg!(windows) {
            language.pick("选择自动保存。固定快捷方式与安装器仍使用默认图标。", "Choices are saved automatically. Pinned shortcuts and the installer keep the default icon.")
        } else {
            language.pick("选择自动保存。图标仅应用于应用内，不更改 Dock 或启动器图标。", "Choices are saved automatically. Icons change within the app, not in the Dock or launcher.")
        };
        let advanced =
            self.appearance_advanced_expanded.then(|| self.appearance_advanced_settings(cx));
        v_flex()
            .w_full()
            .text_color(colors.ink)
            .child(
                v_flex()
                    .w_full()
                    .child(
                        h_flex()
                            .w_full()
                            .min_h(px(58.0))
                            .gap(px(25.0))
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
                    .child(
                        h_flex()
                            .w_full()
                            .mt(px(22.0))
                            .pt(px(18.0))
                            .gap(px(16.0))
                            .justify_between()
                            .border_t_1()
                            .border_color(colors.line)
                            .text_size(px(11.5))
                            .text_color(colors.secondary)
                            .child(language.pick(
                                "随系统自动切换深浅色",
                                "Switch between light and dark with the system",
                            ))
                            .child(
                                h_flex()
                                    .gap(px(9.0))
                                    .child(language.pick("跟随系统", "Follow system"))
                                    .child(
                                        crate::gpui_shell::widgets::NebulaSwitch::new(
                                            "appearance-follow-system",
                                        )
                                        .checked(self.runtime.follow_system_theme)
                                        .on_click(
                                            cx.listener(|this, checked: &bool, window, cx| {
                                                this.set_follow_system_theme(*checked, window, cx);
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .mt(px(29.0))
                    .py(px(25.0))
                    .gap(px(25.0))
                    .justify_between()
                    .border_t_1()
                    .border_b_1()
                    .border_color(colors.line)
                    .child(appearance_copy(
                        language.pick("应用图标", "App icon"),
                        language.pick(
                            "独立于主题，切换配色时保持不变。",
                            "Independent of the theme. Stays the same when colors change.",
                        ),
                        colors,
                    ))
                    .child(icon),
            )
            .child(typography)
            .child(
                h_flex()
                    .w_full()
                    .pt(px(21.0))
                    .gap(px(12.0))
                    .items_start()
                    .justify_between()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .items_start()
                            .text_size(px(10.5))
                            .text_color(colors.muted)
                            .child(Icon::new(IconName::Info).size(px(12.0)))
                            .child(platform_note),
                    )
                    .child(
                        Button::new("appearance-advanced-toggle")
                            .ghost()
                            .h(px(24.0))
                            .text_size(px(10.5))
                            .text_color(colors.secondary)
                            .label(language.pick("更多外观设置", "More appearance settings"))
                            .icon(if self.appearance_advanced_expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.appearance_advanced_expanded =
                                    !this.appearance_advanced_expanded;
                                if !this.appearance_advanced_expanded {
                                    this.close_background_picker(cx);
                                    window.focus(&this.focus_handle, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .when_some(advanced, |section, advanced| {
                section.child(div().mt(px(24.0)).child(advanced))
            })
    }
}
