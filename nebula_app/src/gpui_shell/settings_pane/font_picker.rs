use super::*;

use crate::display::ui::tokens::radius;
use crate::font_install::{FontCatalogEntry, FontSource, REQUIRED_FONT_FAMILY};

impl SettingsPane {
    /// 当前生效字体链（settings.txt 覆盖 toml 后的值，可能含逗号 fallback）。
    pub(in crate::gpui_shell) fn current_font_chain(&self, cx: &App) -> String {
        cx.try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from(REQUIRED_FONT_FAMILY))
    }

    fn configured_font_families(&self, cx: &App) -> Vec<String> {
        let mut families = crate::font_install::font_family_chain(&self.current_font_chain(cx));
        if families.is_empty() {
            families.push(REQUIRED_FONT_FAMILY.to_owned());
        }
        families
    }

    fn toggle_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.font_picker_open {
            self.close_font_picker(window, true, cx);
            return;
        }

        // 查询只属于一次弹层生命周期；昂贵目录在首次打开后复用。
        self.font_query_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.font_picker_open = true;
        self.ensure_font_catalog(cx);
        self.font_query_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// 关闭字体弹层并清理查询。系统文件选择器接管焦点时不强抢窗口焦点。
    pub(super) fn close_font_picker(
        &mut self,
        window: &mut Window,
        restore_focus: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.font_picker_open {
            return;
        }
        self.font_picker_open = false;
        self.font_query_input.update(cx, |input, cx| input.set_value("", window, cx));
        if restore_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    /// 系统字体和导入字体的探测都可能读大量文件，必须离开 UI 线程；目录
    /// 结果只装配一次，整个字体组下拉框共享这一份缓存。
    fn ensure_font_catalog(&mut self, cx: &mut Context<Self>) {
        if self.font_system.is_some() || self.font_loading {
            return;
        }
        #[cfg(windows)]
        {
            self.font_loading = true;
            let task = cx.background_executor().spawn(async move {
                let system = crate::font_install::enumerate_system_font_families();
                let imported: Vec<String> = crate::font_install::imported_font_files()
                    .iter()
                    .filter_map(|path| crate::font_install::probe_font_file_families(path).ok())
                    .flatten()
                    .collect();
                (system, imported)
            });
            cx.spawn(async move |this, cx| {
                let (system, imported) = task.await;
                let _ = this.update(cx, |pane, cx| {
                    pane.font_system = Some(system);
                    for family in imported {
                        if !pane
                            .font_imported
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(&family))
                        {
                            pane.font_imported.push(family);
                        }
                    }
                    pane.font_loading = false;
                    cx.notify();
                });
            })
            .detach();
        }
        #[cfg(not(windows))]
        {
            self.font_system = Some(Vec::new());
        }
    }

    /// 候选行单击即把字体追加到组尾；弹层保持打开，方便连续选择多个 fallback。
    fn append_font_family(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::append_font_fallback(&current, &family);
        self.font_query_input.update(cx, |input, cx| input.set_value("", window, cx));
        if next != current {
            self.persist(&[("font_family", next)], cx);
        } else {
            cx.notify();
        }
    }

    fn move_font_family(&mut self, index: usize, direction: i32, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::move_font_family(&current, index, direction);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
    }

    fn remove_font_family(&mut self, index: usize, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::remove_font_family(&current, index);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
    }

    /// 收起态只有一只下拉框；完整字体链用箭头串起来，最后补上渲染器实际
    /// 使用的内置 Maple。布局逐项对齐 HTML 原型：字体名分别收缩并显示
    /// 省略号，链路箭头与末端 chevron 始终保留。
    fn font_picker_row(&self, stacked: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut families = self.configured_font_families(cx);
        let implicit_required =
            !families.iter().any(|family| family.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY));
        if implicit_required {
            families.push(REQUIRED_FONT_FAMILY.to_owned());
        }
        let mut summary = h_flex().flex_1().min_w_0().items_center().overflow_hidden();
        for (index, family) in families.iter().enumerate() {
            let color = if index == 0 {
                cx.theme().foreground
            } else if implicit_required && index + 1 == families.len() {
                cx.theme().muted_foreground.opacity(0.72)
            } else {
                cx.theme().muted_foreground
            };
            summary = summary.child(
                div()
                    .min_w_0()
                    .truncate()
                    .font(crate::font_install::gpui_font_with_fallbacks(family))
                    .text_color(color)
                    .child(font_display_name(family)),
            );
            if index + 1 < families.len() {
                summary = summary.child(
                    div()
                        .flex_shrink_0()
                        .px(px(7.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.72))
                        .child("→"),
                );
            }
        }
        let skin = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let accent = super::rgb_hsla(skin.accent.r, skin.accent.g, skin.accent.b);
        let ink_dim = super::rgb_hsla(skin.ink_dim.r, skin.ink_dim.g, skin.ink_dim.b);
        let surface = super::rgb_hsla(skin.panel.r, skin.panel.g, skin.panel.b);
        let hover = gpui::Rgba {
            r: f32::from(skin.hover.r) / 255.0,
            g: f32::from(skin.hover.g) / 255.0,
            b: f32::from(skin.hover.b) / 255.0,
            a: (f32::from(skin.hover.a) / 255.0).max(0.35),
        };
        let picker = cx.entity().downgrade();
        let control = div()
            .id("font-picker-toggle")
            .relative()
            .w_full()
            .h(px(32.0))
            .flex_shrink_0()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(cx.theme().border)
            .bg(surface)
            .when(self.font_picker_open, |control| control.border_color(accent).bg(hover))
            .hover(|control| control.bg(hover))
            .cursor_pointer()
            .overflow_hidden()
            .child(
                h_flex()
                    .h_full()
                    .w_full()
                    .min_w_0()
                    .pl(px(12.0))
                    // 与 HTML 的固定 chevron 相同：摘要只能使用右侧图标之前
                    // 的空间，绝不能把图标挤出控件。
                    .pr(px(28.0))
                    .items_center()
                    .text_left()
                    .child(summary),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.0))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(
                        Icon::new(if self.font_picker_open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall()
                        .text_color(ink_dim),
                    ),
            )
            // 与组件库 Popover 相同，用布局后的真实 Bounds 处理滚动、DPI
            // 和缩放；弹层宽度虽与按钮一致，锚点仍必须来自真实布局。
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |picker, _| {
                            picker.font_picker_trigger_bounds = Some(bounds);
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_font_picker(window, cx);
            }));
        self.responsive_wide_row(
            "终端字体",
            "字体组从左到右依次查找缺失字形；第一项是主字体，内置 Maple 自动保留为最后兜底。",
            stacked,
            control,
            cx,
        )
        .into_any_element()
    }

    fn chain_action(
        id: SharedString,
        icon: IconName,
        tooltip: &'static str,
        disabled: bool,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .icon(icon)
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
    }

    /// 同一只下拉框内先列当前顺序，再列可加入字体。候选项不再承担
    /// “替换主字体”和“追加 fallback”两种模式；加入后通过排序自然决定角色。
    fn font_picker_panel(&mut self, width: gpui::Pixels, cx: &mut Context<Self>) -> gpui::Div {
        let muted = cx.theme().muted_foreground;
        let hover_bg = cx.theme().list_hover;
        let selected_bg = cx.theme().list_active;
        let warning = cx.theme().warning;
        let border = cx.theme().border;
        let families = self.configured_font_families(cx);
        let family_count = families.len();
        let primary = families.first().cloned().unwrap_or_else(|| REQUIRED_FONT_FAMILY.to_owned());
        let implicit_required =
            !families.iter().any(|family| family.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY));
        let query = self.font_query_input.read(cx).value().to_string();
        let system = self.font_system.clone().unwrap_or_default();
        let mut catalog = crate::font_install::font_catalog(
            &system,
            &self.font_imported,
            self.font_show_all,
            &query,
            &primary,
        );
        catalog.retain(|entry| {
            !families.iter().any(|family| family.eq_ignore_ascii_case(&entry.name))
                && !entry.name.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY)
        });
        catalog.insert(
            0,
            FontCatalogEntry {
                name: REQUIRED_FONT_FAMILY.to_owned(),
                monospaced: true,
                source: FontSource::Imported,
            },
        );
        if !implicit_required {
            catalog.retain(|entry| !entry.name.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY));
        }

        let badge = |text: &'static str, color: Hsla| {
            div()
                .flex_shrink_0()
                .px(px(5.0))
                .py(px(1.0))
                .rounded_sm()
                .text_xs()
                .text_color(color)
                .border_1()
                .border_color(color.opacity(0.4))
                .child(text)
        };

        let selected_rows: Vec<_> = families
            .iter()
            .enumerate()
            .map(|(index, family)| {
                let family_name: SharedString = family.clone().into();
                h_flex()
                    .id(SharedString::from(format!("font-chain-row-{index}")))
                    .h(px(38.0))
                    .w_full()
                    .min_w_0()
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded_md()
                    .bg(selected_bg)
                    .child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child((index + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family_name)
                            .child(font_display_name(family)),
                    )
                    .when(index == 0, |row| row.child(badge("主", muted)))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-up-{index}")),
                        IconName::ArrowUp,
                        "上移",
                        index == 0,
                        move |this, cx| this.move_font_family(index, -1, cx),
                        cx,
                    ))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-down-{index}")),
                        IconName::ArrowDown,
                        "下移",
                        index + 1 == family_count,
                        move |this, cx| this.move_font_family(index, 1, cx),
                        cx,
                    ))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-delete-{index}")),
                        IconName::Delete,
                        "移出字体组",
                        family_count == 1,
                        move |this, cx| this.remove_font_family(index, cx),
                        cx,
                    ))
            })
            .collect();

        let implicit_row = implicit_required.then(|| {
            h_flex()
                .h(px(36.0))
                .w_full()
                .min_w_0()
                .px_1()
                .gap_1()
                .items_center()
                .text_color(muted)
                .child(
                    div()
                        .w(px(16.0))
                        .flex_shrink_0()
                        .text_xs()
                        .child((family_count + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_family(REQUIRED_FONT_FAMILY)
                        .child(REQUIRED_FONT_FAMILY),
                )
                .child(badge("内置", muted))
        });

        let available_rows: Vec<_> = catalog
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let name = entry.name.clone();
                let family: SharedString = entry.name.clone().into();
                let required = entry.name.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY);
                h_flex()
                    .id(SharedString::from(format!("font-available-row-{index}")))
                    .h(px(36.0))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|row| row.bg(hover_bg))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family)
                            .child(font_display_name(&entry.name)),
                    )
                    .when(required, |row| row.child(badge("内置", muted)))
                    .when(!required && entry.source == FontSource::Imported, |row| {
                        row.child(badge("导入", muted))
                    })
                    .when(!entry.monospaced, |row| row.child(badge("比例", warning)))
                    .child(Icon::new(IconName::Plus).xsmall())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.append_font_family(name.clone(), window, cx);
                    }))
            })
            .collect();
        let available_empty = available_rows.is_empty();

        v_flex()
            // 宽度来自触发器的真实布局 bounds；桌面 460px、窄屏随容器缩小。
            .w(width)
            .max_w_full()
            .p_2()
            .gap_2()
            .popover_style(cx)
            .occlude()
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("字体组"))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("第 1 项是主字体，其余按顺序回退"),
            )
            .child(Input::new(&self.font_query_input))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().text_color(muted).child("显示全部"))
                    .child(Switch::new("font-show-all").checked(self.font_show_all).on_click(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.font_show_all = *checked;
                            cx.notify();
                        }),
                    )),
            )
            .when(self.font_loading, |panel| {
                panel.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().xsmall())
                        .child(div().text_sm().text_color(muted).child("正在枚举系统字体…")),
                )
            })
            .when(!self.font_loading, |panel| {
                panel.child(v_flex().max_h(px(360.0)).overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        // gpui-component 的纵向滚动条以 16px 绝对定位覆盖在
                        // 内容右侧，不会自动挤出布局空间。这里预留同宽安全区，
                        // 避免删除与添加按钮落进滚动条命中区而发生误触。
                        .pr(px(16.0))
                        .gap_1()
                        .child(div().px_1().text_xs().text_color(muted).child("当前顺序"))
                        .children(selected_rows)
                        .when_some(implicit_row, |list, row| list.child(row))
                        .child(
                            div()
                                .mt_2()
                                .pt_2()
                                .border_t_1()
                                .border_color(border.opacity(0.5))
                                .px_1()
                                .text_xs()
                                .text_color(muted)
                                .child("可用字体"),
                        )
                        .children(available_rows)
                        .when(available_empty, |list| {
                            list.child(
                                div().py_2().text_sm().text_color(muted).child("没有匹配的字体"),
                            )
                        }),
                ))
            })
    }

    /// 单一字体组下拉框及其延迟弹层。右边缘与按钮对齐，宽度又完全相同，
    /// 所以滚动、DPI 缩放和窄窗口下都不会出现两套横向尺寸。
    pub(super) fn font_picker_dropdown(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // 与 HTML `@media (max-width: 800px)` 同一个断点。GPUI 的 viewport
        // 尺寸是逻辑像素，DPI 缩放不会改变布局裁定。
        let stacked = f32::from(window.viewport_size().width) <= 800.0;
        let row = self.font_picker_row(stacked, cx);
        let trigger_bounds = self.font_picker_trigger_bounds;
        let panel_width = trigger_bounds
            .as_ref()
            .map(|bounds| bounds.size.width)
            .unwrap_or(px(FONT_PICKER_WIDTH));
        let panel = self.font_picker_open.then(|| self.font_picker_panel(panel_width, cx));

        div().relative().w_full().flex_shrink_0().child(row).when_some(
            panel.zip(trigger_bounds),
            |anchor, (panel, trigger_bounds)| {
                anchor.child(
                    deferred(
                        anchored()
                            .anchor(gpui::Corner::TopRight)
                            .position(trigger_bounds.bottom_right())
                            .offset(gpui::point(px(0.0), px(6.0)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel),
                    )
                    .with_priority(2),
                )
            },
        )
    }
}
