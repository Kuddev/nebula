use super::*;

/// Prepaint records the actual pane column before any titlebar paint runs. Keep
/// this cell for the workspace lifetime, so resizing and panel animations do not
/// allocate a new shared slot or duplicate the body's layout calculations.
#[derive(Clone, Default)]
pub(super) struct TitleBarBackground(Rc<std::cell::Cell<Option<Bounds<Pixels>>>>);

impl TitleBarBackground {
    pub(super) fn record_pane(&self, bounds: Bounds<Pixels>) {
        self.0.set(Some(bounds));
    }

    fn element(&self) -> gpui::Canvas<()> {
        let pane_bounds = self.0.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let card = crate::gpui_shell::theme::PaneCardStyle::current(cx);
                let shell = cx.theme().background;
                let Some(pane) = pane_bounds.get().filter(|_| card.radius == 0.0) else {
                    window.paint_quad(fill(bounds, shell));
                    return;
                };
                let title_left = f32::from(bounds.origin.x);
                let title_right = title_left + f32::from(bounds.size.width);
                let pane_left = f32::from(pane.origin.x).clamp(title_left, title_right);
                let pane_right = f32::from(pane.right()).clamp(pane_left, title_right);
                let content = crate::gpui_shell::theme::card_content_bg(cx);

                // Adjacent bands receive alpha once. The actual pane bounds also
                // account for a hidden sidebar and an open file tree.
                for (left, right, color) in [
                    (title_left, pane_left, shell),
                    (pane_left, pane_right, content),
                    (pane_right, title_right, shell),
                ] {
                    if right > left {
                        window.paint_quad(fill(
                            Bounds::new(
                                gpui::point(px(left), bounds.origin.y),
                                size(px(right - left), bounds.size.height),
                            ),
                            color,
                        ));
                    }
                }
            },
        )
        .absolute()
        .inset_0()
    }
}

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

impl NebulaWorkspace {
    pub(super) fn render_window_title_bar(
        &self,
        files_active: bool,
        git_active: bool,
        settings_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let top_tabs = self.tabs_position == nebula_settings::TabsPositionName::Top;
        let bar = TitleBar::new()
            // Leave 8px above and below the existing 32px controls.
            .h(px(48.0))
            .when(!settings_active, |bar| bar.bg(gpui::transparent_black()).border_b_0())
            .when(settings_active, |bar| {
                bar.border_b_1().border_color(crate::gpui_shell::theme::settings_hairline(cx))
            })
            .when(top_tabs, |bar| {
                bar.pl(px(top_tabs::TOP_TAB_LEFT_INSET)).child(self.render_top_title_bar(
                    files_active,
                    git_active,
                    settings_active,
                    window,
                    cx,
                ))
            })
            .when(!top_tabs, |bar| {
                bar.child(self.render_sidebar_title_bar(
                    files_active,
                    git_active,
                    settings_active,
                    cx,
                ))
            });

        div()
            .relative()
            .flex_shrink_0()
            .when(!settings_active, |title| title.child(self.titlebar_background.element()))
            .child(bar)
    }
}
