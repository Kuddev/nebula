//! Terminal-grid recognition and bounded delimiter searches.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TextGrid {
    pub(super) rows: Vec<Vec<Option<char>>>,
    pub(super) wrapped: Vec<bool>,
    pub(super) columns: usize,
    pub(super) absolute_top: usize,
    pub(super) scrolled_out: usize,
}

impl TextGrid {
    pub(super) fn from_term<T>(terminal: &Term<T>, size: &SizeInfo) -> Self {
        Self::from_term_with_lookback(terminal, size, 0)
    }

    pub(super) fn from_term_with_lookback<T>(
        terminal: &Term<T>,
        size: &SizeInfo,
        requested_lookback: usize,
    ) -> Self {
        let grid = terminal.grid();
        // Grid and PTY resizes are committed together while display geometry
        // updates every drag tick, so the visual viewport can differ from the
        // grid. Anchor the scan to the renderer's viewport origin and only
        // cover their shared rectangle, keeping overlay coordinates in the
        // same space as the rendered cells they are matched against.
        let origin = terminal.viewport_origin_for(size.screen_lines());
        let columns = size.columns().min(grid.columns());
        let screen_lines = size
            .screen_lines()
            .min(((grid.bottommost_line() - origin).0.max(0) as usize).saturating_add(1));
        let scrolled_out = grid.scrolled_out();
        let history_size = grid.history_size();
        let available_lookback = (history_size as i64 + origin.0 as i64).max(0) as usize;
        let lookback = requested_lookback.min(available_lookback);
        let absolute_top = (scrolled_out as i64 + history_size as i64 + origin.0 as i64
            - lookback as i64)
            .max(0) as usize;
        let row_count = screen_lines.saturating_add(lookback);
        let mut rows = Vec::with_capacity(row_count);
        let mut wrapped = Vec::with_capacity(row_count);

        for row in 0..row_count {
            let line = origin + (row as i32 - lookback as i32);
            let mut cells = Vec::with_capacity(columns);
            for column in 0..columns {
                let cell = &grid[line][Column(column)];
                let spacer = cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
                cells.push((!spacer).then_some(cell.c));
            }
            let is_wrapped =
                columns > 0 && grid[line][Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            rows.push(cells);
            wrapped.push(is_wrapped);
        }

        Self { rows, wrapped, columns, absolute_top, scrolled_out }
    }

    #[cfg(test)]
    pub(super) fn from_rows(rows: &[&str]) -> Self {
        let columns = rows.iter().map(|row| row.chars().count()).max().unwrap_or(0);
        let rows = rows
            .iter()
            .map(|row| {
                let mut cells: Vec<_> = row.chars().map(Some).collect();
                cells.resize(columns, Some(' '));
                cells
            })
            .collect::<Vec<_>>();
        let wrapped = vec![false; rows.len()];
        Self { rows, wrapped, columns, absolute_top: 0, scrolled_out: 0 }
    }

    pub(super) fn character(&self, position: GridPosition) -> Option<char> {
        self.rows.get(position.row)?.get(position.column).copied().flatten()
    }

    /// Physical rows joined by terminal wrap flags form one editable logical
    /// line. Suppressing only the cursor cell lets an earlier formula on the
    /// same prompt line render while the user is still typing after it.
    pub(super) fn logical_rows_containing(
        &self,
        row: usize,
    ) -> Option<std::ops::RangeInclusive<usize>> {
        if row >= self.rows.len() {
            return None;
        }
        let mut start = row;
        while start > 0 && self.wrapped.get(start - 1).copied().unwrap_or(false) {
            start -= 1;
        }
        let mut end = row;
        while end + 1 < self.rows.len() && self.wrapped.get(end).copied().unwrap_or(false) {
            end += 1;
        }
        Some(start..=end)
    }

    pub(super) fn starts_with(&self, position: GridPosition, delimiter: &[char]) -> bool {
        delimiter.iter().enumerate().all(|(offset, expected)| {
            self.character(GridPosition { row: position.row, column: position.column + offset })
                == Some(*expected)
        })
    }

    pub(super) fn is_escaped(&self, position: GridPosition) -> bool {
        let mut column = position.column;
        let mut slashes = 0usize;
        while column > 0
            && self.character(GridPosition { row: position.row, column: column - 1 }) == Some('\\')
        {
            slashes += 1;
            column -= 1;
        }
        slashes % 2 == 1
    }

    pub(super) fn after(&self, position: GridPosition, width: usize) -> GridPosition {
        GridPosition { row: position.row, column: position.column + width }
    }

    pub(super) fn next(&self, position: GridPosition) -> Option<GridPosition> {
        if position.column + 1 < self.columns {
            Some(GridPosition { row: position.row, column: position.column + 1 })
        } else if position.row + 1 < self.rows.len() {
            Some(GridPosition { row: position.row + 1, column: 0 })
        } else {
            None
        }
    }

    pub(super) fn previous(&self, position: GridPosition) -> Option<GridPosition> {
        if position.column > 0 {
            Some(GridPosition { row: position.row, column: position.column - 1 })
        } else if position.row > 0 {
            Some(GridPosition { row: position.row - 1, column: self.columns.saturating_sub(1) })
        } else {
            None
        }
    }

    /// Closing search for **display** delimiters: they own the rows between
    /// them, so a real newline is part of the formula. Agent TUIs that hard-wrap
    /// a block formula across rows are recovered by this path.
    ///
    /// A **blank row ends the search**. TeX forbids a paragraph break inside
    /// math mode and Markdown ends a math block at a blank line, so no real
    /// formula spans one — while `$$` is its own closer, so the scanner cannot
    /// otherwise tell an opener from a closer whose partner has scrolled off the
    /// top of the viewport. Without this barrier that orphan closer pairs with
    /// the *next* block's delimiter and swallows every paragraph and formula in
    /// between into one giant candidate, which then compiles (prose renders as
    /// math identifiers) and covers the whole region with one image. Stopping at
    /// the blank row leaves it unmatched instead, which is what hands it to the
    /// history reconstruction in [`TerminalMathState::scan_visible_grid`].
    ///
    /// One blank row is **not** a paragraph break, though: the *TUI's* one. The
    /// agent CLIs run the answer through a Markdown renderer that does not know
    /// `$$`, and two of its rules split a display block with a blank row:
    ///
    /// * Claude Code (09-03 screenshot, see
    ///   `claude_code_display_blocks_from_screenshot_pair_and_compile`): a row
    ///   holding only `=` is a setext underline, so the row above becomes a
    ///   heading, the `=` disappears and a blank row is emitted in its place.
    /// * Codex (`codex-rs/tui/src/markdown_render.rs`, verified in source, not
    ///   observed): a formula row starting with `- ` / `+ ` / `* ` interrupts
    ///   the paragraph as a list item (pulldown-cmark `firstpass.rs`,
    ///   `scan_paragraph_interrupt_no_table`), `start_list` pushes a blank
    ///   line in front of it and `start_item` renders the marker as `- `
    ///   with the rest of the block, closer included, indented two cells.
    ///
    /// `open` is the opener's position; when it stands alone on its row the
    /// search may step over such a gap, under four conditions that together
    /// keep the orphan-closer case above on the pending path:
    ///
    /// 1. the block already has content above the gap — an orphan closer is
    ///    followed by its blank row immediately;
    /// 2. the gap is a single row — two blank rows are a real section break;
    /// 3. the row after the gap carries math evidence of its own — prose does
    ///    not, so the swallowed-paragraph shape stops right there;
    /// 4. past a gap the closer must stand alone on its row, and lie within
    ///    [`DISPLAY_BLOCK_GAP_SEARCH_ROWS`] of the opener.
    pub(super) fn find_closing(
        &self,
        open: GridPosition,
        mut position: GridPosition,
        delimiter: &[char],
    ) -> Option<GridPosition> {
        let standalone_opener =
            self.span_is_blank(open.row, position.column, self.columns) && open.row == position.row;
        let last_row = open.row.saturating_add(DISPLAY_BLOCK_GAP_SEARCH_ROWS);
        let mut content_rows = 0usize;
        let mut bridged = false;
        // 上一行是刚跨过的空行：这一行要先自证是公式，搜索才继续。
        let mut gap_pending = false;
        loop {
            if position.row >= self.rows.len() {
                return None;
            }
            if self.starts_with(position, delimiter)
                && !self.is_escaped(position)
                && (!bridged || self.delimiter_owns_row(position, delimiter.len()))
            {
                return Some(position);
            }
            let previous_row = position.row;
            position = self.next(position)?;
            if position.row == previous_row {
                continue;
            }
            if self.span_is_blank(position.row, 0, self.columns) {
                if !standalone_opener || content_rows == 0 || gap_pending {
                    return None;
                }
                gap_pending = true;
                bridged = true;
                continue;
            }
            content_rows += 1;
            if bridged && position.row > last_row {
                return None;
            }
            if gap_pending {
                gap_pending = false;
                if !self.row_is_only(position.row, delimiter)
                    && !self.row_has_math_evidence(position.row)
                {
                    return None;
                }
            }
        }
    }

    /// Whether the delimiter at `position` is the only ink on its row.
    pub(super) fn delimiter_owns_row(&self, position: GridPosition, length: usize) -> bool {
        self.span_is_blank(position.row, 0, position.column)
            && self.span_is_blank(position.row, position.column + length, self.columns)
    }

    pub(super) fn row_text(&self, row: usize) -> String {
        self.rows.get(row).into_iter().flat_map(|cells| cells.iter().flatten()).collect()
    }

    /// Whether `row` holds nothing but `delimiter`.
    pub(super) fn row_is_only(&self, row: usize, delimiter: &[char]) -> bool {
        self.row_text(row).trim().chars().eq(delimiter.iter().copied())
    }

    /// Row-level version of the source test, for rows a bridged search has to
    /// vouch for on their own. In the Codex shape the row after the gap is a
    /// list item in the renderer's eyes, so its leading sign is also read as
    /// the list marker it became: `- 2xy` has no evidence as a whole, but a
    /// compact operand behind the marker is one, while `- 修复 SSH 重连` (a
    /// real list item) is not.
    pub(super) fn row_has_math_evidence(&self, row: usize) -> bool {
        let text = self.row_text(row);
        let text = text.trim();
        if standard_formula_source(text, true) {
            return true;
        }
        let body = text.trim_start_matches(['-', '+', '*']).trim_start();
        body.len() != text.len()
            && (implicit_product_operand(body) || standard_formula_source(body, true))
    }

    /// Closing search for **inline** delimiters. Inline TeX may cross a
    /// terminal's physical row only when that row was produced by a soft wrap:
    /// a real newline still ends the span, so two separate terminal lines can
    /// never accidentally become one formula.
    pub(super) fn find_closing_soft_wrap(
        &self,
        mut position: GridPosition,
        delimiter: &[char],
    ) -> Option<GridPosition> {
        loop {
            if position.row >= self.rows.len() {
                return None;
            }
            if self.starts_with(position, delimiter) && !self.is_escaped(position) {
                return Some(position);
            }
            let previous_row = position.row;
            position = self.next(position)?;
            if position.row != previous_row && !self.wrapped[previous_row] {
                return None;
            }
        }
    }

    /// Closing search with an explicit row budget **and** a frame-wide cell
    /// budget, for openers that carry no intent of their own. A bare `(` is the
    /// densest character in ordinary terminal output (code, logs, JSON), and an
    /// unclosed one would otherwise scan to the end of the grid — once per `(`,
    /// every frame. Measured on a 50×200 grid of unclosed parens that is a 165×
    /// regression against the same grid of plain text, so the budget is not
    /// optional. `spent` is shared by every bare candidate in the frame.
    pub(super) fn find_closing_bounded(
        &self,
        mut position: GridPosition,
        rows: usize,
        spent: &mut usize,
        mut matcher: impl FnMut(&Self, GridPosition) -> Option<bool>,
    ) -> Option<GridPosition> {
        let last_row = position.row.saturating_add(rows);
        loop {
            if position.row >= self.rows.len() || position.row > last_row || *spent == 0 {
                return None;
            }
            *spent -= 1;
            if matcher(self, position)? {
                return Some(position);
            }
            position = self.next(position)?;
        }
    }

    pub(super) fn extract(&self, start: GridPosition, end: GridPosition) -> Option<Box<str>> {
        let mut output = String::new();
        let mut position = start;
        while position < end {
            if let Some(character) = self.character(position) {
                output.push(character);
                if output.len() > DEFAULT_LIMITS.max_source_bytes {
                    return None;
                }
            }

            let previous_row = position.row;
            position = self.next(position)?;
            if position.row != previous_row && !self.wrapped[previous_row] {
                output.push('\n');
            }
        }
        let trimmed = output.trim();
        (!trimmed.is_empty()).then(|| Box::<str>::from(trimmed))
    }

    pub(super) fn span_fingerprint(
        &self,
        row: usize,
        start: usize,
        end: usize,
        include_wrap: bool,
    ) -> Option<u64> {
        let cells = self.rows.get(row)?.get(start..end)?;
        let mut hasher = DefaultHasher::new();
        start.hash(&mut hasher);
        end.hash(&mut hasher);
        cells.hash(&mut hasher);
        if include_wrap {
            self.wrapped.get(row)?.hash(&mut hasher);
        }
        Some(hasher.finish())
    }

    /// Whether a span currently holds nothing but blanks. TUI redraws clear a
    /// line before repainting it, so a blank span is treated as a transient
    /// state rather than proof that a persisted formula is gone.
    pub(super) fn span_is_blank(&self, row: usize, start: usize, end: usize) -> bool {
        self.rows.get(row).and_then(|cells| cells.get(start..end)).is_some_and(|cells| {
            cells.iter().all(|cell| cell.is_none_or(|character| character == ' '))
        })
    }

    /// Columns between the first and last non-blank cell of `row`, or `None`
    /// for a blank row. Interior gaps are counted as occupied: a formula that
    /// squeezes its ink between two words of the row above would read as an
    /// overlap even where the cells are technically empty.
    pub(super) fn row_ink_columns(&self, row: usize) -> Option<(usize, usize)> {
        let cells = self.rows.get(row)?;
        let occupied = |cell: &Option<char>| cell.is_some_and(|character| character != ' ');
        let start = cells.iter().position(occupied)?;
        let end = cells.iter().rposition(occupied)? + 1;
        Some((start, end))
    }

    pub(super) fn spans(&self, start: GridPosition, end: GridPosition) -> Vec<RowSpan> {
        if start.row == end.row {
            return vec![RowSpan { row: start.row as i32, start: start.column, end: end.column }];
        }

        let mut spans = Vec::with_capacity(end.row - start.row + 1);
        spans.push(RowSpan { row: start.row as i32, start: start.column, end: self.columns });
        for row in start.row + 1..end.row {
            spans.push(RowSpan { row: row as i32, start: 0, end: self.columns });
        }
        spans.push(RowSpan { row: end.row as i32, start: 0, end: end.column });
        spans
    }
}

impl PartialOrd for GridPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GridPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.row, self.column).cmp(&(other.row, other.column))
    }
}

/// Scan the visible grid and attach renderer-resolved colors/fallback cells.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_visible<T>(
    state: &mut TerminalMathState,
    terminal: &Term<T>,
    size: &SizeInfo,
    rendered_cells: &[RenderableCell],
    filter_reasoning_style: bool,
    cursor: Option<Point<usize>>,
    default_foreground: Rgb,
) -> Vec<FormulaOverlay> {
    let grid = TextGrid::from_term(terminal, size);
    let active_edit_rows = cursor.and_then(|cursor| grid.logical_rows_containing(cursor.line));
    state.synchronize_grid(&grid);
    if let Some(anchor) = state.scan_visible_grid(&grid, active_edit_rows.as_ref()) {
        // `extract` counts every terminal cell toward the 16 KiB parser limit.
        // Deriving the row cap from the current width keeps this rare temporary
        // grid near 64 KiB of cell data instead of scanning all scrollback.
        let source_rows = DEFAULT_LIMITS
            .max_source_bytes
            .div_ceil(grid.columns.max(1))
            .saturating_add(4)
            .min(MAX_HISTORY_FORMULA_ROWS);
        // A viewport anchor means the pending delimiter is an orphan closer
        // whose opener sits somewhere above; its depth is unknown, so look
        // back by the full source budget instead of an exact distance.
        let lookback = if anchor.row >= grid.absolute_top {
            source_rows
        } else {
            grid.absolute_top - anchor.row
        };
        if lookback <= source_rows {
            let history = TextGrid::from_term_with_lookback(terminal, size, lookback);
            state.complete_pending_from_history(&history);
        } else {
            state.pending_display = None;
        }
    }

    let mut overlays: Vec<_> = state
        .visible_overlays(&grid)
        .into_iter()
        .filter(|overlay| {
            active_edit_rows.as_ref().is_none_or(|rows| !overlay.intersects_rows(rows))
        })
        .filter_map(|mut overlay| {
            overlay.fallback = rendered_cells
                .iter()
                .filter(|cell| overlay.contains(cell.point))
                .cloned()
                .collect();

            // A non-default background usually belongs to a code block or a
            // TUI-owned surface. Painting a formula over it would require the
            // renderer to understand that application's layout, so the common
            // terminal path deliberately leaves the source untouched.
            let has_ansi_background = overlay.fallback.iter().any(|cell| cell.bg_alpha > 0.0);
            if has_ansi_background {
                return None;
            }

            overlay.foreground = overlay
                .fallback
                .iter()
                .find(|cell| !cell.character.is_whitespace())
                .map_or(default_foreground, |cell| cell.fg);

            // Agent TUIs deliberately render chain-of-thought with a dim or
            // opacity-reduced foreground, while their final answer uses the
            // normal terminal foreground. Keep reasoning formulas literal:
            // replacing them with a polished overlay makes internal work look
            // like part of the answer. Restrict the heuristic to full-screen
            // TUI callers so a user-coloured formula in an ordinary shell is
            // never classified as reasoning.
            if filter_reasoning_style && formula_uses_reasoning_style(&overlay) {
                return None;
            }

            apply_layout_hints(&mut overlay, &grid);
            Some(overlay)
        })
        .collect();
    mark_formula_neighbours(&mut overlays);
    overlays
}

/// Whether the source glyphs carry the presentation used by agent reasoning.
///
/// Codex and Claude Code mark muted reasoning with SGR DIM. Requiring three
/// quarters of the source glyphs to agree avoids classifying a final answer as
/// reasoning because one token happens to be dimmed.
pub(super) fn formula_uses_reasoning_style(overlay: &FormulaOverlay) -> bool {
    let (source_cells, dim_cells) = overlay
        .fallback
        .iter()
        .filter(|cell| !cell.character.is_whitespace())
        .fold((0usize, 0usize), |(total, dim), cell| {
            (total + 1, dim + usize::from(cell.flags.contains(Flags::DIM)))
        });
    source_cells > 0 && dim_cells * 4 >= source_cells * 3
}

/// Room the surrounding grid can lend an overlay, decided purely by what its
/// neighbours hold. A formula's rendered size must not depend on how many rows
/// and columns its source happened to occupy, so `$$x$$` on one line and the
/// same source spread over three lines end up with the same budget whenever
/// the blank space around them allows it. `scan_visible` and the tests share
/// this one decision so the numbers behind fit and clip always agree.
pub(super) fn apply_layout_hints(overlay: &mut FormulaOverlay, grid: &TextGrid) {
    // Record what the neighbouring rows hold; `prepare_overlays` decides how
    // much of it is actually in the way once it knows the rendered width.
    let (Some(first), Some(last)) = (overlay.spans.first(), overlay.spans.last()) else {
        return;
    };
    overlay.neighbours_above = neighbour_rows(grid, first.row, -1, overlay.display);
    overlay.neighbours_below = neighbour_rows(grid, last.row, 1, overlay.display);

    // Rows that hold nothing right of the source span let a display formula
    // lay out across the rest of the line, so its rendered size no longer
    // depends on how many columns the source happened to span. Rows scrolled
    // out of the viewport cannot be inspected; treating them as blank is safe
    // because only visible rows can show a horizontal collision.
    let source_right = overlay.spans.iter().map(|span| span.end).max().unwrap_or(0);
    overlay.widen_right_to = (overlay.display
        && overlay.spans.iter().all(|span| match usize::try_from(span.row) {
            Ok(row) if row < grid.rows.len() => grid.span_is_blank(row, span.end, grid.columns),
            _ => true,
        }))
    .then_some(grid.columns.max(source_right));
}

/// Mark rows occupied by a *different* formula after all visible overlays have
/// been collected. The raw terminal row only tells us that text exists; this
/// second pass preserves the distinction needed by vertical clipping.
pub(super) fn mark_formula_neighbours(overlays: &mut [FormulaOverlay]) {
    let spans: Vec<Vec<(i32, usize, usize)>> = overlays
        .iter()
        .map(|overlay| overlay.spans.iter().map(|span| (span.row, span.start, span.end)).collect())
        .collect();
    for (index, overlay) in overlays.iter_mut().enumerate() {
        let Some(first) = overlay.spans.first() else { continue };
        let Some(last) = overlay.spans.last() else { continue };
        let source_start = overlay.spans.iter().map(|span| span.start).min().unwrap_or(first.start);
        let source_end = overlay.spans.iter().map(|span| span.end).max().unwrap_or(last.end);

        for distance in 0..MAX_ABSORBED_BLANK_ROWS {
            let distance = (distance + 1) as i32;
            let above = first.row - distance;
            let below = last.row + distance;
            overlay.formula_neighbours_above[distance as usize - 1] =
                spans.iter().enumerate().any(|(other_index, other_spans)| {
                    other_index != index
                        && other_spans.iter().any(|&(row, start, end)| {
                            row == above && start < source_end && end > source_start
                        })
                });
            overlay.formula_neighbours_below[distance as usize - 1] =
                spans.iter().enumerate().any(|(other_index, other_spans)| {
                    other_index != index
                        && other_spans.iter().any(|&(row, start, end)| {
                            row == below && start < source_end && end > source_start
                        })
                });
        }
    }
}

/// Ink columns of the [`MAX_ABSORBED_BLANK_ROWS`] rows starting one step from
/// `from`, nearest first. Rows outside the viewport read as free for display
/// math — the clip stops at the viewport edge, so the extra budget cannot
/// reach anything — and as occupied for inline math, which would otherwise
/// lift itself off the prose baseline it shares a row with.
pub(super) fn neighbour_rows(
    grid: &TextGrid,
    from: i32,
    step: i32,
    display: bool,
) -> [Option<(usize, usize)>; MAX_ABSORBED_BLANK_ROWS] {
    std::array::from_fn(|index| {
        let row = from + step * (index as i32 + 1);
        match usize::try_from(row) {
            Ok(row) if row < grid.rows.len() => grid.row_ink_columns(row),
            _ => (!display).then_some((0, grid.columns)),
        }
    })
}

#[cfg(test)]
pub(super) fn scan_grid(grid: &TextGrid) -> Vec<FormulaOverlay> {
    scan_grid_result(grid).overlays
}

/// Test-only mirror of the `scan_visible` tail: scan, then let the neighbours
/// hand out the same layout budget the real pipeline would.
#[cfg(test)]
pub(super) fn scan_grid_with_hints(grid: &TextGrid) -> Vec<FormulaOverlay> {
    let mut overlays = scan_grid(grid);
    for overlay in &mut overlays {
        apply_layout_hints(overlay, grid);
    }
    mark_formula_neighbours(&mut overlays);
    overlays
}

#[derive(Clone)]
pub(super) struct GridScanResult {
    pub(super) overlays: Vec<FormulaOverlay>,
    pub(super) unmatched_display: Option<(GridPosition, DisplayDelimiterKind)>,
}

pub(super) fn scan_grid_result(grid: &TextGrid) -> GridScanResult {
    let mut overlays = Vec::new();
    let mut unmatched_display = None;
    let mut position = GridPosition { row: 0, column: 0 };
    // 只有裸定界符消费它：其余四类各自有 O(1) 前置闸。
    let mut bare_budget = BARE_SEARCH_CELL_BUDGET;

    while position.row < grid.rows.len() && overlays.len() < MAX_VISIBLE_FORMULAS {
        let (candidate, incomplete_display) =
            if grid.starts_with(position, &['$', '$']) && !grid.is_escaped(position) {
                let candidate = find_formula(
                    grid,
                    position,
                    &['$', '$'],
                    &['$', '$'],
                    DelimiterKind::DollarDisplay,
                );
                (candidate, Some(DisplayDelimiterKind::Dollars))
            } else if grid.starts_with(position, &['\\', '[']) && !grid.is_escaped(position) {
                let candidate = find_formula(
                    grid,
                    position,
                    &['\\', '['],
                    &['\\', ']'],
                    DelimiterKind::BracketDisplay,
                );
                (candidate, Some(DisplayDelimiterKind::Brackets))
            } else if grid.starts_with(position, &['\\', ']']) && !grid.is_escaped(position) {
                (None, Some(DisplayDelimiterKind::Brackets))
            } else if grid.starts_with(position, &['\\', '(']) && !grid.is_escaped(position) {
                (
                    find_formula(
                        grid,
                        position,
                        &['\\', '('],
                        &['\\', ')'],
                        DelimiterKind::Parenthesized,
                    ),
                    None,
                )
            } else if grid.character(position) == Some('[') && !grid.is_escaped(position) {
                (find_bare_bracket_formula(grid, position, &mut bare_budget), None)
            } else if grid.character(position) == Some('(') && !grid.is_escaped(position) {
                (find_bare_paren_formula(grid, position, &mut bare_budget), None)
            } else if grid.character(position) == Some('$') && !grid.is_escaped(position) {
                (find_dollar_formula(grid, position), None)
            } else {
                (None, None)
            };

        if let Some((overlay, after)) = candidate {
            overlays.push(overlay);
            position = after;
        } else if let Some(next) = grid.next(position) {
            if let Some(kind) = incomplete_display {
                unmatched_display = Some((position, kind));
            }
            position = next;
        } else {
            if let Some(kind) = incomplete_display {
                unmatched_display = Some((position, kind));
            }
            break;
        }
    }

    GridScanResult { overlays, unmatched_display }
}

pub(super) fn find_formula(
    grid: &TextGrid,
    open: GridPosition,
    opening: &[char],
    closing: &[char],
    kind: DelimiterKind,
) -> Option<(FormulaOverlay, GridPosition)> {
    let source_start = grid.after(open, opening.len());
    // A display delimiter owns the rows between it and its closer, so it may
    // cross a real newline — but only when it *opens* its row the way a block
    // delimiter does. A `$$` reached in the middle of a line of prose is being
    // quoted, not opened: an agent echoing back a question about `$$`, or a
    // sentence that names the delimiter. Letting that one search across real
    // newlines makes it pair with the *opening* delimiter of the next real
    // block and swallow every formula in between — the scan resumes after the
    // match (see `scan_grid_result`), so those rows are never even considered.
    // Inline delimiters follow the same rule for the same reason: crossing a
    // hard wrap merges two unrelated terminal lines into one formula.
    let close = if kind.is_display() && opens_a_block(grid, open) {
        grid.find_closing(open, source_start, closing)?
    } else {
        grid.find_closing_soft_wrap(source_start, closing)?
    };
    let after = grid.after(close, closing.len());
    let source = grid.extract(source_start, close)?;
    let display = kind.is_display();
    if !standard_formula_source(&source, display) {
        return None;
    }
    Some((make_overlay(grid, open, after, source, kind), after))
}

/// Whether a display delimiter opens its row: nothing but blanks — or a
/// Markdown list marker — precedes it. Agents emit `$$` / `\[` that way even
/// when they hard-wrap the formula that follows on the same row, while a
/// sentence that merely mentions the delimiter always has prose in front of it.
/// The prefix is the whole judgement: requiring a blank *tail* as well would
/// reject `\[\displaystyle …` wrapped across rows, which is a real shape
/// (see `screenshot_display_formulas_survive_agent_hard_wraps`). Mirrors the
/// leading-blank half of the standalone rule [`find_bare_bracket_formula`]
/// applies to a bare `[` block.
pub(super) fn opens_a_block(grid: &TextGrid, open: GridPosition) -> bool {
    span_is_blank_or_markdown_list_marker(grid, open.row, open.column)
}

pub(super) fn find_dollar_formula(
    grid: &TextGrid,
    open: GridPosition,
) -> Option<(FormulaOverlay, GridPosition)> {
    let source_start = grid.after(open, 1);
    let first = grid.character(source_start)?;
    // `$ ` is how every sh-family prompt ends, and `$$` belongs to a display
    // delimiter. Neither opens an inline formula.
    if first.is_whitespace() || first == '$' {
        return None;
    }

    let mut search = source_start;
    while let Some(close) = find_inline_dollar_closing(grid, search) {
        // TeX never puts a space right before the closing `$`, while a shell
        // line routinely does (`$HOME $USER`). A following identifier
        // character means this `$` opens the *next* variable rather than
        // closing ours.
        let previous = grid.previous(close).and_then(|position| grid.character(position));
        let next = grid.next(close).and_then(|position| grid.character(position));
        if previous.is_some_and(char::is_whitespace)
            || next.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            search = grid.after(close, 1);
            continue;
        }

        let after = grid.after(close, 1);
        let source = grid.extract(source_start, close)?;
        if standard_formula_source(&source, false) {
            return Some((
                make_overlay(grid, open, after, source, DelimiterKind::DollarInline),
                after,
            ));
        }
        search = after;
    }
    None
}

/// Find a single-dollar closer without borrowing either character from a
/// `$$` display delimiter, and without leaving the logical line: an inline
/// formula may follow a soft wrap, but a real newline ends it. An unmatched
/// shell/currency dollar must not reach across rows to consume another one.
pub(super) fn find_inline_dollar_closing(
    grid: &TextGrid,
    mut position: GridPosition,
) -> Option<GridPosition> {
    loop {
        if position.row >= grid.rows.len() {
            return None;
        }
        if grid.character(position) == Some('$') && !grid.is_escaped(position) {
            let previous_is_dollar =
                grid.previous(position).and_then(|previous| grid.character(previous)) == Some('$');
            let next_is_dollar =
                grid.next(position).and_then(|next| grid.character(next)) == Some('$');
            if !previous_is_dollar && !next_is_dollar {
                return Some(position);
            }
        }
        let previous_row = position.row;
        position = grid.next(position)?;
        if position.row != previous_row && !grid.wrapped[previous_row] {
            return None;
        }
    }
}

/// Markdown-unescaped display block: some AI CLIs run their answer through a
/// markdown renderer that eats the backslash of `\[` / `\]` / `\,` (they are
/// markdown punctuation escapes) while `\int`, `\frac` … survive, leaving a
/// bare `[` block on screen. Bare brackets carry no math intent of their own —
/// JSON, arrays and `[INFO]` logs all use them — so this form is held to a
/// stricter shape than `\[`: `[` must start its row and `]` must end its row.
/// A multi-line block also carries enough presentation intent to accept ordinary
/// mathematical structure such as `E = mc^2`; the compact one-line form still
/// requires a known TeX command so `[x^2]` does not turn into an overlay.
pub(super) fn find_bare_bracket_formula(
    grid: &TextGrid,
    open: GridPosition,
    budget: &mut usize,
) -> Option<(FormulaOverlay, GridPosition)> {
    if !span_is_blank_or_markdown_list_marker(grid, open.row, open.column) {
        return None;
    }
    let source_start = grid.after(open, 1);
    let multiline = grid.span_is_blank(open.row, open.column + 1, grid.columns);
    let close = if multiline {
        // 多行形态：`[` 独占一行，闭合 `]` 也必须独占一行。数学源码里
        // `[0, 1]` 区间的 `]` 不具备闭合资格，跳过继续找。搜索同样带行
        // 预算：屏幕底部一个没闭合的 `[` 不该每帧扫到网格末尾。空行按
        // 与 `find_closing` 同一条理由收尾（数学模式里没有分段），否则一个
        // 没闭合的 `[` 会把下面一整段正文连同它自己的公式吞成一条候选——
        // 也按同一套条件放行 TUI 塞进来的那一行空行：吃掉 `\[` 的正是
        // 这个 Markdown 渲染器，它切开 `$$` 块的手法在这里一模一样。
        let mut content_rows = 0usize;
        let mut gap_pending = false;
        grid.find_closing_bounded(
            source_start,
            BARE_BRACKET_SEARCH_ROWS,
            budget,
            |grid, position| {
                if position.column == 0 && position.row > open.row {
                    if grid.span_is_blank(position.row, 0, grid.columns) {
                        if content_rows == 0 || gap_pending {
                            return None;
                        }
                        gap_pending = true;
                        return Some(false);
                    }
                    content_rows += 1;
                    if std::mem::take(&mut gap_pending)
                        && !grid.row_is_only(position.row, &[']'])
                        && !grid.row_has_math_evidence(position.row)
                    {
                        return None;
                    }
                }
                if grid.character(position) != Some(']') || grid.is_escaped(position) {
                    return Some(false);
                }
                Some(grid.delimiter_owns_row(position, 1))
            },
        )?
    } else {
        // 单行形态 `[ … ]`：闭合必须是本行最后一个非空白字符。
        let cells = grid.rows.get(open.row)?;
        let last = cells.iter().rposition(|cell| cell.is_some_and(|c| c != ' '))?;
        let close = GridPosition { row: open.row, column: last };
        if last <= open.column || grid.character(close) != Some(']') {
            return None;
        }
        close
    };
    let after = grid.after(close, 1);
    let source = grid.extract(source_start, close)?;
    if !bare_formula_source(&source, true) {
        return None;
    }
    Some((make_overlay(grid, open, after, source, DelimiterKind::BareBracketDisplay), after))
}

/// Codex-style Markdown renderers retain the list bullet while unescaping
/// `\[` / `\]`, yielding `• [` followed by TeX and an indented `]`. A list
/// marker is presentation-only, so it may precede an otherwise standalone
/// opening bracket. Other prefixes still reject the candidate (JSON, logs,
/// shell prompts, and prose remain literal).
pub(super) fn span_is_blank_or_markdown_list_marker(
    grid: &TextGrid,
    row: usize,
    end: usize,
) -> bool {
    if grid.span_is_blank(row, 0, end) {
        return true;
    }
    let marker: String =
        grid.rows.get(row).into_iter().flat_map(|cells| cells.iter().take(end).flatten()).collect();
    matches!(marker.trim(), "•" | "-" | "*" | "+")
}

/// Markdown-unescaped inline formula: `\( … \)` stripped down to bare
/// parentheses. Bare parens are prose punctuation, so a known TeX command
/// (`(\sqrt{…})`) is normally required. Codex may also strip the delimiters
/// around a compact equation such as `(E=mc^2)`; that narrow mathematical
/// shape qualifies while regexes and ordinary prose remain literal. The closer
/// is matched by paren depth so `(\sin(x))` keeps its inner `)`.
pub(super) fn find_bare_paren_formula(
    grid: &TextGrid,
    open: GridPosition,
    budget: &mut usize,
) -> Option<(FormulaOverlay, GridPosition)> {
    let source_start = grid.after(open, 1);
    let mut depth = 1usize;
    let close = grid.find_closing_bounded(
        source_start,
        BARE_PAREN_SEARCH_ROWS,
        budget,
        |grid, position| {
            match grid.character(position) {
                Some('(') if !grid.is_escaped(position) => depth += 1,
                Some(')') if !grid.is_escaped(position) => {
                    depth -= 1;
                    return Some(depth == 0);
                },
                _ => {},
            }
            Some(false)
        },
    )?;
    let after = grid.after(close, 1);
    let source = grid.extract(source_start, close)?;
    if !bare_formula_source(&source, false) {
        return None;
    }
    Some((make_overlay(grid, open, after, source, DelimiterKind::BareParenInline), after))
}

/// Strict compact-equation fallback for delimiters whose surrounding context
/// may not yet identify an AI client. Requiring an equality plus an
/// arithmetic/exponent marker or a short implicit product avoids rendering
/// configuration prose such as `key=value`.
pub(super) fn looks_like_compact_equation(source: &str) -> bool {
    let source = compact_equation_source(source);
    let compact_syntax = source.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character.is_ascii_whitespace()
            || matches!(
                character,
                '=' | '+' | '-' | '*' | '/' | '^' | '_' | '{' | '}' | '(' | ')' | '.' | ','
            )
    });
    if !compact_syntax {
        return false;
    }

    let Some((left, right)) = source.split_once('=') else {
        return false;
    };
    if right.contains('=') {
        return false;
    }

    source.chars().any(|character| {
        character.is_ascii_digit() || matches!(character, '+' | '-' | '*' | '/' | '^' | '_')
    }) || looks_like_implicit_product_equation(left, right)
}

pub(super) fn compact_equation_source(source: &str) -> &str {
    let source = source.trim();
    // Presentation commands do not change whether the following source is a
    // compact equation. Markdown renderers may leave this command inside bare
    // parentheses after consuming the original `\(` / `\)` delimiters.
    source.strip_prefix(r"\displaystyle").map(str::trim_start).unwrap_or(source)
}

/// Recognize compact equations whose multiplication signs are conventionally
/// omitted, such as `F=ma` and `PV=nRT`. Requiring short identifiers plus a
/// single-symbol or uppercase variable keeps configuration prose like
/// `key=value` outside the formula path.
pub(super) fn looks_like_implicit_product_equation(left: &str, right: &str) -> bool {
    fn compact_identifier(source: &str) -> Option<(usize, bool)> {
        let source = source.trim();
        let count = source.chars().count();
        if !(1..=3).contains(&count)
            || !source.chars().all(|character| character.is_ascii_alphabetic())
        {
            return None;
        }
        Some((count, source.chars().any(|character| character.is_ascii_uppercase())))
    }

    let Some((left_count, left_uppercase)) = compact_identifier(left) else {
        return false;
    };
    let Some((right_count, right_uppercase)) = compact_identifier(right) else {
        return false;
    };

    left_count == 1 || right_count == 1 || left_uppercase || right_uppercase
}

/// Commands that justify treating bare delimiters as math. Deliberately a
/// whitelist instead of "any `\` + letters": regex escapes (`\d`, `\w`), C
/// escapes and Windows paths all match the loose shape. Single-letter
/// commands are excluded wholesale by the caller-side length check baked in
/// here (every entry is ≥2 chars).
const KNOWN_TEX_COMMANDS: &[&str] = &[
    "alpha",
    "approx",
    "arccos",
    "arcsin",
    "arctan",
    "bar",
    "begin",
    "beta",
    "big",
    "bigg",
    "binom",
    "boldsymbol",
    "bullet",
    "cap",
    "cdot",
    "cdots",
    "chi",
    "circ",
    "cos",
    "cosh",
    "cot",
    "csc",
    "cup",
    "ddot",
    "ddots",
    "delta",
    "det",
    "dfrac",
    "div",
    "dot",
    "dots",
    "emptyset",
    "end",
    "epsilon",
    "equiv",
    "eta",
    "exists",
    "exp",
    "forall",
    "frac",
    "gamma",
    "gcd",
    "ge",
    "geq",
    "gg",
    "hat",
    "iff",
    "iiint",
    "iint",
    "implies",
    "in",
    "inf",
    "infty",
    "int",
    "iota",
    "kappa",
    "lambda",
    "land",
    "langle",
    "lceil",
    "ldots",
    "le",
    "left",
    "leftarrow",
    "leftrightarrow",
    "leq",
    "lfloor",
    "lg",
    "lim",
    "liminf",
    "limsup",
    "ll",
    "ln",
    "log",
    "lor",
    "mapsto",
    "mathbb",
    "mathbf",
    "mathcal",
    "mathfrak",
    "mathit",
    "mathrm",
    "mathsf",
    "max",
    "min",
    "mp",
    "mu",
    "nabla",
    "ne",
    "neg",
    "neq",
    "notin",
    "nu",
    "odot",
    "oint",
    "omega",
    "ominus",
    "operatorname",
    "oplus",
    "otimes",
    "overbrace",
    "overline",
    "partial",
    "phi",
    "pi",
    "pm",
    "prod",
    "propto",
    "psi",
    "qquad",
    "quad",
    "rangle",
    "rceil",
    "rfloor",
    "rho",
    "right",
    "rightarrow",
    "sec",
    "sigma",
    "sim",
    "simeq",
    "sin",
    "sinh",
    "sqrt",
    "subset",
    "subseteq",
    "sum",
    "sup",
    "supset",
    "supseteq",
    "tan",
    "tanh",
    "tau",
    "text",
    "textbf",
    "textit",
    "textrm",
    "theta",
    "tilde",
    "times",
    "to",
    "underbrace",
    "underline",
    "upsilon",
    "varepsilon",
    "varnothing",
    "varphi",
    "varpi",
    "varrho",
    "varsigma",
    "vartheta",
    "vec",
    "vert",
    "widehat",
    "widetilde",
    "xi",
    "zeta",
    "Big",
    "Bigg",
    "Delta",
    "Gamma",
    "Lambda",
    "Leftarrow",
    "Leftrightarrow",
    "Omega",
    "Phi",
    "Pi",
    "Psi",
    "Rightarrow",
    "Sigma",
    "Theta",
    "Upsilon",
    "Vert",
    "Xi",
];

/// Whether `source` contains at least one whitelisted TeX command. Bare
/// delimiters have no `\[` / `\(` intent statement backing them, so a real
/// math command is required as evidence before they may render.
pub(super) fn has_known_tex_command(source: &str) -> bool {
    let mut rest = source;
    while let Some(index) = rest.find('\\') {
        rest = &rest[index + 1..];
        let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        if end > 1 && KNOWN_TEX_COMMANDS.contains(&&rest[..end]) {
            return true;
        }
        rest = &rest[end..];
    }
    false
}

/// Delimiters state intent, content supplies evidence.
///
/// The intent statement alone is not enough: a terminal is full of dollars that
/// are shell sigils, prompts and prices, so a scanner that accepts any non-empty
/// span turns `echo $HOME $USER` into a formula. Display blocks (`$$…$$`,
/// `\[…\]`) rarely occur outside real math, so lax evidence suffices; inline
/// `$…$` and `\(…\)` collide with currency, shell variables and BRE capture
/// groups, so their evidence must be structurally compact.
pub(super) fn standard_formula_source(source: &str, display: bool) -> bool {
    let source = source.trim();
    !obviously_non_math(source) && has_math_evidence(source, display)
}

/// Terminal noise that must never render as math regardless of delimiter
/// strength: comments/URLs (`//`), Windows paths (`:\`), control bytes,
/// currency amounts, and ALL_CAPS shell variables.
pub(super) fn obviously_non_math(source: &str) -> bool {
    if source.is_empty()
        || source.contains("//")
        || source.contains(":\\")
        || source
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return true;
    }

    // Currency and shell variables are the dominant terminal use of dollars.
    let currency = source.chars().all(|character| {
        character.is_ascii_digit()
            || character.is_ascii_whitespace()
            || ".,EURUSDCNYGBPJPY".contains(character)
    });
    let shell_identifier = source.chars().all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    });
    currency || (shell_identifier && source.chars().count() > 1)
}

/// At least one structural sign of mathematics. `lax` (display blocks) lifts
/// the compact-operand requirement on script bases so implicit products like
/// `mc^2` qualify; inline keeps it so `$foo^bar$` stays literal text.
pub(super) fn has_math_evidence(source: &str, lax: bool) -> bool {
    let chars: Vec<_> = source.chars().collect();
    let single_variable = chars.len() == 1 && chars[0].is_alphabetic();
    let tex_command = chars.windows(2).any(|pair| pair[0] == '\\' && pair[1].is_alphabetic());
    let script = source.find(['^', '_']).is_some_and(|index| {
        let (base, suffix) = source.split_at(index);
        let suffix = &suffix[1..];
        let base_qualifies = if lax { !base.trim().is_empty() } else { explicit_operand(base) };
        base_qualifies && !suffix.trim().is_empty()
    });
    let relation = ["<=", ">=", "!=", "==", "=", "<", ">"].into_iter().any(|operator| {
        source.find(operator).is_some_and(|index| {
            relation_operand(&source[..index])
                && relation_operand(&source[index + operator.len()..])
        })
    });
    // `n!` / `\binom` 的裸写法：阶乘是后缀运算符，本身就是数学证据。
    let factorial = source
        .trim_end()
        .strip_suffix('!')
        .is_some_and(|base| explicit_operand(base) || implicit_product_operand(base));
    let structural = script
        || relation
        || factorial
        || source.chars().any(|character| {
            matches!(
                character,
                '±' | '×'
                    | '÷'
                    | '√'
                    | '∑'
                    | '∏'
                    | '∫'
                    | '∞'
                    | '≈'
                    | '≠'
                    | '≤'
                    | '≥'
                    | '∂'
                    | '∇'
                    | '∈'
                    | '∉'
                    | '⊂'
                    | '⊆'
                    | '∪'
                    | '∩'
                    | '→'
                    | '↦'
            )
        });
    let known_function = source.split(|character: char| !character.is_alphabetic()).any(|word| {
        matches!(
            word.to_ascii_lowercase().as_str(),
            "sin" | "cos" | "tan" | "log" | "ln" | "exp" | "lim" | "det" | "max" | "min"
        )
    });
    let function_application = source.find('(').is_some_and(|open| {
        let name = source[..open].trim();
        let arguments = source[open + 1..].strip_suffix(')').unwrap_or("").trim();
        name.chars().count() == 1 && name.chars().all(char::is_alphabetic) && !arguments.is_empty()
    });
    let parenthesized_variable = source
        .strip_prefix('(')
        .and_then(|source| source.strip_suffix(')'))
        .is_some_and(explicit_operand);
    let compact_operator = ['+', '-', '*', '/'].into_iter().any(|operator| {
        source.find(operator).is_some_and(|index| {
            let (left, right) = source.split_at(index);
            let right = &right[operator.len_utf8()..];
            explicit_operand(left) && explicit_operand(right)
        })
    });

    single_variable
        || tex_command
        || structural
        || known_function
        || function_application
        || parenthesized_variable
        || compact_operator
}

/// A single mathematical operand: one variable at most, optionally scripted.
/// Multi-letter runs are rejected so `$foo^bar$` and `key=value` stay text.
pub(super) fn explicit_operand(operand: &str) -> bool {
    let operand = operand.trim().trim_matches(['(', ')', '[', ']', '{', '}']);
    if operand.is_empty() || operand.contains(char::is_whitespace) {
        return false;
    }
    let alphabetic = operand.chars().filter(|character| character.is_alphabetic()).count();
    alphabetic <= 1
        && operand.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '.' | ',' | '\\' | '^' | '_')
        })
}

/// Physics writes products without a multiplication sign: `mc^2`, `nRT`, `ma`.
/// [`explicit_operand`] caps identifiers at one letter and therefore rejects
/// every one of them, which is why `$E=mc^2$` used to stay literal. Allow up to
/// three letters here — still no whitespace and no punctuation beyond scripts,
/// so `PATH=/tmp` (slash), `key=value` (five letters) and `npm install`
/// (whitespace) remain outside the formula path.
pub(super) fn implicit_product_operand(operand: &str) -> bool {
    let operand = operand.trim().trim_matches(['(', ')', '[', ']', '{', '}']);
    if operand.is_empty() || operand.contains(char::is_whitespace) {
        return false;
    }
    let alphabetic = operand.chars().filter(|character| character.is_alphabetic()).count();
    (1..=3).contains(&alphabetic)
        && operand
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '^' | '_' | '.'))
}

/// A relation may also be applied to a function call (`f(x)=0`) or to an
/// implicit product (`E=mc^2`), neither of which is a single operand.
pub(super) fn relation_operand(operand: &str) -> bool {
    let operand = operand.trim();
    explicit_operand(operand)
        || implicit_product_operand(operand)
        || operand.find('(').is_some_and(|open| {
            let name = operand[..open].trim();
            let arguments = operand[open + 1..].strip_suffix(')').unwrap_or("").trim();
            name.chars().count() == 1
                && name.chars().all(char::is_alphabetic)
                && !arguments.is_empty()
        })
}

/// A bare bracket or parenthesis has no explicit TeX intent after Markdown has
/// consumed the delimiter backslashes. Recover it only when the remaining
/// source still carries TeX/equation evidence, and never reinterpret a Windows
/// path as a formula merely because it contains a whitelisted command name.
///
/// Bare delimiters answer to both layers: the standard evidence above, plus
/// their own stricter requirement of a known command or a compact equation.
pub(super) fn bare_formula_source(source: &str, display: bool) -> bool {
    standard_formula_source(source, display)
        && !source.contains(":\\")
        && (has_known_tex_command(source) || looks_like_compact_equation(source))
}

pub(super) fn make_overlay(
    grid: &TextGrid,
    open: GridPosition,
    after: GridPosition,
    source: Box<str>,
    kind: DelimiterKind,
) -> FormulaOverlay {
    let display = kind.is_display();
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    kind.hash(&mut hasher);
    grid.absolute_top.saturating_add(open.row).hash(&mut hasher);
    open.column.hash(&mut hasher);
    let formula_id = hasher.finish();
    FormulaOverlay {
        source: Arc::from(source),
        display,
        formula_id,
        spans: grid.spans(open, after),
        foreground: Rgb::default(),
        fallback: Vec::new(),
        neighbours_above: [NEIGHBOUR_UNKNOWN; MAX_ABSORBED_BLANK_ROWS],
        neighbours_below: [NEIGHBOUR_UNKNOWN; MAX_ABSORBED_BLANK_ROWS],
        formula_neighbours_above: [false; MAX_ABSORBED_BLANK_ROWS],
        formula_neighbours_below: [false; MAX_ABSORBED_BLANK_ROWS],
        widen_right_to: None,
    }
}
