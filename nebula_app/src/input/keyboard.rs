use winit::event::{ElementState, KeyEvent};
#[cfg(target_os = "macos")]
use winit::keyboard::ModifiersKeyState;
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::OptionAsAlt;

use nebula_terminal::event::EventListener;
use nebula_terminal::term::{ClipboardType, TermMode};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::{Action, BindingKey, BindingMode, KeyBinding};
use crate::display::window::ImeInhibitor;
use crate::event::TYPING_SEARCH_DELAY;
use crate::input::{ActionContext, Execute, Processor, terminal_input};
use crate::scheduler::{TimerId, Topic};

/// Modifiers physically held right now, tracked from the key events this
/// module actually saw. `ctx.modifiers()` cannot serve focus-loss key-up
/// synthesis: winit's Windows backend clears it (`ModifiersChanged(empty)`)
/// BEFORE delivering `Focused(false)`, so by the time `on_focus_change` runs
/// that state is already empty. Key events arrive before both, making this
/// the last honest record. One global slot — the physical keyboard is
/// singular, and a spurious synthesized up (e.g. after a cross-window hand-
/// off) is harmless where a dangling down is not.
static HELD_MODIFIERS: std::sync::Mutex<ModifiersState> =
    std::sync::Mutex::new(ModifiersState::empty());

/// Take (and clear) the held-modifier set for focus-loss synthesis.
pub(super) fn take_held_modifiers() -> ModifiersState {
    std::mem::take(&mut *HELD_MODIFIERS.lock().unwrap())
}

fn track_modifier_transition(key: &KeyEvent) {
    let flag = match key.logical_key.as_ref() {
        Key::Named(NamedKey::Shift) => ModifiersState::SHIFT,
        Key::Named(NamedKey::Control) => ModifiersState::CONTROL,
        Key::Named(NamedKey::Alt) => ModifiersState::ALT,
        Key::Named(NamedKey::Super) => ModifiersState::SUPER,
        _ => return,
    };
    HELD_MODIFIERS.lock().unwrap().set(flag, key.state == ElementState::Pressed);
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    /// Process key input.
    pub fn key_input(&mut self, key: KeyEvent) {
        // IME input will be applied on commit and shouldn't trigger key bindings.
        if self.ctx.display().ime.preedit().is_some() {
            return;
        }

        track_modifier_transition(&key);

        let mode = *self.ctx.terminal().mode();
        let mods = self.ctx.modifiers().state();

        if key.state == ElementState::Pressed {
            crate::input::latency::key_received();
        }

        if key.state == ElementState::Released {
            if self.ctx.inline_search_state().char_pending {
                self.ctx.window().set_ime_inhibitor(ImeInhibitor::VI, false);
            }
            self.key_release(key, mode, mods);
            return;
        }

        // A context menu is a transient focus scope, not a true modal. Esc
        // only dismisses it; any other key dismisses first and then continues
        // through the normal shortcut/terminal path.
        if self.ctx.display().context_menu_interactive() {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.ctx.display().close_context_menu();
            self.ctx.mark_dirty();
            if escape {
                return;
            }
        }

        // 键位捕获（spec 002）：设置页「按键映射」等待新组合时独占键盘，
        // 优先于一切快捷键路径——捕获的组合不能同时触发它旧的动作。
        if let Some(row) = self.ctx.display().nebula_keymap_capture {
            use crate::display::keymap::{self, CaptureOutcome};
            match keymap::capture_combo(&key, mods) {
                // 纯修饰键：实时回显按住的前缀（"Ctrl+…"），松开由
                // ModifiersChanged 路径清掉。
                CaptureOutcome::Pending => self.ctx.display().keymap_capture_preview(mods),
                CaptureOutcome::Cancel => self.ctx.display().keymap_cancel_capture(),
                CaptureOutcome::ClearCustom => {
                    self.ctx.display().keymap_clear_custom(row);
                    self.ctx.nebula_quick_hotkey_changed();
                },
                CaptureOutcome::Bind(combo) => {
                    self.ctx.display().keymap_assign(row, combo);
                    self.ctx.nebula_quick_hotkey_changed();
                },
            }
            self.ctx.mark_dirty();
            return;
        }

        // 背景色浮层的 16 进制输入独占键盘：Enter 应用、Esc 关闭、退格删
        // 除、hex 字符追加；dropdown 通用的"任意键关闭"对它不适用。
        if self.ctx.display().nebula_settings_dropdown
            == Some(crate::display::SettingsDropdown::BackgroundColor)
            && self.ctx.display().nebula_bg_hex_active
        {
            match &key.logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.ctx.display().bg_hex_commit();
                },
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().close_settings_dropdown();
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().bg_hex_backspace();
                },
                Key::Character(text) => {
                    for ch in text.chars() {
                        self.ctx.display().bg_hex_push(ch);
                    }
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // 设置→高级→同步的输入框独占键盘：Enter/Tab 提交、Esc 还原、
        // Ctrl+V 粘贴（URL 手打不现实）、其余字符追加。
        if self.ctx.display().settings_open() && self.ctx.display().nebula_sync_focus.is_some() {
            match &key.logical_key {
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                    self.ctx.display().commit_sync_field();
                },
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().cancel_sync_field();
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().sync_field_backspace();
                },
                Key::Named(NamedKey::Space) => {
                    self.ctx.display().sync_field_push(' ');
                },
                Key::Character(text) => {
                    if mods.control_key() && text.eq_ignore_ascii_case("v") {
                        let paste = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                        self.ctx.display().sync_field_paste(&paste);
                    } else if !mods.control_key() {
                        for ch in text.chars() {
                            self.ctx.display().sync_field_push(ch);
                        }
                    }
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // 设置→SSH→代理的输入框：规则与同步输入框一致（Enter/Tab
        // 提交、Esc 还原、Ctrl+V 粘贴、其余字符追加）。
        if self.ctx.display().settings_open() && self.ctx.display().nebula_ssh_proxy_focus.is_some()
        {
            match &key.logical_key {
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                    self.ctx.display().commit_ssh_proxy_field();
                },
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().cancel_ssh_proxy_field();
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().ssh_proxy_field_backspace();
                },
                Key::Named(NamedKey::Space) => {
                    self.ctx.display().ssh_proxy_field_push(' ');
                },
                Key::Character(text) => {
                    if mods.control_key() && text.eq_ignore_ascii_case("v") {
                        let paste = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                        self.ctx.display().ssh_proxy_field_paste(&paste);
                    } else if !mods.control_key() {
                        for ch in text.chars() {
                            self.ctx.display().ssh_proxy_field_push(ch);
                        }
                    }
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // 字体目录展开时键盘归弹层顶部的搜索框：系统字体动辄几百个，没有
        // 过滤就只能靠滚。搜索框是正经输入框，光标与选区走组件层，和其它
        // 字段同一套行为。
        if self.ctx.display().nebula_settings_dropdown
            == Some(crate::display::SettingsDropdown::Font)
        {
            let ctrl = mods.control_key();
            let shift = mods.shift_key();
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().close_settings_dropdown();
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().font_query_edit(None);
                },
                Key::Named(NamedKey::Delete) => {
                    self.ctx.display().font_query_delete_forward();
                },
                Key::Named(NamedKey::ArrowLeft) => {
                    self.ctx.display().font_query_move(false, shift);
                },
                Key::Named(NamedKey::ArrowRight) => {
                    self.ctx.display().font_query_move(true, shift);
                },
                Key::Named(NamedKey::Home) => {
                    self.ctx.display().font_query_jump(false, shift);
                },
                Key::Named(NamedKey::End) => {
                    self.ctx.display().font_query_jump(true, shift);
                },
                Key::Named(NamedKey::Space) if !ctrl => {
                    self.ctx.display().font_query_edit(Some(" "));
                },
                Key::Character(text) if ctrl => match text.as_str() {
                    "a" | "A" => self.ctx.display().font_query_select_all(),
                    "c" | "C" => {
                        if let Some(selected) = self.ctx.display().font_query_selected_text() {
                            self.ctx.clipboard_mut().store(ClipboardType::Clipboard, selected);
                        }
                    },
                    "v" | "V" => {
                        let pasted = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                        if !pasted.is_empty() {
                            self.ctx.display().font_query_edit(Some(&pasted));
                        }
                    },
                    _ => {},
                },
                Key::Character(text) => {
                    self.ctx.display().font_query_edit(Some(text.as_str()));
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // An expanded settings dropdown is a transient picker. Esc only
        // dismisses; any other key dismisses and then follows its normal path.
        if self.ctx.display().nebula_settings_dropdown.is_some() {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.ctx.display().close_settings_dropdown();
            self.ctx.mark_dirty();
            if escape {
                return;
            }
        }

        // Tab rename input: consume keyboard when editing a tab name (before
        // command palette, so rename can't be interrupted by palette shortcuts)
        if self.ctx.display().nebula_tab_rename.is_some() {
            match &key.logical_key {
                Key::Named(NamedKey::Enter) => {
                    // Commit rename
                    if let Some((_, text)) = self.ctx.display().nebula_tab_rename.clone() {
                        self.ctx.nebula_tab(crate::event::TabRequest::CommitRename(text));
                    }
                },
                Key::Named(NamedKey::Escape) => {
                    // Cancel rename
                    self.ctx.nebula_tab(crate::event::TabRequest::CancelRename);
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().tab_rename_backspace();
                    self.ctx.mark_dirty();
                },
                // Real text-field navigation: click already places the caret
                // (mouse path); arrows/Home/End move it, edits happen at it.
                Key::Named(NamedKey::ArrowLeft) => {
                    self.ctx.display().tab_rename_move_caret(-1);
                    self.ctx.mark_dirty();
                },
                Key::Named(NamedKey::ArrowRight) => {
                    self.ctx.display().tab_rename_move_caret(1);
                    self.ctx.mark_dirty();
                },
                Key::Named(NamedKey::Home) => {
                    self.ctx.display().tab_rename_caret_edge(false);
                    self.ctx.mark_dirty();
                },
                Key::Named(NamedKey::End) => {
                    self.ctx.display().tab_rename_caret_edge(true);
                    self.ctx.mark_dirty();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("a") =>
                {
                    self.ctx.display().tab_rename_select_all();
                    self.ctx.mark_dirty();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("c") =>
                {
                    if let Some(text) = self.ctx.display().tab_rename_selected_text() {
                        self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("v") =>
                {
                    let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                    self.ctx.display().tab_rename_insert(&text);
                    self.ctx.mark_dirty();
                },
                Key::Character(s) if mods.is_empty() || mods.shift_key() => {
                    // Insert at the caret (type-to-overwrite on select-all).
                    // Note: on Windows/IME, printable text arrives via
                    // Ime::Commit, not here — this is the non-IME fallback.
                    let text = s.clone();
                    self.ctx.display().tab_rename_insert(&text);
                    self.ctx.mark_dirty();
                },
                _ => {},
            }
            return;
        }

        // SFTP path/filter/create/rename fields share the same editing contract.
        if self
            .ctx
            .display()
            .nebula_sftp_panel
            .as_ref()
            .is_some_and(crate::display::sftp_panel::SftpPanel::editor_active)
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.editor_cancel();
                    }
                },
                Key::Named(NamedKey::Enter) => {
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        let _ = panel.editor_submit();
                    }
                },
                Key::Named(NamedKey::Backspace) => {
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.editor_backspace();
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("a") =>
                {
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.editor_select_all();
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("c") =>
                {
                    let text = self
                        .ctx
                        .display()
                        .nebula_sftp_panel
                        .as_ref()
                        .and_then(|panel| panel.editor_selected_text());
                    if let Some(text) = text {
                        self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("v") =>
                {
                    let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.editor_insert(&text);
                    }
                },
                Key::Character(text) if mods.is_empty() || mods.shift_key() => {
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.editor_insert(text);
                    }
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // Side-panel filter box: consume keyboard while it has focus, same
        // modal contract as tab rename. Printable text arrives via Ime::Commit
        // on Windows/IME; the Character arm is the non-IME fallback.
        if self.ctx.display().nebula_side_panel.search_focus {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    // Esc: clear the filter and leave the box.
                    self.ctx.display().nebula_side_panel.search_unfocus(true);
                },
                Key::Named(NamedKey::Enter) => {
                    self.ctx.display().nebula_side_panel.search_unfocus(false);
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().nebula_side_panel.search_backspace();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("a") =>
                {
                    self.ctx.display().nebula_side_panel.search_select_all();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("c") =>
                {
                    let text = self.ctx.display().nebula_side_panel.search_selected_text();
                    if let Some(text) = text {
                        self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("v") =>
                {
                    let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                    self.ctx.display().nebula_side_panel.search_input(&text);
                },
                Key::Character(s) if mods.is_empty() || mods.shift_key() => {
                    let text = s.clone();
                    self.ctx.display().nebula_side_panel.search_input(&text);
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // Git commit-message box: same modal keyboard contract.
        if self.ctx.display().nebula_side_panel.commit_focus {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().nebula_side_panel.commit_cancel();
                },
                Key::Named(NamedKey::Enter) => {
                    self.ctx.display().nebula_side_panel.git_commit_submit();
                },
                Key::Named(NamedKey::Backspace) => {
                    self.ctx.display().nebula_side_panel.commit_backspace();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("a") =>
                {
                    self.ctx.display().nebula_side_panel.commit_select_all();
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("c") =>
                {
                    let text = self.ctx.display().nebula_side_panel.commit_selected_text();
                    if let Some(text) = text {
                        self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                    }
                },
                Key::Character(c)
                    if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("v") =>
                {
                    let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                    self.ctx.display().nebula_side_panel.commit_input(&text);
                },
                Key::Character(s) if mods.is_empty() || mods.shift_key() => {
                    self.ctx.display().nebula_side_panel.commit_input(s);
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        if self.ctx.display().nebula_ssh_editor.is_some() {
            if self.ctx.display().ssh_editor_active() {
                // 图标列表开着时它独占键盘：打字进搜索、Esc 只收列表、回车挑
                // 第一项。不这样的话，敲 "deb" 会落进正在编辑的名字里，而 Esc
                // 会连整张表单一起关掉——用户只是想收起一个列表。
                if self.ctx.display().ssh_editor_icon_picker_open() {
                    match &key.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.ctx.display().ssh_editor_close_icon_picker();
                        },
                        Key::Named(NamedKey::Enter) => {
                            self.ctx.display().ssh_editor_icon_pick_first()
                        },
                        Key::Named(NamedKey::Backspace) => {
                            self.ctx.display().ssh_editor_icon_filter_edit(None)
                        },
                        Key::Named(NamedKey::Delete) => {
                            self.ctx.display().ssh_editor_icon_filter_delete_forward()
                        },
                        Key::Named(NamedKey::ArrowDown) => {
                            self.ctx.display().ssh_editor_icon_scroll(1)
                        },
                        Key::Named(NamedKey::ArrowUp) => {
                            self.ctx.display().ssh_editor_icon_scroll(-1)
                        },
                        // 搜索框是正经输入框：移动光标、Shift 扩选、Home/End、
                        // Ctrl+A/C/V 一样不缺——这套组合键属于肌肉记忆，缺一个
                        // 都会被读成"这个框是坏的"。
                        Key::Named(NamedKey::ArrowLeft) => {
                            self.ctx.display().ssh_editor_icon_filter_move(false, mods.shift_key())
                        },
                        Key::Named(NamedKey::ArrowRight) => {
                            self.ctx.display().ssh_editor_icon_filter_move(true, mods.shift_key())
                        },
                        Key::Named(NamedKey::Home) => {
                            self.ctx.display().ssh_editor_icon_filter_jump(false, mods.shift_key())
                        },
                        Key::Named(NamedKey::End) => {
                            self.ctx.display().ssh_editor_icon_filter_jump(true, mods.shift_key())
                        },
                        Key::Character(c)
                            if mods.control_key()
                                && !mods.alt_key()
                                && c.eq_ignore_ascii_case("a") =>
                        {
                            self.ctx.display().ssh_editor_icon_filter_select_all();
                        },
                        Key::Character(c)
                            if mods.control_key()
                                && !mods.alt_key()
                                && c.eq_ignore_ascii_case("c") =>
                        {
                            if let Some(text) =
                                self.ctx.display().ssh_editor_icon_filter_selected_text()
                            {
                                self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                            }
                        },
                        Key::Character(c)
                            if mods.control_key()
                                && !mods.alt_key()
                                && c.eq_ignore_ascii_case("v") =>
                        {
                            let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                            self.ctx.display().ssh_editor_icon_filter_edit(Some(&text));
                        },
                        Key::Character(c) if !mods.control_key() && !mods.alt_key() => {
                            self.ctx.display().ssh_editor_icon_filter_edit(Some(c))
                        },
                        Key::Named(NamedKey::Space) => {
                            self.ctx.display().ssh_editor_icon_filter_edit(Some(" "))
                        },
                        _ => {},
                    }
                    self.ctx.mark_dirty();
                    return;
                }
                match &key.logical_key {
                    Key::Named(NamedKey::Escape) => self.ctx.display().close_ssh_editor(),
                    Key::Named(NamedKey::Enter) => self.ctx.display().ssh_editor_activate_focus(),
                    Key::Named(NamedKey::Tab) => {
                        self.ctx.display().ssh_editor_next_field(mods.shift_key())
                    },
                    Key::Named(NamedKey::Backspace) => self.ctx.display().ssh_editor_backspace(),
                    Key::Named(NamedKey::Delete) => self.ctx.display().ssh_editor_delete_forward(),
                    // 光标导航。按住 Shift 是扩选——这套组合键在 Windows 上
                    // 属于肌肉记忆，缺一个都会被读成"这个输入框是坏的"。
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.ctx.display().ssh_editor_move_caret(false, mods.shift_key())
                    },
                    Key::Named(NamedKey::ArrowRight) => {
                        self.ctx.display().ssh_editor_move_caret(true, mods.shift_key())
                    },
                    Key::Named(NamedKey::Home) => {
                        self.ctx.display().ssh_editor_jump_caret(false, mods.shift_key())
                    },
                    Key::Named(NamedKey::End) => {
                        self.ctx.display().ssh_editor_jump_caret(true, mods.shift_key())
                    },
                    Key::Character(c)
                        if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("a") =>
                    {
                        self.ctx.display().ssh_editor_select_all();
                    },
                    Key::Character(c)
                        if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("c") =>
                    {
                        if let Some(text) = self.ctx.display().ssh_editor_selected_text() {
                            self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                        }
                    },
                    Key::Character(c)
                        if mods.control_key() && !mods.alt_key() && c.eq_ignore_ascii_case("v") =>
                    {
                        // SSH 编辑器是应用自有文本框。直接写入当前焦点字段，避免后续
                        // 终端粘贴绑定把地址或密码误发送到后台 PTY。
                        let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                        self.ctx.display().ssh_editor_insert(&text);
                    },
                    Key::Character(c) if mods.is_empty() || mods.shift_key() => {
                        self.ctx.display().ssh_editor_insert(c)
                    },
                    _ => {},
                }
                // Enter on the footer's test action stages a request in
                // Display; ActionContext owns the event proxy that starts it.
                self.ctx.nebula_ssh_test();
            }
            self.ctx.mark_dirty();
            return;
        }

        // Command and Shell/Profile palettes both own the keyboard while open:
        // typing, IME and editing shortcuts target their visible search box, so
        // nothing is accidentally forwarded to the terminal behind the veil.
        if self.ctx.display().command_palette_open() {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => self.ctx.display().close_command_palette(),
                Key::Named(NamedKey::Enter) => {
                    if let Some(action) = self.ctx.display().palette_confirm() {
                        self.run_palette_action(action);
                    }
                },
                Key::Named(NamedKey::Tab) => {
                    self.ctx.display().palette_tab(if mods.shift_key() { -1 } else { 1 });
                },
                Key::Named(NamedKey::ArrowDown) => self.ctx.display().palette_move(1),
                Key::Named(NamedKey::ArrowUp) => self.ctx.display().palette_move(-1),
                Key::Named(NamedKey::Backspace) => self.ctx.display().palette_backspace(),
                Key::Character(c) if mods.control_key() => {
                    if c.eq_ignore_ascii_case("a") {
                        self.ctx.display().palette_select_all();
                    } else if c.eq_ignore_ascii_case("c") {
                        if let Some(text) = self.ctx.display().palette_selected_text() {
                            self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                        }
                    } else if c.eq_ignore_ascii_case("v") {
                        let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                        self.ctx.display().palette_input_text(&text);
                    } else if mods.shift_key() && c.eq_ignore_ascii_case("p") {
                        // Ctrl+Shift+P (the opener) toggles the full palette.
                        self.ctx.display().close_command_palette();
                    }
                },
                Key::Character(c) => {
                    for ch in c.chars() {
                        self.ctx.display().palette_input_char(ch);
                    }
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
        }

        // Nebula confirm modal (busy-process close / multi-line paste) owns
        // the keyboard while visible: Enter approves, Esc cancels, everything
        // else is swallowed so nothing types into the shell behind the veil.
        if let Some(confirm) = self.ctx.display().nebula_confirm.clone() {
            if matches!(confirm, crate::display::NebulaConfirm::BackupPassphrase { .. }) {
                match &key.logical_key {
                    Key::Named(NamedKey::Enter) => self.nebula_confirm_accept(confirm),
                    Key::Named(NamedKey::Escape) => self.nebula_confirm_cancel(confirm),
                    Key::Named(NamedKey::Backspace) => {
                        self.ctx.display().backup_passphrase_backspace();
                    },
                    Key::Named(NamedKey::Space) => {
                        self.ctx.display().backup_passphrase_push(' ');
                    },
                    Key::Character(text) if mods.control_key() => {
                        if text.eq_ignore_ascii_case("a") {
                            self.ctx.display().backup_passphrase_select_all();
                        } else if text.eq_ignore_ascii_case("v") {
                            let paste = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                            self.ctx.display().backup_passphrase_paste(&paste);
                        }
                    },
                    Key::Character(text) if !mods.alt_key() => {
                        for character in text.chars() {
                            self.ctx.display().backup_passphrase_push(character);
                        }
                    },
                    _ => {},
                }
            } else {
                match &key.logical_key {
                    // Shared with the modal's primary button (mouse path).
                    Key::Named(NamedKey::Enter) => self.nebula_confirm_accept(confirm),
                    Key::Named(NamedKey::Escape) => self.nebula_confirm_cancel(confirm),
                    _ => {},
                }
            }
            self.ctx.mark_dirty();
            return;
        }

        // Drawer shortcuts run only after every keyboard-owning overlay has
        // had first refusal, so an Alt chord cannot act on the obscured panel.
        if self.ctx.display().nebula_side_panel.open
            && self.ctx.display().nebula_side_panel.view
                == crate::display::side_panel::PanelView::Files
            && mods.alt_key()
            && !mods.control_key()
        {
            let action = match &key.logical_key {
                Key::Character(character) if character.eq_ignore_ascii_case("r") => Some('r'),
                Key::Character(character) if character.eq_ignore_ascii_case("t") => Some('t'),
                Key::Character(character) if character.eq_ignore_ascii_case("o") => Some('o'),
                _ => None,
            };
            if let Some(action) = action {
                match action {
                    'r' => self.ctx.display().follow_focused_directory(),
                    't' => {
                        let root = self
                            .ctx
                            .display()
                            .nebula_side_panel
                            .root()
                            .map(std::path::Path::to_path_buf);
                        if let Some(root) = root {
                            self.ctx.nebula_tab(crate::event::TabRequest::NewAtDirectory(root));
                        }
                    },
                    'o' => {
                        let root = self
                            .ctx
                            .display()
                            .nebula_side_panel
                            .root()
                            .map(std::path::Path::to_path_buf);
                        if let Some(root) = root {
                            self.ctx.open_path(&root);
                        }
                    },
                    _ => unreachable!(),
                }
                self.ctx.mark_dirty();
                return;
            }
        }

        // Ctrl+Z reverses the latest SSH deletion while the action bar lives.
        // Text-entry and true-modal scopes above retain their own contracts.
        if mods.control_key()
            && !mods.alt_key()
            && !mods.shift_key()
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("z"))
            && self.ctx.display().ssh_delete_undo_available()
        {
            self.undo_ssh_delete();
            self.ctx.mark_dirty();
            return;
        }

        // 助手建议条（spec 001）：Ctrl+. 贴入（只贴不执行，不带回车），
        // Esc 撤条，开始打字也撤。放在模态之后——palette/确认框的 Esc 契约
        // 不受影响；无条子时三个分支全部穿透，Ctrl+. 照常进终端。
        if mods.control_key()
            && !mods.alt_key()
            && !mods.shift_key()
            && matches!(&key.logical_key, Key::Character(c) if c.as_str() == ".")
        {
            if let Some(command) = self.ctx.nebula_take_ai_fix() {
                self.ctx.write_to_pty(command.into_bytes());
                self.ctx.mark_dirty();
                return;
            }
        }
        if matches!(&key.logical_key, Key::Named(NamedKey::Escape))
            && self.ctx.nebula_dismiss_ai_fix()
        {
            self.ctx.mark_dirty();
            return;
        }
        if matches!(&key.logical_key, Key::Character(_))
            && !mods.control_key()
            && !mods.alt_key()
            && self.ctx.nebula_dismiss_ai_fix()
        {
            // 打字撤条：字符本身照常落进终端，不拦截。
            self.ctx.mark_dirty();
        }

        // Document-viewer tab: bare navigation keys scroll the document. Only
        // unmodified keys are taken — chords (Ctrl+Tab, Ctrl+Shift+W, …) fall
        // through to the normal tab bindings, and any stray text ends in the
        // doc pane's sink notifier anyway.
        if self.ctx.doc_view().is_some() && mods.is_empty() {
            let cell_h = self.ctx.size_info().cell_height();
            let viewport_h = self.ctx.display().terminal_card_rect().3;
            let delta = match &key.logical_key {
                Key::Named(NamedKey::ArrowDown) => Some(3.0 * cell_h),
                Key::Named(NamedKey::ArrowUp) => Some(-3.0 * cell_h),
                Key::Named(NamedKey::PageDown | NamedKey::Space) => Some(viewport_h * 0.9),
                Key::Named(NamedKey::PageUp) => Some(-viewport_h * 0.9),
                Key::Named(NamedKey::Home) => Some(f32::NEG_INFINITY),
                Key::Named(NamedKey::End) => Some(f32::INFINITY),
                _ => None,
            };
            if let Some(delta) = delta {
                if let Some(doc) = self.ctx.doc_view() {
                    doc.scroll_by(delta, viewport_h);
                }
                self.ctx.mark_dirty();
                return;
            }
        }

        // Nebula chrome shortcuts (tabs, splits, panels, palette, profiles)
        // live in the binding table now — `default_key_bindings()` +
        // `nebula_settings.txt` `keybind=` overrides + TOML remaps all funnel
        // through `process_key_bindings` below (spec 002).

        // Accept the Nebula ghost-text suggestion with the configured key
        // (Right/Tab/both): write the remaining text so the shell echoes it,
        // as if typed. Tab only accepts when a suggestion exists; otherwise it
        // falls through to the shell's own completion below.
        let accept = self.ctx.nebula_accept();
        let is_accept = mods.is_empty()
            && matches!(&key.logical_key,
                Key::Named(NamedKey::ArrowRight) if accept.accepts_right())
            || mods.is_empty()
                && matches!(&key.logical_key, Key::Named(NamedKey::Tab) if accept.accepts_tab());
        if is_accept {
            let ghost = self.ctx.nebula_take_suggestion();
            if !ghost.is_empty() {
                for c in ghost.chars() {
                    self.ctx.nebula_input_char(c);
                }
                self.ctx.write_to_pty(ghost.into_bytes());
                return;
            }
        }

        let text = key.text_with_all_modifiers().unwrap_or_default();

        // All key bindings are disabled while a hint is being selected.
        if self.ctx.display().hint_state.active() {
            for character in text.chars() {
                self.ctx.hint_input(character);
            }
            return;
        }

        // First key after inline search is captured.
        let inline_state = self.ctx.inline_search_state();
        if inline_state.char_pending {
            self.ctx.inline_search_input(text);
            return;
        }

        // Reset search delay when the user is still typing.
        self.reset_search_delay();

        // Key bindings suppress the character input.
        if self.process_key_bindings(&key) {
            return;
        }

        if self.ctx.search_active() {
            for character in text.chars() {
                self.ctx.search_input(character);
            }

            return;
        }

        // Vi mode on its own doesn't have any input, the search input was done before.
        if mode.contains(TermMode::VI) {
            return;
        }

        // Track the prompt line only while normal shell input is active. This
        // mirrors Nushell/Reedline's separation between editor modes: search,
        // hint-selection, inline-search and vi navigation must not mutate the
        // shell prompt buffer used for ghost history/path hints.
        // Ctrl+V and Ctrl+Shift+V both paste now; neither should feed the
        // literal "v" into the prompt-line tracker below.
        let is_paste_shortcut = mods.control_key()
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("v"));
        if !is_paste_shortcut {
            match &key.logical_key {
                Key::Named(NamedKey::Enter) => self.ctx.nebula_commit_line(),
                Key::Named(NamedKey::Backspace) if mods.control_key() => {
                    self.ctx.nebula_delete_word();
                },
                Key::Named(NamedKey::Backspace) => self.ctx.nebula_input_backspace(),
                Key::Character(s) if mods.is_empty() || mods.shift_key() => {
                    for c in s.chars() {
                        self.ctx.nebula_input_char(c);
                    }
                },
                Key::Named(NamedKey::Space) if mods.is_empty() || mods.shift_key() => {
                    self.ctx.nebula_input_char(' ');
                },
                // Esc, completion, history recall, cursor movement and Delete invalidate
                // our approximation because the shell/editor may rewrite the
                // line or move the edit point away from the end.
                Key::Named(
                    NamedKey::Escape
                    | NamedKey::Tab
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::Delete,
                ) => {
                    self.ctx.nebula_clear_line();
                },
                Key::Character(c) if mods.control_key() && c.eq_ignore_ascii_case("w") => {
                    self.ctx.nebula_delete_word();
                },
                Key::Character(c)
                    if mods.control_key()
                        && (c.eq_ignore_ascii_case("u")
                            || c.eq_ignore_ascii_case("c")
                            || c.eq_ignore_ascii_case("k")) =>
                {
                    self.ctx.nebula_clear_line();
                },
                _ => {},
            }
        }

        // Mask `Alt` modifier from input when we won't send esc.
        let mods = if self.alt_send_esc(&key, text) { mods } else { mods & !ModifiersState::ALT };

        let build_key_sequence = Self::should_build_sequence(&key, text, mode, mods);
        let is_modifier_key = Self::is_modifier_key(&key);

        let bytes = if build_key_sequence {
            terminal_input::build_sequence(&terminal_input::KeyInput::from(&key), mods, mode)
        } else {
            let mut bytes = Vec::with_capacity(text.len() + 1);
            if mods.alt_key() {
                bytes.push(b'\x1b');
            }

            bytes.extend_from_slice(text.as_bytes());
            bytes
        };

        // Write only if we have something to write.
        if !bytes.is_empty() {
            // Don't clear selection/scroll down when writing escaped modifier keys.
            if !is_modifier_key {
                self.ctx.on_terminal_input_start();
            }
            self.ctx.write_to_pty(bytes);
            crate::input::latency::key_written_to_pty();
        }
    }

    /// Execute a command-palette action. Dispatch lives here because it needs
    /// both the window context (tab / split / window requests) and the display
    /// (theme / settings / appearance) — the input layer is the only place with
    /// access to both.
    pub(super) fn run_palette_action(
        &mut self,
        action: crate::display::command_palette::PaletteAction,
    ) {
        use crate::display::command_palette::PaletteAction::*;
        use crate::event::TabRequest;
        match action {
            NewTab => self.ctx.nebula_tab(TabRequest::New),
            CopyCwd => {
                if let Some(path) = self.ctx.display().focused_cwd_string() {
                    self.ctx.clipboard_mut().store(ClipboardType::Clipboard, path);
                }
            },
            RevealCwd => self.ctx.display().reveal_focused_cwd(),
            ToggleSidebar => self.ctx.display().toggle_sidebar(),
            TogglePanelResize => self.ctx.display().request_toggle_panel_resize(),
            OpenDirectoryPicker => self.ctx.display().open_directory_picker(),
            OpenAiSessionPicker => self.ctx.display().open_ai_session_palette(),
            ResumeAiSession(command) => {
                // 非 bracketed：resume 是要**执行**的命令行，bracketed 包裹
                // 会让部分 shell 把它按纯文本粘着不跑。
                self.ctx.paste(&format!("{command}\r"), false);
            },
            CloseTab => self.ctx.nebula_tab(TabRequest::Close),
            NextTab => self.ctx.nebula_tab(TabRequest::SelectNext),
            PrevTab => self.ctx.nebula_tab(TabRequest::SelectPrev),
            NewWindow => {
                #[cfg(not(target_os = "macos"))]
                self.ctx.create_new_window();
                #[cfg(target_os = "macos")]
                self.ctx.create_new_window(None);
            },
            SplitRight => self
                .ctx
                .nebula_tab(TabRequest::SplitToggle(crate::display::SplitDirection::LeftRight)),
            SplitDown => self
                .ctx
                .nebula_tab(TabRequest::SplitToggle(crate::display::SplitDirection::TopBottom)),
            ExportWorkspace => self.ctx.nebula_tab(TabRequest::ExportWorkspace),
            ImportWorkspace => self.ctx.nebula_tab(TabRequest::ImportWorkspace),
            SyncPush => self.ctx.nebula_sync(true),
            SyncPull => self.ctx.nebula_sync(false),
            OpenSettings => self.ctx.nebula_tab(TabRequest::OpenSettings),
            OpenSettingsFile => self.ctx.display().open_user_config_file(),
            ToggleGhost => self.ctx.display().toggle_ghost(),
            CycleAccept => self.ctx.display().cycle_accept(),
            PickBackgroundImage => self.ctx.display().pick_background_image(),
            CycleBackground => self.ctx.display().cycle_background_color(),
            ResetAppearance => self.ctx.display().reset_appearance_settings(),
            SelectTheme(theme) => self.ctx.display().select_nebula_theme(theme),
            LaunchProfile(profile) => self.ctx.nebula_tab(TabRequest::NewProfile(profile)),
            LaunchShell(shell) => self.ctx.nebula_tab(TabRequest::NewShell {
                name: shell.name.clone(),
                shell: shell.shell(),
            }),
            LaunchSsh(host) => self.ctx.nebula_tab(TabRequest::NewSsh(host)),
            SetDefaultShell(shell) => self.ctx.display().set_default_shell(&shell),
            SetDefaultProfile(profile) => self.ctx.display().set_default_profile(&profile),
            NewAtDirectory(path) => self.ctx.nebula_tab(TabRequest::NewAtDirectory(path)),
            ToggleFilesPanel => {
                if let Some(destination) = self.ctx.nebula_ssh_destination().map(str::to_owned) {
                    self.ctx.nebula_open_sftp(destination);
                } else {
                    self.ctx
                        .display()
                        .toggle_side_panel(crate::display::side_panel::PanelView::Files)
                }
            },
            ToggleGitPanel => {
                self.ctx.display().toggle_side_panel(crate::display::side_panel::PanelView::Git)
            },
        }
        self.ctx.mark_dirty();
    }

    fn alt_send_esc(&mut self, key: &KeyEvent, text: &str) -> bool {
        #[cfg(not(target_os = "macos"))]
        let alt_send_esc = self.ctx.modifiers().state().alt_key();

        #[cfg(target_os = "macos")]
        let alt_send_esc = {
            let option_as_alt = self.ctx.config().window.option_as_alt();
            self.ctx.modifiers().state().alt_key()
                && (option_as_alt == OptionAsAlt::Both
                    || (option_as_alt == OptionAsAlt::OnlyLeft
                        && self.ctx.modifiers().lalt_state() == ModifiersKeyState::Pressed)
                    || (option_as_alt == OptionAsAlt::OnlyRight
                        && self.ctx.modifiers().ralt_state() == ModifiersKeyState::Pressed))
        };

        match key.logical_key {
            Key::Named(named) => {
                if named.to_text().is_some() {
                    alt_send_esc
                } else {
                    // Treat `Alt` as modifier for named keys without text, like ArrowUp.
                    self.ctx.modifiers().state().alt_key()
                }
            },
            _ => alt_send_esc && text.chars().count() == 1,
        }
    }

    fn is_modifier_key(key: &KeyEvent) -> bool {
        matches!(
            key.logical_key.as_ref(),
            Key::Named(NamedKey::Shift)
                | Key::Named(NamedKey::Control)
                | Key::Named(NamedKey::Alt)
                | Key::Named(NamedKey::Super)
        )
    }

    /// Check whether we should try to build escape sequence for the [`KeyEvent`].
    fn should_build_sequence(
        key: &KeyEvent,
        text: &str,
        mode: TermMode,
        mods: ModifiersState,
    ) -> bool {
        if terminal_input::use_win32_input_mode(mode)
            && terminal_input::build_win32_input_sequence(&terminal_input::KeyInput::from(key))
                .is_some()
        {
            return true;
        }

        if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
            return true;
        }

        let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)
            && (key.logical_key == Key::Named(NamedKey::Escape)
                || key.location == KeyLocation::Numpad
                || (!mods.is_empty()
                    && (mods != ModifiersState::SHIFT
                        || matches!(
                            key.logical_key,
                            Key::Named(NamedKey::Tab)
                                | Key::Named(NamedKey::Enter)
                                | Key::Named(NamedKey::Backspace)
                        ))));

        match key.logical_key {
            _ if disambiguate => true,
            // Exclude all the named keys unless they have textual representation.
            Key::Named(named) => named.to_text().is_none(),
            _ => text.is_empty(),
        }
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_key_bindings(&mut self, key: &KeyEvent) -> bool {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mods = self.ctx.modifiers().state();

        // Don't suppress char if no bindings were triggered.
        let mut suppress_chars = None;

        // We don't want the key without modifier, because it means something else most of
        // the time. However what we want is to manually lowercase the character to account
        // for both small and capital letters on regular characters at the same time.
        let logical_key = if let Key::Character(ch) = key.logical_key.as_ref() {
            // Match `Alt` bindings without `Alt` being applied, otherwise they use the
            // composed chars, which are not intuitive to bind.
            //
            // On Windows, the `Ctrl + Alt` mangles `logical_key` to unidentified values, thus
            // preventing them from being used in bindings
            //
            // For more see https://github.com/rust-windowing/winit/issues/2945.
            if (cfg!(target_os = "macos") || (cfg!(windows) && mods.control_key()))
                && mods.alt_key()
            {
                key.key_without_modifiers()
            } else {
                Key::Character(ch.to_lowercase().into())
            }
        } else {
            key.logical_key.clone()
        };

        // Get the action of a key binding.
        let mut binding_action = |binding: &KeyBinding| {
            let key = match (&binding.trigger, &logical_key) {
                (BindingKey::Scancode(_), _) => BindingKey::Scancode(key.physical_key),
                (_, code) => {
                    BindingKey::Keycode { key: code.clone(), location: key.location.into() }
                },
            };

            if binding.is_triggered_by(mode, mods, &key) {
                // Pass through the key if any of the bindings has the `ReceiveChar` action.
                *suppress_chars.get_or_insert(true) &= binding.action != Action::ReceiveChar;

                // Binding was triggered; run the action.
                Some(binding.action.clone())
            } else {
                None
            }
        };

        // 用户自定义表（spec 002）优先于 config 默认/TOML 表：命中任意条
        // （含 `none` 的禁用条目）即短路——config 表与 hint 绑定不再参与，
        // 「改键遮蔽默认」由这一条实现。表本身是倒序（最新行在前）。
        for i in 0..self.ctx.display().nebula_keymap.len() {
            let binding = self.ctx.display().nebula_keymap[i].clone();
            if let Some(action) = binding_action(&binding) {
                action.execute(&mut self.ctx);
                return suppress_chars.unwrap_or(false);
            }
        }

        // Trigger matching key bindings.
        for i in 0..self.ctx.config().key_bindings().len() {
            let binding = &self.ctx.config().key_bindings()[i];
            if let Some(action) = binding_action(binding) {
                action.execute(&mut self.ctx);
            }
        }

        // Trigger key bindings for hints.
        for i in 0..self.ctx.config().hints.enabled.len() {
            let hint = &self.ctx.config().hints.enabled[i];
            let binding = match hint.binding.as_ref() {
                Some(binding) => binding.key_binding(hint),
                None => continue,
            };

            if let Some(action) = binding_action(binding) {
                action.execute(&mut self.ctx);
            }
        }

        suppress_chars.unwrap_or(false)
    }

    /// Handle key release.
    fn key_release(&mut self, key: KeyEvent, mode: TermMode, mods: ModifiersState) {
        if !mode.contains(TermMode::REPORT_EVENT_TYPES) && !terminal_input::use_win32_input_mode(mode)
            || mode.contains(TermMode::VI)
            || self.ctx.search_active()
            || self.ctx.display().hint_state.active()
        {
            return;
        }

        // Mask `Alt` modifier from input when we won't send esc.
        let text = key.text_with_all_modifiers().unwrap_or_default();
        let mods = if self.alt_send_esc(&key, text) { mods } else { mods & !ModifiersState::ALT };

        let bytes = match key.logical_key.as_ref() {
            Key::Named(NamedKey::Enter)
            | Key::Named(NamedKey::Tab)
            | Key::Named(NamedKey::Backspace)
                if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
                    && !terminal_input::use_win32_input_mode(mode) =>
            {
                return;
            },
            _ => terminal_input::build_sequence(&terminal_input::KeyInput::from(&key), mods, mode),
        };

        self.ctx.write_to_pty(bytes);
    }

    /// Reset search delay.
    fn reset_search_delay(&mut self) {
        if self.ctx.search_active() {
            let timer_id = TimerId::new(Topic::DelayedSearch, self.ctx.window().id());
            let scheduler = self.ctx.scheduler_mut();
            if let Some(timer) = scheduler.unschedule(timer_id) {
                scheduler.schedule(timer.event, TYPING_SEARCH_DELAY, false, timer.id);
            }
        }
    }
}
