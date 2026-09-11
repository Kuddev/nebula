use super::*;

/// Paint tab and file-tree seams after the terminal, including the titlebar span.
/// Both use the same pixel snapping and theme color; the right seam stays inside
/// the terminal edge so the subsequently painted drawer cannot cover it.
pub(super) fn paint_pane_dividers(
    bounds: Bounds<Pixels>,
    file_tree: bool,
    window: &mut Window,
    cx: &App,
) {
    let card = crate::gpui_shell::theme::PaneCardStyle::current(cx);
    let color = crate::gpui_shell::theme::card_divider_color(cx);
    if let Some(line) = pane_card_divider_bounds(bounds, card.divider, window.scale_factor()) {
        window.paint_quad(fill(line, color));
    }
    if file_tree {
        let mut edge = bounds;
        edge.origin.x += edge.size.width;
        if let Some(mut line) = pane_card_divider_bounds(edge, card.divider, window.scale_factor())
        {
            line.origin.x -= line.size.width;
            window.paint_quad(fill(line, color));
        }
    }
}

pub(super) fn settings_aware_title_bar(settings_active: bool, cx: &App) -> TitleBar {
    TitleBar::new()
        .h(px(48.0))
        .when(!settings_active && crate::gpui_shell::theme::pane_is_flush(cx), |bar| {
            bar.bg(crate::gpui_shell::theme::theme_term_background(cx)).border_b_0()
        })
        .when(settings_active, |bar| {
            bar.border_b_1().border_color(crate::gpui_shell::theme::settings_hairline(cx))
        })
}
