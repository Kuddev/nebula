use std::borrow::Cow;

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
use crate::input::{ActionContext, Execute, Processor};
use crate::scheduler::{TimerId, Topic};

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    /// Process key input.
    pub fn key_input(&mut self, key: KeyEvent) {
        // IME input will be applied on commit and shouldn't trigger key bindings.
        if self.ctx.display().ime.preedit().is_some() {
            return;
        }

        let mode = *self.ctx.terminal().mode();
        let mods = self.ctx.modifiers().state();

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

        // The inline default-shell list is a transient picker. Esc only
        // dismisses; any other key dismisses and then follows its normal path.
        if self.ctx.display().nebula_shell_picker_open {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.ctx.display().close_shell_picker();
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
                match &key.logical_key {
                    Key::Named(NamedKey::Escape) => self.ctx.display().close_ssh_editor(),
                    Key::Named(NamedKey::Enter) => self.ctx.display().save_ssh_editor(),
                    Key::Named(NamedKey::Tab) => self.ctx.display().ssh_editor_next_field(),
                    Key::Named(NamedKey::Backspace) => self.ctx.display().ssh_editor_backspace(),
                    Key::Character(c)
                        if mods.control_key()
                            && !mods.alt_key()
                            && c.eq_ignore_ascii_case("a") =>
                    {
                        self.ctx.display().ssh_editor_select_all();
                    },
                    Key::Character(c)
                        if mods.control_key()
                            && !mods.alt_key()
                            && c.eq_ignore_ascii_case("c") =>
                    {
                        if let Some(text) = self.ctx.display().ssh_editor_selected_text() {
                            self.ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                        }
                    },
                    Key::Character(c)
                        if mods.control_key()
                            && !mods.alt_key()
                            && c.eq_ignore_ascii_case("v") =>
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
            }
            self.ctx.mark_dirty();
            return;
        }

        // The new-tab shell/profile list is a lightweight menu: navigation and
        // confirmation stay inside it, while unrelated keys dismiss it and
        // continue through normal shortcuts/terminal input.
        if self.ctx.display().command_palette_picker_open() {
            let handled = match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().close_command_palette();
                    true
                },
                Key::Named(NamedKey::Enter) => {
                    if let Some(action) = self.ctx.display().palette_confirm() {
                        self.run_palette_action(action);
                    }
                    true
                },
                Key::Named(NamedKey::Tab) => {
                    self.ctx.display().palette_move(if mods.shift_key() { -1 } else { 1 });
                    true
                },
                Key::Named(NamedKey::ArrowDown) => {
                    self.ctx.display().palette_move(1);
                    true
                },
                Key::Named(NamedKey::ArrowUp) => {
                    self.ctx.display().palette_move(-1);
                    true
                },
                _ => {
                    self.ctx.display().close_command_palette();
                    false
                },
            };
            self.ctx.mark_dirty();
            if handled {
                return;
            }
        }

        // The full command palette owns the keyboard while open: route typing /
        // navigation / confirm into it and swallow everything else, so no
        // terminal binding fires behind the modal.
        if self.ctx.display().command_palette_open() {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => self.ctx.display().close_command_palette(),
                Key::Named(NamedKey::Enter) => {
                    if let Some(action) = self.ctx.display().palette_confirm() {
                        self.run_palette_action(action);
                    }
                },
                Key::Named(NamedKey::Tab) => {
                    self.ctx.display().palette_move(if mods.shift_key() { -1 } else { 1 });
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
            match &key.logical_key {
                // Shared with the modal's primary button (mouse path).
                Key::Named(NamedKey::Enter) => self.nebula_confirm_accept(confirm),
                Key::Named(NamedKey::Escape) => {
                    self.ctx.display().nebula_confirm = None;
                },
                _ => {},
            }
            self.ctx.mark_dirty();
            return;
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

        // Nebula tab shortcuts: Ctrl+Shift+T new, Ctrl+Shift+W close,
        // Ctrl+Tab next, Ctrl+Shift+Tab previous, Ctrl+1..5 direct select.
        // Ctrl+Shift+E new window.
        if mods.control_key() {
            let shift = mods.shift_key();
            // Ctrl+Shift+E: open a brand-new Nebula window.
            if shift && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("e"))
            {
                #[cfg(not(target_os = "macos"))]
                self.ctx.create_new_window();
                #[cfg(target_os = "macos")]
                self.ctx.create_new_window(None);
                return;
            }
            // Ctrl+Shift+P: toggle the command palette.
            if shift && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("p"))
            {
                let profiles: Vec<String> =
                    self.ctx.config().profiles.iter().map(|p| p.name.clone()).collect();
                self.ctx.display().toggle_command_palette(&profiles);
                self.ctx.mark_dirty();
                return;
            }
            // Ctrl+Shift+O / Ctrl+Shift+G: toggle the right-side drawer's
            // directory-tree / git view.
            if shift && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("o"))
            {
                self.ctx.display().toggle_side_panel(crate::display::side_panel::PanelView::Files);
                self.ctx.mark_dirty();
                return;
            }
            if shift && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("g"))
            {
                self.ctx.display().toggle_side_panel(crate::display::side_panel::PanelView::Git);
                self.ctx.mark_dirty();
                return;
            }
            // Ctrl+Shift+1..9: launch the quick-launch profile at that index
            // (Windows Terminal parity). Physical digit codes — Shift turns the
            // logical key into "!" "@" … so `logical_key` can't be matched.
            if shift {
                use winit::keyboard::{KeyCode, PhysicalKey};
                let profile = match key.physical_key {
                    PhysicalKey::Code(KeyCode::Digit1) => Some(0),
                    PhysicalKey::Code(KeyCode::Digit2) => Some(1),
                    PhysicalKey::Code(KeyCode::Digit3) => Some(2),
                    PhysicalKey::Code(KeyCode::Digit4) => Some(3),
                    PhysicalKey::Code(KeyCode::Digit5) => Some(4),
                    PhysicalKey::Code(KeyCode::Digit6) => Some(5),
                    PhysicalKey::Code(KeyCode::Digit7) => Some(6),
                    PhysicalKey::Code(KeyCode::Digit8) => Some(7),
                    PhysicalKey::Code(KeyCode::Digit9) => Some(8),
                    _ => None,
                };
                if let Some(i) = profile {
                    if i < self.ctx.config().profiles.len() {
                        self.ctx.nebula_tab(crate::event::TabRequest::NewProfile(i));
                        return;
                    }
                }
            }
            // Ctrl+Shift+Up/Down: jump to the previous/next shell prompt
            // (OSC 133 semantic marks; needs shell integration or Nushell).
            if shift && !mods.alt_key() {
                let jump_up = match &key.logical_key {
                    Key::Named(NamedKey::ArrowUp) => Some(true),
                    Key::Named(NamedKey::ArrowDown) => Some(false),
                    _ => None,
                };
                if let Some(up) = jump_up {
                    if self.ctx.terminal_mut().nebula_prompt_jump(up) {
                        self.ctx.mark_dirty();
                    }
                    return;
                }
            }
            let request = match &key.logical_key {
                Key::Named(NamedKey::Tab) => Some(if shift {
                    crate::event::TabRequest::SelectPrev
                } else {
                    crate::event::TabRequest::SelectNext
                }),
                Key::Character(c) if shift && c.eq_ignore_ascii_case("t") => {
                    Some(crate::event::TabRequest::New)
                },
                Key::Character(c) if shift && c.eq_ignore_ascii_case("w") => {
                    Some(crate::event::TabRequest::Close)
                },
                Key::Character(c) if shift && c.eq_ignore_ascii_case("d") => {
                    Some(crate::event::TabRequest::SplitToggle(
                        crate::display::SplitDirection::LeftRight,
                    ))
                },
                Key::Character(c) if shift && c.eq_ignore_ascii_case("s") => {
                    Some(crate::event::TabRequest::SplitToggle(
                        crate::display::SplitDirection::TopBottom,
                    ))
                },
                // Ctrl+Shift+Enter: zoom the focused pane to fill the window (toggle).
                Key::Named(NamedKey::Enter) if shift => Some(crate::event::TabRequest::ToggleZoom),
                // Ctrl+Alt+Arrow: move focus between split panes.
                Key::Named(NamedKey::ArrowLeft) if mods.alt_key() => {
                    Some(crate::event::TabRequest::FocusSplit(crate::display::SplitNav::Left))
                },
                Key::Named(NamedKey::ArrowRight) if mods.alt_key() => {
                    Some(crate::event::TabRequest::FocusSplit(crate::display::SplitNav::Right))
                },
                Key::Named(NamedKey::ArrowUp) if mods.alt_key() => {
                    Some(crate::event::TabRequest::FocusSplit(crate::display::SplitNav::Up))
                },
                Key::Named(NamedKey::ArrowDown) if mods.alt_key() => {
                    Some(crate::event::TabRequest::FocusSplit(crate::display::SplitNav::Down))
                },
                Key::Character(c) if !shift => match c.as_str() {
                    // Ctrl+1..9 direct tab select (Windows Terminal parity).
                    "1" => Some(crate::event::TabRequest::Select(0)),
                    "2" => Some(crate::event::TabRequest::Select(1)),
                    "3" => Some(crate::event::TabRequest::Select(2)),
                    "4" => Some(crate::event::TabRequest::Select(3)),
                    "5" => Some(crate::event::TabRequest::Select(4)),
                    "6" => Some(crate::event::TabRequest::Select(5)),
                    "7" => Some(crate::event::TabRequest::Select(6)),
                    "8" => Some(crate::event::TabRequest::Select(7)),
                    "9" => Some(crate::event::TabRequest::Select(8)),
                    _ => None,
                },
                _ => None,
            };
            if let Some(request) = request {
                self.ctx.nebula_tab(request);
                return;
            }
        }

        // Alt+1..9: direct tab select (Windows Terminal style). Alt-only —
        // Ctrl+Alt (AltGr) must stay clear so layouts that type digits/symbols
        // via AltGr keep working, and plain Alt+letter chords still reach the
        // shell as ESC-prefixed input below.
        if mods.alt_key() && !mods.control_key() && !mods.shift_key() && !mods.super_key() {
            if let Key::Character(c) = &key.logical_key {
                if let Some(digit @ 1..=9) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                    self.ctx.nebula_tab(crate::event::TabRequest::Select(digit as usize - 1));
                    return;
                }
            }
        }

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
            build_sequence(key, mods, mode)
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
            OpenSettings => self.ctx.display().toggle_settings(),
            OpenSettingsFile => self.ctx.display().open_settings_file(),
            ToggleGhost => self.ctx.display().toggle_ghost(),
            CycleAccept => self.ctx.display().cycle_accept(),
            PickBackgroundImage => self.ctx.display().pick_background_image(),
            CycleBackground => self.ctx.display().cycle_background_color(),
            ResetAppearance => self.ctx.display().reset_appearance_settings(),
            SelectTheme(theme) => self.ctx.display().select_nebula_theme(theme),
            LaunchProfile(i) => self.ctx.nebula_tab(TabRequest::NewProfile(i)),
            LaunchShell(shell) => self.ctx.nebula_tab(TabRequest::NewShell {
                name: shell.name.clone(),
                shell: shell.shell(),
            }),
            SetDefaultShell(shell) => self.ctx.display().set_default_shell(&shell),
            ToggleFilesPanel => {
                self.ctx.display().toggle_side_panel(crate::display::side_panel::PanelView::Files)
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
        if !mode.contains(TermMode::REPORT_EVENT_TYPES)
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
                if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) =>
            {
                return;
            },
            _ => build_sequence(key, mods, mode),
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

/// Build a key's keyboard escape sequence based on the given `key`, `mods`, and `mode`.
///
/// The key sequences for `APP_KEYPAD` and alike are handled inside the bindings.
#[inline(never)]
fn build_sequence(key: KeyEvent, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
    let mut modifiers = mods.into();

    let kitty_seq = mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES,
    );

    let kitty_encode_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    // The default parameter is 1, so we can omit it.
    let kitty_event_type = mode.contains(TermMode::REPORT_EVENT_TYPES)
        && (key.repeat || key.state == ElementState::Released);

    let context =
        SequenceBuilder { mode, modifiers, kitty_seq, kitty_encode_all, kitty_event_type };

    let associated_text = key.text_with_all_modifiers().filter(|text| {
        mode.contains(TermMode::REPORT_ASSOCIATED_TEXT)
            && key.state != ElementState::Released
            && !text.is_empty()
            && !is_control_character(text)
    });

    let sequence_base = context
        .try_build_numpad(&key)
        .or_else(|| context.try_build_named_kitty(&key))
        .or_else(|| context.try_build_named_normal(&key, associated_text.is_some()))
        .or_else(|| context.try_build_control_char_or_mod(&key, &mut modifiers))
        .or_else(|| context.try_build_textual(&key, associated_text));

    let (payload, terminator) = match sequence_base {
        Some(SequenceBase { payload, terminator }) => (payload, terminator),
        _ => return Vec::new(),
    };

    let mut payload = format!("\x1b[{payload}");

    // Add modifiers information.
    if kitty_event_type || !modifiers.is_empty() || associated_text.is_some() {
        payload.push_str(&format!(";{}", modifiers.encode_esc_sequence()));
    }

    // Push event type.
    if kitty_event_type {
        payload.push(':');
        let event_type = match key.state {
            _ if key.repeat => '2',
            ElementState::Pressed => '1',
            ElementState::Released => '3',
        };
        payload.push(event_type);
    }

    if let Some(text) = associated_text {
        let mut codepoints = text.chars().map(u32::from);
        if let Some(codepoint) = codepoints.next() {
            payload.push_str(&format!(";{codepoint}"));
        }
        for codepoint in codepoints {
            payload.push_str(&format!(":{codepoint}"));
        }
    }

    payload.push(terminator.encode_esc_sequence());

    payload.into_bytes()
}

/// Helper to build escape sequence payloads from [`KeyEvent`].
pub struct SequenceBuilder {
    mode: TermMode,
    /// The emitted sequence should follow the kitty keyboard protocol.
    kitty_seq: bool,
    /// Encode all the keys according to the protocol.
    kitty_encode_all: bool,
    /// Report event types.
    kitty_event_type: bool,
    modifiers: SequenceModifiers,
}

impl SequenceBuilder {
    /// Try building sequence from the event's emitting text.
    fn try_build_textual(
        &self,
        key: &KeyEvent,
        associated_text: Option<&str>,
    ) -> Option<SequenceBase> {
        let character = match key.logical_key.as_ref() {
            Key::Character(character) if self.kitty_seq => character,
            _ => return None,
        };

        if character.chars().count() == 1 {
            let shift = self.modifiers.contains(SequenceModifiers::SHIFT);

            let ch = character.chars().next().unwrap();
            let unshifted_ch = if shift { ch.to_lowercase().next().unwrap() } else { ch };

            let alternate_key_code = u32::from(ch);
            let mut unicode_key_code = u32::from(unshifted_ch);

            // Try to get the base for keys which change based on modifier, like `1` for `!`.
            //
            // However it should only be performed when `SHIFT` is pressed.
            if shift && alternate_key_code == unicode_key_code {
                if let Key::Character(unmodded) = key.key_without_modifiers().as_ref() {
                    unicode_key_code = u32::from(unmodded.chars().next().unwrap_or(unshifted_ch));
                }
            }

            // NOTE: Base layouts are ignored, since winit doesn't expose this information
            // yet.
            let payload = if self.mode.contains(TermMode::REPORT_ALTERNATE_KEYS)
                && alternate_key_code != unicode_key_code
            {
                format!("{unicode_key_code}:{alternate_key_code}")
            } else {
                unicode_key_code.to_string()
            };

            Some(SequenceBase::new(payload.into(), SequenceTerminator::Kitty))
        } else if self.kitty_encode_all && associated_text.is_some() {
            // Fallback when need to report text, but we don't have any key associated with this
            // text.
            Some(SequenceBase::new("0".into(), SequenceTerminator::Kitty))
        } else {
            None
        }
    }

    /// Try building from numpad key.
    ///
    /// `None` is returned when the key is neither known nor numpad.
    fn try_build_numpad(&self, key: &KeyEvent) -> Option<SequenceBase> {
        if !self.kitty_seq || key.location != KeyLocation::Numpad {
            return None;
        }

        let base = match key.logical_key.as_ref() {
            Key::Character("0") => "57399",
            Key::Character("1") => "57400",
            Key::Character("2") => "57401",
            Key::Character("3") => "57402",
            Key::Character("4") => "57403",
            Key::Character("5") => "57404",
            Key::Character("6") => "57405",
            Key::Character("7") => "57406",
            Key::Character("8") => "57407",
            Key::Character("9") => "57408",
            Key::Character(".") => "57409",
            Key::Character("/") => "57410",
            Key::Character("*") => "57411",
            Key::Character("-") => "57412",
            Key::Character("+") => "57413",
            Key::Character("=") => "57415",
            Key::Named(named) => match named {
                NamedKey::Enter => "57414",
                NamedKey::ArrowLeft => "57417",
                NamedKey::ArrowRight => "57418",
                NamedKey::ArrowUp => "57419",
                NamedKey::ArrowDown => "57420",
                NamedKey::PageUp => "57421",
                NamedKey::PageDown => "57422",
                NamedKey::Home => "57423",
                NamedKey::End => "57424",
                NamedKey::Insert => "57425",
                NamedKey::Delete => "57426",
                _ => return None,
            },
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
    }

    /// Try building from [`NamedKey`] using the kitty keyboard protocol encoding
    /// for functional keys.
    fn try_build_named_kitty(&self, key: &KeyEvent) -> Option<SequenceBase> {
        let named = match key.logical_key {
            Key::Named(named) if self.kitty_seq => named,
            _ => return None,
        };

        let (base, terminator) = match named {
            // F3 in kitty protocol diverges from nebula's terminfo.
            NamedKey::F3 => ("13", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("57376", SequenceTerminator::Kitty),
            NamedKey::F14 => ("57377", SequenceTerminator::Kitty),
            NamedKey::F15 => ("57378", SequenceTerminator::Kitty),
            NamedKey::F16 => ("57379", SequenceTerminator::Kitty),
            NamedKey::F17 => ("57380", SequenceTerminator::Kitty),
            NamedKey::F18 => ("57381", SequenceTerminator::Kitty),
            NamedKey::F19 => ("57382", SequenceTerminator::Kitty),
            NamedKey::F20 => ("57383", SequenceTerminator::Kitty),
            NamedKey::F21 => ("57384", SequenceTerminator::Kitty),
            NamedKey::F22 => ("57385", SequenceTerminator::Kitty),
            NamedKey::F23 => ("57386", SequenceTerminator::Kitty),
            NamedKey::F24 => ("57387", SequenceTerminator::Kitty),
            NamedKey::F25 => ("57388", SequenceTerminator::Kitty),
            NamedKey::F26 => ("57389", SequenceTerminator::Kitty),
            NamedKey::F27 => ("57390", SequenceTerminator::Kitty),
            NamedKey::F28 => ("57391", SequenceTerminator::Kitty),
            NamedKey::F29 => ("57392", SequenceTerminator::Kitty),
            NamedKey::F30 => ("57393", SequenceTerminator::Kitty),
            NamedKey::F31 => ("57394", SequenceTerminator::Kitty),
            NamedKey::F32 => ("57395", SequenceTerminator::Kitty),
            NamedKey::F33 => ("57396", SequenceTerminator::Kitty),
            NamedKey::F34 => ("57397", SequenceTerminator::Kitty),
            NamedKey::F35 => ("57398", SequenceTerminator::Kitty),
            NamedKey::ScrollLock => ("57359", SequenceTerminator::Kitty),
            NamedKey::PrintScreen => ("57361", SequenceTerminator::Kitty),
            NamedKey::Pause => ("57362", SequenceTerminator::Kitty),
            NamedKey::ContextMenu => ("57363", SequenceTerminator::Kitty),
            NamedKey::MediaPlay => ("57428", SequenceTerminator::Kitty),
            NamedKey::MediaPause => ("57429", SequenceTerminator::Kitty),
            NamedKey::MediaPlayPause => ("57430", SequenceTerminator::Kitty),
            NamedKey::MediaStop => ("57432", SequenceTerminator::Kitty),
            NamedKey::MediaFastForward => ("57433", SequenceTerminator::Kitty),
            NamedKey::MediaRewind => ("57434", SequenceTerminator::Kitty),
            NamedKey::MediaTrackNext => ("57435", SequenceTerminator::Kitty),
            NamedKey::MediaTrackPrevious => ("57436", SequenceTerminator::Kitty),
            NamedKey::MediaRecord => ("57437", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeDown => ("57438", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeUp => ("57439", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeMute => ("57440", SequenceTerminator::Kitty),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building from [`NamedKey`].
    fn try_build_named_normal(
        &self,
        key: &KeyEvent,
        has_associated_text: bool,
    ) -> Option<SequenceBase> {
        let named = match key.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        // The default parameter is 1, so we can omit it.
        let one_based =
            if self.modifiers.is_empty() && !self.kitty_event_type && !has_associated_text {
                ""
            } else {
                "1"
            };
        let (base, terminator) = match named {
            NamedKey::PageUp => ("5", SequenceTerminator::Normal('~')),
            NamedKey::PageDown => ("6", SequenceTerminator::Normal('~')),
            NamedKey::Insert => ("2", SequenceTerminator::Normal('~')),
            NamedKey::Delete => ("3", SequenceTerminator::Normal('~')),
            NamedKey::Home => (one_based, SequenceTerminator::Normal('H')),
            NamedKey::End => (one_based, SequenceTerminator::Normal('F')),
            NamedKey::ArrowLeft => (one_based, SequenceTerminator::Normal('D')),
            NamedKey::ArrowRight => (one_based, SequenceTerminator::Normal('C')),
            NamedKey::ArrowUp => (one_based, SequenceTerminator::Normal('A')),
            NamedKey::ArrowDown => (one_based, SequenceTerminator::Normal('B')),
            NamedKey::F1 => (one_based, SequenceTerminator::Normal('P')),
            NamedKey::F2 => (one_based, SequenceTerminator::Normal('Q')),
            NamedKey::F3 => (one_based, SequenceTerminator::Normal('R')),
            NamedKey::F4 => (one_based, SequenceTerminator::Normal('S')),
            NamedKey::F5 => ("15", SequenceTerminator::Normal('~')),
            NamedKey::F6 => ("17", SequenceTerminator::Normal('~')),
            NamedKey::F7 => ("18", SequenceTerminator::Normal('~')),
            NamedKey::F8 => ("19", SequenceTerminator::Normal('~')),
            NamedKey::F9 => ("20", SequenceTerminator::Normal('~')),
            NamedKey::F10 => ("21", SequenceTerminator::Normal('~')),
            NamedKey::F11 => ("23", SequenceTerminator::Normal('~')),
            NamedKey::F12 => ("24", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("25", SequenceTerminator::Normal('~')),
            NamedKey::F14 => ("26", SequenceTerminator::Normal('~')),
            NamedKey::F15 => ("28", SequenceTerminator::Normal('~')),
            NamedKey::F16 => ("29", SequenceTerminator::Normal('~')),
            NamedKey::F17 => ("31", SequenceTerminator::Normal('~')),
            NamedKey::F18 => ("32", SequenceTerminator::Normal('~')),
            NamedKey::F19 => ("33", SequenceTerminator::Normal('~')),
            NamedKey::F20 => ("34", SequenceTerminator::Normal('~')),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building escape from control characters (e.g. Enter) and modifiers.
    fn try_build_control_char_or_mod(
        &self,
        key: &KeyEvent,
        mods: &mut SequenceModifiers,
    ) -> Option<SequenceBase> {
        if !self.kitty_encode_all && !self.kitty_seq {
            return None;
        }

        let named = match key.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        let base = match named {
            NamedKey::Tab => "9",
            NamedKey::Enter => "13",
            NamedKey::Escape => "27",
            NamedKey::Space => "32",
            NamedKey::Backspace => "127",
            _ => "",
        };

        // Fail when the key is not a named control character and the active mode prohibits us
        // from encoding modifier keys.
        if !self.kitty_encode_all && base.is_empty() {
            return None;
        }

        let base = match (named, key.location) {
            (NamedKey::Shift, KeyLocation::Left) => "57441",
            (NamedKey::Control, KeyLocation::Left) => "57442",
            (NamedKey::Alt, KeyLocation::Left) => "57443",
            (NamedKey::Super, KeyLocation::Left) => "57444",
            (NamedKey::Hyper, KeyLocation::Left) => "57445",
            (NamedKey::Meta, KeyLocation::Left) => "57446",
            (NamedKey::Shift, _) => "57447",
            (NamedKey::Control, _) => "57448",
            (NamedKey::Alt, _) => "57449",
            (NamedKey::Super, _) => "57450",
            (NamedKey::Hyper, _) => "57451",
            (NamedKey::Meta, _) => "57452",
            (NamedKey::CapsLock, _) => "57358",
            (NamedKey::NumLock, _) => "57360",
            _ => base,
        };

        // NOTE: Kitty's protocol mandates that the modifier state is applied before
        // key press, however winit sends them after the key press, so for modifiers
        // itself apply the state based on keysyms and not the _actual_ modifiers
        // state, which is how kitty is doing so and what is suggested in such case.
        let press = key.state.is_pressed();
        match named {
            NamedKey::Shift => mods.set(SequenceModifiers::SHIFT, press),
            NamedKey::Control => mods.set(SequenceModifiers::CONTROL, press),
            NamedKey::Alt => mods.set(SequenceModifiers::ALT, press),
            NamedKey::Super => mods.set(SequenceModifiers::SUPER, press),
            _ => (),
        }

        if base.is_empty() {
            None
        } else {
            Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
        }
    }
}

pub struct SequenceBase {
    /// The base of the payload, which is the `number` and optionally an alt base from the kitty
    /// spec.
    payload: Cow<'static, str>,
    terminator: SequenceTerminator,
}

impl SequenceBase {
    fn new(payload: Cow<'static, str>, terminator: SequenceTerminator) -> Self {
        Self { payload, terminator }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceTerminator {
    /// The normal key esc sequence terminator defined by xterm/dec.
    Normal(char),
    /// The terminator is for kitty escape sequence.
    Kitty,
}

impl SequenceTerminator {
    fn encode_esc_sequence(self) -> char {
        match self {
            SequenceTerminator::Normal(char) => char,
            SequenceTerminator::Kitty => 'u',
        }
    }
}

bitflags::bitflags! {
    /// The modifiers encoding for escape sequence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SequenceModifiers : u8 {
        const SHIFT   = 0b0000_0001;
        const ALT     = 0b0000_0010;
        const CONTROL = 0b0000_0100;
        const SUPER   = 0b0000_1000;
        // NOTE: Kitty protocol defines additional modifiers to what is present here, like
        // Capslock, but it's not a modifier as per winit.
    }
}

impl SequenceModifiers {
    /// Get the value which should be passed to escape sequence.
    pub fn encode_esc_sequence(self) -> u8 {
        self.bits() + 1
    }
}

impl From<ModifiersState> for SequenceModifiers {
    fn from(mods: ModifiersState) -> Self {
        let mut modifiers = Self::empty();
        modifiers.set(Self::SHIFT, mods.shift_key());
        modifiers.set(Self::ALT, mods.alt_key());
        modifiers.set(Self::CONTROL, mods.control_key());
        modifiers.set(Self::SUPER, mods.super_key());
        modifiers
    }
}

/// Check whether the `text` is `0x7f`, `C0` or `C1` control code.
fn is_control_character(text: &str) -> bool {
    // 0x7f (DEL) is included here since it has a dedicated control code (`^?`) which generally
    // does not match the reported text (`^H`), despite not technically being part of C0 or C1.
    let codepoint = text.bytes().next().unwrap();
    text.len() == 1 && (codepoint < 0x20 || (0x7f..=0x9f).contains(&codepoint))
}
