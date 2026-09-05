use super::*;
use gpui::accesskit::{Role, Toggled};
use gpui_component::FocusTrapElement as _;
use nebula_settings::AppIconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AppearanceSelection {
    Theme(ThemeName),
    Icon(AppIconName),
}

impl AppearanceSelection {
    pub(super) fn is_theme(self) -> bool {
        matches!(self, Self::Theme(_))
    }

    pub(super) fn label(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::Theme(theme) => chrome_theme(theme).short_label(),
            Self::Icon(icon) => language.pick(icon.palette().name_zh, icon.palette().name_en),
        }
    }

    pub(super) fn updates(self) -> Vec<(&'static str, String)> {
        match self {
            Self::Theme(theme) => {
                crate::gpui_shell::theme::theme_card_persist_updates(theme).to_vec()
            },
            Self::Icon(icon) => vec![("app_icon", icon.settings_value().to_owned())],
        }
    }

    pub(super) fn choices(self, filter: usize) -> Vec<Self> {
        match self {
            Self::Theme(_) => super::theme_picker::THEME_ORDER
                .into_iter()
                .filter(|theme| match filter {
                    1 => chrome_theme(*theme).palette().is_light,
                    2 => !chrome_theme(*theme).palette().is_light,
                    _ => true,
                })
                .map(Self::Theme)
                .collect(),
            Self::Icon(_) => AppIconName::ALL
                .into_iter()
                .filter(|icon| filter == 0 || super::app_icon::icon_family(*icon) == filter)
                .map(Self::Icon)
                .collect(),
        }
    }
}

pub(super) struct AppearancePicker {
    pub(super) draft: AppearanceSelection,
    pub(super) filter: usize,
    pub(super) dark_preview: bool,
    pub(super) focus: FocusHandle,
    pub(super) options: Vec<(AppearanceSelection, FocusHandle)>,
    pub(super) error: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct AppearanceColors {
    pub(super) surface: Hsla,
    pub(super) subtle: Hsla,
    pub(super) selected: Hsla,
    pub(super) ink: Hsla,
    pub(super) secondary: Hsla,
    pub(super) muted: Hsla,
    pub(super) line: Hsla,
    pub(super) control: Hsla,
    pub(super) primary: Hsla,
    pub(super) on_primary: Hsla,
    pub(super) scrim: Hsla,
}

impl AppearanceColors {
    pub(super) fn current(cx: &App) -> Self {
        let light = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().is_light;
        let colors = if light {
            [
                0xffffff, 0xf6f7f8, 0xe5e9ed, 0x2c3440, 0x697586, 0x7e8998, 0xe9ecf0, 0xd9dfe6,
                0x35414f, 0xffffff, 0x1d2733,
            ]
        } else {
            [
                0x282d36, 0x303640, 0x3f4957, 0xe3e8ef, 0xafb9c7, 0x99a5b5, 0x3a414d, 0x4b5564,
                0xd8e1ed, 0x25303f, 0x080c14,
            ]
        };
        let [
            surface,
            subtle,
            selected,
            ink,
            secondary,
            muted,
            line,
            control,
            primary,
            on_primary,
            scrim,
        ] = colors.map(|color| Hsla::from(gpui::rgb(color)));
        Self {
            surface,
            subtle,
            selected,
            ink,
            secondary,
            muted,
            line,
            control,
            primary,
            on_primary,
            scrim: scrim.opacity(if light { 0.30 } else { 0.42 }),
        }
    }
}

pub(super) fn picker_columns(theme: bool, viewport_width: f32) -> usize {
    match (theme, viewport_width <= 390.0) {
        (true, true) => 2,
        (true, false) => 3,
        (false, true) => 4,
        (false, false) => 5,
    }
}

pub(super) fn picker_next_index(
    key: &str,
    current: usize,
    count: usize,
    columns: usize,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let offset = match key {
        "left" => -1,
        "right" => 1,
        "up" => -(columns as isize),
        "down" => columns as isize,
        "home" => return Some(0),
        "end" => return Some(count - 1),
        _ => return None,
    };
    Some((current as isize + offset).rem_euclid(count as isize) as usize)
}

impl SettingsPane {
    pub(super) fn open_appearance_picker(
        &mut self,
        theme: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_font_picker(window, false, cx);
        self.close_background_picker(cx);
        let draft = if theme {
            AppearanceSelection::Theme(crate::gpui_shell::theme::effective_theme_name(cx))
        } else {
            AppearanceSelection::Icon(crate::app_icon::selected())
        };
        let focus = cx.focus_handle();
        let options =
            draft.choices(0).into_iter().map(|choice| (choice, cx.focus_handle())).collect();
        window.focus(&focus, cx);
        self.appearance_picker = Some(AppearancePicker {
            draft,
            filter: 0,
            dark_preview: false,
            focus,
            options,
            error: None,
        });
        cx.notify();
    }

    pub(super) fn close_appearance_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.appearance_picker.take() {
            let trigger = if picker.draft.is_theme() {
                &self.theme_picker_trigger
            } else {
                &self.icon_picker_trigger
            };
            window.focus(trigger, cx);
            cx.notify();
        }
    }

    fn apply_appearance_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.appearance_picker.as_ref().map(|picker| picker.draft) else {
            return;
        };
        if let Err(error) = self.try_persist(&draft.updates(), cx) {
            let language = crate::gpui_shell::config::ui_language(cx);
            if let Some(picker) = self.appearance_picker.as_mut() {
                picker.error = Some(format!(
                    "{}: {error}",
                    language.pick("无法保存，请重试", "Could not save; please retry")
                ));
            }
            cx.notify();
            return;
        }
        if draft.is_theme() {
            self.sync_background_color_picker(window, cx);
        }
        self.close_appearance_picker(window, cx);
    }

    pub(super) fn intercept_appearance_picker(
        &mut self,
        event: &gpui::KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.appearance_picker.as_ref() else {
            return;
        };
        if !picker.focus.contains_focused(window, cx) {
            return;
        }
        let key = event.keystroke.key.to_ascii_lowercase();
        if key == "escape" {
            cx.stop_propagation();
            self.close_appearance_picker(window, cx);
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            cx.stop_propagation();
            return;
        }
        let visible = picker.draft.choices(picker.filter);
        let current = visible.iter().position(|choice| {
            picker
                .options
                .iter()
                .any(|(option, focus)| option == choice && focus.is_focused(window))
        });
        let Some(current) = current else {
            return;
        };
        let columns =
            picker_columns(picker.draft.is_theme(), f32::from(window.viewport_size().width));
        if let Some(next) = picker_next_index(&key, current, visible.len(), columns) {
            cx.stop_propagation();
            self.choose_appearance_draft(visible[next], window, cx);
        } else if matches!(key.as_str(), "space" | "enter") {
            cx.stop_propagation();
            self.choose_appearance_draft(visible[current], window, cx);
        }
    }

    fn choose_appearance_draft(
        &mut self,
        choice: AppearanceSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = self.appearance_picker.as_mut() {
            picker.draft = choice;
            picker.error = None;
            if let Some((_, focus)) = picker.options.iter().find(|(option, _)| *option == choice) {
                window.focus(focus, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn appearance_option(
        &self,
        choice: AppearanceSelection,
        width: f32,
        content: impl IntoElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let picker = self.appearance_picker.as_ref().unwrap();
        let selected = choice == picker.draft;
        let visible = picker.draft.choices(picker.filter);
        let tab_choice = if visible.contains(&picker.draft) { picker.draft } else { visible[0] };
        let focus = &picker.options.iter().find(|(option, _)| *option == choice).unwrap().1;
        let colors = AppearanceColors::current(cx);
        let language = crate::gpui_shell::config::ui_language(cx);
        div()
            .id(SharedString::from(format!("appearance-option-{}", choice.label(language))))
            .track_focus(&focus.clone().tab_stop(choice == tab_choice))
            .role(Role::RadioButton)
            .aria_label(choice.label(language))
            .aria_toggled(if selected { Toggled::True } else { Toggled::False })
            .w(px(width))
            .flex_shrink_0()
            .rounded(px(8.0))
            .border_1()
            .border_color(if selected || focus.is_focused(window) {
                colors.primary
            } else {
                gpui::transparent_black()
            })
            .when(selected, |option| option.bg(colors.subtle))
            .hover(move |option| option.bg(colors.subtle))
            .cursor_pointer()
            .child(content)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.choose_appearance_draft(choice, window, cx);
            }))
            .into_any_element()
    }

    fn appearance_filters(&self, compact: bool, cx: &mut Context<Self>) -> gpui::Div {
        let picker = self.appearance_picker.as_ref().unwrap();
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let filters: &[(&'static str, &'static str)] = if picker.draft.is_theme() {
            &[("全部", "All"), ("浅色", "Light"), ("深色", "Dark")]
        } else {
            &[
                ("全部", "All"),
                ("中性", "Neutral"),
                ("蓝色", "Blue"),
                ("紫色", "Violet"),
                ("青绿", "Green"),
            ]
        };
        let count = picker.draft.choices(picker.filter).len();
        h_flex()
            .mx(px(if compact { 19.0 } else { 27.0 }))
            .pb(px(15.0))
            .gap(px(4.0))
            .border_b_1()
            .border_color(colors.line)
            .flex_shrink_0()
            .children(filters.iter().enumerate().map(|(index, (chinese, english))| {
                let selected = picker.filter == index;
                Button::new(("appearance-filter", index))
                    .label(language.pick(chinese, english))
                    .ghost()
                    .h(px(27.0))
                    .px(px(if compact { 7.0 } else { 10.0 }))
                    .text_size(px(11.0))
                    .rounded(px(5.0))
                    .text_color(if selected { colors.ink } else { colors.secondary })
                    .when(selected, |button| button.bg(colors.selected).font_semibold())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(picker) = this.appearance_picker.as_mut() {
                            picker.filter = index;
                        }
                        cx.notify();
                    }))
            }))
            .when(!compact, |filters| {
                filters.child(
                    div().flex_1().text_right().text_size(px(10.5)).text_color(colors.muted).child(
                        format!(
                            "{count} {}",
                            if picker.draft.is_theme() {
                                language.pick("款主题", "themes")
                            } else {
                                language.pick("款配色", "colors")
                            }
                        ),
                    ),
                )
            })
    }

    pub(super) fn appearance_picker_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let picker = self.appearance_picker.as_ref()?;
        let draft = picker.draft;
        let focus = picker.focus.clone();
        let error = picker.error.clone();
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let viewport = window.viewport_size();
        let compact = f32::from(viewport.width) <= 720.0;
        let padding = if compact { 19.0 } else { 27.0 };
        let width = (f32::from(viewport.width) - if compact { 24.0 } else { 40.0 }).min(770.0);
        let height = f32::from(viewport.height) - 48.0;
        let title = if draft.is_theme() {
            language.pick("选择主题", "Choose theme")
        } else {
            language.pick("选择应用图标", "Choose app icon")
        };
        let subtitle = if draft.is_theme() {
            language.pick(
                "找到舒服的配色，先预览，再应用。",
                "Find a comfortable palette. Preview, then apply.",
            )
        } else {
            language.pick(
                "保留同一个标识，找到喜欢的配色。",
                "The same identity, in your favorite color.",
            )
        };
        let body = if draft.is_theme() {
            self.theme_picker_body(width - 2.0 - padding * 2.0, compact, window, cx)
        } else {
            self.icon_picker_body(width - 2.0 - padding * 2.0, compact, window, cx)
        };
        let apply_label = match draft {
            AppearanceSelection::Theme(theme)
                if !self.runtime.follow_system_theme && theme == self.runtime.theme =>
            {
                language.pick("保留此主题", "Keep this theme")
            },
            AppearanceSelection::Theme(_) => language.pick("使用此主题", "Use this theme"),
            AppearanceSelection::Icon(_) => language.pick("使用此图标", "Use this icon"),
        };
        let dialog = v_flex()
            .id("appearance-picker-dialog")
            .role(Role::Dialog)
            .aria_label(title)
            .w(px(width.max(1.0)))
            .max_h(px(height.max(1.0)))
            .flex_shrink_0()
            .rounded(px(14.0))
            .border_1()
            .border_color(colors.control)
            .bg(colors.surface)
            .text_color(colors.ink)
            .shadow_2xl()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .px(px(padding))
                    .pt(px(25.0))
                    .pb(px(21.0))
                    .gap_4()
                    .items_start()
                    .flex_shrink_0()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.0))
                            .child(div().text_size(px(19.0)).font_semibold().child(title))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(colors.secondary)
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        Button::new("close-appearance-picker")
                            .icon(IconName::Close)
                            .ghost()
                            .size(px(28.0))
                            .text_color(colors.secondary)
                            .tooltip(language.pick("关闭", "Close"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_appearance_picker(window, cx)
                            })),
                    ),
            )
            .child(self.appearance_filters(compact, cx))
            .child(
                div()
                    .id("appearance-picker-scroll")
                    .min_h_0()
                    .flex_shrink(1.0)
                    .overflow_y_scroll()
                    .px(px(padding))
                    .pt(px(21.0))
                    .pb(px(24.0))
                    .child(body),
            )
            .when_some(error, |dialog, error| {
                dialog.child(
                    div()
                        .px(px(padding))
                        .pb_2()
                        .text_size(px(11.0))
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .px(px(padding))
                    .py(px(17.0))
                    .gap(px(9.0))
                    .justify_between()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(colors.line)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(7.0))
                            .text_size(px(if compact { 10.0 } else { 11.0 }))
                            .child(
                                div()
                                    .text_color(colors.secondary)
                                    .child(language.pick("已选择", "Selected")),
                            )
                            .child(div().font_medium().truncate().child(draft.label(language))),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .flex_shrink_0()
                            .child(
                                Button::new("cancel-appearance-picker")
                                    .label(language.pick("取消", "Cancel"))
                                    .h(px(33.0))
                                    .px(px(if compact { 11.0 } else { 16.0 }))
                                    .text_size(px(12.0))
                                    .rounded(px(6.0))
                                    .bg(colors.surface)
                                    .border_color(colors.control)
                                    .text_color(colors.ink)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_appearance_picker(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("apply-appearance-picker")
                                    .label(apply_label)
                                    .h(px(33.0))
                                    .px(px(if compact { 11.0 } else { 16.0 }))
                                    .text_size(px(12.0))
                                    .rounded(px(6.0))
                                    .bg(colors.primary)
                                    .border_color(colors.primary)
                                    .text_color(colors.on_primary)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_appearance_selection(window, cx)
                                    })),
                            ),
                    ),
            )
            .focus_trap("appearance-picker-focus-trap", &focus);
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .id("appearance-picker-overlay")
                            .w(viewport.width)
                            .h(viewport.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .occlude()
                            .bg(colors.scrim)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_appearance_picker(window, cx);
                                }),
                            )
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .child(dialog),
                    ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }
}
