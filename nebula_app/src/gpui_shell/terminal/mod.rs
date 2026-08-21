//! 终端垂直切片：nebula_terminal（PTY/网格）× GPUI（渲染/输入/IME）。
//!
//! 边界：本模块之外不出现 nebula_terminal 类型；nebula_terminal 之内不出现
//! GPUI 类型。会话生命周期归 `session`，绘制归 `element`，交互归 `view`。

pub mod colors;
pub mod element;
pub mod keymap;
pub mod math_overlay;
pub mod mouse_protocol;
mod osc_links;
pub mod session;
mod ssh_connect_overlay;
pub mod suggest;
pub mod view;

use gpui::{App, KeyBinding};

gpui::actions!(nebula_terminal, [TerminalTab, TerminalBackTab]);

pub(super) const KEY_CONTEXT: &str = "NebulaTerminal";

/// 终端必须先于组件库 Root 接住 Tab；否则同一次按键在补齐写入后还会触发
/// `focus_next`，表现为光标停闪、终端失焦。设置页不在此 context 内，仍保留
/// 原生的 Tab/Shift-Tab 焦点遍历。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", TerminalTab, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", TerminalBackTab, Some(KEY_CONTEXT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::{KEY_CONTEXT, TerminalTab};
    use gpui::{KeyBinding, KeyContext, Keymap, Keystroke};

    gpui::actions!(test_root, [RootTab]);

    #[test]
    fn terminal_tab_binding_shadows_root_focus_traversal() {
        let keymap = Keymap::new(vec![
            KeyBinding::new("tab", RootTab, Some("Root")),
            KeyBinding::new("tab", TerminalTab, Some(KEY_CONTEXT)),
        ]);
        let contexts =
            [KeyContext::parse("Root").unwrap(), KeyContext::parse(KEY_CONTEXT).unwrap()];
        let input = [Keystroke::parse("tab").unwrap()];

        let (bindings, pending) = keymap.bindings_for_input(&input, &contexts);

        assert!(!pending);
        assert!(bindings[0].action().as_any().is::<TerminalTab>());
    }
}
