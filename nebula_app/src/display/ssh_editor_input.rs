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
            password: String::new(),
            save_password: true,
            show_password: false,
            auth: crate::ssh_profiles::SshAuthMode::Auto,
            private_keys: Vec::new(),
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
            // Never pull a stored secret back into a text field. Leaving this
            // blank preserves the existing credential when the address stays
            // unchanged; typing a new value explicitly replaces it.
            password: String::new(),
            save_password: true,
            show_password: false,
            auth: profile.auth,
            private_keys: profile.private_keys,
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

    pub fn ssh_editor_hit(&self, x: f32, y: f32) -> SshEditorHit {
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else {
            return SshEditorHit::None;
        };
        let hit = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        if hit(rects.close) {
            SshEditorHit::Close
        } else if hit(rects.destination) {
            SshEditorHit::Destination
        } else if hit(rects.port) {
            SshEditorHit::Port
        } else if hit(rects.label) {
            SshEditorHit::Label
        } else if let Some((mode, _)) = rects.auth.iter().find(|(_, rect)| hit(*rect)) {
            SshEditorHit::Auth(*mode)
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
            let shows_password = ssh_ui::auth_sections(editor.auth).0;
            let count = if shows_password { 7 } else { 6 };
            editor.focus.set(slot.min(count - 1), count);
            editor.clear_selections();
            editor.field = match slot {
                1 => SshEditorField::Port,
                2 => SshEditorField::Label,
                3 if shows_password => SshEditorField::Password,
                _ => SshEditorField::Destination,
            };
            if select_all {
                let (value, cursor) = editor.active_field();
                let text = value.clone();
                cursor.select_all(&text);
            }
        }
    }

    /// 鼠标按在输入框里：聚焦该字段并把光标放到点击的字符缝隙上，同时开一段
    /// 拖选。`x` 是物理像素的窗口坐标。
    pub fn ssh_editor_press_field(&mut self, hit: SshEditorHit, x: f32) {
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else { return };
        let Some((field, rect)) = rects.field_of(hit) else { return };
        let origin = rects.metrics.origin(field);
        let cell_w = rects.metrics.cell_w;
        let slot = match field {
            SshEditorField::Destination => 0,
            SshEditorField::Port => 1,
            SshEditorField::Label => 2,
            SshEditorField::Password => 3,
        };
        self.focus_ssh_editor_field(slot, false);
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return };
        let index = editor.index_at(field, x - origin, cell_w);
        let (value, cursor) = editor.field_mut(field);
        let text = value.clone();
        cursor.place(&text, index);
        let _ = rect;
        self.nebula_ssh_editor_drag = Some(field);
    }

    /// 拖动中：把选区从按下的锚点拉到当前位置。返回是否真的在拖，让调用方
    /// 决定要不要重绘。
    pub fn ssh_editor_drag_to(&mut self, x: f32) -> bool {
        let Some(field) = self.nebula_ssh_editor_drag else { return false };
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else { return false };
        let origin = rects.metrics.origin(field);
        let cell_w = rects.metrics.cell_w;
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return false };
        let index = editor.index_at(field, x - origin, cell_w);
        let (value, cursor) = editor.field_mut(field);
        let text = value.clone();
        cursor.extend_to(&text, index);
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
                let selected = editor
                    .port_cursor
                    .range(&editor.port)
                    .map_or(0, |(start, end)| end - start);
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
            let shows_password = ssh_ui::auth_sections(editor.auth).0;
            // 地址、端口、标签、[密码]、测试、取消、保存。
            let count = if shows_password { 7 } else { 6 };
            editor.focus.advance(count, reverse);
            editor.field = match (shows_password, editor.focus.current()) {
                (_, 0) => SshEditorField::Destination,
                (_, 1) => SshEditorField::Port,
                (_, 2) => SshEditorField::Label,
                (true, 3) => SshEditorField::Password,
                _ => editor.field,
            };
            // Tab 进一个输入框时全选：Windows 上所有输入框都这样，让"切过去
            // 直接覆写"是一次按键的事。焦点落在按钮上时不选，否则会有一段和
            // 当前操作无关的高亮一直亮着。
            if editor.focus.current() < if shows_password { 4 } else { 3 } {
                let (value, cursor) = editor.active_field();
                let text = value.clone();
                cursor.select_all(&text);
            }
        }
    }

    pub fn ssh_editor_activate_focus(&mut self) {
        let Some(editor) = self.nebula_ssh_editor.as_ref() else { return };
        let shows_password = ssh_ui::auth_sections(editor.auth).0;
        match (shows_password, editor.focus.current()) {
            (true, 4) | (false, 3) => self.queue_ssh_test(),
            (true, 5) | (false, 4) => self.close_ssh_editor(),
            (true, 6) | (false, 5) => self.save_ssh_editor(),
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

        let shows_password = ssh_ui::auth_sections(editor.auth).0;
        editor.focus.set(if shows_password { 2 } else { 1 }, if shows_password { 5 } else { 4 });
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
        match self.ssh_editor_hit(x, y) {
            // 点哪个框就聚焦哪个框，并把光标放到点中的字符缝隙上，同时开一段
            // 拖选。焦点序号必须和 `ssh_editor_next_field` 的顺序一致，否则点
            // 完再按 Tab 会跳到毫不相干的地方。
            hit @ (SshEditorHit::Destination
            | SshEditorHit::Port
            | SshEditorHit::Label
            | SshEditorHit::Password) => self.ssh_editor_press_field(hit, x),
            SshEditorHit::PasswordToggle => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.show_password = !editor.show_password;
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
                    let shows_password = ssh_ui::auth_sections(editor.auth).0;
                    editor.focus.set(
                        if shows_password { 3 } else { 2 },
                        if shows_password { 5 } else { 4 },
                    );
                }
                self.close_ssh_editor();
            },
            SshEditorHit::Primary => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let shows_password = ssh_ui::auth_sections(editor.auth).0;
                    editor.focus.set(
                        if shows_password { 4 } else { 3 },
                        if shows_password { 5 } else { 4 },
                    );
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
        profiles.upsert(crate::ssh_profiles::SshProfileAuth {
            destination: destination.clone(),
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
            // 空标签存成 None 而不是空串：两者语义相同，但 None 会被跳过序列化，
            // 于是没起名字的主机在配置文件里干干净净。
            label: Some(editor.label.trim().to_owned()).filter(|label| !label.is_empty()),
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
