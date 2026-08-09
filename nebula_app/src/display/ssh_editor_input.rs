//! SSH 主机编辑器的**交互层**：打开/关闭、命中、光标定位与拖选、字段校验、
//! 测试连接、保存。
//!
//! # 为什么单独一个文件
//!
//! 这个域此前是 model（[`ssh_ui`](super::ssh_ui)）+ view
//! （[`ssh_editor_render`](super::ssh_editor_render)）两分，交互逻辑却留在
//! `display::mod` 里，跟另外三百多个方法堆在一起。结果是想弄清"点一下地址框
//! 会发生什么"，得在一个八千行的文件里翻。
//!
//! 分法参照 Warp：`app/src/terminal/` 下 `model/`、`view.rs`、`input.rs` 三分，
//! 每个功能域自带这三块，中枢只负责装配。这里照搬同一条线——`mod.rs` 不该
//! 知道端口框只收五位数字。
//!
//! # 与相邻两层的边界
//!
//! - **model**（[`ssh_ui`](super::ssh_ui)）—— 只有状态与不变量，不碰
//!   [`Display`]。
//! - **view**（[`ssh_editor_render`](super::ssh_editor_render)）—— 只吃状态
//!   吐 quad 与文字，不改状态。
//! - **input**（本文件）—— 唯一同时持有两边的地方：把指针与按键翻译成对
//!   model 的修改，并拿 view 算出的矩形做命中。

use super::*;

impl Display {
    pub fn open_ssh_editor(&mut self) {
        self.nebula_ssh_editor = Some(SshHostEditor {
            original_destination: None,
            error: None,
            destination_cursor: Default::default(),
            port_cursor: Default::default(),
            label_cursor: Default::default(),
            password_cursor: Default::default(),
            destination: String::new(),
            port: String::new(),
            label: String::new(),
            icon: String::new(),
            icon_picker: false,
            icon_filter: String::new(),
            icon_filter_cursor: Default::default(),
            icon_scroll: 0,
            password: String::new(),
            save_password: true,
            show_password: false,
            // 默认密码：第一次添加主机的人手里通常只有一串密码；「自动」要
            // 先有配好的私钥才走得通，拿它当默认等于让新手先撞一次失败。
            auth: crate::ssh_profiles::SshAuthMode::Password,
            private_keys: Vec::new(),
            proxy_choice: Default::default(),
            proxy_url: String::new(),
            proxy_cursor: Default::default(),
            field: SshEditorField::Destination,
            focus: crate::ux::FocusIndex::default(),
            test: Default::default(),
        });
        self.nebula_ssh_editor_rects = None;
        self.nebula_ssh_editor_open = true;
        self.nebula_ssh_editor_hover = SshEditorHit::None;
        // 每次打开都从零开始，避免上一次退出动画的残余进度造成闪跳。
        self.nebula_ui_anims.ssh_editor = UiAnim::new(0.0);
        self.pending_update.dirty = true;
    }

    pub fn edit_ssh_host(&mut self, index: usize) {
        let Some(destination) = self.nebula_ssh_hosts.get(index).cloned() else { return };
        let profile =
            crate::ssh_profiles::SshProfiles::load(&nebula_data_dir().join("ssh_profiles.json"))
                .unwrap_or_else(|err| {
                    warn!("加载 SSH Profile 失败，编辑器使用自动认证: {err}");
                    crate::ssh_profiles::SshProfiles::default()
                })
                .for_destination(&destination);
        // 存盘的地址串里端口是内嵌的；编辑器分两个框显示，所以这里拆开。
        let (address, port) = ssh_ui::split_destination_port(&destination);
        let (proxy_choice, proxy_url) =
            ssh_ui::SshProxyChoice::from_saved(profile.proxy.as_deref());
        self.nebula_ssh_editor = Some(SshHostEditor {
            original_destination: Some(destination.clone()),
            error: None,
            destination_cursor: Default::default(),
            port_cursor: Default::default(),
            label_cursor: Default::default(),
            password_cursor: Default::default(),
            destination: address,
            port,
            label: profile.label.clone().unwrap_or_default(),
            icon: profile.icon.clone().unwrap_or_default(),
            icon_picker: false,
            icon_filter: String::new(),
            icon_filter_cursor: Default::default(),
            icon_scroll: 0,
            // Never pull a stored secret back into a text field. Leaving this
            // blank preserves the existing credential when the address stays
            // unchanged; typing a new value explicitly replaces it.
            password: String::new(),
            save_password: true,
            show_password: false,
            auth: profile.auth,
            private_keys: profile.private_keys,
            proxy_choice,
            proxy_url,
            proxy_cursor: Default::default(),
            field: SshEditorField::Destination,
            focus: crate::ux::FocusIndex::default(),
            test: Default::default(),
        });
        self.nebula_ssh_editor_rects = None;
        self.nebula_ssh_editor_open = true;
        self.nebula_ssh_editor_hover = SshEditorHit::None;
        self.nebula_ui_anims.ssh_editor = UiAnim::new(0.0);
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_editor_active(&self) -> bool {
        self.nebula_ssh_editor_open && self.nebula_ssh_editor.is_some()
    }

    /// 「测试连接」正在跑。转圈指示器要按显示刷新率画，8fps 下一圈只有五六
    /// 帧，转起来是一顿一顿的——那种顿挫读作"卡住了"，恰好和它要表达的
    /// "正在工作"相反。
    pub fn ssh_test_running(&self) -> bool {
        self.nebula_ssh_editor
            .as_ref()
            .is_some_and(|editor| matches!(editor.test, ssh_ui::SshTestState::Running { .. }))
    }

    pub fn close_ssh_editor(&mut self) {
        if self.nebula_ssh_editor.is_some() {
            self.nebula_ssh_editor_open = false;
            self.nebula_ssh_editor_hover = SshEditorHit::None;
            self.nebula_ssh_editor_drag = None;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    /// 图标选择器是否正开着。开着的时候它**独占键盘**：打字进搜索框、Esc
    /// 只关它、回车挑第一项——一个浮层开着的时候，键盘属于最上面那一层。
    pub fn ssh_editor_icon_picker_open(&self) -> bool {
        self.nebula_ssh_editor.as_ref().is_some_and(|editor| editor.icon_picker)
    }

    pub fn ssh_editor_close_icon_picker(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.icon_picker = false;
            editor.icon_filter.clear();
            editor.icon_filter_cursor = Default::default();
            editor.icon_scroll = 0;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    /// 搜索词改一个字，列表就得回到顶：筛完之后的第 5 行和筛之前的第 5 行
    /// 毫无关系，停在原处等于随机翻了一页。
    ///
    /// 编辑走光标模型：插入落在**光标处**、替换选区，Backspace 先删选区。
    /// 它是一个正经输入框，不是只能尾部追加的过滤串。
    pub fn ssh_editor_icon_filter_edit(&mut self, insert: Option<&str>) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            match insert {
                Some(text) => editor.icon_filter_cursor.insert(&mut editor.icon_filter, text),
                None => editor.icon_filter_cursor.backspace(&mut editor.icon_filter),
            }
            editor.icon_scroll = 0;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn ssh_editor_icon_filter_delete_forward(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.icon_filter_cursor.delete_forward(&mut editor.icon_filter);
            editor.icon_scroll = 0;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    /// 搜索框的光标移动 / Home / End / 全选 / 取选中——全部转发组件层。
    /// 它和表单字段的差别只有"值存在哪"，行为必须是同一套。
    pub fn ssh_editor_icon_filter_move(&mut self, forward: bool, extend: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let text = editor.icon_filter.clone();
            editor.icon_filter_cursor.step(&text, forward, extend);
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn ssh_editor_icon_filter_jump(&mut self, to_end: bool, extend: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let text = editor.icon_filter.clone();
            editor.icon_filter_cursor.jump(&text, to_end, extend);
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn ssh_editor_icon_filter_select_all(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let text = editor.icon_filter.clone();
            editor.icon_filter_cursor.select_all(&text);
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn ssh_editor_icon_filter_selected_text(&self) -> Option<String> {
        let editor = self.nebula_ssh_editor.as_ref()?;
        editor.icon_filter_cursor.selected_text(&editor.icon_filter)
    }

    /// 回车挑当前列表的第一项。搜到只剩一个的时候，让人还得再去点一下鼠标
    /// 是没道理的。
    pub fn ssh_editor_icon_pick_first(&mut self) {
        let first = self
            .nebula_ssh_editor_rects
            .as_ref()
            .and_then(|rects| rects.icon_rows.first().map(|(pick, _)| *pick));
        if let (Some(pick), Some(editor)) = (first, self.nebula_ssh_editor.as_mut()) {
            editor.icon = match pick {
                Some(index) => super::ui::os_icons::CATALOG[index].id.to_owned(),
                None => String::new(),
            };
        }
        self.ssh_editor_close_icon_picker();
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_editor_icon_scroll(&mut self, lines: i32) {
        let max = self.nebula_ssh_editor_rects.as_ref().map_or(0, |rects| rects.icon_max_scroll);
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let next = (editor.icon_scroll as i32 + lines).clamp(0, max as i32) as usize;
            if next != editor.icon_scroll {
                editor.icon_scroll = next;
                self.pending_update.dirty = true;
                self.window.request_redraw();
            }
        }
    }

    pub fn ssh_editor_hit(&self, x: f32, y: f32) -> SshEditorHit {
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else {
            return SshEditorHit::None;
        };
        let hit = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        // 弹层最先判：它画在最上面，命中顺序就得跟绘制顺序反过来，否则被它
        // 盖住的字段会"隔着浮层"被点到。落在浮层里但不在任何一行上的点由
        // `IconPopupChrome` 吞掉——盖住的地方不再属于下面那一层。
        if hit(rects.icon_popup) {
            if hit(rects.icon_search) {
                return SshEditorHit::IconSearch;
            }
            return rects
                .icon_rows
                .iter()
                .find(|(_, rect)| hit(*rect))
                .map_or(SshEditorHit::IconPopupChrome, |(pick, _)| {
                    SshEditorHit::IconOption(*pick)
                });
        }
        if hit(rects.avatar) {
            SshEditorHit::Avatar
        } else if hit(rects.close) {
            SshEditorHit::Close
        } else if hit(rects.destination) {
            SshEditorHit::Destination
        } else if hit(rects.port) {
            SshEditorHit::Port
        } else if hit(rects.label) {
            SshEditorHit::Label
        } else if let Some((mode, _)) = rects.auth.iter().find(|(_, rect)| hit(*rect)) {
            SshEditorHit::Auth(*mode)
        } else if let Some((choice, _)) = rects.proxy.iter().find(|(_, rect)| hit(*rect)) {
            SshEditorHit::ProxyChoice(*choice)
        } else if hit(rects.proxy_url) {
            SshEditorHit::ProxyUrl
        } else if hit(rects.add_private_key) {
            SshEditorHit::AddPrivateKey
        } else if let Some((index, _)) = rects.private_key_rows.iter().find(|(_, rect)| hit(*rect))
        {
            SshEditorHit::RemovePrivateKey(*index)
        } else if hit(rects.password_toggle) {
            SshEditorHit::PasswordToggle
        } else if hit(rects.password) {
            SshEditorHit::Password
        } else if hit(rects.save_checkbox) {
            SshEditorHit::SaveToggleBox
        } else if hit(rects.save_toggle) {
            SshEditorHit::SaveToggleLabel
        } else if hit(rects.test) {
            SshEditorHit::Test
        } else if hit(rects.test_status) {
            SshEditorHit::TestStatus
        } else if hit(rects.cancel) {
            SshEditorHit::Cancel
        } else if hit(rects.primary) {
            SshEditorHit::Primary
        } else {
            SshEditorHit::None
        }
    }

    pub fn set_ssh_editor_hover(&mut self, hover: SshEditorHit) {
        if self.nebula_ssh_editor_hover != hover {
            self.nebula_ssh_editor_hover = hover;
            self.pending_update.dirty = true;
        }
    }

    /// 把键盘焦点与文本光标一起放到 Tab 顺序里第 `slot` 个位置上。点击和 Tab
    /// 走同一套序号，否则"点一下地址框再按 Tab"会跳到和视觉顺序无关的地方。
    ///
    /// `select_all` 区分两种进入方式：Tab 键进来是全选（原生行为，方便直接
    /// 覆写），鼠标点进来是把光标放在点的位置——那时全选等于把用户瞄准的
    /// 那个字符位置扔掉。
    fn focus_ssh_editor_field(&mut self, slot: usize, select_all: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let slots = editor.slots();
            let slot = slot.min(slots.len() - 1);
            editor.focus.set(slot, slots.len());
            editor.clear_selections();
            if let ssh_ui::SshEditorSlot::Field(field) = slots[slot] {
                editor.field = field;
                if select_all {
                    let (value, cursor) = editor.active_field();
                    let text = value.clone();
                    cursor.select_all(&text);
                }
            }
        }
    }

    /// 鼠标按在输入框里：聚焦该字段并把光标放到点击的字符缝隙上，同时开一段
    /// 拖选。`x` 是物理像素的窗口坐标。
    pub fn ssh_editor_press_field(&mut self, hit: SshEditorHit, x: f32) {
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else { return };
        let Some((field, rect)) = rects.field_of(hit) else { return };
        let origin = rects.metrics.origin(field);
        let cell_w = rects.metrics.cell_w_of(field);
        let slot = self
            .nebula_ssh_editor
            .as_ref()
            .map(|editor| editor.slot_of(ssh_ui::SshEditorSlot::Field(field)))
            .unwrap_or(0);
        self.focus_ssh_editor_field(slot, false);
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return };
        let index = editor.index_at(field, x - origin, cell_w);
        let (value, cursor) = editor.field_mut(field);
        let text = value.clone();
        cursor.place(&text, index);
        let _ = rect;
        self.nebula_ssh_editor_drag = Some(ssh_ui::SshEditorDrag::Field(field));
    }

    /// 拖动中：把选区从按下的锚点拉到当前位置。返回是否真的在拖，让调用方
    /// 决定要不要重绘。
    pub fn ssh_editor_drag_to(&mut self, x: f32) -> bool {
        let Some(drag) = self.nebula_ssh_editor_drag else { return false };
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else { return false };
        match drag {
            ssh_ui::SshEditorDrag::Field(field) => {
                let origin = rects.metrics.origin(field);
                let cell_w = rects.metrics.cell_w_of(field);
                let Some(editor) = self.nebula_ssh_editor.as_mut() else { return false };
                let index = editor.index_at(field, x - origin, cell_w);
                let (value, cursor) = editor.field_mut(field);
                let text = value.clone();
                cursor.extend_to(&text, index);
            },
            ssh_ui::SshEditorDrag::IconSearch => {
                let (text_x, cell_w) = (rects.icon_search_text_x, rects.icon_search_cell_w);
                let Some(editor) = self.nebula_ssh_editor.as_mut() else { return false };
                let index =
                    super::ui::text_field::index_at(&editor.icon_filter, x - text_x, cell_w);
                let text = editor.icon_filter.clone();
                editor.icon_filter_cursor.extend_to(&text, index);
            },
        }
        true
    }

    pub fn ssh_editor_end_drag(&mut self) {
        self.nebula_ssh_editor_drag = None;
    }

    pub fn ssh_editor_dragging(&self) -> bool {
        self.nebula_ssh_editor_drag.is_some()
    }

    pub fn ssh_editor_insert(&mut self, text: &str) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.error = None;
            // 端口框只收数字，且不超过 5 位。非法字符当场挡住，比让人填完
            // 再在保存时弹一句"端口无效"要好——那时已经不知道错在哪一格。
            let text = if editor.field == SshEditorField::Port {
                let mut digits: String = text.chars().filter(char::is_ascii_digit).collect();
                if digits.is_empty() {
                    return;
                }
                // 位数上限在插入**前**算：先插再 `truncate` 会砍掉尾部，而
                // 用户刚打的那个字符正在光标处——被砍的看起来是别人的字符。
                let selected =
                    editor.port_cursor.range(&editor.port).map_or(0, |(start, end)| end - start);
                let room = 5usize.saturating_sub(editor.port.chars().count() - selected);
                if room == 0 {
                    return;
                }
                digits.truncate(room);
                digits
            } else {
                text.to_owned()
            };
            let (value, cursor) = editor.active_field();
            cursor.insert(value, &text);
            editor.test = Default::default();
        }
    }

    pub fn ssh_editor_backspace(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let before = editor.active_text().len();
            let (value, cursor) = editor.active_field();
            cursor.backspace(value);
            if before != editor.active_text().len() {
                editor.test = Default::default();
            }
        }
    }

    pub fn ssh_editor_delete_forward(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let before = editor.active_text().len();
            let (value, cursor) = editor.active_field();
            cursor.delete_forward(value);
            if before != editor.active_text().len() {
                editor.test = Default::default();
            }
        }
    }

    /// ←/→ 移动光标；`extend` 为真时按住 Shift 扩选。
    pub fn ssh_editor_move_caret(&mut self, forward: bool, extend: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let (value, cursor) = editor.active_field();
            let text = value.clone();
            cursor.step(&text, forward, extend);
        }
    }

    /// Home / End。
    pub fn ssh_editor_jump_caret(&mut self, to_end: bool, extend: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let (value, cursor) = editor.active_field();
            let text = value.clone();
            cursor.jump(&text, to_end, extend);
        }
    }

    pub fn ssh_editor_select_all(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            let (value, cursor) = editor.active_field();
            let text = value.clone();
            cursor.select_all(&text);
        }
    }

    /// Copying an invisible password would persist a secret in the system
    /// clipboard without visible intent, so it is enabled only after Reveal.
    pub fn ssh_editor_selected_text(&self) -> Option<String> {
        let editor = self.nebula_ssh_editor.as_ref()?;
        if editor.field == SshEditorField::Password && !editor.show_password {
            return None;
        }
        let (text, cursor) = editor.field_view(editor.field);
        cursor.selected_text(text)
    }

    pub fn ssh_editor_next_field(&mut self, reverse: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.clear_selections();
            let slots = editor.slots();
            editor.focus.advance(slots.len(), reverse);
            // Tab 进一个输入框时全选：Windows 上所有输入框都这样，让"切过去
            // 直接覆写"是一次按键的事。焦点落在按钮上时不选，否则会有一段和
            // 当前操作无关的高亮一直亮着。
            if let Some(ssh_ui::SshEditorSlot::Field(field)) = slots.get(editor.focus.current()) {
                editor.field = *field;
                let (value, cursor) = editor.active_field();
                let text = value.clone();
                cursor.select_all(&text);
            }
        }
    }

    pub fn ssh_editor_activate_focus(&mut self) {
        let Some(editor) = self.nebula_ssh_editor.as_ref() else { return };
        match editor.slots().get(editor.focus.current()) {
            Some(ssh_ui::SshEditorSlot::Test) => self.queue_ssh_test(),
            Some(ssh_ui::SshEditorSlot::Cancel) => self.close_ssh_editor(),
            Some(ssh_ui::SshEditorSlot::Save) => self.save_ssh_editor(),
            _ => {},
        }
    }

    pub fn ssh_editor_toggle_save(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.save_password = !editor.save_password;
            editor.test = Default::default();
        }
    }

    /// input 层在处理完编辑器点击后调用：取走「测试连接」暂存的请求。
    pub fn take_ssh_test_request(&mut self) -> Option<crate::ssh_session::SshTestRequest> {
        self.nebula_ssh_test_request.take()
    }

    /// Validate the current draft and stage an exact, numbered test request.
    /// The input layer owns the event proxy and takes this request immediately
    /// after mouse/keyboard activation.
    fn queue_ssh_test(&mut self) {
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return };
        let destination = editor.destination.trim().to_owned();
        let valid = !destination.is_empty()
            && !destination
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || ";&|<>\"'`".contains(c));
        if !valid {
            editor.error = Some(if destination.is_empty() {
                "请输入 SSH 地址，例如 user@example.com".to_owned()
            } else {
                "地址不能包含空白、控制字符或 shell 分隔符".to_owned()
            });
            editor.test = Default::default();
            return;
        }
        if matches!(editor.test, ssh_ui::SshTestState::Running { .. }) {
            return;
        }

        let slot = editor.slot_of(ssh_ui::SshEditorSlot::Test);
        editor.focus.set(slot, editor.slots().len());
        self.nebula_ssh_test_seq = self.nebula_ssh_test_seq.wrapping_add(1).max(1);
        let request_id = self.nebula_ssh_test_seq;
        editor.error = None;
        editor.test = ssh_ui::SshTestState::Running { request_id };
        self.nebula_ssh_test_request = Some(crate::ssh_session::SshTestRequest {
            request_id,
            destination,
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
            password: (!editor.password.is_empty()).then(|| editor.password.clone()),
            proxy: editor.proxy_choice.to_saved(&editor.proxy_url),
        });
    }

    /// 后台测试完成回报。结果只属于发起时的草稿：地址已改或状态已非
    /// Running（用户改过字段被清位）时直接丢弃，不让旧结果背书新配置。
    pub fn ssh_test_done(
        &mut self,
        request_id: u64,
        destination: &str,
        ok: bool,
        message: &str,
        elapsed_ms: u64,
    ) {
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return };
        if editor.destination.trim() != destination
            || editor.test != (ssh_ui::SshTestState::Running { request_id })
        {
            return;
        }
        editor.test = if ok {
            ssh_ui::SshTestState::Ok { elapsed_ms }
        } else {
            ssh_ui::SshTestState::Failed { summary: message.to_owned() }
        };
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_editor_click(&mut self, x: f32, y: f32) -> bool {
        let hit = self.ssh_editor_hit(x, y);
        // 列表开着的时候，点在它以外的任何地方都只做一件事：关掉它。这一下
        // **不**穿透到底下的控件——用户的意图是"收起这个列表"，顺手把光标
        // 落进某个输入框、甚至切了认证方式，都是他没要的第二个动作。
        if self.nebula_ssh_editor.as_ref().is_some_and(|editor| editor.icon_picker)
            && !matches!(
                hit,
                SshEditorHit::IconOption(_)
                    | SshEditorHit::IconSearch
                    | SshEditorHit::IconPopupChrome
                    | SshEditorHit::Avatar
            )
        {
            self.ssh_editor_close_icon_picker();
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return true;
        }
        match hit {
            // 点哪个框就聚焦哪个框，并把光标放到点中的字符缝隙上，同时开一段
            // 拖选。焦点序号必须和 `ssh_editor_next_field` 的顺序一致，否则点
            // 完再按 Tab 会跳到毫不相干的地方。
            hit @ (SshEditorHit::Destination
            | SshEditorHit::Port
            | SshEditorHit::Label
            | SshEditorHit::Password
            | SshEditorHit::ProxyUrl) => self.ssh_editor_press_field(hit, x),
            SshEditorHit::PasswordToggle => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.show_password = !editor.show_password;
                }
            },
            SshEditorHit::Avatar => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.icon_picker = !editor.icon_picker;
                    // 每次打开都从干净的状态开始：上次搜过的词留在框里，下次
                    // 打开只剩一两项可选，人会以为图标丢了。
                    editor.icon_filter.clear();
                    editor.icon_filter_cursor = Default::default();
                    editor.icon_scroll = 0;
                    if editor.icon_picker {
                        // 弹层一开搜索框即聚焦：光标此刻必须是亮的。
                        super::ui::caret::note_activity();
                    }
                }
            },
            SshEditorHit::IconOption(pick) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.icon = match pick {
                        Some(index) => super::ui::os_icons::CATALOG[index].id.to_owned(),
                        // 「自动识别」存空串：配置里只写用户明确挑过的形状。
                        None => String::new(),
                    };
                    // 图标不参与连接，所以**不**清测试结果——刚测通的那条绿字
                    // 依然对这份草稿有效，换个形状不该把它抹掉。
                    editor.icon_picker = false;
                    editor.icon_filter.clear();
                    editor.icon_scroll = 0;
                }
            },
            // 浮层自己的空白：吞掉，什么都不做。
            SshEditorHit::IconPopupChrome => {},
            // 搜索框：点击定位光标并开一段拖选——正经输入框的第一课。
            SshEditorHit::IconSearch => {
                let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else { return true };
                let (text_x, cell_w) = (rects.icon_search_text_x, rects.icon_search_cell_w);
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let index =
                        super::ui::text_field::index_at(&editor.icon_filter, x - text_x, cell_w);
                    let text = editor.icon_filter.clone();
                    editor.icon_filter_cursor.place(&text, index);
                    self.nebula_ssh_editor_drag = Some(ssh_ui::SshEditorDrag::IconSearch);
                }
            },
            SshEditorHit::Auth(mode) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.auth = mode;
                    editor.error = None;
                    editor.test = Default::default();
                    if !ssh_ui::auth_sections(mode).0 {
                        editor.field = SshEditorField::Destination;
                    }
                }
            },
            SshEditorHit::AddPrivateKey => {
                if let Some(result) = file_dialog::pick_private_key_file(&self.window) {
                    if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                        match result {
                            Ok(path) => {
                                ssh_ui::push_private_key(&mut editor.private_keys, path);
                                editor.error = None;
                                editor.test = Default::default();
                            },
                            Err(err) => editor.error = Some(err),
                        }
                    }
                }
            },
            SshEditorHit::RemovePrivateKey(index) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    if index < editor.private_keys.len() {
                        editor.private_keys.remove(index);
                        editor.test = Default::default();
                    }
                }
            },
            SshEditorHit::ProxyChoice(choice) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    if editor.proxy_choice != choice {
                        editor.proxy_choice = choice;
                        // 代理参与连接：切换后旧的测试结果不再为这份草稿背书。
                        editor.error = None;
                        editor.test = Default::default();
                        if choice == ssh_ui::SshProxyChoice::Custom {
                            // 选「自定义」的下一步必然是填地址：直接聚焦 URL 框。
                            let slot = editor
                                .slot_of(ssh_ui::SshEditorSlot::Field(SshEditorField::ProxyUrl));
                            editor.focus.set(slot, editor.slots().len());
                            editor.field = SshEditorField::ProxyUrl;
                            super::ui::caret::note_activity();
                        } else if editor.field == SshEditorField::ProxyUrl {
                            // URL 框随 Custom 显隐；藏起来的框不能继续持有键盘。
                            editor.field = SshEditorField::Destination;
                        }
                    }
                }
            },
            SshEditorHit::SaveToggleBox | SshEditorHit::SaveToggleLabel => {
                self.ssh_editor_toggle_save();
            },
            SshEditorHit::Test => self.queue_ssh_test(),
            SshEditorHit::TestStatus => {},
            SshEditorHit::Close => {
                self.close_ssh_editor();
            },
            SshEditorHit::Cancel => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let slot = editor.slot_of(ssh_ui::SshEditorSlot::Cancel);
                    editor.focus.set(slot, editor.slots().len());
                }
                self.close_ssh_editor();
            },
            SshEditorHit::Primary => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let slot = editor.slot_of(ssh_ui::SshEditorSlot::Save);
                    editor.focus.set(slot, editor.slots().len());
                }
                self.save_ssh_editor();
            },
            SshEditorHit::None => return false,
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    pub fn save_ssh_editor(&mut self) {
        let Some(mut editor) = self.nebula_ssh_editor.take() else { return };
        // 端口先单独校验：它有自己的输入框，错误也该指回那个框。u16 的上界拦不住
        // `0`——那不是一个能连的端口，却能通过 parse。
        let port = editor.port.trim().to_owned();
        if !port.is_empty() && !port.parse::<u16>().is_ok_and(|p| p > 0) {
            editor.error = Some("端口需要是 1–65535 之间的数字".to_owned());
            editor.field = SshEditorField::Port;
            self.nebula_ssh_editor = Some(editor);
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return;
        }
        // 存盘仍然是一条地址串（凭据、profile、历史都以它为键），端口在这里拼回去。
        let destination = ssh_ui::join_destination_port(&editor.destination, &port);
        let valid = !destination.is_empty()
            && !destination
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || ";&|<>\"'`".contains(c));
        if !valid {
            editor.error = Some(if destination.is_empty() {
                "请输入 SSH 地址，例如 user@example.com".to_owned()
            } else {
                "地址不能包含空白、控制字符或 shell 分隔符".to_owned()
            });
            editor.field = SshEditorField::Destination;
            self.nebula_ssh_editor = Some(editor);
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return;
        }
        // 自定义代理的 URL 在保存时就校验：写错的地址留到连接时才报，
        // 用户看到的会是「连不上主机」而不是「代理没写对」。
        if editor.proxy_choice == ssh_ui::SshProxyChoice::Custom {
            let url = editor.proxy_url.trim();
            if !url.is_empty() {
                if let Err(err) = crate::ssh_proxy::ProxyLink::parse(url) {
                    editor.error = Some(err);
                    editor.field = SshEditorField::ProxyUrl;
                    self.nebula_ssh_editor = Some(editor);
                    self.pending_update.dirty = true;
                    self.window.request_redraw();
                    return;
                }
            }
        }
        if let Some(original) = editor.original_destination.as_deref() {
            if original != destination {
                self.nebula_saved_hosts.retain(|host| host != original);
                self.nebula_pinned_hosts.retain(|host| host != original);
                if !self.nebula_hidden_hosts.iter().any(|host| host == original) {
                    self.nebula_hidden_hosts.push(original.to_owned());
                }
                #[cfg(windows)]
                {
                    let _ = crate::ssh_credentials::forget_password(original);
                }
            }
        }
        // Saving/editing is an explicit request to surface this destination,
        // so it also undoes a previous Delete of the same address.
        self.nebula_hidden_hosts.retain(|host| host != &destination);
        self.nebula_saved_hosts.retain(|host| host != &destination);
        self.nebula_saved_hosts.insert(0, destination.clone());
        self.nebula_saved_hosts.truncate(20);
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        let profile_path = nebula_data_dir().join("ssh_profiles.json");
        let mut profiles =
            crate::ssh_profiles::SshProfiles::load(&profile_path).unwrap_or_else(|err| {
                warn!("加载 SSH Profile 失败，将创建新文件: {err}");
                crate::ssh_profiles::SshProfiles::default()
            });
        if let Some(original) = editor.original_destination.as_deref() {
            if original != destination {
                profiles.rename(original, &destination);
            }
        }
        // 标签留空就发一个自增的默认名（「主机 6」）而不是存 None。
        //
        // 侧栏的主机行是双行的：第一行名字、第二行地址。没有名字那一行就得
        // 拿地址顶上，于是同一行里出现两遍同样的字符串——比留空更糟。给个
        // 编号至少让它可读、可改、可搜。
        //
        // 编号取现有默认标签的最大值 +1（见 `next_default_label`），所以删掉
        // 中间几台再新建也不会撞名。
        let label = match editor.label.trim() {
            "" => profiles.next_default_label(self.ui_language().pick("主机", "Host")),
            named => named.to_owned(),
        };
        profiles.upsert(crate::ssh_profiles::SshProfileAuth {
            destination: destination.clone(),
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
            label: Some(label),
            // 空串 = 自动识别，不落盘；配置里只存用户明确选过的形状。
            icon: (!editor.icon.is_empty()).then(|| editor.icon.clone()),
            // 三态编码：跟随全局不落盘、直连存 "direct"、自定义存 URL
            // （空 URL 视同跟随，见 `SshProxyChoice::to_saved`）。
            proxy: editor.proxy_choice.to_saved(&editor.proxy_url),
        });
        if let Err(err) = profiles.save(&profile_path) {
            editor.error = Some(format!("保存 SSH Profile 失败: {err}"));
            self.nebula_ssh_editor = Some(editor);
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return;
        }
        // 只在存盘成功后才刷新缓存：写失败时侧栏应当继续显示旧名字，而不是
        // 显示一个磁盘上并不存在的标签。
        self.nebula_ssh_labels = profiles.labels();
        self.nebula_ssh_icons = profiles.icons();
        if ssh_ui::auth_sections(editor.auth).0
            && editor.save_password
            && !editor.password.is_empty()
        {
            #[cfg(windows)]
            {
                let _ = crate::ssh_credentials::store_password(
                    &destination,
                    editor.password.as_bytes(),
                );
            }
        }
        // 凭据落盘后立即清除内存中的明文，但保留其余内容完成短退出动画。
        editor.password.clear();
        self.persist_nebula_settings();
        self.nebula_ssh_editor = Some(editor);
        self.close_ssh_editor();
    }
}
