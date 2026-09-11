use super::*;

impl SettingsPane {
    pub(super) fn matching_settings_sections(&self, cx: &App) -> Vec<usize> {
        let query = self.settings_search_input.read(cx).value();
        matching_sections(&query, crate::gpui_shell::config::ui_language(cx))
    }

    pub(super) fn update_settings_search(&mut self, cx: &mut Context<Self>) {
        let searching = !self.settings_search_input.read(cx).value().trim().is_empty();
        if searching {
            self.search_origin_section.get_or_insert(self.active_section);
            let sections = self.matching_settings_sections(cx);
            if !sections.contains(&self.active_section) {
                if let Some(index) = sections.first() {
                    self.active_section = *index;
                }
                self.font_picker_open = false;
                self.bg_picker_open = false;
            }
        } else if let Some(origin) = self.search_origin_section.take() {
            self.active_section = origin;
        }
        cx.notify();
    }

    pub(super) fn render_search_header(&self, _: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        h_flex()
            .w_full()
            .h(px(SETTINGS_HEADER_HEIGHT))
            .flex_shrink_0()
            .px_5()
            .items_center()
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
            .into_any_element()
    }
}

pub(super) fn matching_sections(query: &str, language: crate::display::UiLanguage) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    visible_nav_sections()
        .filter(|index| {
            let haystack = format!(
                "{} {}",
                SECTION_SEARCH_TERMS[*index],
                section_label(*index, language).to_lowercase()
            );
            query.split_whitespace().all(|word| haystack.contains(word))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_filters_navigation_without_an_extra_click() {
        let en = crate::display::UiLanguage::EnUs;
        let zh = crate::display::UiLanguage::ZhCn;
        assert_eq!(matching_sections(" FONT ", en), vec![1]);
        assert_eq!(matching_sections("字体", zh), vec![1]);
        assert_eq!(matching_sections("quick terminal", en), vec![7]);
        assert!(matching_sections("no such setting", en).is_empty());
        assert_eq!(matching_sections("", en), visible_nav_sections().collect::<Vec<_>>());
        assert!(!matching_sections("AI", en).contains(&3));
    }

    #[test]
    fn ai_toast_search_opens_the_terminal_alert_controls() {
        for query in ["AI 消息弹窗", "AI消息通知", "右下角", "ai toast", "notifications"]
        {
            for language in [crate::display::UiLanguage::ZhCn, crate::display::UiLanguage::EnUs] {
                assert_eq!(matching_sections(query, language), vec![2], "{query}");
            }
        }
    }
}
