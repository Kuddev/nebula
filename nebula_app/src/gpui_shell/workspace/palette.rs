use super::*;

impl NebulaWorkspace {
    pub(super) fn render_command_palette(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::display::ui::tokens::{control, radius, space};

        let theme = cx.theme();
        let panel_bg = theme.popover;
        let surface_bg = theme.muted;
        let selected_bg = theme.list_active;
        let hover_bg = theme.list_hover;
        let accent = theme.primary;
        let border = theme.border;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let overlay = theme.overlay;
        let mono_family = theme.mono_font_family.clone();
        let language = workspace_ui_language();
        let palette_filters = if self.shell_picker_open {
            Some((
                WorkspacePaletteFilter::Launcher(self.launcher_filter),
                self.launcher_chip_counts()
                    .into_iter()
                    .map(|(filter, count)| (WorkspacePaletteFilter::Launcher(filter), count))
                    .collect::<Vec<_>>(),
            ))
        } else {
            self.quick_jump_filter.map(|selected| {
                (
                    WorkspacePaletteFilter::QuickJump(selected),
                    self.quick_jump_chip_counts()
                        .into_iter()
                        .map(|(filter, count)| (WorkspacePaletteFilter::QuickJump(filter), count))
                        .collect::<Vec<_>>(),
                )
            })
        };
        let items = self.filtered_palette_rows(cx);
        if self.command_palette_selected >= items.len() {
            self.command_palette_selected = items.len().saturating_sub(1);
        }
        let has_filter_bar = palette_filters.is_some();
        let item_count = items.len();
        let mut group_count = 0;
        let mut measured_group: Option<&str> = None;
        for item in &items {
            if measured_group != Some(item.group.as_str()) {
                measured_group = Some(item.group.as_str());
                group_count += 1;
            }
        }
        let content_node_count = item_count + group_count;
        let content_height = if item_count == 0 {
            0.0
        } else {
            item_count as f32 * PALETTE_ROW_HEIGHT
                + group_count as f32 * PALETTE_GROUP_HEADER_HEIGHT
                + content_node_count.saturating_sub(1) as f32 * PALETTE_ROW_GAP
        };
        // 面板外框必须稳定：筛选结果变少时只留下空白，不能让居中的面板连同
        // 搜索框一起上下跳。Command Palette 没有筛选条，结果视口自然多占一行。
        let chrome_gap_count = if has_filter_bar { 2.0 } else { 1.0 };
        let results_height = PALETTE_PANEL_HEIGHT
            - space::XS * 2.0
            - control::MIN_HIT_TARGET
            - if has_filter_bar { PALETTE_FILTER_BAR_HEIGHT } else { 0.0 }
            - space::XS * chrome_gap_count;
        let results_scrollable = content_height > results_height;
        // 同一份 picker 只要有一行带图标，就为全部行保留固定 icon rail；常规
        // 命令目录没有图标时整列消失，不会凭空多出一层缩进。
        let has_icon_rail = items.iter().any(|item| {
            item.icon.is_some() || item.icon_glyph.is_some() || item.icon_path.is_some()
        });

        let mut rows = Vec::new();
        let mut previous_group: Option<String> = None;
        for (ix, item) in items.into_iter().enumerate() {
            if previous_group.as_deref() != Some(item.group.as_str()) {
                previous_group = Some(item.group.clone());
                rows.push(
                    h_flex()
                        .h(px(PALETTE_GROUP_HEADER_HEIGHT))
                        .flex_shrink_0()
                        .px_3()
                        .when(results_scrollable, |header| {
                            header.pr(px(PALETTE_SCROLLBAR_CONTENT_GUTTER))
                        })
                        .gap(px(space::XS))
                        .items_center()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(item.group.clone()),
                        )
                        .child(div().h(px(control::HAIRLINE)).flex_1().bg(border))
                        .into_any_element(),
                );
            }

            let icon_content = if let Some(image) = item.icon.clone() {
                Some(
                    gpui::StyledImage::object_fit(
                        img(image).size(px(22.0)),
                        gpui::ObjectFit::Contain,
                    )
                    .into_any_element(),
                )
            } else if let Some(glyph) = item.icon_glyph {
                Some(
                    div()
                        .size(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(crate::font_install::REQUIRED_FONT_FAMILY)
                        .text_size(px(16.0))
                        .text_color(foreground)
                        .child(glyph.to_string())
                        .into_any_element(),
                )
            } else {
                item.icon_path.clone().map(|path| {
                    div()
                        .size(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::default().path(path).small().text_color(foreground))
                        .into_any_element()
                })
            };
            let icon_slot = has_icon_rail.then(|| {
                let slot =
                    div().size(px(22.0)).flex_shrink_0().flex().items_center().justify_center();
                match icon_content {
                    Some(icon) => slot.child(icon).into_any_element(),
                    None => slot.into_any_element(),
                }
            });

            let hint = if item.hint.is_empty() {
                None
            } else {
                match item.hint_style {
                    WorkspacePaletteHintStyle::Metadata => Some(
                        div()
                            .max_w(px(PALETTE_METADATA_MAX_WIDTH))
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(muted)
                            .child(item.hint.clone())
                            .into_any_element(),
                    ),
                    WorkspacePaletteHintStyle::Shortcut => {
                        let keycaps = item
                            .hint
                            .split('+')
                            .filter(|key| !key.is_empty())
                            .map(|key| {
                                h_flex()
                                    .h(px(crate::display::ui::keycap::KEY_H))
                                    .px(px(space::S / 2.0))
                                    .items_center()
                                    .rounded(px(radius::CHIP))
                                    .border_1()
                                    .border_color(border)
                                    .bg(surface_bg)
                                    .font_family(mono_family.clone())
                                    .text_size(px(11.0))
                                    .text_color(muted)
                                    .child(key.to_owned())
                            })
                            .collect::<Vec<_>>();
                        Some(
                            h_flex()
                                .max_w(px(PALETTE_METADATA_MAX_WIDTH))
                                .gap(px(space::XXS))
                                .children(keycaps)
                                .into_any_element(),
                        )
                    },
                }
            };

            let action = item.action.clone();
            let row_tooltip = item.label.clone();
            let selected = ix == self.command_palette_selected;
            let hover_group = SharedString::from(format!("command-palette-row-hover-{ix}"));
            let row_content = h_flex()
                .id(SharedString::from(format!("command-palette-row-{ix}")))
                .group(hover_group.clone())
                .h(px(PALETTE_ROW_HEIGHT))
                .flex_shrink_0()
                .w_full()
                .px_2()
                .when(results_scrollable, |row| row.pr(px(PALETTE_SCROLLBAR_CONTENT_GUTTER)))
                .gap(px(space::XS))
                .items_center()
                .rounded(px(radius::CONTROL))
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(row_tooltip.clone()).build(window, cx)
                })
                .when(selected, |row| row.bg(selected_bg))
                .when(!selected, |row| row.group_hover(hover_group.clone(), |row| row.bg(hover_bg)))
                .when_some(icon_slot, |row, icon| row.child(icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_height(relative(1.0))
                        .text_color(foreground)
                        .child(item.label),
                )
                .when_some(hint, |row, hint| row.child(hint))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.command_palette_selected = ix;
                    match action.clone() {
                        WorkspacePaletteAction::Shared(action) => {
                            this.run_palette_action(action, window, cx);
                        },
                        WorkspacePaletteAction::FocusTab(tab) => {
                            this.dismiss_palette_state();
                            this.activate_tab(tab, window, cx);
                            this.focus_active(window, cx);
                            cx.notify();
                        },
                        WorkspacePaletteAction::FocusPane { tab, pane } => {
                            this.dismiss_palette_state();
                            this.activate_tab(tab, window, cx);
                            this.focus_pane(tab, pane, window, cx);
                            this.focus_active(window, cx);
                            cx.notify();
                        },
                        WorkspacePaletteAction::OpenDirectory(path) => {
                            this.dismiss_palette_state();
                            this.add_terminal_at(Some(path), None, window, cx);
                        },
                        WorkspacePaletteAction::RunAiSession { command, cwd } => {
                            this.dismiss_palette_state();
                            this.add_terminal_at(cwd, Some(command), window, cx);
                        },
                        WorkspacePaletteAction::LaunchSshHost(host) => {
                            this.dismiss_palette_state();
                            this.add_ssh_terminal(host, window, cx);
                        },
                        WorkspacePaletteAction::LaunchShell(detected) => {
                            this.launch_palette_shell(detected, window, cx);
                        },
                        WorkspacePaletteAction::LaunchProfile(profile) => {
                            this.launch_palette_profile(profile, window, cx);
                        },
                    }
                }));

            rows.push(row_content.into_any_element());
        }

        if rows.is_empty() {
            rows.push(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap(px(space::XS))
                    .text_color(muted)
                    .child(Icon::new(IconName::Search).small())
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(language.pick("没有匹配结果", "No matching results")),
                    )
                    .into_any_element(),
            );
        }

        // 搜索框采用紧凑表面、低对比描边和前置搜索图标；边界只包住
        // 输入本身，避免再用整行分隔线把面板切成多层容器。
        let search_box = h_flex()
            .w_full()
            .h(px(control::MIN_HIT_TARGET))
            .flex_shrink_0()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(border)
            .bg(surface_bg)
            .overflow_hidden()
            .child(
                Input::new(&self.command_palette_input)
                    .w_full()
                    .appearance(false)
                    .focus_bordered(false)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).xsmall().text_color(muted))
                    .text_size(px(13.0)),
            );

        let filter_bar = palette_filters.map(|(selected_filter, counts)| {
            let chips = counts
                .into_iter()
                .map(|(filter, count)| {
                    let selected = selected_filter == filter;
                    h_flex()
                        .id(SharedString::from(format!("workspace-palette-filter-{filter:?}")))
                        .h(px(26.0))
                        .px_2()
                        .gap(px(space::XXS))
                        .items_center()
                        .cursor_pointer()
                        .rounded(px(radius::CONTROL))
                        .when(selected, |chip| chip.bg(selected_bg))
                        .when(!selected, |chip| chip.hover(|chip| chip.bg(hover_bg)))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(if selected { accent } else { foreground })
                                .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
                                .child(filter.label(language)),
                        )
                        .child(
                            div()
                                .font_family(mono_family.clone())
                                .text_size(px(11.0))
                                .text_color(if selected { accent } else { muted })
                                .child(count.to_string()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_workspace_palette_filter(filter, window, cx);
                        }))
                })
                .collect::<Vec<_>>();
            h_flex()
                .h(px(PALETTE_FILTER_BAR_HEIGHT))
                .flex_shrink_0()
                .w_full()
                .px_1()
                .gap_1()
                .items_center()
                .children(chips)
                .into_any_element()
        });

        let scroll_handle = self.command_palette_scroll.clone();
        let result_list = v_flex()
            .id("workspace-palette-results-scroll")
            .size_full()
            .gap(px(PALETTE_ROW_GAP))
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .children(rows);
        let results = div()
            .relative()
            .h(px(results_height))
            .flex_shrink_0()
            .min_h_0()
            .overflow_hidden()
            .child(result_list)
            .when(results_scrollable, |results| {
                results.child(
                    gpui_component::scroll::Scrollbar::vertical(&scroll_handle)
                        .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always),
                )
            });

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .key_context(PALETTE_KEY_CONTEXT)
            .bg(overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_command_palette(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        this.move_command_palette_selection(-1, cx);
                        cx.stop_propagation();
                    },
                    "down" => {
                        this.move_command_palette_selection(1, cx);
                        cx.stop_propagation();
                    },
                    "escape" => {
                        this.close_command_palette(window, cx);
                        cx.stop_propagation();
                    },
                    _ => {},
                }
            }))
            .child(
                v_flex()
                    .w(px(PALETTE_PANEL_WIDTH))
                    .h(px(PALETTE_PANEL_HEIGHT))
                    .max_h_full()
                    .rounded(px(radius::OVERLAY))
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .shadow_lg()
                    .p(px(space::XS))
                    .gap(px(space::XS))
                    .overflow_hidden()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .child(search_box)
                    .children(filter_bar)
                    .child(results),
            )
            .into_any_element()
    }
}
