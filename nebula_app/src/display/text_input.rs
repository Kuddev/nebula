//! Shared select-all editing semantics for Nebula's small self-drawn fields.
//!
//! These fields do not yet expose arbitrary drag selections, but they must
//! still agree on the Windows muscle-memory baseline: Ctrl+A selects the whole
//! buffer, Ctrl+C copies that selection, and typing/paste replaces it.

#[derive(Debug, Clone, Default)]
pub(crate) struct SelectAllState {
    selected: bool,
}

impl SelectAllState {
    pub(crate) fn select(&mut self, text: &str) {
        self.selected = !text.is_empty();
    }

    pub(crate) fn clear(&mut self) {
        self.selected = false;
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
    }

    pub(crate) fn backspace(&mut self, text: &mut String) {
        if self.selected {
            text.clear();
            self.selected = false;
        } else {
            text.pop();
        }
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
