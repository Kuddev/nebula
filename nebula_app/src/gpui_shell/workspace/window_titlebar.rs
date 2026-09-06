use super::*;

pub(super) fn settings_aware_title_bar(settings_active: bool, cx: &App) -> TitleBar {
    TitleBar::new().h(px(48.0)).when(settings_active, |bar| {
        bar.border_b_1().border_color(crate::gpui_shell::theme::settings_hairline(cx))
    })
}
