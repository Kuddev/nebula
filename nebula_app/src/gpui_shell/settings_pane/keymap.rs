//! 按键映射页：改键的捕获、冲突判定与键帽渲染。
//!
//! 键位模型本身在 `display::keymap`（两壳同读同写），这里只是它的编辑界面。
//! 从 `settings_pane.rs` 拆出来的理由不是"文件太长"，而是这一页与其余八页的
//! 耦合面只有两个方法：`section_keymap`（内容区取这一页）和
//! `handle_keymap_capture`（改键期间整页键盘事件改道给它）。其余十个——搜索
//! 口径、冲突集合、键帽拆分、行渲染——只服务这一页，摊在主文件里只是噪音。
//!
//! 其余项不提 `pub`：子模块看得见父模块的私有成员（`self.group`、
//! `self.runtime`、`design` 的行原语都照旧用），反向不行，所以只有上面那两
//! 个标了 `pub(super)`。

use super::*;

impl SettingsPane {
    // ---- 按键映射编辑器（模型层 `display::keymap`，两壳同读同写）----

    /// 搜索口径与旧壳一致：动作名（中/英）+ 当前生效键文本。
    fn keymap_row_haystack(&self, flat: usize) -> String {
        use crate::display::keymap;
        let custom = keymap::build_bindings(&self.keymap_binds);
        let combo = if flat == keymap::QUICK_TERMINAL_ROW {
            keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey)
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                .map(|(combo, _)| combo)
                .unwrap_or_default()
        };
        let (zh, en) = if flat == keymap::QUICK_TERMINAL_ROW {
            ("快速终端", "Quick terminal")
        } else {
            keymap::EDITABLE_ACTIONS.get(flat - 1).map(|(_, zh, en)| (*zh, *en)).unwrap_or(("", ""))
        };
        format!("{zh} {en} {combo}").to_lowercase()
    }

    /// 过滤后的可见行（flat 下标，升序）。空查询 = 全部。
    fn keymap_visible(&self, cx: &App) -> Vec<usize> {
        use crate::display::keymap;
        let query = self.keymap_search_input.read(cx).value().trim().to_lowercase();
        (0..keymap::editable_row_count())
            .filter(|flat| query.is_empty() || self.keymap_row_haystack(*flat).contains(&query))
            .collect()
    }

    /// 冲突检测（旧壳 `keymap_clash_info` 同义）：同一 combo 多动作 → 逐行
    /// 标记 + 只报第一组的提示条。
    fn keymap_clashes(&self) -> (Vec<bool>, Option<String>) {
        use crate::display::keymap;
        let total = keymap::editable_row_count();
        let custom = keymap::build_bindings(&self.keymap_binds);
        let mut combos: Vec<Option<String>> = Vec::with_capacity(total);
        for flat in 0..total {
            let combo = if flat == keymap::QUICK_TERMINAL_ROW {
                Some(keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey))
            } else {
                keymap::EDITABLE_ACTIONS
                    .get(flat - 1)
                    .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                    .map(|(combo, _)| combo)
            };
            combos.push(combo.filter(|combo| !combo.is_empty()));
        }
        let name = |flat: usize| -> String {
            if flat == keymap::QUICK_TERMINAL_ROW {
                "快速终端".to_owned()
            } else {
                keymap::EDITABLE_ACTIONS
                    .get(flat - 1)
                    .map(|(_, zh, _)| (*zh).to_owned())
                    .unwrap_or_default()
            }
        };
        let mut rows = vec![false; total];
        let mut note = None;
        for a in 0..total {
            let Some(combo_a) = combos[a].clone() else { continue };
            for b in (a + 1)..total {
                let Some(combo_b) = &combos[b] else { continue };
                if !combo_a.eq_ignore_ascii_case(combo_b) {
                    continue;
                }
                rows[a] = true;
                rows[b] = true;
                if note.is_none() {
                    let (a_name, b_name) = (name(a), name(b));
                    note = Some(format!(
                        "{combo_a} 同时绑定了「{a_name}」与「{b_name}」——只有排前面的「{a_name}」会触发"
                    ));
                }
            }
        }
        (rows, note)
    }

    /// interceptor 与修饰键预览共用的录制状态机；按键是否应被独占由调用
    /// 方在进入这里前裁定，避免数据转换层意外吞掉普通设置输入。
    pub(super) fn handle_keymap_capture(
        &mut self,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.keymap_capture else { return };
        match crate::display::keymap::capture_gpui(keystroke) {
            crate::display::keymap::CaptureOutcome::Cancel => {
                self.keymap_capture = None;
                self.keymap_capture_preview.clear();
                cx.notify();
            },
            crate::display::keymap::CaptureOutcome::ClearCustom => {
                self.keymap_clear_custom(row, cx);
            },
            crate::display::keymap::CaptureOutcome::Bind(combo) => {
                self.keymap_assign(row, combo, cx);
            },
            crate::display::keymap::CaptureOutcome::Pending => {},
        }
    }

    /// 捕获完成：一个动作只保留一条自定义绑定，但同一 combo 可以同时归属
    /// 多个动作。冲突不再靠静默注销旧动作来“解决”，而是由
    /// `keymap_clashes` 标记双方并显示警告条，让用户自己决定改哪一行。
    fn keymap_assign(&mut self, row: usize, combo: String, cx: &mut Context<Self>) {
        use crate::display::keymap;
        self.keymap_capture = None;
        self.keymap_capture_preview.clear();
        if row == keymap::QUICK_TERMINAL_ROW {
            self.persist(&[("quick_terminal_hotkey", combo)], cx);
            return;
        }
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row - 1) else { return };
        let name = keymap::action_storage_name(action);
        self.keymap_binds.retain(|(_, a)| !a.eq_ignore_ascii_case(&name));
        self.keymap_binds.push((combo, name));
        self.persist_keybinds(cx);
    }

    /// 捕获态裸 Backspace：删除自定义绑定，回落内置默认。
    fn keymap_clear_custom(&mut self, row: usize, cx: &mut Context<Self>) {
        use crate::display::keymap;
        self.keymap_capture = None;
        self.keymap_capture_preview.clear();
        if row == keymap::QUICK_TERMINAL_ROW {
            self.persist(
                &[("quick_terminal_hotkey", keymap::DEFAULT_QUICK_TERMINAL_HOTKEY.to_owned())],
                cx,
            );
            return;
        }
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row - 1) else { return };
        let name = keymap::action_storage_name(action);
        self.keymap_binds.retain(|(_, a)| !a.eq_ignore_ascii_case(&name));
        self.persist_keybinds(cx);
    }

    /// keybind= 整表落盘 → 重载镜像 → 通知宿主热应用（工作区会重注入
    /// gpui 键位表）。persist 走通用路径拿重载与事件，这里补镜像。
    fn persist_keybinds(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = nebula_settings::persist_keybinds(&self.keymap_binds) {
            crate::gpui_shell::try_write_stderr(format_args!(
                "[nebula:gpui] failed to persist keybinds: {err}"
            ));
        }
        self.keymap_binds = nebula_settings::keybind_pairs();
        self.runtime = RuntimeSettings::load();
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    /// 键帽 chip：捕获行回显预览；自定义 accent 描边；冲突 danger 底；
    /// 默认 ink_dim；未绑定 ink_faint（旧壳墨色分级裁定）。
    fn keymap_keycap(
        &self,
        text: &str,
        custom: bool,
        clash: bool,
        capturing: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let color = if clash {
            theme.danger
        } else if capturing {
            theme.link
        } else if custom {
            theme.link
        } else if text.is_empty() {
            crate::gpui_shell::theme::faint_ink(cx)
        } else {
            theme.muted_foreground
        };
        div()
            .min_w(px(72.0))
            .px_2()
            .h(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL * 0.75))
            .text_size(px(self.font_size_px(cx) * 0.86))
            .when(clash, |chip| chip.bg(theme.danger).text_color(theme.danger_foreground))
            .when(!clash && (custom || capturing), |chip| {
                chip.border_1().border_color(theme.link).text_color(color)
            })
            .when(!clash && !custom && !capturing, |chip| {
                chip.border_1().border_color(theme.border).text_color(color)
            })
            .child(if text.is_empty() && !capturing {
                SharedString::from("未绑定")
            } else {
                SharedString::from(text.to_owned())
            })
    }

    /// 一行「动作 + 键帽」。点击行进入捕获态（旧壳点行即捕获）。
    fn keymap_row(&self, flat: usize, clash: bool, cx: &Context<Self>) -> impl IntoElement {
        use crate::display::keymap;
        let custom = keymap::build_bindings(&self.keymap_binds);
        let label: SharedString = if flat == keymap::QUICK_TERMINAL_ROW {
            "快速终端".into()
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .map(|(_, zh, _)| (*zh).to_owned())
                .unwrap_or_default()
                .into()
        };
        let (text, is_custom) = if flat == keymap::QUICK_TERMINAL_ROW {
            (
                keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey),
                self.runtime.quick_terminal_hotkey != keymap::DEFAULT_QUICK_TERMINAL_HOTKEY,
            )
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                .map(|(combo, custom)| (combo, custom))
                .unwrap_or_else(|| (String::new(), false))
        };
        let capturing = self.keymap_capture == Some(flat);
        let cap_text = if capturing {
            if self.keymap_capture_preview.is_empty() {
                "按新按键…".to_owned()
            } else {
                format!("{}…", self.keymap_capture_preview)
            }
        } else {
            text.clone()
        };
        h_flex()
            .id(("keymap-row", flat))
            .w_full()
            .h(px(SETTINGS_ROW_HEIGHT))
            .flex_shrink_0()
            .items_center()
            .pr_4()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
            .cursor_pointer()
            .hover(|style| style.bg(crate::gpui_shell::theme::settings_hover_bg(cx, false)))
            // mouse_down 而非 click：容器（section 根）同捕一个 mouse_down
            // 做「点击任何位置先撤销」，这里 stop_propagation 抢先处理
            // 「点别的行 = 捕获转移、点同一行 = 取消」（旧壳 input/chrome.rs
            // 的 SettingsHit::KeymapRow 分支同合同）。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.keymap_capture == Some(flat) {
                        this.keymap_capture = None;
                        this.keymap_capture_preview.clear();
                    } else {
                        this.keymap_capture = Some(flat);
                        this.keymap_capture_preview.clear();
                        // 焦点收到分区根：键盘事件沿根路径冒泡给捕获处理器，
                        // 搜索框不再分走按键。
                        window.focus(&this.focus_handle, cx);
                    }
                    cx.notify();
                }),
            )
            .child(div().flex_1().min_w_0().pl_4().child(label))
            .child(self.keymap_keycap(&cap_text, is_custom, clash, capturing, cx))
    }

    pub(super) fn section_keymap(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::display::keymap;

        let visible = self.keymap_visible(cx);
        let (clash_rows, clash_note) = self.keymap_clashes();

        // 分组渲染（旧壳无框分组裁定）：组内可见行为空的组整组隐藏；组
        // 标题 0.86× 小字压在行块上方。
        let mut groups_block = v_flex().w_full().gap_1();
        let mut start = 0usize;
        for (zh, _en, count) in keymap::GROUPS {
            let end = start + count;
            let rows: Vec<_> = visible
                .iter()
                .filter(|flat| (start..end).contains(*flat))
                .map(|flat| self.keymap_row(*flat, clash_rows[*flat], cx))
                .collect();
            if !rows.is_empty() {
                groups_block = groups_block.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_size(px(self.font_size_px(cx) * 0.86))
                        .text_color(cx.theme().muted_foreground)
                        .child(*zh),
                );
                for row in rows {
                    groups_block = groups_block.child(row);
                }
            }
            start = end;
        }

        // 只读行：数字系/AI 贴入键（表驱动，不可在图形页编辑）。随搜索过滤。
        let query = self.keymap_search_input.read(cx).value().trim().to_lowercase();
        let readonly: Vec<&(&str, &str, &str)> = keymap::READONLY_ROWS
            .iter()
            .filter(|(zh, en, combo)| {
                query.is_empty() || format!("{zh} {en} {combo}").to_lowercase().contains(&query)
            })
            .collect();
        if !readonly.is_empty() {
            groups_block = groups_block.child(
                div()
                    .pt_3()
                    .pb_1()
                    .text_size(px(self.font_size_px(cx) * 0.86))
                    .text_color(cx.theme().muted_foreground)
                    .child("只读"),
            );
            for (zh, _en, combo) in readonly {
                groups_block = groups_block.child(
                    h_flex()
                        .w_full()
                        .h(px(SETTINGS_ROW_HEIGHT))
                        .flex_shrink_0()
                        .items_center()
                        .pr_4()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pl_4()
                                .text_color(crate::gpui_shell::theme::faint_ink(cx))
                                .child(*zh),
                        )
                        .child(
                            div()
                                .min_w(px(72.0))
                                .px_2()
                                .h(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL * 0.75))
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_size(px(self.font_size_px(cx) * 0.86))
                                .text_color(crate::gpui_shell::theme::faint_ink(cx))
                                .child(*combo),
                        ),
                );
            }
        }

        // 旧壳按键映射页没有悬挂分组标题：搜索框独占整行（row_w × 34px），
        // 占位「搜索动作或按键…」，下面再空 12px 才到冲突条 / 分组。
        v_flex()
            .w_full()
            // 捕获态的「点击任何位置先撤销」（旧壳 input/chrome.rs 的统一撤
            // 销合同）：行的 mouse_down 会 stop_propagation 自行处理转移/
            // 取消，搜索框这里显式撤销（旧壳点搜索框 = blur 捕获），其余
            // 任何落点冒泡到这里 = 纯取消。
            .when(self.keymap_capture.is_some(), |section| {
                section.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.keymap_capture.take().is_some() {
                            this.keymap_capture_preview.clear();
                            cx.notify();
                        }
                    }),
                )
            })
            .child(
                div()
                    .w_full()
                    .h(px(34.0))
                    .flex_shrink_0()
                    .child(Input::new(&self.keymap_search_input).w_full()),
            )
            .child(div().h(px(12.0)).w_full().flex_shrink_0())
            // 冲突是允许存在的可见状态，用组件库的 Warning Alert 呈现；
            // 不再用自绘 danger 色块，也不静默删掉另一个动作。
            .when_some(clash_note, |section, note| {
                section.child(Alert::warning("keymap-clash-warning", note).small())
            })
            .child(groups_block)
            .child(
                div()
                    .pt_4()
                    .text_size(px(self.font_size_px(cx) * 0.86))
                    .text_color(cx.theme().muted_foreground)
                    .child("点击行改键 · Backspace 恢复默认 · Esc 取消"),
            )
    }
}
