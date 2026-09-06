#[derive(Default)]
pub(super) struct CompletionViewport {
    pub offset: usize,
    pub hovered: Option<usize>,
    pub scrollbar_grab: Option<f32>,
    query: Option<String>,
    wheel_remainder: f32,
    pointer: Option<(f32, f32)>,
}

impl CompletionViewport {
    pub fn clear(&mut self) {
        self.offset = 0;
        self.hovered = None;
        self.scrollbar_grab = None;
        self.wheel_remainder = 0.0;
        self.query = None;
    }

    pub fn update_query(&mut self, query: &str, count: usize) {
        if self.query.as_deref() != Some(query) || count == 0 {
            self.clear();
            self.query = Some(query.to_owned());
        }
    }

    pub fn hover(&mut self, position: (f32, f32), index: Option<usize>) -> bool {
        if self.pointer.replace(position) == Some(position) {
            return false;
        }
        let changed = self.hovered != index;
        self.hovered = index;
        changed
    }

    pub fn reveal(&mut self, selected: Option<usize>, count: usize, rows: usize) {
        self.hovered = None;
        let rows = rows.max(1);
        let Some(selected) = selected.filter(|index| *index < count) else { return };
        if selected < self.offset {
            self.offset = selected;
        } else if selected >= self.offset.saturating_add(rows) {
            self.offset = selected + 1 - rows;
        }
        self.offset = self.offset.min(count.saturating_sub(rows));
    }

    pub fn scroll_to(&mut self, offset: usize, count: usize, rows: usize) -> bool {
        let next = offset.min(count.saturating_sub(rows));
        let changed = self.offset != next || self.hovered.is_some();
        self.offset = next;
        self.hovered = None;
        changed
    }

    pub fn drag_scrollbar(
        &mut self,
        pointer_offset: f32,
        travel: f32,
        count: usize,
        rows: usize,
        released: bool,
    ) -> bool {
        let Some(grab) = self.scrollbar_grab else { return false };
        let progress = ((pointer_offset - grab) / travel.max(1.0)).clamp(0.0, 1.0);
        let offset = (progress * count.saturating_sub(rows) as f32).round() as usize;
        self.scroll_to(offset, count, rows);
        if released {
            self.scrollbar_grab = None;
        }
        true
    }

    pub fn scroll(&mut self, delta: f32, row_height: f32, count: usize, rows: usize) -> bool {
        if !delta.is_finite() || !row_height.is_finite() || row_height <= 0.0 {
            return false;
        }
        self.wheel_remainder -= delta;
        let movement = (self.wheel_remainder / row_height).trunc() as isize;
        self.wheel_remainder -= movement as f32 * row_height;
        let current = self.offset.min(count.saturating_sub(rows));
        self.scroll_to(current.saturating_add_signed(movement), count, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_scrolls_the_viewport_in_native_direction_without_wrapping() {
        let mut viewport = CompletionViewport::default();
        assert!(viewport.scroll(-84.0, 28.0, 30, 8));
        assert_eq!(viewport.offset, 3);
        viewport.scroll(28.0, 28.0, 30, 8);
        assert_eq!(viewport.offset, 2);
        viewport.scroll(-10000.0, 28.0, 30, 8);
        assert_eq!(viewport.offset, 22);
        viewport.scroll(10000.0, 28.0, 30, 8);
        assert_eq!(viewport.offset, 0);
    }

    #[test]
    fn precise_wheel_deltas_accumulate_instead_of_selecting_candidates() {
        let mut viewport = CompletionViewport::default();
        viewport.scroll(-10.0, 28.0, 30, 8);
        viewport.scroll(-10.0, 28.0, 30, 8);
        assert_eq!(viewport.offset, 0);
        viewport.scroll(-10.0, 28.0, 30, 8);
        assert_eq!(viewport.offset, 1);
    }

    #[test]
    fn keyboard_navigation_only_scrolls_when_selection_leaves_the_viewport() {
        let mut viewport = CompletionViewport::default();
        viewport.scroll_to(8, 30, 8);
        viewport.reveal(Some(10), 30, 8);
        assert_eq!(viewport.offset, 8);
        viewport.reveal(Some(18), 30, 8);
        assert_eq!(viewport.offset, 11);
        viewport.reveal(Some(2), 30, 8);
        assert_eq!(viewport.offset, 2);
    }

    #[test]
    fn stationary_pointer_cannot_reclaim_hover_after_keyboard_or_wheel_input() {
        let mut viewport = CompletionViewport::default();
        viewport.hover((10.0, 20.0), Some(3));
        viewport.reveal(Some(0), 30, 8);
        assert!(!viewport.hover((10.0, 20.0), Some(3)));
        assert_eq!(viewport.hovered, None);
        viewport.hover((10.0, 21.0), Some(3));
        viewport.scroll(-28.0, 28.0, 30, 8);
        assert!(!viewport.hover((10.0, 21.0), Some(4)));
        assert_eq!(viewport.hovered, None);
    }

    #[test]
    fn query_changes_reset_scroll_but_repainting_the_same_query_does_not() {
        let mut viewport = CompletionViewport::default();
        viewport.update_query("git", 30);
        viewport.scroll_to(10, 30, 8);
        viewport.update_query("git", 30);
        assert_eq!(viewport.offset, 10);
        viewport.update_query("git s", 20);
        assert_eq!(viewport.offset, 0);
        assert_eq!(viewport.hovered, None);
    }

    #[test]
    fn scrollbar_drag_tracks_outside_positions_and_applies_the_final_release() {
        let mut viewport = CompletionViewport::default();
        viewport.scrollbar_grab = Some(7.0);
        assert!(viewport.drag_scrollbar(42.0, 140.0, 30, 8, false));
        assert_eq!(viewport.offset, 6);
        assert!(viewport.drag_scrollbar(-200.0, 140.0, 30, 8, false));
        assert_eq!(viewport.offset, 0);
        assert!(viewport.drag_scrollbar(1000.0, 140.0, 30, 8, true));
        assert_eq!(viewport.offset, 22);
        assert_eq!(viewport.scrollbar_grab, None);
        assert!(!viewport.drag_scrollbar(-200.0, 140.0, 30, 8, false));
        assert_eq!(viewport.offset, 22);
    }

    #[test]
    fn releasing_a_drag_outside_preserves_the_keyboard_accept_target() {
        use crate::display::{NebulaCompletionItem, NebulaCompletionKind, NebulaPaneState};

        let mut state = NebulaPaneState::default();
        state.completion_items = (0..30)
            .map(|index| NebulaCompletionItem {
                label: format!("candidate-{index}"),
                insert: index.to_string(),
                kind: NebulaCompletionKind::Command,
            })
            .collect();
        state.completion_selected = Some(3);
        let mut viewport = CompletionViewport::default();
        viewport.scrollbar_grab = Some(7.0);
        viewport.hovered = Some(4);
        viewport.drag_scrollbar(42.0, 140.0, state.completion_items.len(), 8, false);
        viewport.drag_scrollbar(1000.0, 140.0, state.completion_items.len(), 8, true);

        assert_eq!(viewport.offset, 22);
        assert_eq!(viewport.hovered, None);
        assert_eq!(viewport.scrollbar_grab, None);
        assert_eq!(state.completion_selected, Some(3));
        assert_eq!(super::super::suggest::popup_take(&mut state).as_deref(), Some("3"));
    }

    #[test]
    fn cleared_viewport_cannot_capture_another_panes_drag() {
        let mut viewport = CompletionViewport::default();
        assert!(!viewport.drag_scrollbar(100.0, 140.0, 30, 8, false));
        viewport.scrollbar_grab = Some(7.0);
        viewport.clear();
        assert!(!viewport.drag_scrollbar(100.0, 140.0, 30, 8, true));
        assert_eq!(viewport.offset, 0);
    }
}
