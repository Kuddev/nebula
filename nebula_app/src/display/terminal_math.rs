//! Native TeX overlays for formula delimiters emitted into a terminal grid.
//!
//! The terminal remains the source of truth: this module never mutates cells,
//! scrollback, cursor positions, selections, or copied text. It only replaces
//! visible delimiter spans during the final paint pass.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::size_of;
use std::sync::Arc;

use nebula_terminal::grid::Dimensions;
use nebula_terminal::index::{Column, Line, Point};
use nebula_terminal::term::Term;
use nebula_terminal::term::cell::Flags;

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::math::cache::{FormulaCacheKey, MathLayoutCache};
use crate::math::layout::MathLayout;
use crate::math::{DEFAULT_LIMITS, compile_formula};
use crate::renderer::math::MathClip;
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

const MAX_VISIBLE_FORMULAS: usize = 64;
const MAX_PERSISTED_FORMULAS: usize = 2_048;
const PERSISTED_FORMULA_BUDGET: usize = 1024 * 1024;
const MAX_HISTORY_FORMULA_ROWS: usize = 512;
const MIN_MATH_PIXEL_SIZE: f32 = 6.0;
const FORMULA_INSET: f32 = 2.0;

/// Per-pane state. Layout data is intentionally discarded when a pane state is
/// cloned: cloned UI metadata can outlive a renderer/font scale, while layouts
/// are cheap to rebuild and remain bounded by `MathLayoutCache` afterwards.
pub(super) struct TerminalMathState {
    cache: MathLayoutCache,
    ai_cli_seen: bool,
    formulas: BTreeMap<FormulaAnchor, PersistedFormula>,
    persisted_bytes: usize,
    max_formula_rows: usize,
    columns: Option<usize>,
    last_scrolled_out: Option<usize>,
    pending_display: Option<PendingDisplayFormula>,
}

impl Default for TerminalMathState {
    fn default() -> Self {
        Self {
            cache: MathLayoutCache::default(),
            ai_cli_seen: false,
            formulas: BTreeMap::new(),
            persisted_bytes: 0,
            max_formula_rows: 0,
            columns: None,
            last_scrolled_out: None,
            pending_display: None,
        }
    }
}

impl Clone for TerminalMathState {
    fn clone(&self) -> Self {
        // Pane clones can be attached to a different PTY. Absolute grid anchors
        // must therefore be rebuilt from that pane instead of crossing sessions.
        Self { ai_cli_seen: self.ai_cli_seen, ..Self::default() }
    }
}

impl fmt::Debug for TerminalMathState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalMathState")
            .field("ai_cli_seen", &self.ai_cli_seen)
            .field("persisted_formulas", &self.formulas.len())
            .field("persisted_bytes", &self.persisted_bytes)
            .finish()
    }
}

impl TerminalMathState {
    pub(super) fn observe_program(&mut self, program: Option<&str>) {
        self.ai_cli_seen |= program.is_some_and(is_ai_cli);
    }

    pub(super) fn inline_dollar_enabled(&self) -> bool {
        self.ai_cli_seen
    }

    fn synchronize_grid(&mut self, grid: &TextGrid) {
        let columns_changed = self.columns.is_some_and(|columns| columns != grid.columns);
        let absolute_epoch_changed =
            self.last_scrolled_out.is_some_and(|floor| grid.scrolled_out < floor);
        if columns_changed || absolute_epoch_changed {
            // 宽度变化会重排历史行，绝对行号回退则代表网格生命周期已重置；
            // 两种情况下沿用旧锚点都会把公式覆盖到无关文本上。
            self.clear_formulas();
        }
        self.columns = Some(grid.columns);

        if self.last_scrolled_out != Some(grid.scrolled_out) {
            self.prune_before(grid.scrolled_out);
            self.last_scrolled_out = Some(grid.scrolled_out);
        }
    }

    /// Remember complete formulas in the current viewport and track one
    /// streaming display formula whose opening delimiter may scroll away before
    /// its closing delimiter arrives.
    fn scan_visible_grid(
        &mut self,
        grid: &TextGrid,
        allow_inline_dollar: bool,
    ) -> Option<FormulaAnchor> {
        let mut completed_pending = false;
        let scan = scan_grid_result(grid, allow_inline_dollar);
        for overlay in scan.overlays {
            let anchor = overlay_anchor(grid, &overlay);
            completed_pending |= self
                .pending_display
                .is_some_and(|pending| Some(pending.anchor) == anchor && overlay.display);
            self.remember(grid, &overlay);
        }
        if completed_pending {
            self.pending_display = None;
        }

        let Some((position, kind)) = scan.unmatched_display else {
            return None;
        };
        let current = PendingDisplayFormula {
            anchor: FormulaAnchor {
                row: grid.absolute_top.saturating_add(position.row),
                column: position.column,
            },
            kind,
        };

        match self.pending_display {
            None => {
                self.pending_display = Some(current);
                None
            },
            Some(pending) if pending == current => None,
            Some(pending) if pending.kind == current.kind && pending.anchor < current.anchor => {
                Some(pending.anchor)
            },
            Some(_) => {
                self.pending_display = Some(current);
                None
            },
        }
    }

    fn complete_pending_from_history(
        &mut self,
        history: &TextGrid,
        allow_inline_dollar: bool,
    ) -> bool {
        let Some(pending) = self.pending_display.take() else {
            return false;
        };

        for overlay in scan_grid_result(history, allow_inline_dollar).overlays {
            if overlay.display && overlay_anchor(history, &overlay) == Some(pending.anchor) {
                self.remember(history, &overlay);
                return true;
            }
        }
        false
    }

    fn remember(&mut self, grid: &TextGrid, overlay: &FormulaOverlay) {
        let Some(formula) = PersistedFormula::from_overlay(grid, overlay) else {
            return;
        };
        let anchor = formula.anchor;
        if self.formulas.get(&anchor).is_some_and(|existing| existing.same_content(&formula)) {
            return;
        }
        self.remove(anchor);
        if formula.charge > PERSISTED_FORMULA_BUDGET {
            return;
        }

        self.persisted_bytes = self.persisted_bytes.saturating_add(formula.charge);
        self.max_formula_rows = self.max_formula_rows.max(formula.row_count());
        self.formulas.insert(anchor, formula);
        while self.formulas.len() > MAX_PERSISTED_FORMULAS
            || self.persisted_bytes > PERSISTED_FORMULA_BUDGET
        {
            let Some(oldest) = self.formulas.first_key_value().map(|(&anchor, _)| anchor) else {
                break;
            };
            self.remove(oldest);
        }
    }

    fn visible_overlays(&mut self, grid: &TextGrid) -> Vec<FormulaOverlay> {
        if grid.rows.is_empty() || self.formulas.is_empty() {
            return Vec::new();
        }

        let viewport_bottom = grid.absolute_top.saturating_add(grid.rows.len() - 1);
        let search_top = grid.absolute_top.saturating_sub(self.max_formula_rows.saturating_sub(1));
        let lower = FormulaAnchor { row: search_top, column: 0 };
        let upper = FormulaAnchor { row: viewport_bottom, column: usize::MAX };
        let candidates: Vec<_> = self
            .formulas
            .range(lower..=upper)
            .filter(|(_, formula)| formula.intersects(grid.absolute_top, viewport_bottom))
            .map(|(&anchor, formula)| (anchor, formula.matches_visible_rows(grid)))
            .collect();

        let mut overlays = Vec::with_capacity(candidates.len().min(MAX_VISIBLE_FORMULAS));
        for (anchor, valid) in candidates {
            if !valid {
                self.remove(anchor);
                continue;
            }
            if overlays.len() < MAX_VISIBLE_FORMULAS {
                if let Some(formula) = self.formulas.get(&anchor) {
                    overlays.push(formula.to_overlay(grid.absolute_top));
                }
            }
        }
        overlays
    }

    fn prune_before(&mut self, absolute_floor: usize) {
        if self.pending_display.is_some_and(|pending| pending.anchor.row < absolute_floor) {
            self.pending_display = None;
        }
        loop {
            let stale = self
                .formulas
                .first_key_value()
                .filter(|(_, formula)| formula.last_row() < absolute_floor)
                .map(|(&anchor, _)| anchor);
            match stale {
                Some(anchor) => self.remove(anchor),
                None => break,
            }
        }
    }

    fn remove(&mut self, anchor: FormulaAnchor) {
        let Some(removed) = self.formulas.remove(&anchor) else {
            return;
        };
        self.persisted_bytes = self.persisted_bytes.saturating_sub(removed.charge);
        if removed.row_count() == self.max_formula_rows {
            self.max_formula_rows =
                self.formulas.values().map(PersistedFormula::row_count).max().unwrap_or(0);
        }
    }

    fn clear_formulas(&mut self) {
        self.formulas.clear();
        self.persisted_bytes = 0;
        self.max_formula_rows = 0;
        self.pending_display = None;
    }

    fn layout(
        &mut self,
        formula_id: u64,
        source: &str,
        pixel_size: f32,
        pixels_per_point: f32,
        display: bool,
    ) -> Result<&MathLayout, crate::math::MathError> {
        let key = FormulaCacheKey::new(formula_id, pixel_size, pixels_per_point, display);
        self.cache.get_or_insert_with(key, || {
            compile_formula(source, display, pixel_size, pixels_per_point, DEFAULT_LIMITS)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FormulaAnchor {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayDelimiterKind {
    Dollars,
    Brackets,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingDisplayFormula {
    anchor: FormulaAnchor,
    kind: DisplayDelimiterKind,
}

fn overlay_anchor(grid: &TextGrid, overlay: &FormulaOverlay) -> Option<FormulaAnchor> {
    let first = overlay.spans.first()?;
    let row = usize::try_from(first.row).ok()?;
    Some(FormulaAnchor { row: grid.absolute_top.checked_add(row)?, column: first.start })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PersistedRowSpan {
    row: usize,
    start: usize,
    end: usize,
    fingerprint: u64,
    include_wrap: bool,
}

#[derive(Clone, Debug)]
struct PersistedFormula {
    anchor: FormulaAnchor,
    source: Arc<str>,
    display: bool,
    formula_id: u64,
    spans: Box<[PersistedRowSpan]>,
    charge: usize,
}

impl PersistedFormula {
    fn from_overlay(grid: &TextGrid, overlay: &FormulaOverlay) -> Option<Self> {
        let mut spans = Vec::with_capacity(overlay.spans.len());
        for (index, span) in overlay.spans.iter().enumerate() {
            let row = usize::try_from(span.row).ok()?;
            let include_wrap = index + 1 < overlay.spans.len();
            let fingerprint = grid.span_fingerprint(row, span.start, span.end, include_wrap)?;
            spans.push(PersistedRowSpan {
                row: grid.absolute_top.checked_add(row)?,
                start: span.start,
                end: span.end,
                fingerprint,
                include_wrap,
            });
        }
        let first = spans.first()?;
        let anchor = FormulaAnchor { row: first.row, column: first.start };
        let charge = size_of::<Self>()
            .saturating_add(overlay.source.len())
            .saturating_add(spans.capacity().saturating_mul(size_of::<PersistedRowSpan>()));
        Some(Self {
            anchor,
            source: Arc::clone(&overlay.source),
            display: overlay.display,
            formula_id: overlay.formula_id,
            spans: spans.into_boxed_slice(),
            charge,
        })
    }

    fn row_count(&self) -> usize {
        self.last_row().saturating_sub(self.anchor.row).saturating_add(1)
    }

    fn same_content(&self, other: &Self) -> bool {
        self.display == other.display
            && self.formula_id == other.formula_id
            && self.source == other.source
            && self.spans == other.spans
    }

    fn last_row(&self) -> usize {
        self.spans.last().map_or(self.anchor.row, |span| span.row)
    }

    fn intersects(&self, top: usize, bottom: usize) -> bool {
        self.anchor.row <= bottom && self.last_row() >= top
    }

    fn matches_visible_rows(&self, grid: &TextGrid) -> bool {
        let mut compared = false;
        for span in &self.spans {
            let Some(row) = span.row.checked_sub(grid.absolute_top) else {
                continue;
            };
            if row >= grid.rows.len() {
                continue;
            }
            compared = true;
            if grid.span_fingerprint(row, span.start, span.end, span.include_wrap)
                != Some(span.fingerprint)
            {
                return false;
            }
        }
        compared
    }

    fn to_overlay(&self, absolute_top: usize) -> FormulaOverlay {
        let spans = self
            .spans
            .iter()
            .map(|span| RowSpan {
                row: relative_row(span.row, absolute_top),
                start: span.start,
                end: span.end,
            })
            .collect();
        FormulaOverlay {
            source: Arc::clone(&self.source),
            display: self.display,
            formula_id: self.formula_id,
            spans,
            foreground: Rgb::default(),
            background: Rgb::default(),
            fallback: Vec::new(),
        }
    }
}

fn relative_row(row: usize, absolute_top: usize) -> i32 {
    if row >= absolute_top {
        i32::try_from(row - absolute_top).unwrap_or(i32::MAX)
    } else {
        -i32::try_from(absolute_top - row).unwrap_or(i32::MAX)
    }
}

fn is_ai_cli(program: &str) -> bool {
    matches!(
        program,
        "claude"
            | "codex"
            | "gemini"
            | "copilot"
            | "cursor"
            | "cursor-agent"
            | "aider"
            | "goose"
            | "crush"
            | "opencode"
            | "pi"
    )
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DelimiterKind {
    DollarInline,
    Parenthesized,
    DollarDisplay,
    BracketDisplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GridPosition {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RowSpan {
    row: i32,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
pub(super) struct FormulaOverlay {
    source: Arc<str>,
    display: bool,
    formula_id: u64,
    spans: Vec<RowSpan>,
    foreground: Rgb,
    background: Rgb,
    fallback: Vec<RenderableCell>,
}

impl FormulaOverlay {
    fn contains(&self, point: Point<usize>) -> bool {
        self.spans.iter().any(|span| {
            usize::try_from(span.row) == Ok(point.line)
                && (span.start..span.end).contains(&point.column.0)
        })
    }

    fn bounds(&self, size: &SizeInfo) -> Option<FormulaBounds> {
        let first = self.spans.first()?;
        let last = self.spans.last()?;
        let left_col = self.spans.iter().map(|span| span.start).min()?;
        let right_col = self.spans.iter().map(|span| span.end).max()?;
        let left = size.padding_x() + left_col as f32 * size.cell_width();
        let right = size.padding_x() + right_col as f32 * size.cell_width();
        let top = size.padding_y() + first.row as f32 * size.cell_height();
        let bottom = size.padding_y() + (last.row + 1) as f32 * size.cell_height();
        (right > left && bottom > top).then_some(FormulaBounds { left, top, right, bottom })
    }
}

#[derive(Clone, Copy)]
struct FormulaBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl FormulaBounds {
    fn width(self) -> f32 {
        self.right - self.left
    }

    fn height(self) -> f32 {
        self.bottom - self.top
    }
}

#[derive(Clone, Debug)]
struct TextGrid {
    rows: Vec<Vec<Option<char>>>,
    wrapped: Vec<bool>,
    columns: usize,
    absolute_top: usize,
    scrolled_out: usize,
}

impl TextGrid {
    fn from_term<T>(terminal: &Term<T>, size: &SizeInfo) -> Self {
        Self::from_term_with_lookback(terminal, size, 0)
    }

    fn from_term_with_lookback<T>(
        terminal: &Term<T>,
        size: &SizeInfo,
        requested_lookback: usize,
    ) -> Self {
        let grid = terminal.grid();
        // PTY resize is debounced during a live window drag, so display geometry
        // can lead the grid by one frame. Scan only their shared rectangle.
        let columns = size.columns().min(grid.columns());
        let screen_lines = size.screen_lines().min(grid.screen_lines());
        let display_offset = grid.display_offset();
        let scrolled_out = grid.scrolled_out();
        let history_size = grid.history_size();
        let available_lookback = history_size.saturating_sub(display_offset);
        let lookback = requested_lookback.min(available_lookback);
        let absolute_top = scrolled_out
            .saturating_add(history_size)
            .saturating_sub(display_offset)
            .saturating_sub(lookback);
        let row_count = screen_lines.saturating_add(lookback);
        let mut rows = Vec::with_capacity(row_count);
        let mut wrapped = Vec::with_capacity(row_count);

        for row in 0..row_count {
            let line = Line(row as i32 - display_offset as i32 - lookback as i32);
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
    fn from_rows(rows: &[&str]) -> Self {
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

    fn character(&self, position: GridPosition) -> Option<char> {
        self.rows.get(position.row)?.get(position.column).copied().flatten()
    }

    fn starts_with(&self, position: GridPosition, delimiter: &[char]) -> bool {
        delimiter.iter().enumerate().all(|(offset, expected)| {
            self.character(GridPosition { row: position.row, column: position.column + offset })
                == Some(*expected)
        })
    }

    fn is_escaped(&self, position: GridPosition) -> bool {
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

    fn after(&self, position: GridPosition, width: usize) -> GridPosition {
        GridPosition { row: position.row, column: position.column + width }
    }

    fn next(&self, position: GridPosition) -> Option<GridPosition> {
        if position.column + 1 < self.columns {
            Some(GridPosition { row: position.row, column: position.column + 1 })
        } else if position.row + 1 < self.rows.len() {
            Some(GridPosition { row: position.row + 1, column: 0 })
        } else {
            None
        }
    }

    fn find_closing(
        &self,
        mut position: GridPosition,
        delimiter: &[char],
        same_row: bool,
    ) -> Option<GridPosition> {
        let initial_row = position.row;
        loop {
            if position.row >= self.rows.len() || (same_row && position.row != initial_row) {
                return None;
            }
            if self.starts_with(position, delimiter) && !self.is_escaped(position) {
                return Some(position);
            }
            position = self.next(position)?;
        }
    }

    fn extract(&self, start: GridPosition, end: GridPosition) -> Option<Box<str>> {
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

    fn span_fingerprint(
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

    fn spans(&self, start: GridPosition, end: GridPosition) -> Vec<RowSpan> {
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
pub(super) fn scan_visible<T>(
    state: &mut TerminalMathState,
    terminal: &Term<T>,
    size: &SizeInfo,
    rendered_cells: &[RenderableCell],
    allow_inline_dollar: bool,
    cursor: Option<Point<usize>>,
    default_foreground: Rgb,
    default_background: Rgb,
) -> Vec<FormulaOverlay> {
    let grid = TextGrid::from_term(terminal, size);
    state.synchronize_grid(&grid);
    if let Some(anchor) = state.scan_visible_grid(&grid, allow_inline_dollar) {
        let lookback = grid.absolute_top.saturating_sub(anchor.row);
        // `extract` counts every terminal cell toward the 16 KiB parser limit.
        // Deriving the row cap from the current width keeps this rare temporary
        // grid near 64 KiB of cell data instead of scanning all scrollback.
        let source_rows = DEFAULT_LIMITS
            .max_source_bytes
            .div_ceil(grid.columns.max(1))
            .saturating_add(4)
            .min(MAX_HISTORY_FORMULA_ROWS);
        if lookback <= source_rows {
            let history = TextGrid::from_term_with_lookback(terminal, size, lookback);
            state.complete_pending_from_history(&history, allow_inline_dollar);
        } else {
            state.pending_display = None;
        }
    }

    state
        .visible_overlays(&grid)
        .into_iter()
        .filter(|overlay| cursor.is_none_or(|cursor| !overlay.contains(cursor)))
        .filter_map(|mut overlay| {
            overlay.fallback = rendered_cells
                .iter()
                .filter(|cell| overlay.contains(cell.point))
                .cloned()
                .collect();

            // AI tools commonly emit fenced code with an ANSI background. TeX
            // delimiters there are source code, not presentation math.
            if overlay.fallback.iter().any(|cell| cell.bg_alpha > 0.0) {
                return None;
            }

            overlay.foreground = overlay
                .fallback
                .iter()
                .find(|cell| !cell.character.is_whitespace())
                .map_or(default_foreground, |cell| cell.fg);
            overlay.background = overlay
                .fallback
                .iter()
                .find(|cell| cell.bg_alpha > 0.0)
                .map_or(default_background, |cell| cell.bg);
            Some(overlay)
        })
        .collect()
}

#[cfg(test)]
fn scan_grid(grid: &TextGrid, allow_inline_dollar: bool) -> Vec<FormulaOverlay> {
    scan_grid_result(grid, allow_inline_dollar).overlays
}

struct GridScanResult {
    overlays: Vec<FormulaOverlay>,
    unmatched_display: Option<(GridPosition, DisplayDelimiterKind)>,
}

fn scan_grid_result(grid: &TextGrid, allow_inline_dollar: bool) -> GridScanResult {
    let mut overlays = Vec::new();
    let mut unmatched_display = None;
    let mut position = GridPosition { row: 0, column: 0 };

    while position.row < grid.rows.len() && overlays.len() < MAX_VISIBLE_FORMULAS {
        let (candidate, incomplete_display) =
            if grid.starts_with(position, &['$', '$']) && !grid.is_escaped(position) {
                let candidate = find_formula(
                    grid,
                    position,
                    &['$', '$'],
                    &['$', '$'],
                    DelimiterKind::DollarDisplay,
                    false,
                );
                (candidate, Some(DisplayDelimiterKind::Dollars))
            } else if grid.starts_with(position, &['\\', '[']) && !grid.is_escaped(position) {
                let candidate = find_formula(
                    grid,
                    position,
                    &['\\', '['],
                    &['\\', ']'],
                    DelimiterKind::BracketDisplay,
                    false,
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
                        true,
                    ),
                    None,
                )
            } else if allow_inline_dollar
                && grid.character(position) == Some('$')
                && !grid.is_escaped(position)
            {
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

fn find_formula(
    grid: &TextGrid,
    open: GridPosition,
    opening: &[char],
    closing: &[char],
    kind: DelimiterKind,
    same_row: bool,
) -> Option<(FormulaOverlay, GridPosition)> {
    let source_start = grid.after(open, opening.len());
    let close = grid.find_closing(source_start, closing, same_row)?;
    let after = grid.after(close, closing.len());
    let source = grid.extract(source_start, close)?;
    if !plausible_math_source(&source) {
        return None;
    }
    Some((make_overlay(grid, open, after, source, kind), after))
}

fn find_dollar_formula(
    grid: &TextGrid,
    open: GridPosition,
) -> Option<(FormulaOverlay, GridPosition)> {
    let source_start = grid.after(open, 1);
    let first = grid.character(source_start)?;
    if first.is_whitespace() || first == '$' {
        return None;
    }

    let mut search = source_start;
    while let Some(close) = grid.find_closing(search, &['$'], true) {
        let previous = (close.column > 0)
            .then(|| grid.character(GridPosition { row: close.row, column: close.column - 1 }))
            .flatten();
        let after = grid.after(close, 1);
        let next = grid.character(after);
        if previous.is_some_and(char::is_whitespace)
            || next.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            search = grid.after(close, 1);
            continue;
        }

        let source = grid.extract(source_start, close)?;
        if plausible_math_source(&source) {
            return Some((
                make_overlay(grid, open, after, source, DelimiterKind::DollarInline),
                after,
            ));
        }
        return None;
    }
    None
}

fn plausible_math_source(source: &str) -> bool {
    let source = source.trim();
    if source.is_empty()
        || source.contains("//")
        || source.contains(":\\")
        || source
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return false;
    }

    // Currency and shell variables are the dominant terminal use of dollars.
    // Reject them before considering mathematical punctuation.
    let currency = source.chars().all(|character| {
        character.is_ascii_digit()
            || character.is_ascii_whitespace()
            || ".,EURUSDCNYGBPJPY".contains(character)
    });
    let shell_identifier = source.chars().all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    });
    if currency || (shell_identifier && source.chars().count() > 1) {
        return false;
    }

    let chars: Vec<_> = source.chars().collect();
    let single_variable = chars.len() == 1 && chars[0].is_alphabetic();
    let tex_command = chars.windows(2).any(|pair| pair[0] == '\\' && pair[1].is_alphabetic());
    let script = source.find(['^', '_']).is_some_and(|index| {
        let (base, suffix) = source.split_at(index);
        let suffix = &suffix[1..];
        explicit_operand(base) && !suffix.trim().is_empty()
    });
    let relation = ["<=", ">=", "!=", "==", "=", "<", ">"].into_iter().any(|operator| {
        source.find(operator).is_some_and(|index| {
            relation_operand(&source[..index])
                && relation_operand(&source[index + operator.len()..])
        })
    });
    let structural = script
        || relation
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

fn explicit_operand(operand: &str) -> bool {
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

fn relation_operand(operand: &str) -> bool {
    let operand = operand.trim();
    explicit_operand(operand)
        || operand.find('(').is_some_and(|open| {
            let name = operand[..open].trim();
            let arguments = operand[open + 1..].strip_suffix(')').unwrap_or("").trim();
            name.chars().count() == 1
                && name.chars().all(char::is_alphabetic)
                && !arguments.is_empty()
        })
}

fn make_overlay(
    grid: &TextGrid,
    open: GridPosition,
    after: GridPosition,
    source: Box<str>,
    kind: DelimiterKind,
) -> FormulaOverlay {
    let display = matches!(kind, DelimiterKind::DollarDisplay | DelimiterKind::BracketDisplay);
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
        background: Rgb::default(),
        fallback: Vec::new(),
    }
}

/// Draw prepared overlays after terminal rectangles. A layout is fully built
/// before its source-covering quads are submitted, so parser/layout failures
/// leave the original grid untouched.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_overlays(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    state: &mut TerminalMathState,
    overlays: &[FormulaOverlay],
    size: &SizeInfo,
    font_pixel_size: f32,
    pixels_per_point: f32,
) {
    for overlay in overlays {
        let Some(bounds) = overlay.bounds(size) else {
            continue;
        };
        let available_width = (bounds.width() - FORMULA_INSET * 2.0).max(1.0);
        let available_height = (bounds.height() - FORMULA_INSET * 2.0).max(1.0);

        let base_metrics = match state.layout(
            overlay.formula_id,
            &overlay.source,
            font_pixel_size,
            pixels_per_point,
            overlay.display,
        ) {
            Ok(layout) => layout.metrics,
            Err(_) => continue,
        };
        let total_height = (base_metrics.height + base_metrics.depth).max(1.0);
        let fit = (available_width / base_metrics.width.max(1.0))
            .min(available_height / total_height)
            .min(1.0);
        let fitted_pixel_size = font_pixel_size * fit * 0.96;
        if !fitted_pixel_size.is_finite() || fitted_pixel_size < MIN_MATH_PIXEL_SIZE {
            continue;
        }

        let layout = match state.layout(
            overlay.formula_id,
            &overlay.source,
            fitted_pixel_size,
            pixels_per_point,
            overlay.display,
        ) {
            Ok(layout) => layout,
            Err(_) => continue,
        };
        let total_height = layout.metrics.height + layout.metrics.depth;
        let origin_x = bounds.left + (bounds.width() - layout.metrics.width) / 2.0;
        let baseline_y =
            bounds.top + (bounds.height() - total_height) / 2.0 + layout.metrics.height;

        let quads: Vec<_> = overlay
            .spans
            .iter()
            .filter(|span| {
                span.row >= 0
                    && usize::try_from(span.row).is_ok_and(|row| row < size.screen_lines())
            })
            .map(|span| {
                UiQuad::solid(
                    size.padding_x() + span.start as f32 * size.cell_width(),
                    size.padding_y() + span.row as f32 * size.cell_height(),
                    (span.end - span.start) as f32 * size.cell_width(),
                    size.cell_height(),
                    0.0,
                    Rgba::opaque(overlay.background),
                )
            })
            .collect();
        renderer.draw_ui(size, &quads);

        let viewport_right = size.padding_x() + size.columns() as f32 * size.cell_width();
        let viewport_bottom = size.padding_y() + size.screen_lines() as f32 * size.cell_height();
        let clip = MathClip {
            left: bounds.left.max(size.padding_x()),
            top: bounds.top.max(size.padding_y()),
            right: bounds.right.min(viewport_right),
            bottom: bounds.bottom.min(viewport_bottom),
        };
        if clip.right <= clip.left || clip.bottom <= clip.top {
            continue;
        }
        if renderer.draw_math(size, layout, origin_x, baseline_y, overlay.foreground, clip).is_err()
        {
            renderer.draw_cells(size, glyph_cache, overlay.fallback.iter().cloned());
            continue;
        }

        let base_ascent = (size.cell_height() + glyph_cache.font_metrics().descent).max(1.0);
        for operation in &layout.text {
            let scale = operation.pixel_size / fitted_pixel_size;
            let x = origin_x + operation.x;
            let y = baseline_y + operation.baseline_y - base_ascent * scale;
            let width = size.cell_width() * scale;
            let height = size.cell_height() * scale;
            if x < clip.left || x + width > clip.right || y < clip.top || y + height > clip.bottom {
                continue;
            }
            let mut text = [0u8; 4];
            renderer.draw_doc_text(
                size,
                x,
                y,
                scale,
                overlay.foreground,
                Flags::empty(),
                operation.character.encode_utf8(&mut text),
                glyph_cache,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(rows: &[&str], allow_inline: bool) -> Vec<(String, bool)> {
        scan_grid(&TextGrid::from_rows(rows), allow_inline)
            .into_iter()
            .map(|formula| (formula.source.to_string(), formula.display))
            .collect()
    }

    fn grid_at(rows: &[&str], absolute_top: usize, scrolled_out: usize) -> TextGrid {
        let mut grid = TextGrid::from_rows(rows);
        grid.absolute_top = absolute_top;
        grid.scrolled_out = scrolled_out;
        grid
    }

    fn remember_visible(state: &mut TerminalMathState, grid: &TextGrid) {
        state.synchronize_grid(grid);
        for overlay in scan_grid(grid, true) {
            state.remember(grid, &overlay);
        }
    }

    #[test]
    fn recognizes_cli_math_delimiters_and_utf8_prose() {
        assert_eq!(
            sources(&[r"中文 \(x^2+y^2=z^2\) and $\alpha+1$"], true),
            vec![("x^2+y^2=z^2".into(), false), (r"\alpha+1".into(), false)]
        );
    }

    #[test]
    fn display_math_can_cross_hard_terminal_rows() {
        assert_eq!(
            sources(&["answer:", "$$", r"\frac{1}{2} + x^2", "$$", "done"], false),
            vec![(r"\frac{1}{2} + x^2".into(), true)]
        );
        assert_eq!(
            sources(&[r"\[\sum_{i=1}^n i\]"], false),
            vec![(r"\sum_{i=1}^n i".into(), true)]
        );

        let multiline = sources(
            &["$$", r"\begin{aligned}", r"f(x) &= x^2 \\", r"g(x) &= x+1", r"\end{aligned}", "$$"],
            false,
        );
        assert_eq!(multiline.len(), 1);
        assert!(multiline[0].0.contains('\n'));
        assert!(multiline[0].1);
    }

    #[test]
    fn screenshot_cli_formulas_reach_the_shared_compiler() {
        let samples = [
            sources(&["$$", r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}", "$$"], false),
            sources(
                &["$$", r"f(x)=\begin{cases}", "x^2,&x\\geq 0\\", r"-x,&x<0", r"\end{cases}", "$$"],
                false,
            ),
            sources(
                &[
                    "$$",
                    r"A=\begin{pmatrix}",
                    r"1&amp;2&amp;3\&amp;nbsp;",
                    r"4&amp;5&amp;6\&amp;#160;",
                    r"7&amp;8&amp;9",
                    r"\end{pmatrix}",
                    "$$",
                ],
                false,
            ),
        ];

        for extracted in samples {
            let [(source, true)] = extracted.as_slice() else {
                panic!("expected one display formula, got {extracted:?}");
            };
            let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
                .unwrap_or_else(|error| panic!("CLI formula failed: {source:?}: {error:?}"));
            assert!(layout.metrics.width > 0.0 && layout.metrics.height > 0.0);
        }
    }

    #[test]
    fn assistant_aligned_formula_with_row_spacing_reaches_native_layout() {
        let source = r"\begin{aligned}
\Psi(x,t)
&=
\sum_{n=1}^{\infty}
c_n
\sqrt{\frac{2}{L}}
\sin\left(\frac{n\pi x}{L}\right)
\exp\left(-\frac{i n^2\pi^2\hbar}{2mL^2}t\right), \\[6pt]
\int_{0}^{L}\left|\Psi(x,t)\right|^2\,dx
&= 1,
\qquad
E_n=\frac{n^2\pi^2\hbar^2}{2mL^2}, \\[6pt]
\mathbf{A}^{-1}
&=
\frac{1}{ad-bc}
\begin{bmatrix}
d & -b \\
-c & a
\end{bmatrix},
\qquad ad-bc\ne 0, \\[6pt]
\lim_{N\to\infty}
\sum_{k=1}^{N}\frac{(-1)^{k+1}}{k^2}
&=
\frac{\pi^2}{12},
\qquad
\int_{-\infty}^{\infty}e^{-x^2}\,dx=\sqrt{\pi}.
\end{aligned}";

        let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
            .unwrap_or_else(|error| panic!("formula compile failed: {error:?}"));

        assert!(!layout.glyphs.is_empty());
    }

    #[test]
    fn escaped_dollars_currency_and_shell_variables_stay_literal() {
        assert!(sources(&[r"escaped \$x$"], true).is_empty());
        assert!(sources(&["price $5$ and $12.50$"], true).is_empty());
        assert!(sources(&["echo $HOME/$USER"], true).is_empty());
        assert!(sources(&["env $LONG_VARIABLE$"], true).is_empty());
        assert!(sources(&["quote $USD 20$ today"], true).is_empty());
        assert!(sources(&["literal $hello$ text"], true).is_empty());
        assert!(sources(&["path $foo/bar$"], true).is_empty());
        assert!(sources(&["total $10 USD$"], true).is_empty());
        assert!(sources(&["literal $hello_world$"], true).is_empty());
        assert!(sources(&["date $2026-07-19$"], true).is_empty());
        assert!(sources(&["config $PATH=/tmp$"], true).is_empty());
        assert!(sources(&["echo $$", "pid", "echo $$"], true).is_empty());
        assert!(sources(&[r"plain \(normal prose\)"], true).is_empty());
        assert!(sources(&["$$hello world$$"], true).is_empty());
    }

    #[test]
    fn single_dollar_accepts_only_explicit_math_shapes() {
        assert_eq!(sources(&["$x$ $x_1$ $2+2$ $a/b$ $x=y$ $f(x)$ $f(x)=0$"], true).len(), 7);
        assert_eq!(sources(&[r"$\frac{1}{2}$ $\sin x$ $x^2$"], true).len(), 3);
    }

    #[test]
    fn single_dollar_requires_an_ai_cli_context() {
        assert!(sources(&["Euler: $e^{i\\pi}+1=0$"], false).is_empty());
        assert_eq!(
            sources(&["Euler: $e^{i\\pi}+1=0$"], true),
            vec![(r"e^{i\pi}+1=0".into(), false)]
        );
    }

    #[test]
    fn ai_context_survives_command_completion_and_cache_clone_resets_cleanly() {
        let mut state = TerminalMathState::default();
        state.observe_program(Some("codex"));
        state.observe_program(None);
        assert!(state.inline_dollar_enabled());
        assert!(state.clone().inline_dollar_enabled());
    }

    #[test]
    fn persisted_formula_survives_partial_scroll_without_rescaling_its_bounds() {
        let mut state = TerminalMathState::default();
        let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
        remember_visible(&mut state, &initial);
        assert_eq!(state.formulas.len(), 1);

        let scrolled = grid_at(&["x^2  ", "$$   ", "tail ", "next "], 41, 0);
        state.synchronize_grid(&scrolled);
        assert!(scan_grid(&scrolled, true).is_empty());
        let overlays = state.visible_overlays(&scrolled);

        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].source.as_ref(), "x^2");
        assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-1));
        assert_eq!(overlays[0].spans.last().map(|span| span.row), Some(1));
    }

    #[test]
    fn streamed_display_formula_completes_after_opening_scrolls_into_history() {
        let mut state = TerminalMathState::default();
        let opening = grid_at(
            &[
                "$$                      ",
                r"\begin{aligned}         ",
                r"f(x) &= x^2 \\          ",
                "h(x) &= x^3            ",
            ],
            40,
            0,
        );
        state.synchronize_grid(&opening);
        assert!(state.scan_visible_grid(&opening, false).is_none());

        let visible_tail = grid_at(
            &[
                r"g(x) &= x+1            ",
                r"\end{aligned}           ",
                "$$                      ",
                "tail                    ",
            ],
            44,
            0,
        );
        state.synchronize_grid(&visible_tail);
        let history_anchor = state
            .scan_visible_grid(&visible_tail, false)
            .expect("closing delimiter requests the pending opening from history");
        assert_eq!(history_anchor, FormulaAnchor { row: 40, column: 0 });

        let history = grid_at(
            &[
                "$$                      ",
                r"\begin{aligned}         ",
                r"f(x) &= x^2 \\          ",
                "h(x) &= x^3            ",
                "g(x) &= x+1            ",
                r"\end{aligned}           ",
                "$$                      ",
                "tail                    ",
            ],
            40,
            0,
        );
        assert!(state.complete_pending_from_history(&history, false));

        let overlays = state.visible_overlays(&visible_tail);
        assert_eq!(overlays.len(), 1);
        assert!(overlays[0].source.contains("f(x)"));
        assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-4));
        assert_eq!(overlays[0].spans.last().map(|span| span.row), Some(2));
    }

    #[test]
    fn visible_content_mismatch_drops_a_persisted_formula() {
        let mut state = TerminalMathState::default();
        let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
        remember_visible(&mut state, &initial);

        let changed = grid_at(&["y^2  ", "$$   ", "tail ", "next "], 41, 0);
        state.synchronize_grid(&changed);
        assert!(state.visible_overlays(&changed).is_empty());
        assert!(state.formulas.is_empty());
    }

    #[test]
    fn reflow_history_pruning_and_memory_limits_invalidate_bounded_state() {
        let mut state = TerminalMathState::default();
        for index in 0..MAX_PERSISTED_FORMULAS + 32 {
            let absolute_top = index * 3;
            let grid = grid_at(&["$$ ", "x^2", "$$ "], absolute_top, 0);
            remember_visible(&mut state, &grid);
        }
        assert!(state.formulas.len() <= MAX_PERSISTED_FORMULAS);
        assert!(state.persisted_bytes <= PERSISTED_FORMULA_BUDGET);

        let reflowed = grid_at(&["$$  ", "x^2 ", "$$  "], 0, 0);
        state.synchronize_grid(&reflowed);
        assert!(state.formulas.is_empty());

        let initial = grid_at(&["$$ ", "x^2", "$$ "], 90, 0);
        remember_visible(&mut state, &initial);
        let pruned = grid_at(&["text"], 93, 93);
        state.synchronize_grid(&pruned);
        assert!(state.formulas.is_empty());
    }
}
