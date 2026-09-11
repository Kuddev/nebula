//! Registration and restoration of user keyboard overrides.

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleared_shortcut_reaches_terminal_and_can_be_restored_without_restart() {
        use crate::config::Action;
        use gpui::{KeyContext, Keymap, Keystroke};
        let contexts = [
            KeyContext::parse("Root").unwrap(),
            KeyContext::parse(crate::gpui_shell::terminal::KEY_CONTEXT).unwrap(),
        ];
        let input = [Keystroke::parse("ctrl-k").unwrap()];
        let original = custom_workspace_binding("ctrl+k", &Action::ToggleShellPicker).unwrap();
        let clear = workspace_binding_in_context(
            "ctrl+k",
            &Action::ReceiveChar,
            Some(crate::gpui_shell::terminal::KEY_CONTEXT),
        )
        .unwrap();
        let disabled = Keymap::new(vec![original.clone(), clear.clone()]);
        let (bindings, pending) = disabled.bindings_for_input(&input, &contexts);
        assert!(bindings.is_empty() && !pending, "No app action should consume the key");
        let restored = workspace_binding_in_context(
            "ctrl+k",
            &Action::ToggleShellPicker,
            Some(crate::gpui_shell::terminal::KEY_CONTEXT),
        )
        .unwrap();
        let restored = Keymap::new(vec![original, clear, restored]);
        let (bindings, pending) = restored.bindings_for_input(&input, &contexts);
        assert!(!pending);
        assert!(bindings[0].action().as_any().is::<ToggleShellPicker>());
    }
}

pub(super) fn custom_workspace_binding(
    combo: &str,
    action: &crate::config::Action,
) -> Option<KeyBinding> {
    workspace_binding_in_context(combo, action, None)
}

fn workspace_binding_in_context(
    combo: &str,
    action: &crate::config::Action,
    scope: Option<&str>,
) -> Option<KeyBinding> {
    use crate::config::Action;
    let combo = gpui_binding_combo(combo);
    match action {
        Action::ToggleCommandPalette => Some(KeyBinding::new(&combo, ToggleCommandPalette, scope)),
        Action::ToggleShellPicker => Some(KeyBinding::new(&combo, ToggleShellPicker, scope)),
        Action::CreateNewTab => Some(KeyBinding::new(&combo, NewTerminal, scope)),
        Action::CreateNewWindow => Some(KeyBinding::new(&combo, NewWindow, scope)),
        Action::CloseTab => Some(KeyBinding::new(&combo, CloseActiveTerminal, scope)),
        Action::ToggleFilesPanel => Some(KeyBinding::new(&combo, ToggleFileTree, scope)),
        Action::ToggleGitPanel => Some(KeyBinding::new(&combo, ToggleGitPanel, scope)),
        Action::SplitRight => Some(KeyBinding::new(&combo, SplitRight, scope)),
        Action::SplitDown => Some(KeyBinding::new(&combo, SplitDown, scope)),
        Action::ToggleZoom => Some(KeyBinding::new(&combo, ToggleZoom, scope)),
        Action::FocusPaneLeft => Some(KeyBinding::new(&combo, FocusPaneLeft, scope)),
        Action::FocusPaneRight => Some(KeyBinding::new(&combo, FocusPaneRight, scope)),
        Action::FocusPaneUp => Some(KeyBinding::new(&combo, FocusPaneUp, scope)),
        Action::FocusPaneDown => Some(KeyBinding::new(&combo, FocusPaneDown, scope)),
        Action::SelectNextTab => Some(KeyBinding::new(&combo, SelectNextTab, scope)),
        Action::SelectPreviousTab => Some(KeyBinding::new(&combo, SelectPreviousTab, scope)),
        Action::IncreaseFontSize => Some(KeyBinding::new(&combo, IncreaseFontSize, scope)),
        Action::DecreaseFontSize => Some(KeyBinding::new(&combo, DecreaseFontSize, scope)),
        Action::ResetFontSize => Some(KeyBinding::new(&combo, ResetFontSize, scope)),
        Action::Copy => Some(KeyBinding::new(
            &combo,
            CopySelection,
            scope.or(Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        )),
        Action::Paste => Some(KeyBinding::new(
            &combo,
            PasteClipboard,
            scope.or(Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        )),
        Action::ToggleFullscreen => Some(KeyBinding::new(&combo, ToggleFullscreen, scope)),
        Action::OpenQuickJump => Some(KeyBinding::new(&combo, OpenQuickJump, scope)),
        // `none` 禁用键：gpui 的 NoAction 绑定在最高优先级命中时吞掉按键，
        // 与旧壳 keybind=combo:none 的语义一致。
        Action::None | Action::ReceiveChar => Some(KeyBinding::new(&combo, gpui::NoAction, scope)),
        _ => None,
    }
}

impl NebulaWorkspace {
    pub(super) fn apply_custom_keybinds(&mut self, cx: &mut Context<Self>) {
        let raw = nebula_settings::keybind_pairs();
        let applied: Vec<_> = raw.iter().map(|(combo, _)| gpui_binding_combo(combo)).collect();
        let defaults = crate::display::keymap::default_shortcuts();
        let mut bindings = Vec::new();
        for stale in self.custom_keybinds_applied.iter().filter(|combo| !applied.contains(combo)) {
            let restored =
                defaults.iter().rev().find(|(combo, _)| gpui_binding_combo(combo) == *stale);
            if let Some((combo, action)) = restored {
                for scope in [None, Some(crate::gpui_shell::terminal::KEY_CONTEXT)] {
                    if let Some(binding) = workspace_binding_in_context(combo, action, scope) {
                        bindings.push(binding);
                    }
                }
            } else {
                for scope in [None, Some(crate::gpui_shell::terminal::KEY_CONTEXT)] {
                    bindings.push(KeyBinding::new(stale, gpui::NoAction, scope));
                }
            }
        }
        for (combo, action) in raw {
            let Some(action) = crate::display::keymap::parse_action(&action) else { continue };
            if crate::display::keymap::parse_combo(&combo).is_none() {
                continue;
            }
            // Register at both workspace and terminal depth so a cleared default
            // cannot continue to intercept input through a more specific context.
            for scope in [None, Some(crate::gpui_shell::terminal::KEY_CONTEXT)] {
                if let Some(binding) = workspace_binding_in_context(&combo, &action, scope) {
                    bindings.push(binding);
                }
            }
        }
        self.custom_keybinds_applied = applied;
        cx.bind_keys(bindings);
    }
}
