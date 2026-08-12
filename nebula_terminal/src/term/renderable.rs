//! Read-only terminal viewport assembled for the renderer.

use crate::grid::{Dimensions, GridIterator};
use crate::index::{Line, Point};
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
    /// First grid row rendered at viewport line zero.
    pub viewport_origin: Line,
    pub colors: &'a Colors,
    pub mode: TermMode,
}

impl<'a> RenderableContent<'a> {
    pub(super) fn new<T>(term: &'a Term<T>) -> Self {
        Self::with_viewport(term, term.grid().screen_lines(), term.grid().columns())
    }

    pub(super) fn with_viewport<T>(term: &'a Term<T>, lines: usize, columns: usize) -> Self {
        let grid = term.grid();
        let display_offset = grid.display_offset();
        // Crop the committed grid to the rows the deferred resize will keep,
        // so rendering during the drag and the settled reflow agree.
        let viewport_origin = term.viewport_origin_for(lines);

        Self {
            display_iter: grid.display_iter_from(viewport_origin, lines, columns),
            display_offset,
            viewport_origin,
            cursor: RenderableCursor::new(term),
            selection: term.selection.as_ref().and_then(|selection| selection.to_range(term)),
            colors: &term.colors,
            mode: *term.mode(),
        }
    }
}
