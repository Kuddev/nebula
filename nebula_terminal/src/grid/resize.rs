//! Grid resize and reflow.

use std::cmp::{Ordering, max, min};
use std::mem;

use crate::index::{Boundary, Column, Line};
use crate::term::cell::{Flags, ResetDiscriminant};

use crate::grid::row::Row;
use crate::grid::{Dimensions, Grid, GridCell};

impl<T: GridCell + Default + PartialEq> Grid<T> {
    /// Resize the grid's width and/or height.
    pub fn resize<D>(&mut self, reflow: bool, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.resize_impl(reflow, false, lines, columns);
    }

    /// Resize the grid with the row anchoring used by Windows ConPTY.
    ///
    /// ConPTY silently re-anchors its private screen buffer when its geometry
    /// changes, then keeps emitting absolute CUP coordinates against that new
    /// layout. The primary grid must make the same row decisions or PSReadLine
    /// redraws target a different line after a window resize.
    pub fn resize_conpty<D>(&mut self, reflow: bool, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.resize_impl(reflow, true, lines, columns);
    }

    fn resize_impl<D>(&mut self, reflow: bool, conpty: bool, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Use empty template cell for resetting cells due to resize.
        let template = mem::take(&mut self.cursor.template);

        match self.lines.cmp(&lines) {
            Ordering::Less => self.grow_lines(conpty, lines),
            Ordering::Greater => self.shrink_lines(conpty, lines),
            Ordering::Equal => (),
        }

        // Anchor the cursor to the content *before* reflowing, and put it back
        // afterwards, instead of trusting the running total that `grow_columns` /
        // `shrink_columns` keep as they splice rows.
        //
        // Those two maintain the cursor by incremental bookkeeping
        // (`cursor_line_delta`, `point.column -= columns`, and a couple of
        // branches that `continue` before recording anything). A single step is
        // right, but an interactive drag calls this once per column — around 80
        // times for a 120→40 column drag, and 80 more on the way back — and the
        // error compounds until the cursor sits in the middle of old output.
        //
        // ConPTY `TextBuffer::Reflow` avoids the problem by not keeping a
        // separate tally at all: it rewrites the content cell by cell and reads
        // the cursor back off wherever its cell landed
        // (`newCursorPos = { AdjustToGlyphStart(oldCursorPos.x - oldX + newX), newY }`,
        // textBuffer.cpp:2879). The cursor rides the content. This does the same
        // thing without touching the splice logic: remember which logical line
        // the cursor is on and how far into it, then re-derive the position from
        // the reflowed buffer.
        //
        // Only when the rows actually get spliced, though. Widening with reflow
        // disabled just makes every row wider without moving a single cell, and
        // anchoring through that would *introduce* a jump: the offset was
        // measured in old columns and each row now spans more of them.
        let column_order = self.columns.cmp(&columns);
        let splices_rows = match column_order {
            Ordering::Less => reflow && self.reflow_on_grow,
            Ordering::Greater => reflow,
            Ordering::Equal => false,
        };
        let anchor = if splices_rows { self.cursor_anchor() } else { None };

        match column_order {
            Ordering::Less => self.grow_columns(reflow && self.reflow_on_grow, columns),
            Ordering::Greater => self.shrink_columns(reflow, columns),
            Ordering::Equal => (),
        }

        if let Some(anchor) = anchor {
            self.restore_cursor_anchor(anchor);
        }

        // Restore template cell.
        self.cursor.template = template;
    }

    /// Add lines to the visible area.
    ///
    /// Nebula keeps the cursor at the bottom of the terminal as long as there
    /// is scrollback available. Once scrollback is exhausted, new lines are
    /// simply added to the bottom of the screen.
    ///
    /// ConPTY 增高时不会把先前推出视口的行重新拉回：现有内容和光标保持
    /// 原位，只在底部补空行。这里必须复刻 host 的行坐标，否则下一次
    /// PSReadLine 绝对定位重绘会落到旧输出中间。
    fn grow_lines<D>(&mut self, conpty: bool, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        let lines_added = target - self.lines;

        // Need to resize before updating buffer.
        self.raw.grow_visible_lines(target);
        self.lines = target;

        let history_size = self.history_size();
        let from_history = if conpty { 0 } else { min(history_size, lines_added) };

        // Move existing lines up for every line that couldn't be pulled from history.
        if from_history != lines_added {
            let delta = lines_added - from_history;
            self.scroll_up(&(Line(0)..Line(target as i32)), delta);
        }

        // Move cursor down for every line pulled from history.
        self.saved_cursor.point.line += from_history;
        self.cursor.point.line += from_history;

        self.display_offset = self.display_offset.saturating_sub(lines_added);
        self.decrease_scroll_limit(lines_added);
    }

    /// Remove lines from the visible area.
    ///
    /// Keep the cursor at the bottom of the screen by pushing history
    /// "out the top" of the terminal window.
    ///
    /// ConPTY anchors the shrink to the last written row rather than only the
    /// parser cursor: PSReadLine can leave a continuation hint below its cursor
    /// and will repaint it with absolute CUP coordinates after the resize.
    fn shrink_lines<D>(&mut self, conpty: bool, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        let cursor_line = self.cursor.point.line.0 as usize;

        // Scroll up to keep content inside the window.
        let required_scrolling = self.shrink_scroll_amount(conpty, target);
        if required_scrolling > 0 {
            self.scroll_up(&(Line(0)..Line(self.lines as i32)), required_scrolling);

            // The primary cursor follows its content, but never scrolls above
            // the viewport when the content anchor was below it.
            self.cursor.point.line = min(
                Line((cursor_line - min(required_scrolling, cursor_line)) as i32),
                Line(target as i32 - 1),
            );
        }

        // Clamp saved cursor, since only primary cursor is scrolled into viewport.
        self.saved_cursor.point.line = min(self.saved_cursor.point.line, Line(target as i32 - 1));

        self.raw.rotate((self.lines - target) as isize);
        self.raw.shrink_visible_lines(target);
        self.lines = target;
    }

    /// Viewport rows a shrink to `target` visible lines will scroll out.
    ///
    /// This is exposed to the renderer so a host that defers the actual
    /// resize to a commit point can crop exactly the rows the upcoming
    /// shrink will keep visible, making the settled reflow land without a
    /// visual jump.
    pub(crate) fn shrink_scroll_amount(&self, conpty: bool, target: usize) -> usize {
        let cursor_line = self.cursor.point.line.0 as usize;

        let mut required_scrolling = (cursor_line + 1).saturating_sub(target);
        if conpty {
            let last_written = max(cursor_line, self.bottommost_occupied_line());
            let by_content = (last_written + 1).saturating_sub(target);
            // Never scroll farther than the cursor can follow; ConPTY keeps a
            // cursor near the top visible even if stale content extends below.
            required_scrolling = max(required_scrolling, min(by_content, cursor_line));
        }

        required_scrolling
    }

    /// Bottommost viewport row containing a non-empty cell.
    #[inline]
    fn bottommost_occupied_line(&self) -> usize {
        (0..self.lines).rev().find(|&line| !self.raw[Line(line as i32)].is_clear()).unwrap_or(0)
    }

    /// Grow number of columns in each row, reflowing if necessary.
    fn grow_columns(&mut self, reflow: bool, columns: usize) {
        // Check if a row needs to be wrapped.
        let should_reflow = |row: &Row<T>| -> bool {
            let len = Column(row.len());
            reflow && len.0 > 0 && len < columns && row[len - 1].flags().contains(Flags::WRAPLINE)
        };

        self.columns = columns;

        let mut reversed: Vec<Row<T>> = Vec::with_capacity(self.raw.len());
        let mut cursor_line_delta = 0;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut rows = self.raw.take_all();

        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // Check if reflowing should be performed.
            let last_row = match reversed.last_mut() {
                Some(last_row) if should_reflow(last_row) => last_row,
                _ => {
                    reversed.push(row);
                    continue;
                },
            };

            // Remove wrap flag before appending additional cells.
            if let Some(cell) = last_row.last_mut() {
                cell.flags_mut().remove(Flags::WRAPLINE);
            }

            // Remove leading spacers when reflowing wide char to the previous line.
            let mut last_len = last_row.len();
            if last_len >= 1
                && last_row[Column(last_len - 1)].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                last_row.shrink(last_len - 1);
                last_len -= 1;
            }

            // Don't try to pull more cells from the next line than available.
            let mut num_wrapped = columns - last_len;
            let len = min(row.len(), num_wrapped);

            // Insert leading spacer when there's not enough room for reflowing wide char.
            let mut cells = if row[Column(len - 1)].flags().contains(Flags::WIDE_CHAR) {
                num_wrapped -= 1;

                let mut cells = row.front_split_off(len - 1);

                let mut spacer = T::default();
                spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);
                cells.push(spacer);

                cells
            } else {
                row.front_split_off(len)
            };

            // Add removed cells to previous row and reflow content.
            last_row.append(&mut cells);

            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;

            if i == cursor_buffer_line && reflow {
                // Resize cursor's line and reflow the cursor if necessary.
                let mut target = self.cursor.point.sub(self, Boundary::Cursor, num_wrapped);

                // Clamp to the last column, if no content was reflown with the cursor.
                if target.column.0 == 0 && row.is_clear() {
                    self.cursor.input_needs_wrap = true;
                    target = target.sub(self, Boundary::Cursor, 1);
                }
                self.cursor.point.column = target.column;

                // Get required cursor line changes. Since `num_wrapped` is smaller than `columns`
                // this will always be either `0` or `1`.
                let line_delta = self.cursor.point.line - target.line;

                if row.is_clear() {
                    // The row was merged away in full, so it must go, exactly
                    // like any other emptied continuation row.
                    //
                    // Upstream only drops it when `line_delta != 0`, which never
                    // holds for a cursor on the top visible row: `Point::sub`
                    // clamps at `Line(0)` under `Boundary::Cursor` (index.rs:103),
                    // so the delta comes out `0` and the blank row survives. It
                    // then stands exactly where the content used to be and pushes
                    // that content up a row into the scrollback — which reads as
                    // the whole viewport having scrolled by itself.
                    //
                    // Dropping it unconditionally is safe now that the cursor is
                    // re-derived from `CursorAnchor` after the reflow rather than
                    // from these running deltas.
                    if i < self.display_offset {
                        self.display_offset = self.display_offset.saturating_sub(1);
                    }

                    continue;
                }

                cursor_line_delta += line_delta.0 as usize;
            } else if row.is_clear() {
                if i < self.display_offset {
                    // Since we removed a line, rotate down the viewport.
                    self.display_offset = self.display_offset.saturating_sub(1);
                }

                // Rotate cursor down if content below them was pulled from history.
                if i < cursor_buffer_line {
                    self.cursor.point.line += 1;
                }

                // Don't push line into the new buffer.
                continue;
            }

            if let Some(cell) = last_row.last_mut() {
                // Set wrap flag if next line still has cells.
                cell.flags_mut().insert(Flags::WRAPLINE);
            }

            reversed.push(row);
        }

        // Make sure we have at least the viewport filled.
        if reversed.len() < self.lines {
            let delta = (self.lines - reversed.len()) as i32;
            self.cursor.point.line = max(self.cursor.point.line - delta, Line(0));
            reversed.resize_with(self.lines, || Row::new(columns));
        }

        // Pull content down to put cursor in correct position, or move cursor up if there's no
        // more lines to delete below the cursor.
        if cursor_line_delta != 0 {
            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
            let available = min(cursor_buffer_line, reversed.len() - self.lines);
            let overflow = cursor_line_delta.saturating_sub(available);
            reversed.truncate(reversed.len() + overflow - cursor_line_delta);
            self.cursor.point.line = max(self.cursor.point.line - overflow, Line(0));
        }

        // Reverse iterator and fill all rows that are still too short.
        let mut new_raw = Vec::with_capacity(reversed.len());
        for mut row in reversed.drain(..).rev() {
            if row.len() < columns {
                row.grow(columns);
            }
            new_raw.push(row);
        }

        self.raw.replace_inner(new_raw);

        // Clamp display offset in case lines above it got merged.
        self.display_offset = min(self.display_offset, self.history_size());
    }

    /// Shrink number of columns in each row, reflowing if necessary.
    fn shrink_columns(&mut self, reflow: bool, columns: usize) {
        self.columns = columns;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut new_raw = Vec::with_capacity(self.raw.len());
        let mut buffered: Option<Vec<T>> = None;

        let mut rows = self.raw.take_all();
        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // Append lines left over from the previous row.
            if let Some(buffered) = buffered.take() {
                // Add a column for every cell added before the cursor, if it goes beyond the new
                // width it is then later reflown.
                let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                if i == cursor_buffer_line {
                    self.cursor.point.column += buffered.len();
                }

                row.append_front(buffered);
            }

            loop {
                let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                let minimum = if reflow && i == cursor_buffer_line {
                    // WT's `oldCursorPos.x + 1`: default cells before a cursor
                    // are part of the logical row even though `Row::shrink`
                    // would normally trim them as invisible trailing space.
                    self.cursor.point.column.0 + 1
                } else {
                    0
                };

                // Remove all cells which require reflowing.
                let mut wrapped = match row.shrink_with_minimum(columns, minimum) {
                    Some(wrapped) if reflow => wrapped,
                    _ => {
                        if reflow && i == cursor_buffer_line && self.cursor.point.column >= columns
                        {
                            // If there are empty cells before the cursor, we assume it is explicit
                            // whitespace and need to wrap it like normal content.
                            Vec::new()
                        } else {
                            // Since it fits, just push the existing line without any reflow.
                            new_raw.push(row);
                            break;
                        }
                    },
                };

                // Insert spacer if a wide char would be wrapped into the last column.
                if row.len() >= columns
                    && row[Column(columns - 1)].flags().contains(Flags::WIDE_CHAR)
                {
                    let mut spacer = T::default();
                    spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);

                    let wide_char = mem::replace(&mut row[Column(columns - 1)], spacer);
                    wrapped.insert(0, wide_char);
                }

                // Remove wide char spacer before shrinking.
                let len = wrapped.len();
                if len > 0 && wrapped[len - 1].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    if len == 1 {
                        row[Column(columns - 1)].flags_mut().insert(Flags::WRAPLINE);
                        new_raw.push(row);
                        break;
                    } else {
                        // Remove the leading spacer from the end of the wrapped row.
                        wrapped[len - 2].flags_mut().insert(Flags::WRAPLINE);
                        wrapped.truncate(len - 1);
                    }
                }

                new_raw.push(row);

                // Set line as wrapped if cells got removed.
                if let Some(cell) = new_raw.last_mut().and_then(|r| r.last_mut()) {
                    cell.flags_mut().insert(Flags::WRAPLINE);
                }

                if wrapped
                    .last()
                    .map(|c| c.flags().contains(Flags::WRAPLINE) && i >= 1)
                    .unwrap_or(false)
                    && wrapped.len() < columns
                {
                    // Make sure previous wrap flag doesn't linger around.
                    if let Some(cell) = wrapped.last_mut() {
                        cell.flags_mut().remove(Flags::WRAPLINE);
                    }

                    // Add removed cells to start of next row.
                    buffered = Some(wrapped);
                    break;
                } else {
                    // Reflow cursor if a line below it is deleted.
                    let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                    if (i == cursor_buffer_line && self.cursor.point.column < columns)
                        || i < cursor_buffer_line
                    {
                        self.cursor.point.line = max(self.cursor.point.line - 1, Line(0));
                    }

                    // Reflow the cursor if it is on this line beyond the width.
                    if i == cursor_buffer_line && self.cursor.point.column >= columns {
                        // Since only a single new line is created, we subtract only `columns`
                        // from the cursor instead of reflowing it completely.
                        self.cursor.point.column -= columns;
                    }

                    // Make sure new row is at least as long as new width.
                    let occ = wrapped.len();
                    if occ < columns {
                        wrapped.resize_with(columns, T::default);
                    }
                    row = Row::from_vec(wrapped, occ);

                    if i < self.display_offset {
                        // Since we added a new line, rotate up the viewport.
                        self.display_offset += 1;
                    }
                }
            }
        }

        // Reverse iterator and use it as the new grid storage.
        let mut reversed: Vec<Row<T>> = new_raw.drain(..).rev().collect();
        reversed.truncate(self.max_scroll_limit + self.lines);
        self.raw.replace_inner(reversed);

        // Clamp display offset in case some lines went off.
        self.display_offset = min(self.display_offset, self.history_size());

        // Reflow the primary cursor, or clamp it if reflow is disabled.
        if !reflow {
            self.cursor.point.column = min(self.cursor.point.column, Column(columns - 1));
        } else if self.cursor.point.column == columns
            && !self[self.cursor.point.line][Column(columns - 1)].flags().contains(Flags::WRAPLINE)
        {
            self.cursor.input_needs_wrap = true;
            self.cursor.point.column -= 1;
        } else {
            self.cursor.point = self.cursor.point.grid_clamp(self, Boundary::Cursor);
        }

        // Clamp the saved cursor to the grid.
        self.saved_cursor.point.column = min(self.saved_cursor.point.column, Column(columns - 1));
    }

    /// Whether this row soft-wraps into the one below it.
    ///
    /// The flag lives on the row's *physical* last cell, which is where both
    /// `shrink_columns` writes it and `grow_columns` reads it.
    fn has_wrapline(&self, line: Line) -> bool {
        if line < self.topmost_line() || line > self.bottommost_line() {
            return false;
        }

        let row = &self[line];
        let len = row.len();
        len > 0 && row[Column(len - 1)].flags().contains(Flags::WRAPLINE)
    }

    /// Columns this row contributes to its logical line.
    ///
    /// A trailing [`Flags::LEADING_WIDE_CHAR_SPACER`] is padding inserted because
    /// a wide char would not fit — the char itself lives on the continuation row,
    /// so the spacer holds no content and must not shift the offset.
    fn logical_width(&self, line: Line) -> usize {
        let row = &self[line];
        let len = row.len();
        if len > 0 && row[Column(len - 1)].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            len - 1
        } else {
            len
        }
    }

    /// Record where the cursor sits relative to the *content*, so it can be found
    /// again after the rows underneath it have been split or merged.
    ///
    /// Returns `None` when the cursor is outside the viewport, which leaves the
    /// existing bookkeeping in charge rather than guessing.
    fn cursor_anchor(&self) -> Option<CursorAnchor> {
        let cursor = self.cursor.point;
        if cursor.line < Line(0) || cursor.line > self.bottommost_line() {
            return None;
        }

        // Walk up to the first row of the cursor's logical line, accumulating the
        // columns every row above the cursor contributes. `input_needs_wrap`
        // means "one past the last column", so it counts as one more column.
        let topmost = self.topmost_line();
        let mut start = cursor.line;
        let mut offset = cursor.column.0 + usize::from(self.cursor.input_needs_wrap);
        while start > topmost && self.has_wrapline(start - 1) {
            start -= 1;
            offset += self.logical_width(start);
        }

        // Find the logical line's last row, then count the logical lines below
        // it. Reflow splits and merges rows *within* a logical line and never
        // moves a hard line break, so this count is what survives it — physical
        // row indices do not. Counting from the bottom also makes it immune to
        // scrollback truncation, which only ever drops the oldest rows.
        let bottommost = self.bottommost_line();
        let mut end = cursor.line;
        while end < bottommost && self.has_wrapline(end) {
            end += 1;
        }

        let mut logical_lines_below = 0;
        let mut line = end + 1;
        while line <= bottommost {
            // Every logical line ends in exactly one row without a wrap flag.
            if !self.has_wrapline(line) {
                logical_lines_below += 1;
            }
            line += 1;
        }

        Some(CursorAnchor { logical_lines_below, offset })
    }

    /// Put the cursor back on the cell it was anchored to, overriding whatever
    /// the reflow's own bookkeeping arrived at.
    fn restore_cursor_anchor(&mut self, anchor: CursorAnchor) {
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();

        // Walk up from the bottom of the buffer until `logical_lines_below`
        // logical lines have been passed; the next row up ends the anchored one.
        let mut end = bottommost;
        let mut seen = 0;
        while seen < anchor.logical_lines_below {
            if !self.has_wrapline(end) {
                seen += 1;
            }

            if end == topmost {
                // The anchored line is no longer in the buffer; leave the
                // cursor where the reflow put it rather than inventing a spot.
                return;
            }
            end -= 1;
        }

        // Then back up to that logical line's first row and walk down through it,
        // spending the recorded offset one row at a time.
        let mut line = end;
        while line > topmost && self.has_wrapline(line - 1) {
            line -= 1;
        }

        let mut remaining = anchor.offset;
        while line < end {
            let width = self.logical_width(line);
            if remaining < width {
                break;
            }
            remaining -= width;
            line += 1;
        }

        let last_column = Column(self.columns - 1);
        if remaining > last_column.0 {
            // Offset landed one past the end: the pending-wrap state, which is
            // how a full row keeps the cursor addressable.
            self.cursor.point.column = last_column;
            self.cursor.input_needs_wrap = true;
        } else {
            self.cursor.point.column = Column(remaining);
            self.cursor.input_needs_wrap = false;
        }

        // A logical line long enough to push its own start above the viewport
        // leaves the cursor clamped to the top row, matching what the reflow
        // paths already do in that case.
        self.cursor.point.line = max(line, Line(0));
    }
}

/// Where the cursor sits in the content, in terms that survive a reflow.
///
/// Both fields are measured against the *logical* line — the run of rows joined
/// by [`Flags::WRAPLINE`] — because that is the unit reflow preserves.
struct CursorAnchor {
    /// Logical lines between the cursor's own and the bottom of the buffer.
    logical_lines_below: usize,

    /// Cursor's column offset from the start of its logical line.
    offset: usize,
}
