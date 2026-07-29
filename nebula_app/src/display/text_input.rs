//! Shared select-all editing semantics for Nebula's small self-drawn fields.
//!
//! These fields do not yet expose arbitrary drag selections, but they must
//! still agree on the Windows muscle-memory baseline: Ctrl+A selects the whole
//! buffer, Ctrl+C copies that selection, and typing/paste replaces it.
//!
//! 每个改动缓冲区的方法都会给 [`ui::caret`](super::ui::caret) 打一次活动点，
//! 于是光标在打字期间常亮、停手后才开始闪。把打点放在这一层而不是各个调用
//! 方，是因为**漏打点的表现（"某个操作之后光标闪得不对劲"）几乎不会有人
//! 报告**，只能靠让它无法漏来保证。

use super::ui::caret;

#[derive(Debug, Clone, Default)]
pub(crate) struct SelectAllState {
    selected: bool,
}

impl SelectAllState {
    pub(crate) fn select(&mut self, text: &str) {
        self.selected = !text.is_empty();
        caret::note_activity();
    }

    pub(crate) fn clear(&mut self) {
        self.selected = false;
        caret::note_activity();
    }

    pub(crate) fn is_selected(&self) -> bool {
        self.selected
    }

    pub(crate) fn selected_text(&self, text: &str) -> Option<String> {
        self.selected.then(|| text.to_owned())
    }

    pub(crate) fn insert(&mut self, text: &mut String, incoming: &str) {
        if self.selected {
            text.clear();
            self.selected = false;
        }
        text.extend(incoming.chars().filter(|character| !character.is_control()));
        caret::note_activity();
    }

    pub(crate) fn backspace(&mut self, text: &mut String) {
        if self.selected {
            text.clear();
            self.selected = false;
        } else {
            text.pop();
        }
        caret::note_activity();
    }
}

#[cfg(test)]
mod tests {
    use super::SelectAllState;

    #[test]
    fn paste_replaces_a_select_all_buffer() {
        let mut state = SelectAllState::default();
        let mut text = "old".to_owned();
        state.select(&text);
        assert_eq!(state.selected_text(&text).as_deref(), Some("old"));
        state.insert(&mut text, "new\r\n");
        assert_eq!(text, "new");
        assert!(!state.is_selected());
    }

    #[test]
    fn backspace_clears_a_select_all_buffer() {
        let mut state = SelectAllState::default();
        let mut text = "selected".to_owned();
        state.select(&text);
        state.backspace(&mut text);
        assert!(text.is_empty());
        assert!(!state.is_selected());
    }
}
