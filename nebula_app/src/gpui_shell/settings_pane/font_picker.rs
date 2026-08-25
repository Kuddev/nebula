use super::*;

use crate::font_install::{FontSource, REQUIRED_FONT_FAMILY};

const FONT_PICKER_LIST_PREFERRED_HEIGHT: f32 = 220.0;
const FONT_PICKER_PANEL_CHROME_HEIGHT: f32 = 74.0;
const FONT_PICKER_OFFSET_Y: f32 = 6.0;
const FONT_PICKER_WINDOW_MARGIN: f32 = 8.0;

impl SettingsPane {
    pub(super) fn new_font_family_input(
        runtime_font_family: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let initial = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .or(runtime_font_family)
            .unwrap_or_else(|| REQUIRED_FONT_FAMILY.to_owned());
        cx.new(|cx| InputState::new(window, cx).placeholder("输入字体名称").default_value(initial))
    }

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

    fn open_font_picker(&mut self, cx: &mut Context<Self>) {
        if self.font_picker_open {
            return;
        }

        self.font_picker_open = true;
        self.ensure_font_catalog(cx);
        cx.notify();
    }

    pub(super) fn on_font_family_input_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Focus => self.open_font_picker(cx),
            // 输入时只刷新末段候选；落盘会重建所有终端字体，不能每键执行。
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => self.close_font_picker(window, true, cx),
            InputEvent::Blur => self.close_font_picker(window, false, cx),
        }
    }

    /// 关闭弹层时才规范化并提交手写字体链。系统文件选择器接管焦点时不强抢焦点。
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
        self.commit_font_family_input(window, cx);
        if restore_focus {
            window.focus(&self.focus_handle, cx);
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

    fn commit_font_family_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let raw = self.font_family_input.read(cx).value().to_string();
        let normalized = crate::font_install::normalize_font_family_chain(&raw);
        // 空链无法提供主字体；恢复当前生效值，而不是写入一个随后被忽略的空项。
        let next = if normalized.is_empty() { current.clone() } else { normalized };
        self.set_font_family_input(next.clone(), window, cx);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
    }

    fn set_font_family_input(&self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        self.font_family_input.update(cx, |input, cx| input.set_value(value, window, cx));
    }

    /// 加号始终把字体追加到组尾；与候选行的 WT 式末段补全是两个独立动作。
    fn append_font_family(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::append_font_fallback(&current, &family);
        self.set_font_family_input(next.clone(), window, cx);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
        self.font_family_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// 候选点击替换最后一个逗号段；要追加 fallback 时先在输入末尾键入逗号。
    fn select_font_family(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.font_family_input.read(cx).value().to_string();
        let next = crate::font_install::complete_font_family_input(&raw, &family);
        let current = self.current_font_chain(cx);
        self.set_font_family_input(next.clone(), window, cx);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
        self.font_family_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn move_font_family(
        &mut self,
        index: usize,
        direction: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::move_font_family(&current, index, direction);
        self.set_font_family_input(next.clone(), window, cx);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
    }

    fn remove_font_family(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_font_chain(cx);
        let next = crate::font_install::remove_font_family(&current, index);
        self.set_font_family_input(next.clone(), window, cx);
        if next != current {
            self.persist(&[("font_family", next)], cx);
        }
    }

    /// 与标准 Select 共用 220px 控件列。Input 自己负责长文本的单行滚动，
    /// 逗号链再长也只能在字段内部移动，不能反向撑开设置行。
    fn font_picker_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let picker = cx.entity().downgrade();
        let control = div()
            .id("font-family-input-shell")
            .relative()
            .w(px(SETTINGS_SELECT_WIDTH))
            .min_w_0()
            .h(px(32.0))
            .flex_shrink_0()
            .overflow_hidden()
            .child(
                Input::new(&self.font_family_input)
                    .w_full()
                    .cleanable(false)
                    .aria_label("终端字体"),
            )
            // 弹层仍须取输入框的真实窗口坐标，才能正确处理滚动、缩放与 DPI。
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |picker, cx| {
                            if picker.font_picker_trigger_bounds == Some(bounds) {
                                return;
                            }
                            picker.font_picker_trigger_bounds = Some(bounds);
                            // anchored() 消费的是上一轮 render 拿到的窗口坐标；
                            // 聚焦自动滚动后补一帧，弹层才会继续贴住输入框。
                            if picker.font_picker_open {
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        self.row(
            "终端字体",
            "第 1 项优先显示；缺失字形依次使用后续字体，内置 Maple 是最终安全兜底。",
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
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .icon(icon)
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }

    /// 弹层先列当前顺序，再列输入框末段的建议。整行点击补全，加号则追加到
    /// 字体组末尾，两种常用操作互不抢占。
    fn font_picker_panel(
        &mut self,
        width: gpui::Pixels,
        list_height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let muted = cx.theme().muted_foreground;
        let hover_bg = cx.theme().list_hover;
        let selected_bg = cx.theme().list_active;
        let warning = cx.theme().warning;
        let border = cx.theme().border;
        let families = self.configured_font_families(cx);
        let family_count = families.len();
        let input_value = self.font_family_input.read(cx).value().to_string();
        let current = self.current_font_chain(cx);
        let input_normalized = crate::font_install::normalize_font_family_chain(&input_value);
        let current_normalized = crate::font_install::normalize_font_family_chain(&current);
        // 初次聚焦默认展开完整目录；开始编辑后只拿最后一个逗号段过滤，
        // 这样键入 `Font A,` 会重新显示全部候选供追加 fallback。
        let query = if input_normalized == current_normalized {
            ""
        } else {
            input_value.rsplit(',').next().unwrap_or_default().trim()
        };
        let system = self.font_system.clone().unwrap_or_default();
        let catalog =
            crate::font_install::font_catalog(&system, &self.font_imported, true, query, "");

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
                let display_name: SharedString = font_display_name(family).into();
                let tooltip_name = display_name.clone();
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
                            .id(("font-chain-name", index))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family_name)
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tooltip_name.clone())
                                    .build(window, cx)
                            })
                            .child(display_name),
                    )
                    .when(index == 0, |row| row.child(badge("主", muted)))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-up-{index}")),
                        IconName::ArrowUp,
                        "上移",
                        index == 0,
                        move |this, window, cx| {
                            this.move_font_family(index, -1, window, cx);
                        },
                        cx,
                    ))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-down-{index}")),
                        IconName::ArrowDown,
                        "下移",
                        index + 1 == family_count,
                        move |this, window, cx| {
                            this.move_font_family(index, 1, window, cx);
                        },
                        cx,
                    ))
                    .child(Self::chain_action(
                        SharedString::from(format!("font-chain-delete-{index}")),
                        IconName::Delete,
                        "移出字体组",
                        family_count == 1,
                        move |this, window, cx| this.remove_font_family(index, window, cx),
                        cx,
                    ))
            })
            .collect();

        let available_rows: Vec<_> = catalog
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let select_name = entry.name.clone();
                let append_name = entry.name.clone();
                let family: SharedString = entry.name.clone().into();
                let display_name: SharedString = font_display_name(&entry.name).into();
                let tooltip_name = display_name.clone();
                let bundled = entry.source == FontSource::Bundled;
                let already_selected =
                    families.iter().any(|family| family.eq_ignore_ascii_case(&entry.name));
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
                            .id(("font-available-name", index))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family)
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tooltip_name.clone())
                                    .build(window, cx)
                            })
                            .child(display_name),
                    )
                    .when(bundled, |row| row.child(badge("内置", muted)))
                    .when(entry.source == FontSource::Imported, |row| {
                        row.child(badge("导入", muted))
                    })
                    .when(!entry.monospaced, |row| row.child(badge("比例", warning)))
                    .child(
                        Button::new(SharedString::from(format!("font-add-{index}")))
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .tooltip("加入字体组")
                            .disabled(already_selected)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.append_font_family(append_name.clone(), window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_font_family(select_name.clone(), window, cx);
                    }))
            })
            .collect();
        let available_empty = available_rows.is_empty();

        v_flex()
            // 与输入字段使用同一真实宽度，右缘和左右边界都不会跳变。
            .w(width)
            .max_w_full()
            .p_2()
            .gap_2()
            .popover_style(cx)
            .occlude()
            .overflow_hidden()
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("字体组"))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("第 1 项是主字体，其余按顺序回退"),
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
                panel.child(v_flex().h(list_height).min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        // gpui-component 的纵向滚动条以 16px 绝对定位覆盖在
                        // 内容右侧，不会自动挤出布局空间。这里预留同宽安全区，
                        // 避免排序操作与候选行落进滚动条命中区而发生误触。
                        .pr(px(16.0))
                        .gap_1()
                        .child(div().px_1().text_xs().text_color(muted).child("当前顺序"))
                        .children(selected_rows)
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

    /// 可编辑字体字段及其同宽延迟弹层；较长内容只在列表内部滚动。
    pub(super) fn font_picker_dropdown(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let row = self.font_picker_row(cx);
        let trigger_bounds = self.font_picker_trigger_bounds;
        let panel_width = trigger_bounds
            .as_ref()
            .map(|bounds| bounds.size.width)
            .unwrap_or(px(SETTINGS_SELECT_WIDTH));
        let list_height = trigger_bounds
            .as_ref()
            .map(|bounds| {
                let viewport_height = f32::from(window.viewport_size().height);
                let trigger_bottom = f32::from(bounds.origin.y + bounds.size.height);
                let available_below = (viewport_height
                    - trigger_bottom
                    - FONT_PICKER_OFFSET_Y
                    - FONT_PICKER_WINDOW_MARGIN)
                    .max(0.0);

                // 头部保持稳定，只压缩候选区。弹层因此不会再因超出窗口
                // 而被整体吸附到输入框上方，候选区仍有明确高度承接滚轮。
                px((available_below - FONT_PICKER_PANEL_CHROME_HEIGHT)
                    .clamp(0.0, FONT_PICKER_LIST_PREFERRED_HEIGHT))
            })
            .unwrap_or(px(FONT_PICKER_LIST_PREFERRED_HEIGHT));
        let panel =
            self.font_picker_open.then(|| self.font_picker_panel(panel_width, list_height, cx));

        div().relative().w_full().flex_shrink_0().child(row).when_some(
            panel.zip(trigger_bounds),
            |anchor, (panel, trigger_bounds)| {
                anchor.child(
                    deferred(
                        anchored()
                            .anchor(gpui::Anchor::TopRight)
                            .position(trigger_bounds.bottom_right())
                            .offset(gpui::point(px(0.0), px(FONT_PICKER_OFFSET_Y)))
                            .snap_to_window_with_margin(px(FONT_PICKER_WINDOW_MARGIN))
                            .child(panel),
                    )
                    .with_priority(2),
                )
            },
        )
    }
}
