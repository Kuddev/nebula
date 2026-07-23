//! Read-only terminal viewport assembled for the renderer.

use crate::grid::GridIterator;
use crate::index::Point;
use crate::selection::SelectionRange;
use crate::term::cell::{Cell, Flags};
use crate::term::color::Colors;
use crate::term::{Term, TermMode};
use crate::vte::ansi::CursorShape;

/// Terminal cursor rendering information.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct RenderableCursor {
    pub shape: CursorShape,
    pub point: Point,
}

impl RenderableCursor {
    fn new<T>(term: &Term<T>) -> Self {
        let vi_mode = term.mode().contains(TermMode::VI);
        let mut point = if vi_mode { term.vi_mode_cursor.point } else { term.grid.cursor.point };
        if term.grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
            point.column -= 1;
        }

        let shape = if !vi_mode && !term.mode().contains(TermMode::SHOW_CURSOR) {
            CursorShape::Hidden
        } else {
            term.cursor_style().shape
        };

        Self { shape, point }
    }
}

/// All content required to render the current terminal view.
pub struct RenderableContent<'a> {
    pub display_iter: GridIterator<'a, Cell>,
    pub selection: Option<SelectionRange>,
    pub cursor: RenderableCursor,
    pub display_offset: usize,
    pub colors: &'a Colors,
    pub mode: TermMode,
}

impl<'a> RenderableContent<'a> {
    pub(super) fn new<T>(term: &'a Term<T>) -> Self {
        Self {
            display_iter: term.grid().display_iter(),
            display_offset: term.grid().display_offset(),
            cursor: RenderableCursor::new(term),
            selection: term.selection.as_ref().and_then(|selection| selection.to_range(term)),
            colors: &term.colors,
            mode: *term.mode(),
        }
    }
}
