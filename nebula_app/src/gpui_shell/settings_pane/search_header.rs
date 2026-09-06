use super::*;

impl SettingsPane {
    pub(super) fn render_search_header(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let query = self.settings_search_input.read(cx).value().trim().to_lowercase();
        let results: Vec<usize> = if query.is_empty() {
            Vec::new()
        } else {
            visible_nav_sections()
                .filter(|index| {
                    SECTION_SEARCH_TERMS[*index].contains(&query)
                        || section_label(*index, language).to_lowercase().contains(&query)
                })
                .take(6)
                .collect()
        };
        let bounds = self.settings_search_trigger_bounds;
        let hairline = crate::gpui_shell::theme::settings_hairline(cx);
        let hover = cx.theme().list_hover;
        let focused = self.settings_search_input.read(cx).focus_handle(cx).is_focused(window);
        let panel = (focused && !results.is_empty()).then(|| {
            v_flex()
                .w(bounds.map(|bounds| bounds.size.width).unwrap_or(px(680.0)))
                .p_1()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(hairline)
                .bg(crate::gpui_shell::theme::settings_panel_bg(cx))
                .text_sm()
                .line_height(px(20.0))
                .shadow_lg()
                .occlude()
                .children(results.into_iter().map(|index| {
                    h_flex()
                        .id(("settings-search-result", index))
                        .h(px(SETTINGS_NAV_ROW_HEIGHT))
                        .px_2()
                        .gap_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |row| row.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.active_section = index;
                            this.settings_search_input
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            cx.notify();
                        }))
                        .child(
                            Icon::default()
                                .path(section_icon(index))
                                .size(px(SETTINGS_NAV_ICON_SIZE)),
                        )
                        .child(section_label(index, language))
                }))
        });
        let trigger = cx.entity().downgrade();
        let search = div()
            .relative()
            .w_full()
            .child(
                Input::new(&self.settings_search_input)
                    .w_full()
                    .cleanable(true)
                    .prefix(
                        Icon::new(IconName::Search)
                            .xsmall()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .aria_label(language.pick("在全部设置中搜索", "Search all settings")),
            )
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = trigger.update(cx, |pane, cx| {
                            if pane.settings_search_trigger_bounds != Some(bounds) {
                                pane.settings_search_trigger_bounds = Some(bounds);
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when_some(panel.zip(bounds), |search, (panel, bounds)| {
                search.child(
                    deferred(
                        anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .position(bounds.bottom_left())
                            .offset(gpui::point(px(0.0), px(6.0)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel),
                    )
                    .with_priority(3),
                )
            });
        h_flex()
            .w_full()
            .h(px(SETTINGS_HEADER_HEIGHT))
            .flex_shrink_0()
            .px_5()
            .items_center()
            .child(search)
            .into_any_element()
    }
}
