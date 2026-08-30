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
use nebula_terminal::index::{Column, Line, Point, Side};
use nebula_terminal::term::cell::Flags;
use nebula_terminal::term::{self, Term};

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::math::cache::{FormulaCacheKey, MathLayoutCache};
use crate::math::layout::{MathLayout, MathMetrics};
use crate::math::{DEFAULT_LIMITS, MIN_READABLE_MATH_PX, compile_formula};
use crate::renderer::math::MathClip;
use crate::renderer::{GlyphCache, Renderer};

const MAX_VISIBLE_FORMULAS: usize = 64;
const MAX_PERSISTED_FORMULAS: usize = 2_048;
const PERSISTED_FORMULA_BUDGET: usize = 1024 * 1024;
const MAX_HISTORY_FORMULA_ROWS: usize = 512;
/// Row budget for the closing search of a **bare** delimiter (`(…)` / `[…]`
/// left over after Markdown ate the backslashes). `MAX_VISIBLE_FORMULAS` caps
/// how many formulas *succeed*, not how many candidates fail, and a bare opener
/// carries no intent of its own — an unclosed `(` would scan to the end of the
/// grid, once per `(`, inside the paint frame. A real display block spans a
/// handful of rows; anything longer is not a formula that got wrapped.
const BARE_PAREN_SEARCH_ROWS: usize = 8;
/// Same budget for the multi-row `[` block. It may legitimately hold an
/// `aligned` environment, so it gets more room than an inline paren.
const BARE_BRACKET_SEARCH_ROWS: usize = 24;
/// Cells the whole frame may spend on closing searches for **bare** delimiters.
///
/// The per-candidate row cap above is not enough on its own: one screen can hold
/// a few thousand unclosed `(`, and 8 rows × 200 columns each still adds up to
/// millions of cell visits per frame. This second gate keeps the frame's worst
/// case proportional to the grid rather than to its square. Running out only
/// costs literal text for the remaining bare candidates — every delimiter that
/// states its own intent (`$$`, `\[`, `\(`, `$`) has an O(1) pre-filter and is
/// never charged here.
const BARE_SEARCH_CELL_BUDGET: usize = 4 * 1024;
const FORMULA_INSET: f32 = 2.0;
/// How many consecutive blank rows one side of a formula may lend it. Two is
/// what an AI answer's `$$` block is normally surrounded by; taking more would
/// start moving the formula visibly away from the text it belongs to.
const MAX_ABSORBED_BLANK_ROWS: usize = 2;
/// Sliver of a lent blank row kept free, in rows, so the ink never reaches the
/// row past it.
const BLANK_ROW_MARGIN: f32 = 0.1;
/// Neighbour state before [`apply_layout_hints`] has looked at the grid: the
/// whole row counts as occupied, so an overlay that somehow skipped the scan
/// gets the line gap and nothing more.
const NEIGHBOUR_UNKNOWN: Option<(usize, usize)> = Some((0, usize::MAX));
/// Bleed budget when the neighbouring row holds text right under the formula:
/// the natural line gap plus the sliver a monospace glyph leaves inside its
/// cell. Sized so an inline fraction at [`crate::math::MIN_SCRIPT_SCALE`] —
/// the tallest thing that routinely shares a row with prose — still renders
/// whole. Anything larger starts landing on the neighbour's glyphs; an
/// unconditional allowance here is what once produced formulas painted over
/// adjacent text.
const BLEED_INTO_PROSE: f32 = 0.2;
/// Display math should read as a separate block. It may use less of an
/// occupied prose row's internal leading than inline math, leaving a small but
/// visible boundary without requiring the emitter to add blank terminal rows.
const DISPLAY_BLEED_INTO_PROSE: f32 = 0.18;
/// Height a formula may overrun its budget by, as a fraction of that budget,
/// before it is scaled down. Sized so an inline fraction — the tallest thing
/// that routinely appears inside one prose row — keeps the terminal font size:
/// its overrun lands in the line gap and the clip trims what is left.
const HEIGHT_OVERRUN_TOLERANCE: f32 = 0.12;

/// Per-pane state. Layout data is intentionally discarded when a pane state is
/// cloned: cloned UI metadata can outlive a renderer/font scale, while layouts
/// are cheap to rebuild and remain bounded by `MathLayoutCache` afterwards.
pub(crate) struct TerminalMathState {
    cache: MathLayoutCache,
    projection: LineProjection,
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
            projection: LineProjection::default(),
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
        Self::default()
    }
}

impl fmt::Debug for TerminalMathState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalMathState")
            .field("projected_spans", &self.projection.spans.len())
            .field("persisted_formulas", &self.formulas.len())
            .field("persisted_bytes", &self.persisted_bytes)
            .finish()
    }
}

impl TerminalMathState {
    pub(crate) fn update_projection(
        &mut self,
        overlays: &[FormulaOverlay],
        prepared: &[Option<PreparedFormula>],
        reflow_inline: bool,
    ) {
        let _ = self.update_projection_with_survivors(overlays, prepared, reflow_inline);
    }

    pub(crate) fn update_projection_with_survivors(
        &mut self,
        overlays: &[FormulaOverlay],
        prepared: &[Option<PreparedFormula>],
        reflow_inline: bool,
    ) -> Vec<bool> {
        if reflow_inline {
            self.projection.rebuild(overlays, prepared)
        } else {
            self.projection.spans.clear();
            vec![true; overlays.len()]
        }
    }

    pub(crate) fn project_cell(&self, point: Point<usize>, columns: usize) -> Option<Point<usize>> {
        self.projection.project_cell(point, columns)
    }

    pub(crate) fn project_formula_background(
        &self,
        point: Point<usize>,
        columns: usize,
    ) -> Option<Point<usize>> {
        self.projection.project_formula_background(point, columns)
    }

    /// 冻结当前帧的稀疏投影给锁外渲染使用。GPUI 的 term 快照与数学扫描在
    /// 同一次锁内完成，后续背景/字形绘制不能再回头读取可能已经变化的状态。
    pub(crate) fn projection_snapshot(&self) -> LineProjection {
        self.projection.clone()
    }

    /// Convert the visual mouse cell back to the immutable terminal-grid cell.
    /// Formula spans are atoms: their left/right halves select the corresponding
    /// source boundary instead of inventing cursor positions inside TeX syntax.
    ///
    /// `viewport_origin` is the renderer's viewport top row: projection spans
    /// live in rendered-viewport coordinates, which can be cropped relative to
    /// the grid while a resize commit is pending.
    pub(crate) fn source_point(
        &self,
        point: Point,
        side: Side,
        viewport_origin: Line,
    ) -> (Point, Side) {
        let Some(viewport_point) = term::point_to_viewport_from(viewport_origin, point) else {
            return (point, side);
        };
        let (source, source_side) = self.projection.source_from_visual(viewport_point, side);
        (term::viewport_to_point_from(viewport_origin, source), source_side)
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
        active_edit_rows: Option<&std::ops::RangeInclusive<usize>>,
    ) -> Option<FormulaAnchor> {
        let mut completed_pending = false;
        let scan = scan_grid_result(grid);
        for overlay in scan.overlays {
            // 当前光标所在的逻辑行仍由 CLI 编辑器拥有。这里必须在持久化
            // 之前排除整段换行链，否则公式会先进入缓存，下一帧又覆盖输入。
            if active_edit_rows.is_some_and(|rows| overlay.intersects_rows(rows)) {
                continue;
            }
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
        if active_edit_rows.is_some_and(|rows| rows.contains(&position.row)) {
            return None;
        }
        let current = FormulaAnchor {
            row: grid.absolute_top.saturating_add(position.row),
            column: position.column,
        };

        match self.pending_display {
            None => {
                self.pending_display =
                    Some(PendingDisplayFormula { anchor: current, kind, attempted_at: None });
                None
            },
            // 同一位置的孤立定界符稳定存在：它既可能是仍在流式输出的开头，
            // 也可能是"公式被误删后只剩下的闭合"。对这个位置回看一次历史，
            // 尝试把完整公式重新组装出来。
            Some(pending) if pending.anchor == current && pending.kind == kind => {
                if pending.attempted_at == Some(current) {
                    None
                } else {
                    self.pending_display =
                        Some(PendingDisplayFormula { attempted_at: Some(current), ..pending });
                    Some(current)
                }
            },
            Some(pending) if pending.kind == kind && pending.anchor < current => {
                if pending.attempted_at == Some(current) {
                    None
                } else {
                    self.pending_display =
                        Some(PendingDisplayFormula { attempted_at: Some(current), ..pending });
                    Some(pending.anchor)
                }
            },
            Some(_) => {
                self.pending_display =
                    Some(PendingDisplayFormula { anchor: current, kind, attempted_at: None });
                None
            },
        }
    }

    fn complete_pending_from_history(&mut self, history: &TextGrid) -> bool {
        let Some(pending) = self.pending_display.take() else {
            return false;
        };

        for overlay in scan_grid_result(history).overlays {
            if !overlay.display {
                continue;
            }
            // 原场景：pending 记录的是已滚入历史的开头。孤立闭合场景：
            // pending 记录的是视口里那个落单的 `$$`，此时要求组装出的公式
            // 恰好以它收尾，才能证明它确实是被误删公式的闭合定界符。
            let opens_at_pending = overlay_anchor(history, &overlay) == Some(pending.anchor);
            let closes_at_pending = overlay.spans.last().is_some_and(|span| {
                usize::try_from(span.row).ok().and_then(|row| history.absolute_top.checked_add(row))
                    == Some(pending.anchor.row)
                    && span.end >= 2
                    && span.end - 2 == pending.anchor.column
            });
            if opens_at_pending || closes_at_pending {
                self.remember(history, &overlay);
                // 放回带 attempted 标记的 pending：孤立闭合在网格里仍会被
                // 扫成 unmatched（persisted 覆盖对扫描不可见），清空会让它
                // 每两帧重建并再次触发历史回看。
                self.pending_display = Some(pending);
                return true;
            }
        }
        // 失败时放回（保留单次尝试标记），否则同一个孤立定界符每帧都会
        // 触发一轮历史扫描。
        self.pending_display = Some(pending);
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
        let replaced: Vec<_> = self
            .formulas
            .iter()
            .filter_map(|(&existing_anchor, existing)| {
                existing.overlaps(&formula).then_some(existing_anchor)
            })
            .collect();
        for existing_anchor in replaced {
            self.remove(existing_anchor);
        }
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
    /// The unmatched-delimiter position that already triggered one history
    /// reconstruction. Each position gets a single attempt: without this, a
    /// delimiter that stays unmatched (e.g. its formula was already persisted)
    /// would re-scan history every frame.
    attempted_at: Option<FormulaAnchor>,
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

    fn overlaps(&self, other: &Self) -> bool {
        self.spans.iter().any(|left| {
            other.spans.iter().any(|right| {
                left.row == right.row && left.start < right.end && right.start < left.end
            })
        })
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
            if grid.span_fingerprint(row, span.start, span.end, span.include_wrap)
                == Some(span.fingerprint)
            {
                compared = true;
                continue;
            }
            // 整段被清空是 TUI 重绘的中间帧（先清行再重画）：跳过比较，
            // 让公式在这一两帧里继续渲染，避免输入期间不停闪回原文。
            // 只要所有可比行都空白（真清屏），compared 保持 false 仍会淘汰。
            if grid.span_is_blank(row, span.start, span.end) {
                continue;
            }
            return false;
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
            fallback: Vec::new(),
            neighbours_above: [NEIGHBOUR_UNKNOWN; MAX_ABSORBED_BLANK_ROWS],
            neighbours_below: [NEIGHBOUR_UNKNOWN; MAX_ABSORBED_BLANK_ROWS],
            formula_neighbours_above: [false; MAX_ABSORBED_BLANK_ROWS],
            formula_neighbours_below: [false; MAX_ABSORBED_BLANK_ROWS],
            widen_right_to: None,
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DelimiterKind {
    DollarInline,
    Parenthesized,
    DollarDisplay,
    BracketDisplay,
    /// Markdown-unescaped `\[ … \]`: bare `[` / `]` left behind by an AI CLI
    /// whose markdown renderer ate the backslashes.
    BareBracketDisplay,
    /// Markdown-unescaped `\( … \)`: bare `( … )` around TeX content.
    BareParenInline,
}

impl DelimiterKind {
    fn is_display(self) -> bool {
        matches!(self, Self::DollarDisplay | Self::BracketDisplay | Self::BareBracketDisplay)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GridPosition {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RowSpan {
    row: i32,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct FormulaOverlay {
    source: Arc<str>,
    display: bool,
    formula_id: u64,
    spans: Vec<RowSpan>,
    foreground: Rgb,
    fallback: Vec<RenderableCell>,
    /// Occupied column range of each neighbouring row, nearest first, capped
    /// at [`MAX_ABSORBED_BLANK_ROWS`] per side. `None` marks a row that can
    /// never get in the way (blank, or outside the viewport where the clip
    /// already stops the ink). Whether an occupied row actually blocks depends
    /// on which columns the formula's ink lands in — prose above a centred
    /// formula usually ends long before it — so the decision belongs to
    /// [`prepare_overlays`], which knows the rendered width.
    neighbours_above: [Option<(usize, usize)>; MAX_ABSORBED_BLANK_ROWS],
    neighbours_below: [Option<(usize, usize)>; MAX_ABSORBED_BLANK_ROWS],
    /// Whether the corresponding occupied neighbour row is painted by another
    /// formula. Formula rows cannot lend even the prose antialiasing sliver:
    /// two adjacent math clips must meet at the row boundary, not overlap it.
    formula_neighbours_above: [bool; MAX_ABSORBED_BLANK_ROWS],
    formula_neighbours_below: [bool; MAX_ABSORBED_BLANK_ROWS],
    /// Right edge (in grid columns) available to a display formula whose rows
    /// hold nothing after the source.
    widen_right_to: Option<usize>,
}

impl FormulaOverlay {
    /// 归一化后的 TeX 源（GPUI 壳绘制/缓存键用）。
    ///
    /// PR #55 重写扫描器时把这个访问器删了，但 `gpui_shell` 的
    /// `math_overlay.rs` 一直在调它——默认 features 不编译 gpui 壳，所以
    /// `cargo build -p nebula` 看不出来。加 feature 才暴露。
    pub(crate) fn source_arc(&self) -> Arc<str> {
        Arc::clone(&self.source)
    }

    /// 源格跨度 `(行, 起, 止)`。GPUI 壳在位图预检之后要按存活的公式重建覆盖
    /// 掩码，因此这里把跨度交出去，而不是让它去猜 `spans` 的内部表示。
    pub(crate) fn covered_spans(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.spans.iter().filter_map(|span| {
            usize::try_from(span.row).ok().map(|row| (row, span.start, span.end))
        })
    }

    fn contains(&self, point: Point<usize>) -> bool {
        self.spans.iter().any(|span| {
            usize::try_from(span.row) == Ok(point.line)
                && (span.start..span.end).contains(&point.column.0)
        })
    }

    fn intersects_rows(&self, rows: &std::ops::RangeInclusive<usize>) -> bool {
        self.spans
            .iter()
            .filter_map(|span| usize::try_from(span.row).ok())
            .any(|row| rows.contains(&row))
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

    /// Per-side vertical room past `bounds()`, in pixels, given how many
    /// neighbouring rows the formula's ink is allowed to reach into. A lent
    /// row keeps a sliver free so antialiasing never touches the row past it;
    /// a row that stays occupied lends only the natural line gap — terminal
    /// glyph ink does not fill the whole cell — because anything larger lands
    /// on that neighbour's glyphs. The fitted size and the clip consume these
    /// same numbers, which is the invariant that keeps formula ink off
    /// adjacent text: whatever the fit could not absorb, the clip crops.
    fn vertical_bleed(&self, size: &SizeInfo, absorbed: (usize, usize)) -> (f32, f32) {
        let budget = |rows: usize, formula_neighbour: bool| {
            size.cell_height()
                * if rows == 0 {
                    if formula_neighbour {
                        0.0
                    } else if self.display {
                        DISPLAY_BLEED_INTO_PROSE
                    } else {
                        BLEED_INTO_PROSE
                    }
                } else {
                    rows as f32 - BLANK_ROW_MARGIN
                }
        };
        (
            budget(absorbed.0, self.formula_neighbours_above[0]),
            budget(absorbed.1, self.formula_neighbours_below[0]),
        )
    }

    /// How many neighbouring rows per side the ink may reach into: the run of
    /// nearest rows whose own ink does not overlap `columns`. Inline math
    /// expands symmetrically — it shares its row with prose, and a one-sided
    /// expansion would visibly lift it off that text's baseline.
    fn absorbable_rows(&self, columns: (usize, usize)) -> (usize, usize) {
        let free = |rows: &[Option<(usize, usize)>; MAX_ABSORBED_BLANK_ROWS]| {
            rows.iter()
                .take_while(|row| {
                    row.is_none_or(|(start, end)| end <= columns.0 || start >= columns.1)
                })
                .count()
        };
        let (above, below) = (free(&self.neighbours_above), free(&self.neighbours_below));
        if self.display { (above, below) } else { (above.min(below), above.min(below)) }
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

    /// Physical rows joined by terminal wrap flags form one editable logical
    /// line. Suppressing only the cursor cell lets an earlier formula on the
    /// same prompt line render while the user is still typing after it.
    fn logical_rows_containing(&self, row: usize) -> Option<std::ops::RangeInclusive<usize>> {
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

    fn previous(&self, position: GridPosition) -> Option<GridPosition> {
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
    fn find_closing(&self, mut position: GridPosition, delimiter: &[char]) -> Option<GridPosition> {
        loop {
            if position.row >= self.rows.len() {
                return None;
            }
            if self.starts_with(position, delimiter) && !self.is_escaped(position) {
                return Some(position);
            }
            let previous_row = position.row;
            position = self.next(position)?;
            if position.row != previous_row && self.span_is_blank(position.row, 0, self.columns) {
                return None;
            }
        }
    }

    /// Closing search for **inline** delimiters. Inline TeX may cross a
    /// terminal's physical row only when that row was produced by a soft wrap:
    /// a real newline still ends the span, so two separate terminal lines can
    /// never accidentally become one formula.
    fn find_closing_soft_wrap(
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
    fn find_closing_bounded(
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

    /// Whether a span currently holds nothing but blanks. TUI redraws clear a
    /// line before repainting it, so a blank span is treated as a transient
    /// state rather than proof that a persisted formula is gone.
    fn span_is_blank(&self, row: usize, start: usize, end: usize) -> bool {
        self.rows.get(row).and_then(|cells| cells.get(start..end)).is_some_and(|cells| {
            cells.iter().all(|cell| cell.is_none_or(|character| character == ' '))
        })
    }

    /// Columns between the first and last non-blank cell of `row`, or `None`
    /// for a blank row. Interior gaps are counted as occupied: a formula that
    /// squeezes its ink between two words of the row above would read as an
    /// overlap even where the cells are technically empty.
    fn row_ink_columns(&self, row: usize) -> Option<(usize, usize)> {
        let cells = self.rows.get(row)?;
        let occupied = |cell: &Option<char>| cell.is_some_and(|character| character != ' ');
        let start = cells.iter().position(occupied)?;
        let end = cells.iter().rposition(occupied)? + 1;
        Some((start, end))
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
fn formula_uses_reasoning_style(overlay: &FormulaOverlay) -> bool {
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
fn apply_layout_hints(overlay: &mut FormulaOverlay, grid: &TextGrid) {
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
fn mark_formula_neighbours(overlays: &mut [FormulaOverlay]) {
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
fn neighbour_rows(
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
fn scan_grid(grid: &TextGrid) -> Vec<FormulaOverlay> {
    scan_grid_result(grid).overlays
}

/// Test-only mirror of the `scan_visible` tail: scan, then let the neighbours
/// hand out the same layout budget the real pipeline would.
#[cfg(test)]
fn scan_grid_with_hints(grid: &TextGrid) -> Vec<FormulaOverlay> {
    let mut overlays = scan_grid(grid);
    for overlay in &mut overlays {
        apply_layout_hints(overlay, grid);
    }
    mark_formula_neighbours(&mut overlays);
    overlays
}

struct GridScanResult {
    overlays: Vec<FormulaOverlay>,
    unmatched_display: Option<(GridPosition, DisplayDelimiterKind)>,
}

fn scan_grid_result(grid: &TextGrid) -> GridScanResult {
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

fn find_formula(
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
        grid.find_closing(source_start, closing)?
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
fn opens_a_block(grid: &TextGrid, open: GridPosition) -> bool {
    span_is_blank_or_markdown_list_marker(grid, open.row, open.column)
}

fn find_dollar_formula(
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
fn find_inline_dollar_closing(grid: &TextGrid, mut position: GridPosition) -> Option<GridPosition> {
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
fn find_bare_bracket_formula(
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
        // 没闭合的 `[` 会把下面一整段正文连同它自己的公式吞成一条候选。
        grid.find_closing_bounded(
            source_start,
            BARE_BRACKET_SEARCH_ROWS,
            budget,
            |grid, position| {
                if position.column == 0
                    && position.row > open.row
                    && grid.span_is_blank(position.row, 0, grid.columns)
                {
                    return None;
                }
                if grid.character(position) != Some(']') || grid.is_escaped(position) {
                    return Some(false);
                }
                Some(
                    grid.span_is_blank(position.row, 0, position.column)
                        && grid.span_is_blank(position.row, position.column + 1, grid.columns),
                )
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
fn span_is_blank_or_markdown_list_marker(grid: &TextGrid, row: usize, end: usize) -> bool {
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
fn find_bare_paren_formula(
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
fn looks_like_compact_equation(source: &str) -> bool {
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

fn compact_equation_source(source: &str) -> &str {
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
fn looks_like_implicit_product_equation(left: &str, right: &str) -> bool {
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
fn has_known_tex_command(source: &str) -> bool {
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
fn standard_formula_source(source: &str, display: bool) -> bool {
    let source = source.trim();
    !obviously_non_math(source) && has_math_evidence(source, display)
}

/// Terminal noise that must never render as math regardless of delimiter
/// strength: comments/URLs (`//`), Windows paths (`:\`), control bytes,
/// currency amounts, and ALL_CAPS shell variables.
fn obviously_non_math(source: &str) -> bool {
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
fn has_math_evidence(source: &str, lax: bool) -> bool {
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

/// Physics writes products without a multiplication sign: `mc^2`, `nRT`, `ma`.
/// [`explicit_operand`] caps identifiers at one letter and therefore rejects
/// every one of them, which is why `$E=mc^2$` used to stay literal. Allow up to
/// three letters here — still no whitespace and no punctuation beyond scripts,
/// so `PATH=/tmp` (slash), `key=value` (five letters) and `npm install`
/// (whitespace) remain outside the formula path.
fn implicit_product_operand(operand: &str) -> bool {
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
fn relation_operand(operand: &str) -> bool {
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
fn bare_formula_source(source: &str, display: bool) -> bool {
    standard_formula_source(source, display)
        && !source.contains(":\\")
        && (has_known_tex_command(source) || looks_like_compact_equation(source))
}

fn make_overlay(
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

/// A formula that passed layout pre-flight: the grid pass omits its source
/// cells and [`draw_overlays`] paints the compiled layout in their place.
/// Every geometric decision is taken here, so the fit and the clip can never
/// disagree about how much room the formula was given.
#[derive(Clone, Copy)]
pub(crate) struct PreparedFormula {
    fitted_pixel_size: f32,
    /// Typesetting style actually used. A block formula that does not fit its
    /// row falls back to text style before it gives up any size: readers judge
    /// a formula by how big its letters are next to the prose, not by whether
    /// `\sum` carries its limits above or beside it.
    display_style: bool,
    /// Room past `bounds()` on each side, and the right edge of the layout
    /// box: [`draw_overlays`] consumes these instead of recomputing them.
    bleed_top: f32,
    bleed_bottom: f32,
    box_right: f32,
    centered: bool,
    /// Quantized visual width for a single-row inline formula. Display formulas
    /// retain their source-grid box and therefore do not participate here.
    compact_cells: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct ProjectionSpan {
    source_index: usize,
    row: usize,
    source_start: usize,
    source_end: usize,
    visual_cells: usize,
    shift_before: isize,
    shift_after: isize,
}

/// Sparse source-to-screen mapping for compact inline formulas in the current
/// viewport. One entry represents one formula; no per-cell objects are built.
#[derive(Clone, Debug, Default)]
pub(crate) struct LineProjection {
    spans: Vec<ProjectionSpan>,
}

impl LineProjection {
    /// 返回与 `overlays` 对齐的存活表。流式输出可能短暂产生重叠公式，调用方
    /// 必须和投影采用同一裁决，否则位图、coverage 与后续文字会落在三套坐标上。
    fn rebuild(
        &mut self,
        overlays: &[FormulaOverlay],
        prepared: &[Option<PreparedFormula>],
    ) -> Vec<bool> {
        self.spans.clear();
        let mut retained = vec![true; overlays.len()];
        // `reserve` after `clear` reuses the existing allocation on stable
        // frames and grows at most with the visible formula count.
        self.spans.reserve(overlays.len());
        for (source_index, (overlay, prepared)) in overlays.iter().zip(prepared).enumerate() {
            let Some(visual_cells) = prepared.and_then(|prepared| prepared.compact_cells) else {
                continue;
            };
            let [span] = overlay.spans.as_slice() else { continue };
            let Ok(row) = usize::try_from(span.row) else { continue };
            if span.end <= span.start {
                continue;
            }
            self.spans.push(ProjectionSpan {
                source_index,
                row,
                source_start: span.start,
                source_end: span.end,
                visual_cells,
                shift_before: 0,
                shift_after: 0,
            });
        }
        self.spans.sort_unstable_by(|left, right| {
            (left.row, left.source_start)
                .cmp(&(right.row, right.source_start))
                .then_with(|| right.source_end.cmp(&left.source_end))
        });

        // Streaming TUIs can briefly expose an inner formula before its outer
        // delimiter arrives. Persistence normally replaces that stale entry,
        // but projection remains defensive: prefer the widest earlier span and
        // discard any overlapping remainder instead of panicking in a frame.
        let mut retained_row = None;
        let mut retained_end = 0usize;
        self.spans.retain(|span| {
            if retained_row != Some(span.row) {
                retained_row = Some(span.row);
                retained_end = 0;
            }
            if span.source_start < retained_end {
                retained[span.source_index] = false;
                return false;
            }
            retained_end = span.source_end;
            true
        });

        let mut row = None;
        let mut shift = 0isize;
        for span in &mut self.spans {
            if row != Some(span.row) {
                row = Some(span.row);
                shift = 0;
            }
            span.shift_before = shift;
            let source_cells = span.source_end - span.source_start;
            shift = shift.saturating_add(span.visual_cells as isize - source_cells as isize);
            span.shift_after = shift;
        }
        retained
    }

    #[cfg(test)]
    fn build(overlays: &[FormulaOverlay], prepared: &[Option<PreparedFormula>]) -> Self {
        let mut projection = Self::default();
        let _ = projection.rebuild(overlays, prepared);
        projection
    }

    fn row_spans(&self, row: usize) -> &[ProjectionSpan] {
        let start = self.spans.partition_point(|span| span.row < row);
        let end = start + self.spans[start..].partition_point(|span| span.row == row);
        &self.spans[start..end]
    }

    fn shift_before(&self, row: usize, source_column: usize) -> isize {
        self.row_spans(row)
            .iter()
            .take_while(|span| span.source_end <= source_column)
            .last()
            .map_or(0, |span| span.shift_after)
    }

    pub(crate) fn project_cell(&self, point: Point<usize>, columns: usize) -> Option<Point<usize>> {
        let spans = self.row_spans(point.line);
        let preceding = spans.partition_point(|span| span.source_start <= point.column.0);
        let shift = match preceding.checked_sub(1).and_then(|index| spans.get(index)) {
            Some(span) if point.column.0 < span.source_end => return None,
            Some(span) => span.shift_after,
            None => 0,
        };
        let column = apply_shift(point.column.0, shift);
        (column < columns).then(|| Point::new(point.line, Column(column)))
    }

    /// Map blanked source cells onto the compact visual box so their resolved
    /// ANSI background remains behind an inline formula. Surplus source cells
    /// are discarded when TeX is wider than the rendered formula.
    pub(crate) fn project_formula_background(
        &self,
        point: Point<usize>,
        columns: usize,
    ) -> Option<Point<usize>> {
        let span = self
            .row_spans(point.line)
            .iter()
            .find(|span| (span.source_start..span.source_end).contains(&point.column.0));
        let Some(span) = span else {
            return (point.column.0 < columns).then_some(point);
        };
        let relative = point.column.0 - span.source_start;
        if relative >= span.visual_cells {
            return None;
        }
        let visual_start = apply_shift(span.source_start, span.shift_before);
        let column = visual_start.saturating_add(relative);
        (column < columns).then(|| Point::new(point.line, Column(column)))
    }

    fn source_from_visual(&self, point: Point<usize>, side: Side) -> (Point<usize>, Side) {
        let spans = self.row_spans(point.line);
        let visual_column = point.column.0;
        let mut shift = 0isize;

        for span in spans {
            let visual_start = apply_shift(span.source_start, span.shift_before);
            let visual_end = visual_start.saturating_add(span.visual_cells);
            if visual_column < visual_start {
                break;
            }
            if visual_column < visual_end {
                let relative_twice = (visual_column - visual_start)
                    .saturating_mul(2)
                    .saturating_add(usize::from(side == Side::Right));
                return if relative_twice < span.visual_cells {
                    (Point::new(point.line, Column(span.source_start)), Side::Left)
                } else {
                    (Point::new(point.line, Column(span.source_end.saturating_sub(1))), Side::Right)
                };
            }
            shift = span.shift_after;
        }

        let source_column = apply_shift(visual_column, shift.saturating_neg());
        (Point::new(point.line, Column(source_column)), side)
    }
}

fn apply_shift(column: usize, shift: isize) -> usize {
    if shift >= 0 {
        column.saturating_add(shift as usize)
    } else {
        column.saturating_sub(shift.unsigned_abs())
    }
}

fn quantized_visual_cells(width: f32, cell_width: f32, remaining_columns: usize) -> usize {
    let cells = ((width + FORMULA_INSET * 2.0) / cell_width.max(1.0)).ceil().max(1.0) as usize;
    cells.min(remaining_columns.max(1))
}

/// Largest uniform scale at which `metrics` fits the given room, capped at 1.
///
/// Height is allowed to overrun by [`HEIGHT_OVERRUN_TOLERANCE`] before the
/// size gives way. Formulas of the same kind reading at the same size is what
/// users actually notice; a few percent of height costs the outermost row of
/// antialiasing, which the clip trims at the budget anyway. Width gets no such
/// tolerance — what overruns there is a real symbol at the right edge, and
/// cropping it loses content.
/// A formula-to-formula boundary is a hard clip: unlike a prose gap it cannot
/// absorb a nominal overrun without cutting a neighbouring superscript.
fn fit_ratio(
    metrics: &MathMetrics,
    available_width: f32,
    available_height: f32,
    height_overrun_tolerance: f32,
) -> f32 {
    let total_height = (metrics.height + metrics.depth).max(1.0);
    let height_fit = available_height / total_height;
    let height_fit = if height_fit >= 1.0 - height_overrun_tolerance { 1.0 } else { height_fit };
    (available_width / metrics.width.max(1.0)).min(height_fit).min(1.0)
}

/// Grid columns the ink of a `width`-wide layout would occupy, following the
/// same placement rule [`draw_overlays`] uses. Measured at the terminal font
/// size, which is the widest the formula can end up: shrinking only pulls the
/// ink further inside this range, so neighbours judged clear stay clear.
fn ink_columns(
    overlay: &FormulaOverlay,
    size: &SizeInfo,
    bounds: FormulaBounds,
    box_right: f32,
    width: f32,
) -> (usize, usize) {
    let box_width = box_right - bounds.left;
    let left = if overlay.display || width >= box_width * 0.75 {
        bounds.left + (box_width - width) / 2.0
    } else {
        bounds.left + FORMULA_INSET
    }
    .max(bounds.left);
    let column = |x: f32| ((x - size.padding_x()) / size.cell_width().max(1.0)).max(0.0);
    (column(left).floor() as usize, column(left + width).ceil() as usize)
}

/// Pre-compile every overlay before the cell pass so the renderer knows which
/// source cells to skip. Returns one entry per overlay; `None` keeps the raw
/// source visible (layout failure, or the fitted size would be unreadable).
pub(crate) fn prepare_overlays(
    state: &mut TerminalMathState,
    overlays: &[FormulaOverlay],
    size: &SizeInfo,
    font_pixel_size: f32,
    pixels_per_point: f32,
) -> Vec<Option<PreparedFormula>> {
    overlays
        .iter()
        .map(|overlay| {
            let bounds = overlay.bounds(size)?;
            let viewport_right = size.padding_x() + size.columns() as f32 * size.cell_width();
            // Both candidate styles are measured at the terminal font size
            // first: the widest of them decides which columns the ink can
            // possibly land in, and only rows holding text in those columns
            // are in the way. Prose above a centred formula usually stops long
            // before it, which is what lets a `$$` block wedged between two
            // list items still use their vertical space.
            let styles: &[bool] = if overlay.display { &[true, false] } else { &[false] };
            let mut widest = 0.0f32;
            for &display_style in styles {
                let metrics = state
                    .layout(
                        overlay.formula_id,
                        &overlay.source,
                        font_pixel_size,
                        pixels_per_point,
                        display_style,
                    )
                    .ok()?
                    .metrics;
                widest = widest.max(metrics.width);
            }
            let compact_inline = !overlay.display && overlay.spans.len() == 1;
            let source_start = overlay.spans.first()?.start;
            let remaining_columns = size.columns().saturating_sub(source_start);
            let natural_cells =
                quantized_visual_cells(widest, size.cell_width(), remaining_columns);
            let box_right = if compact_inline {
                (bounds.left + natural_cells as f32 * size.cell_width()).min(viewport_right)
            } else if let Some(right_column) = overlay.widen_right_to {
                (size.padding_x() + right_column as f32 * size.cell_width()).max(bounds.right)
            } else {
                bounds.right
            };
            let available_width = (box_right - bounds.left - FORMULA_INSET * 2.0).max(1.0);
            let absorbed =
                overlay.absorbable_rows(ink_columns(overlay, size, bounds, box_right, widest));
            // Vertical room is the bounds plus each side's budget. Rows the
            // ink may reach into effectively hand a formula their whole
            // height, so `$$` blocks render at the terminal font size like the
            // prose around them; a formula truly hemmed in by text only gets
            // the line gap.
            let (bleed_top, bleed_bottom) = overlay.vertical_bleed(size, absorbed);
            // 相邻公式共享的是硬边界，不能沿用正文行的 12% 容差，否则上标会先被裁掉。
            let formula_boundary = (absorbed.0 == 0 && overlay.formula_neighbours_above[0])
                || (absorbed.1 == 0 && overlay.formula_neighbours_below[0]);
            let height_overrun_tolerance =
                if formula_boundary { 0.0 } else { HEIGHT_OVERRUN_TOLERANCE };
            // 2026-08-05 裁定（用户两次实测后）：字号一致优先于呼吸感。这里
            // 曾经先扣一条"留白边距"再去 fit，结果被长正文夹住的 `$$` 掉到
            // 74–76%，而前后有空行的同款仍是 100%——同组两个尺寸，正是要消
            // 除的现象。固定行高里造不出高度，留白只能靠 emitter 在 `$$` 前
            // 后留空行。别再加回来。
            let available_height = (bounds.height() + bleed_top + bleed_bottom).max(1.0);

            // Style ladder before size ladder. A `$$` block wedged between two
            // prose rows has barely one row of height, and display style wants
            // two: `\sum` stacks its limits, fractions stay full size. Laying
            // that same source out in text style keeps the letters at the
            // terminal font size and moves the limits beside the operator —
            // exactly what TeX does for math inside a paragraph. Shrinking
            // uniformly is the last resort, because that is what makes a
            // formula read as "too small next to the text".
            let mut best: Option<(bool, f32)> = None;
            for &display_style in styles {
                let metrics = state
                    .layout(
                        overlay.formula_id,
                        &overlay.source,
                        font_pixel_size,
                        pixels_per_point,
                        display_style,
                    )
                    .ok()?
                    .metrics;
                let fit = fit_ratio(
                    &metrics,
                    available_width,
                    available_height,
                    height_overrun_tolerance,
                );
                if fit >= 1.0 {
                    best = Some((display_style, font_pixel_size));
                    break;
                }
                // Math layout is linear in pixel size only approximately; the
                // same rounding margin the markdown reader uses keeps the
                // re-laid-out ink inside the budget the clip will enforce.
                let fitted = font_pixel_size * fit * 0.98;
                if best.is_none_or(|(_, previous)| fitted > previous) {
                    best = Some((display_style, fitted));
                }
            }

            let (display_style, fitted_pixel_size) = best?;
            if !fitted_pixel_size.is_finite() || fitted_pixel_size < MIN_READABLE_MATH_PX {
                return None;
            }
            let metrics = state
                .layout(
                    overlay.formula_id,
                    &overlay.source,
                    fitted_pixel_size,
                    pixels_per_point,
                    display_style,
                )
                .ok()?
                .metrics;
            let compact_cells = compact_inline.then(|| {
                quantized_visual_cells(metrics.width, size.cell_width(), remaining_columns)
            });
            let box_right = compact_cells
                .map_or(box_right, |cells| bounds.left + cells as f32 * size.cell_width());
            // Display math centers like block typography. Inline math whose
            // source is replaced by a compact cell box centres inside that
            // quantized box; it never centres inside the old source span.
            let centered = overlay.display
                || compact_inline
                || metrics.width >= (box_right - bounds.left) * 0.75;
            Some(PreparedFormula {
                fitted_pixel_size,
                display_style,
                bleed_top,
                bleed_bottom,
                box_right,
                centered,
                compact_cells,
            })
        })
        .collect()
}

/// Row→span lookup of source cells covered by formulas that WILL render.
/// The grid pass keeps each source cell's resolved background but suppresses
/// its glyph and decorations, so full-screen TUIs retain a continuous surface.
#[derive(Default)]
pub(crate) struct CoverageMask {
    rows: BTreeMap<usize, Vec<(usize, usize)>>,
}

impl CoverageMask {
    /// `gates` 与 overlays 一一对应；`None` 的公式保留原文（其源格不进
    /// 掩码）。旧壳传 `Option<PreparedFormula>`，GPUI 壳传
    /// `Option<OverlayDrawPlan>`——覆盖判定与各自的回退路径保持一致。
    pub(crate) fn build<T>(overlays: &[FormulaOverlay], gates: &[Option<T>]) -> Self {
        Self::from_spans(
            overlays
                .iter()
                .zip(gates)
                .filter(|(_, gate)| gate.is_some())
                .flat_map(|(overlay, _)| overlay.covered_spans()),
        )
    }

    /// 由调用方自己挑出的源格跨度重建掩码。GPUI 壳在位图预检之后用它：那一步
    /// 之前拿不到位图的公式还留在 `build` 的结果里，必须整条剔除。
    pub(crate) fn from_spans(spans: impl IntoIterator<Item = (usize, usize, usize)>) -> Self {
        let mut rows: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        for (row, start, end) in spans {
            rows.entry(row).or_default().push((start, end));
        }
        Self { rows }
    }

    pub(crate) fn covers(&self, point: Point<usize>) -> bool {
        self.rows.get(&point.line).is_some_and(|spans| {
            spans.iter().any(|&(start, end)| (start..end).contains(&point.column.0))
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// 一个 overlay 的后端无关绘制计划：几何决策（fit/居中/bleed/裁剪）的
/// 单一出口，OpenGL（旧壳 [`draw_overlays`]）与 GPUI 壳共同消费，保证两壳
/// 的公式落点与裁剪像素级同源。坐标系与 [`SizeInfo`] 一致。
#[derive(Clone, Copy, Debug)]
pub(crate) struct OverlayDrawPlan {
    pub(crate) fitted_pixel_size: f32,
    pub(crate) display_style: bool,
    pub(crate) origin_x: f32,
    pub(crate) baseline_y: f32,
    pub(crate) clip_left: f32,
    pub(crate) clip_top: f32,
    pub(crate) clip_right: f32,
    pub(crate) clip_bottom: f32,
    pub(crate) foreground: Rgb,
}

/// 计算一个已通过预检的 overlay 的绘制几何。`None` = 本帧回退原文
/// （布局失败或裁剪退化），调用方自行决定回退方式（旧壳补画源格，GPUI
/// 壳保留源格不跳过）。
pub(crate) fn plan_overlay_draw(
    state: &mut TerminalMathState,
    overlay: &FormulaOverlay,
    prepared: &PreparedFormula,
    size: &SizeInfo,
    pixels_per_point: f32,
) -> Option<OverlayDrawPlan> {
    let mut bounds = overlay.bounds(size)?;
    let shift_columns = match overlay.spans.as_slice() {
        [span] => usize::try_from(span.row)
            .ok()
            .map_or(0, |row| state.projection.shift_before(row, span.start)),
        _ => 0,
    };
    let shift_pixels = shift_columns as f32 * size.cell_width();
    bounds.left += shift_pixels;
    bounds.right += shift_pixels;
    let projected_box_right = prepared.box_right + shift_pixels;

    // Pre-flight succeeded, so failure here is unreachable in practice.
    let metrics = state
        .layout(
            overlay.formula_id,
            &overlay.source,
            prepared.fitted_pixel_size,
            pixels_per_point,
            prepared.display_style,
        )
        .ok()?
        .metrics;
    let total_height = metrics.height + metrics.depth;
    let viewport_right = size.padding_x() + size.columns() as f32 * size.cell_width();
    let viewport_bottom = size.padding_y() + size.screen_lines() as f32 * size.cell_height();
    let box_right = projected_box_right;
    let box_width = box_right - bounds.left;
    let origin_x = if prepared.centered {
        bounds.left + (box_width - metrics.width) / 2.0
    } else {
        bounds.left + FORMULA_INSET
    };
    // Centre inside the source rows while the ink fits them. What does not
    // fit is split between the sides in proportion to what each side can
    // actually lend: with a blank row above and prose below, the overflow
    // goes up, and the formula keeps the full line gap between itself and
    // the text underneath instead of splitting the crowding evenly.
    let slack = bounds.height() - total_height;
    let room = prepared.bleed_top + prepared.bleed_bottom;
    let top = if slack >= 0.0 {
        bounds.top + slack / 2.0
    } else if room > 0.0 {
        bounds.top + slack * (prepared.bleed_top / room)
    } else {
        bounds.top + slack / 2.0
    };
    let baseline_y = (top + metrics.height)
        .max(bounds.top - prepared.bleed_top + metrics.height)
        .min(bounds.bottom + prepared.bleed_bottom - metrics.depth);

    // The clip follows the bleed budget, never the ink: whatever the fit
    // could not absorb gets cropped instead of landing on neighbouring
    // prose. (Following the ink is what painted formulas over adjacent
    // rows.)
    let clip_left = bounds.left.max(size.padding_x());
    let clip_top = (bounds.top - prepared.bleed_top).max(size.padding_y());
    let clip_right = box_right.min(viewport_right);
    let clip_bottom = (bounds.bottom + prepared.bleed_bottom).min(viewport_bottom);
    if clip_right <= clip_left || clip_bottom <= clip_top {
        return None;
    }
    Some(OverlayDrawPlan {
        fitted_pixel_size: prepared.fitted_pixel_size,
        display_style: prepared.display_style,
        origin_x,
        baseline_y,
        clip_left,
        clip_top,
        clip_right,
        clip_bottom,
        foreground: overlay.foreground,
    })
}

/// Draw prepared overlays after terminal rectangles. The grid pass has already
/// replaced source glyphs with spaces while retaining their resolved terminal
/// backgrounds, so no opaque cover quad is needed here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_overlays(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    state: &mut TerminalMathState,
    overlays: &[FormulaOverlay],
    prepared: &[Option<PreparedFormula>],
    size: &SizeInfo,
    pixels_per_point: f32,
) {
    for (overlay, prepared) in overlays.iter().zip(prepared) {
        let Some(prepared) = prepared else {
            continue;
        };
        let Some(plan) = plan_overlay_draw(state, overlay, prepared, size, pixels_per_point) else {
            // Repaint the skipped source cells rather than leave a hole.
            renderer.draw_cells(size, glyph_cache, overlay.fallback.iter().cloned());
            continue;
        };
        let layout = match state.layout(
            overlay.formula_id,
            &overlay.source,
            plan.fitted_pixel_size,
            pixels_per_point,
            plan.display_style,
        ) {
            Ok(layout) => layout,
            Err(_) => {
                renderer.draw_cells(size, glyph_cache, overlay.fallback.iter().cloned());
                continue;
            },
        };
        let clip = MathClip {
            left: plan.clip_left,
            top: plan.clip_top,
            right: plan.clip_right,
            bottom: plan.clip_bottom,
        };
        if renderer
            .draw_math(size, layout, plan.origin_x, plan.baseline_y, plan.foreground, clip)
            .is_err()
        {
            renderer.draw_cells(size, glyph_cache, overlay.fallback.iter().cloned());
            continue;
        }

        let base_ascent = (size.cell_height() + glyph_cache.font_metrics().descent).max(1.0);
        for operation in &layout.text {
            let scale = operation.pixel_size / plan.fitted_pixel_size;
            let x = plan.origin_x + operation.x;
            let y = plan.baseline_y + operation.baseline_y - base_ascent * scale;
            let width = size.cell_width() * scale;
            let height = size.cell_height() * scale;
            if x < plan.clip_left
                || x + width > plan.clip_right
                || y < plan.clip_top
                || y + height > plan.clip_bottom
            {
                continue;
            }
            let mut text = [0u8; 4];
            renderer.draw_doc_text(
                size,
                x,
                y,
                scale,
                plan.foreground,
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

    fn overlay_with_source_style(flags: Flags, foreground: Rgb, background: Rgb) -> FormulaOverlay {
        let mut overlay = scan_grid(&TextGrid::from_rows(&[r"$$E=mc^2$$"]))
            .into_iter()
            .next()
            .expect("test formula");
        overlay.fallback = "$$E=mc^2$$"
            .chars()
            .enumerate()
            .map(|(column, character)| RenderableCell {
                character,
                point: Point::new(0, Column(column)),
                fg: foreground,
                bg: background,
                bg_alpha: 1.0,
                underline: foreground,
                flags,
                extra: None,
            })
            .collect();
        overlay
    }

    fn compact_prepared(cells: usize) -> Option<PreparedFormula> {
        Some(PreparedFormula {
            fitted_pixel_size: TEST_FONT_PX,
            display_style: false,
            bleed_top: 0.0,
            bleed_bottom: 0.0,
            box_right: 0.0,
            centered: true,
            compact_cells: Some(cells),
        })
    }

    fn sources(rows: &[&str]) -> Vec<(String, bool)> {
        scan_grid(&TextGrid::from_rows(rows))
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

    #[test]
    fn compact_projection_moves_only_render_coordinates() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let original = grid.rows.clone();
        let overlays = scan_grid(&grid);
        assert_eq!(overlays.len(), 1);
        let span = overlays[0].spans[0];
        let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

        assert!(projection.project_cell(Point::new(0, Column(span.start)), 80).is_none());
        let suffix = projection
            .project_cell(Point::new(0, Column(span.end)), 80)
            .expect("suffix remains visible");
        assert_eq!(suffix.column.0, span.start + 2);
        assert_eq!(grid.rows, original, "projection must not mutate terminal cells");
    }

    #[test]
    fn compact_projection_retains_background_under_visual_formula_box() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let overlays = scan_grid(&grid);
        let span = overlays[0].spans[0];
        let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

        let first = projection
            .project_formula_background(Point::new(0, Column(span.start)), 80)
            .expect("first visual formula cell keeps its background");
        let second = projection
            .project_formula_background(Point::new(0, Column(span.start + 1)), 80)
            .expect("second visual formula cell keeps its background");
        assert_eq!(first.column.0, span.start);
        assert_eq!(second.column.0, span.start + 1);
        assert!(
            projection
                .project_formula_background(Point::new(0, Column(span.start + 2)), 80)
                .is_none(),
            "source cells beyond the compact formula width must not create a background strip"
        );
    }

    #[test]
    fn disabled_projection_keeps_fixed_columns() {
        let grid = TextGrid::from_rows(&["pre $x^2$                         sidebar"]);
        let overlays = scan_grid(&grid);
        let span = overlays[0].spans[0];
        let prepared = [compact_prepared(2)];
        let mut state = TerminalMathState::default();

        state.update_projection(&overlays, &prepared, false);

        let source = state
            .project_formula_background(Point::new(0, Column(span.start)), 80)
            .expect("formula background remains at its TUI column");
        let sidebar_column = grid.rows[0].iter().position(|cell| *cell == Some('s')).unwrap();
        let sidebar = state
            .project_cell(Point::new(0, Column(sidebar_column)), 80)
            .expect("sidebar remains visible");
        assert_eq!(source.column.0, span.start);
        assert_eq!(sidebar.column.0, sidebar_column);
    }

    #[test]
    fn compact_projection_discards_overlapping_streaming_candidate() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let outer = scan_grid(&grid).remove(0);
        let outer_span = outer.spans[0];
        let mut inner = outer.clone();
        inner.spans[0].start += 1;
        inner.spans[0].end -= 1;
        let mut state = TerminalMathState::default();
        let retained = state.update_projection_with_survivors(
            &[inner, outer],
            &[compact_prepared(1), compact_prepared(2)],
            true,
        );
        let projection = state.projection_snapshot();

        assert_eq!(projection.spans.len(), 1);
        assert_eq!(projection.spans[0].source_start, outer_span.start);
        assert_eq!(projection.spans[0].source_end, outer_span.end);
        assert_eq!(retained, [false, true], "GPUI must drop the same overlapping candidate");
    }

    #[test]
    fn compact_projection_accumulates_multiple_formula_shifts() {
        let grid = TextGrid::from_rows(&["a $x^2$ b $y^2$ c"]);
        let overlays = scan_grid(&grid);
        assert_eq!(overlays.len(), 2);
        let prepared = [compact_prepared(2), compact_prepared(2)];
        let projection = LineProjection::build(&overlays, &prepared);
        let last = overlays[1].spans[0];
        let source_reduction: usize =
            overlays.iter().map(|overlay| overlay.spans[0].end - overlay.spans[0].start - 2).sum();

        let suffix = projection
            .project_cell(Point::new(0, Column(last.end)), 80)
            .expect("suffix remains visible");
        assert_eq!(suffix.column.0, last.end - source_reduction);
    }

    #[test]
    fn formula_hit_testing_returns_source_boundaries() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let overlays = scan_grid(&grid);
        let span = overlays[0].spans[0];
        let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

        let (left, left_side) =
            projection.source_from_visual(Point::new(0, Column(span.start)), Side::Left);
        let (right, right_side) =
            projection.source_from_visual(Point::new(0, Column(span.start + 1)), Side::Right);
        assert_eq!((left.column.0, left_side), (span.start, Side::Left));
        assert_eq!((right.column.0, right_side), (span.end - 1, Side::Right));
    }

    #[test]
    fn formula_hit_testing_preserves_nonzero_viewport_origin() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let overlays = scan_grid(&grid);
        let span = overlays[0].spans[0];
        let mut state = TerminalMathState::default();
        state.update_projection(&overlays, &[compact_prepared(2)], true);
        let viewport_origin = Line(-7);

        let (left, left_side) = state.source_point(
            Point::new(viewport_origin, Column(span.start)),
            Side::Left,
            viewport_origin,
        );
        let (suffix, suffix_side) = state.source_point(
            Point::new(viewport_origin, Column(span.start + 2)),
            Side::Left,
            viewport_origin,
        );

        assert_eq!(
            (left.line, left.column.0, left_side),
            (viewport_origin, span.start, Side::Left)
        );
        assert_eq!(
            (suffix.line, suffix.column.0, suffix_side),
            (viewport_origin, span.end, Side::Left)
        );
    }

    fn remember_visible(state: &mut TerminalMathState, grid: &TextGrid) {
        state.synchronize_grid(grid);
        for overlay in scan_grid(grid) {
            state.remember(grid, &overlay);
        }
    }

    #[test]
    fn completed_formula_replaces_overlapping_streaming_candidate() {
        let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
        let outer = scan_grid(&grid).remove(0);
        let mut inner = outer.clone();
        inner.spans[0].start += 1;
        inner.spans[0].end -= 1;

        let mut state = TerminalMathState::default();
        state.remember(&grid, &inner);
        state.remember(&grid, &outer);

        assert_eq!(state.formulas.len(), 1);
        assert!(
            state.formulas.contains_key(&FormulaAnchor { row: 0, column: outer.spans[0].start })
        );
    }

    #[test]
    fn recognizes_cli_math_delimiters_and_utf8_prose() {
        assert_eq!(
            sources(&[r"中文 \(x^2+y^2=z^2\) and $\alpha+1$"]),
            vec![("x^2+y^2=z^2".into(), false), (r"\alpha+1".into(), false)]
        );
    }

    #[test]
    fn agent_reasoning_style_is_distinct_from_final_answer_style() {
        let normal = Rgb::new(240, 240, 240);
        let background = Rgb::new(8, 8, 8);

        let codex_reasoning = overlay_with_source_style(Flags::DIM, normal, background);
        assert!(formula_uses_reasoning_style(&codex_reasoning));

        let final_answer = overlay_with_source_style(Flags::empty(), normal, background);
        assert!(!formula_uses_reasoning_style(&final_answer));
    }

    /// 行内定界符只跨软换行。跨真实换行会把互不相关的两行终端输出合成
    /// 一个公式（`$x` / `$y` 两行、WSL bash 两个提示符之间的整段输出），
    /// 代价是 Agent TUI 硬换行的**行内**公式恢复不了——那种情况保持原文，
    /// 是安全的失败模式。块级公式仍然跨真实换行，见
    /// [`display_math_can_cross_hard_terminal_rows`]。
    #[test]
    fn explicit_inline_formulas_cross_soft_wraps_only() {
        for mut grid in [
            TextGrid::from_rows(&["$e^    ", r"{i\pi}+1=0$"]),
            TextGrid::from_rows(&[r"\(e^    ", r"{i\pi}+1=0\)"]),
        ] {
            grid.wrapped[0] = true;
            let overlays = scan_grid(&grid);
            assert_eq!(overlays.len(), 1, "软换行内要接起来");
            assert!(overlays[0].source.contains("e^"));
            assert!(overlays[0].source.contains(r"{i\pi}+1=0"));

            grid.wrapped[0] = false;
            assert!(scan_grid(&grid).is_empty(), "真实换行必须断开行内公式");
        }
    }

    /// Agent TUI 的硬换行：**块级**形态（`\[ \]`、裸 `[`、`$$`）照旧恢复，
    /// 行内形态保持原文。
    #[test]
    fn screenshot_display_formulas_survive_agent_hard_wraps() {
        for rows in [
            vec![r"\[\displaystyle \frac{d}{dx}\sin", r"x=\cos x\]"],
            vec!["$$", r"\lim_{x\to0}\frac{\sin", r"x}{x}=1", "$$"],
        ] {
            let overlays = scan_grid(&TextGrid::from_rows(&rows));
            assert!(!overlays.is_empty(), "expected wrapped formula in {rows:?}");
            for overlay in overlays {
                compile_formula(&overlay.source, true, 18.0, 1.0, DEFAULT_LIMITS).unwrap_or_else(
                    |error| panic!("wrapped formula failed: {:?}: {error:?}", overlay.source),
                );
            }
        }
        // 同一批内容的行内形态被硬换行拆开时保持原文，不再跨行拼接。
        assert!(
            scan_grid(&TextGrid::from_rows(
                &[r"• $\lim_{x\to0}\frac{\sin", r"x}{x}=1$，$PV=nRT$",]
            ))
            .iter()
            .all(|overlay| !overlay.source.contains("lim")),
            "行内公式不得跨真实换行拼接"
        );
    }

    #[test]
    fn display_math_can_cross_hard_terminal_rows() {
        assert_eq!(
            sources(&["answer:", "$$", r"\frac{1}{2} + x^2", "$$", "done"]),
            vec![(r"\frac{1}{2} + x^2".into(), true)]
        );
        assert_eq!(sources(&[r"\[\sum_{i=1}^n i\]"]), vec![(r"\sum_{i=1}^n i".into(), true)]);

        let multiline = sources(&[
            "$$",
            r"\begin{aligned}",
            r"f(x) &= x^2 \\",
            r"g(x) &= x+1",
            r"\end{aligned}",
            "$$",
        ]);
        assert_eq!(multiline.len(), 1);
        assert!(multiline[0].0.contains('\n'));
        assert!(multiline[0].1);
    }

    #[test]
    fn stray_display_opener_in_prose_does_not_swallow_following_formulas() {
        let screen = [
            "行内，$$,\\[等公式都输出一些",
            "",
            "• 行内公式：质能方程 (E = mc^2)，二次方程的求根公式是 (x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a})。",
            "单美元符号行内公式：欧拉公式 $e^{ix}=\\cos x+i\\sin x$。",
            "双美元符号块级公式：",
            "$$",
            "\\int_{-\\infty}^{+\\infty} e^{-x^2},dx=\\sqrt{\\pi}",
            "$$",
            "方括号块级公式：",
            "\\[",
            "\\sum_{n=1}^{\\infty}\\frac{1}{n^2}=\\frac{\\pi^2}{6}",
            "\\]",
            "带编号的公式：",
            "[",
            "\\boxed{\\nabla\\cdot\\mathbf{E}=\\frac{\\rho}{\\varepsilon_0}}",
            "\\tag{1}",
            "]",
        ];
        let expected = vec![
            ("E = mc^2".into(), false),
            (r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}".into(), false),
            (r"e^{ix}=\cos x+i\sin x".into(), false),
            (r"\int_{-\infty}^{+\infty} e^{-x^2},dx=\sqrt{\pi}".into(), true),
            (r"\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}".into(), true),
            (
                // The row break inside the block survives extraction; it is
                // collapsed together with the padding below because TeX treats
                // every run of whitespace alike.
                concat!(
                    r"\boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}}",
                    " ",
                    r"\tag{1}",
                )
                .into(),
                true,
            ),
        ];

        // `extract` only trims the whole span, so a multi-row source still
        // carries each row's blank padding before the newline. Whitespace is
        // insignificant to TeX, so compare on collapsed runs instead of
        // pinning the terminal width into the expectation.
        let extracted: Vec<(String, bool)> = sources(&screen)
            .into_iter()
            .map(|(source, display)| {
                (source.split_whitespace().collect::<Vec<_>>().join(" "), display)
            })
            .collect();
        assert_eq!(extracted, expected);
        assert!(sources(&screen)[5].0.contains('\n'), "the block keeps its real row break");
        assert!(
            scan_grid(&TextGrid::from_rows(&screen))
                .iter()
                .flat_map(|overlay| overlay.spans.iter())
                .all(|span| span.row != 0),
            "prose on the first row must stay literal",
        );
    }

    /// `cat math.txt`：块的开头 `$$` 滚出视口顶部之后，视口里第一个 `$$` 是
    /// **闭合**。它曾经跟下面那个块的定界符配对，把 27 行正文、三个方括号块
    /// 连同末尾的 `$$` 块吞成一条候选——而且编译成功，于是整片区域被一张渲染
    /// 图盖掉，中文正文以数学字形出现在图里。
    #[test]
    fn orphan_display_closer_at_the_viewport_top_stops_at_the_blank_row() {
        let screen = [
            "  $$",
            "",
            "  方括号块级公式：",
            "",
            "  [",
            r"  \sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}",
            "  ]",
            "",
            "  带编号的公式：",
            "",
            "  [",
            r"  \boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}}",
            r"  \tag{1}",
            "  ]",
            "",
            "",
            r"$$(F, D_{\text{few}}) \xrightarrow{\text{Prompting}} \boxed{C}",
            r"  \boxed{(F_{\text{ref}}, F_{\text{neg}})} \xrightarrow{\text{ToT Search}}$$",
        ];
        let expected = [
            r"\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}",
            r"\boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}} \tag{1}",
            concat!(
                r"(F, D_{\text{few}}) \xrightarrow{\text{Prompting}} \boxed{C} ",
                r"\boxed{(F_{\text{ref}}, F_{\text{neg}})} \xrightarrow{\text{ToT Search}}",
            ),
        ];

        let extracted = sources(&screen);
        let collapsed: Vec<String> = extracted
            .iter()
            .map(|(source, _)| source.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        assert_eq!(collapsed, expected, "正文和孤立闭合必须保持原文");
        assert!(extracted.iter().all(|(_, display)| *display));

        // 孤立闭合要留成 pending，历史回看才有机会把滚出去的真公式配回来。
        let scan = scan_grid_result(&TextGrid::from_rows(&screen));
        assert!(
            matches!(
                scan.unmatched_display,
                Some((position, DisplayDelimiterKind::Dollars)) if position.row == 0
            ),
            "orphan closer must stay pending, got {:?}",
            scan.unmatched_display
        );
    }

    #[test]
    fn long_display_formula_with_trailing_literal_dollar_remains_renderable() {
        let source = concat!(
            r"(F, D_{few}) \xrightarrow{\text{Prompting}} \boxed{C} ",
            r"\xrightarrow{\text{MultiAgent}} \boxed{(F_{ref}, F_{neg})} ",
            r"\xrightarrow{\text{ToT Search}} ",
        );
        let line = format!("$${source}$$$");

        assert_eq!(sources(&[&line]), vec![(source.trim_end().into(), true)]);
        let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
            .expect("the long display formula must reach native layout");
        assert!(layout.metrics.width > 0.0 && layout.metrics.height > 0.0);
    }

    #[test]
    fn active_cli_input_logical_line_is_never_persisted_as_math_output() {
        let mut grid =
            TextGrid::from_rows(&["prompt $$lim_x{x}$$", "continued $x^1$", "assistant $$x^2$$"]);
        // The first physical row wraps into the cursor row, so both belong to
        // one live editor buffer and must be excluded together.
        grid.wrapped[0] = true;
        let active_rows = grid.logical_rows_containing(1).expect("cursor logical line");
        assert_eq!(active_rows, 0..=1);

        let mut state = TerminalMathState::default();
        state.synchronize_grid(&grid);
        assert!(state.scan_visible_grid(&grid, Some(&active_rows)).is_none());

        let overlays = state.visible_overlays(&grid);
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].source.as_ref(), "x^2");
    }

    #[test]
    fn unmatched_display_delimiter_on_live_input_does_not_start_history_reconstruction() {
        let grid = TextGrid::from_rows(&["prompt $$"]);
        let active_rows = grid.logical_rows_containing(0).expect("cursor row");
        let mut state = TerminalMathState::default();
        state.synchronize_grid(&grid);

        assert!(state.scan_visible_grid(&grid, Some(&active_rows)).is_none());
        assert!(state.pending_display.is_none());
    }

    #[test]
    fn screenshot_cli_formulas_reach_the_shared_compiler() {
        let samples = [
            sources(&["$$", r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}", "$$"]),
            sources(&[
                "$$",
                r"f(x)=\begin{cases}",
                "x^2,&x\\geq 0\\",
                r"-x,&x<0",
                r"\end{cases}",
                "$$",
            ]),
            sources(&[
                "$$",
                r"A=\begin{pmatrix}",
                r"1&amp;2&amp;3\&amp;nbsp;",
                r"4&amp;5&amp;6\&amp;#160;",
                r"7&amp;8&amp;9",
                r"\end{pmatrix}",
                "$$",
            ]),
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
    fn compact_equations_use_the_same_rule_for_every_standard_delimiter() {
        let expected_inline = vec![("E = mc^2".into(), false)];
        let expected_display = vec![("E = mc^2".into(), true)];
        assert_eq!(sources(&["$E = mc^2$"]), expected_inline);
        assert_eq!(sources(&[r"\(E = mc^2\)"]), expected_inline);
        assert_eq!(sources(&["$$E = mc^2$$"]), expected_display);
        assert_eq!(sources(&[r"\[E = mc^2\]"]), expected_display);
    }

    #[test]
    fn markdown_unescaped_bare_brackets_render_as_display_math() {
        // 部分 AI CLI 的 markdown 渲染会吃掉 `\[` `\]` `\,` 的反斜杠
        // （它们是 markdown 标点转义），`\int`/`\frac` 不是转义所以幸存，
        // 屏幕上只剩裸 `[` 块——2026-08-17 截图的真实形态。
        assert_eq!(
            sources(&["[", r"\int_0^1 x^2,dx = \frac{1}{3}", "]"]),
            vec![(r"\int_0^1 x^2,dx = \frac{1}{3}".into(), true)]
        );
        assert_eq!(
            sources(&["[", r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}", "]"]),
            vec![(r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}".into(), true)]
        );
        assert_eq!(
            sources(&[r"[ \lim_{x\to 0}\frac{\sin x}{x}=1 ]"]),
            vec![(r"\lim_{x\to 0}\frac{\sin x}{x}=1".into(), true)]
        );
        // Codex CLI under WSL renders `\[ E = mc^2 \]` as three plain rows
        // because its Markdown layer consumes the bracket escapes.
        assert_eq!(sources(&["[", "E = mc^2", "]"]), vec![("E = mc^2".into(), true)]);
        // Codex's list renderer keeps the bullet on the opening delimiter.
        assert_eq!(
            sources(&["• [", r"  \int_{-\infty}^{+\infty} e^{-x^2}\,dx=\sqrt{\pi}", "  ]"]),
            vec![(r"\int_{-\infty}^{+\infty} e^{-x^2}\,dx=\sqrt{\pi}".into(), true)]
        );
    }

    #[test]
    fn bare_bracket_interval_inside_formula_does_not_close_early() {
        // 内容行里的 `[0, 1]` 区间：其 `]` 不独占一行，没有闭合资格。
        assert_eq!(sources(&["[", r"x \in [0, 1]", "]"]), vec![(r"x \in [0, 1]".into(), true)]);
    }

    #[test]
    fn bare_brackets_without_tex_evidence_stay_literal() {
        assert!(sources(&["[", "  1, 2, 3,", "]"]).is_empty());
        assert!(sources(&[r#"["alpha", "beta"]"#]).is_empty());
        assert!(sources(&["[INFO] server started"]).is_empty());
        assert!(sources(&["[x^2]"]).is_empty());
        assert!(sources(&["result [ x^2 ] done"]).is_empty());
        assert!(sources(&["[", r"C:\temp\integral.txt", "]"]).is_empty());
    }

    #[test]
    fn markdown_unescaped_bare_parens_render_inline() {
        assert_eq!(
            sources(&[r"也可以使用 (\sqrt{x^2+y^2}) 表示行内公式"]),
            vec![(r"\sqrt{x^2+y^2}".into(), false)]
        );
        assert_eq!(sources(&["• 质能方程为 (E=mc^2)。"]), vec![("E=mc^2".into(), false)]);
        let sequence = r"a_n=a_1+(n-1)d";
        assert_eq!(sources(&[&format!("• ({sequence})，done")]), vec![(sequence.into(), false)]);
        compile_formula(sequence, false, 18.0, 1.0, DEFAULT_LIMITS)
            .expect("the recovered sequence formula must compile");
        // 括号深度配对：`\sin(x)` 的内层 `)` 不提前截断。
        assert_eq!(sources(&[r"值 (\sin(x)) 收敛"]), vec![(r"\sin(x)".into(), false)]);
    }

    #[test]
    fn codex_line_with_multiple_inline_formulas_keeps_bare_paren_math() {
        let source = r"$E = mc^2$，$a^2 + b^2 = c^2$，$e^{i\pi}+1=0$，(\displaystyle \int_0^1 x^2,dx=\frac13)，$\sum_{n=1}^{\infty}\frac1{n^2}=\frac{\pi^2}{6}$";
        let formulas = sources(&[source]);
        assert_eq!(formulas.len(), 5);
        assert_eq!(formulas[3], (r"\displaystyle \int_0^1 x^2,dx=\frac13".into(), false));
        compile_formula(&formulas[3].0, false, 18.0, 1.0, DEFAULT_LIMITS)
            .expect("Codex's unbraced fraction arguments must compile");
    }

    #[test]
    fn implicit_product_equations_use_the_standard_delimiter_rule() {
        let expected = vec![("F=ma".into(), false), ("PV=nRT".into(), false)];
        assert_eq!(sources(&[r"\(F=ma\), \(PV=nRT\)"]), expected);
        assert_eq!(sources(&["$F=ma$, $PV=nRT$"]), expected);
        assert_eq!(
            sources(&[r"(\displaystyle F=ma), (\displaystyle PV=nRT)"]),
            vec![(r"\displaystyle F=ma".into(), false), (r"\displaystyle PV=nRT".into(), false),]
        );

        assert_eq!(sources(&[r"\(key=value\)"]), Vec::new());
        assert_eq!(sources(&["$key=value$"]), Vec::new());
        assert!(sources(&["setting (key=value)"]).is_empty());
    }

    #[test]
    fn bare_parens_reject_regex_prose_and_single_letter_escapes() {
        assert!(sources(&[r"grep -E (\d+) input.txt"]).is_empty());
        assert!(sources(&[r"match (\w*) here"]).is_empty());
        assert!(sources(&["plain (normal prose) text"]).is_empty());
        assert!(sources(&["setting (key=value)"]).is_empty());
        assert!(sources(&[r"case (\n) newline"]).is_empty());
        assert!(sources(&[r"path (C:\temp\frac) oops"]).is_empty());
    }

    /// 定界符表明意图，内容提供证据——**两者都要**。只看定界符会把终端里
    /// 满地的 shell sigil、提示符和价格全渲染成公式，所以这些形状必须保持
    /// 原文。它们不是理论候选：`HOME`、`npm install`、`PATH=/tmp` 在
    /// pulldown-latex 里都能成功解析成公式事件，判定放过就是真替换。
    #[test]
    fn explicit_delimiters_still_require_content_evidence() {
        assert!(sources(&[r"escaped \$x$"]).is_empty());
        // 货币、全大写环境变量、散文、配置串：噪音否决层与证据层各管一段。
        assert!(sources(&["price $5$ and $12.50$"]).is_empty());
        assert!(sources(&["env $LONG_VARIABLE$"]).is_empty());
        assert!(sources(&["quote $USD 20$ today"]).is_empty());
        assert!(sources(&["literal $hello$ text"]).is_empty());
        assert!(sources(&["config $PATH=/tmp$"]).is_empty());
        assert!(sources(&[r"plain \(normal prose\)"]).is_empty());
        assert!(sources(&["$ $", r"\(  \)", "$$  $$"]).is_empty());
        // 路径不是公式：`explicit_operand` 把标识符卡在一个字母，`foo/bar`
        // 的两侧都不合格，所以 `/` 不构成紧凑运算符证据。
        assert!(sources(&["path $foo/bar$"]).is_empty());
        // 真数学照旧：display 块的 lax 证据。
        assert_eq!(sources(&["$$E = mc^2$$"]), vec![("E = mc^2".into(), true)]);
        assert_eq!(sources(&["$a/b$"]), vec![("a/b".into(), false)]);
    }

    /// 普通 shell 里真实出现过的形状，一个都不许变成公式。
    /// 每一行都对应实测过的误报（见 PR #55 review）。
    #[test]
    fn ordinary_shell_output_stays_literal() {
        // 变量 sigil 成对出现，中间那截会被当成源码。
        assert!(sources(&["echo $HOME $USER"]).is_empty());
        assert!(sources(&["echo $HOME"]).is_empty());
        // sh 家族提示符以 `$ ` 结尾——`$` 后紧跟空白直接否决。
        assert!(sources(&["$ npm install", "$ npm test"]).is_empty());
        assert!(sources(&["user@host:~$ echo hello", "hello", "user@host:~$ ls"]).is_empty());
        // 行内定界符不跨真实换行，两行各自的 `$` 不配对。
        assert!(sources(&["$x", "$y"]).is_empty());
        assert!(sources(&["export $FOO=1", "export $BAR=2"]).is_empty());
        // 价格与提交信息。
        assert!(sources(&["cost $5 vs $7 today"]).is_empty());
        assert!(sources(&["fix: charge $9.99", "feat: refund $9.99"]).is_empty());
        // BRE 捕获组：`\(…\)` 撞 sed/grep 的日常写法。
        assert!(sources(&[r"sed 's/\(abc\)/x/' input.txt"]).is_empty());
        assert!(sources(&[r"grep '\(foo\|bar\)' log.txt"]).is_empty());
    }

    /// 软换行仍要跨——这是行内公式在窄窗口里的正常形态；跨的是软换行，
    /// 不是真实换行。
    #[test]
    fn inline_formula_crosses_soft_wrap_but_not_a_real_newline() {
        let mut wrapped = TextGrid::from_rows(&["prefix $x^2", "+ y^2$ tail"]);
        wrapped.wrapped[0] = true;
        assert_eq!(scan_grid(&wrapped).len(), 1, "soft wrap 内的行内公式要接起来");

        let hard = TextGrid::from_rows(&["prefix $x^2", "+ y^2$ tail"]);
        assert!(scan_grid(&hard).is_empty(), "真实换行必须断开行内公式");
    }

    /// display 定界符照旧跨真实换行：`$$` / `\[ \]` 占住它们之间的整块，
    /// Agent TUI 硬换行的块级公式靠这条恢复。
    #[test]
    fn display_formula_still_crosses_a_real_newline() {
        assert_eq!(sources(&["$$", r"\frac{1}{2}", "$$"]).len(), 1);
        assert_eq!(sources(&[r"\[", r"\sum_{i=1}^n i", r"\]"]).len(), 1);
    }

    /// 基线证据层漏掉的两个真公式。`explicit_operand` 把标识符卡在一个字母，
    /// 物理里省略乘号的 `mc^2` / `nRT` 全被误杀；阶乘则压根没有证据项。
    #[test]
    fn implicit_products_and_factorials_are_math() {
        assert_eq!(sources(&["$E=mc^2$"]), vec![("E=mc^2".into(), false)]);
        assert_eq!(sources(&["$E = mc^2$"]), vec![("E = mc^2".into(), false)]);
        assert_eq!(sources(&["$PV=nRT$"]), vec![("PV=nRT".into(), false)]);
        assert_eq!(sources(&["$F=ma$"]), vec![("F=ma".into(), false)]);
        assert_eq!(sources(&["$n!$"]), vec![("n!".into(), false)]);
        assert_eq!(sources(&[r"\(E=mc^2\)"]), vec![("E=mc^2".into(), false)]);
        // 放宽到三个字母不等于放开散文：这些仍然保持原文。
        assert!(sources(&["$key=value$"]).is_empty());
        assert!(sources(&["$PATH=/tmp$"]).is_empty());
        assert!(sources(&["$npm install$"]).is_empty());
        assert!(sources(&["$Hello!$"]).is_empty());
    }

    /// 失败候选必须有预算：`MAX_VISIBLE_FORMULAS` 只限成功数，不限失败数。
    /// 满屏未闭合的 `(` 曾让单帧扫描从 ~2ms 涨到 ~322ms（50×200 debug）。
    #[test]
    fn unclosed_bare_delimiters_stay_within_budget() {
        let dense: String = std::iter::repeat("(x ").take(66).collect();
        let rows: Vec<&str> = std::iter::repeat(dense.as_str()).take(50).collect();
        let grid = TextGrid::from_rows(&rows);
        let plain: String = std::iter::repeat("plain text ").take(18).collect();
        let plain_rows: Vec<&str> = std::iter::repeat(plain.as_str()).take(50).collect();
        let baseline_grid = TextGrid::from_rows(&plain_rows);

        let start = std::time::Instant::now();
        assert!(scan_grid(&grid).is_empty(), "未闭合的裸括号不产生公式");
        let dense_cost = start.elapsed();
        let start = std::time::Instant::now();
        assert!(scan_grid(&baseline_grid).is_empty());
        let baseline_cost = start.elapsed();

        // 预算把每个候选的搜索钉在常数行数上，于是总成本跟着格子数走而不是
        // 格子数的平方。放宽到 12 倍是给 CI 抖动留的余量，回归会是三位数倍。
        assert!(
            dense_cost < baseline_cost * 12,
            "未闭合括号扫描 {dense_cost:?} 相对基准 {baseline_cost:?} 退化成二次复杂度"
        );
    }

    #[test]
    fn single_dollar_accepts_only_explicit_math_shapes() {
        assert_eq!(sources(&["$x$ $x_1$ $2+2$ $a/b$ $x=y$ $f(x)$ $f(x)=0$"]).len(), 7);
        assert_eq!(sources(&[r"$\frac{1}{2}$ $\sin x$ $x^2$"]).len(), 3);
    }

    #[test]
    fn single_dollar_uses_the_standard_formula_rule_without_agent_context() {
        assert_eq!(sources(&["plain $x$ text"]), vec![("x".into(), false)]);
        assert_eq!(
            sources(&["质能方程 $E = mc^2$，其中 c 为光速。"]),
            vec![("E = mc^2".into(), false)]
        );
        assert_eq!(
            sources(&[
                r"二次方程：$x = \dfrac{-b \pm \sqrt{b^2 - 4ac}}{2a}$",
                r"斯特林公式：$n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n$",
            ],),
            vec![
                (r"x = \dfrac{-b \pm \sqrt{b^2 - 4ac}}{2a}".into(), false),
                (r"n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n".into(), false),
            ]
        );
    }

    #[test]
    fn persisted_formula_survives_partial_scroll_without_rescaling_its_bounds() {
        let mut state = TerminalMathState::default();
        let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
        remember_visible(&mut state, &initial);
        assert_eq!(state.formulas.len(), 1);

        let scrolled = grid_at(&["x^2  ", "$$   ", "tail ", "next "], 41, 0);
        state.synchronize_grid(&scrolled);
        assert!(scan_grid(&scrolled).is_empty());
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
        assert!(state.scan_visible_grid(&opening, None).is_none());

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
            .scan_visible_grid(&visible_tail, None)
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
        assert!(state.complete_pending_from_history(&history));

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
    fn tui_redraw_blank_interim_frame_keeps_the_persisted_formula() {
        let mut state = TerminalMathState::default();
        let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
        remember_visible(&mut state, &initial);
        assert_eq!(state.formulas.len(), 1);

        // A TUI clears a line before repainting it: a partially blanked frame
        // must not evict the formula (this was the input-time flicker).
        let interim = grid_at(&["$$   ", "     ", "$$   ", "tail "], 40, 0);
        state.synchronize_grid(&interim);
        assert_eq!(state.visible_overlays(&interim).len(), 1);
        assert_eq!(state.formulas.len(), 1);

        // Rewritten with different visible content -> genuinely gone.
        let changed = grid_at(&["other", "words", "here ", "tail "], 40, 0);
        state.synchronize_grid(&changed);
        assert!(state.visible_overlays(&changed).is_empty());
    }

    #[test]
    fn orphan_closing_delimiter_recovers_the_formula_from_history() {
        let mut state = TerminalMathState::default();
        // Viewport starts below the formula opener: only the closing `$$` is
        // visible (e.g. the persisted copy was evicted by a redraw glitch).
        let visible = grid_at(&["$$   ", "tail "], 45, 0);
        state.synchronize_grid(&visible);
        assert!(state.scan_visible_grid(&visible, None).is_none());

        let anchor = state
            .scan_visible_grid(&visible, None)
            .expect("stable orphan delimiter requests one history pass");
        assert_eq!(anchor, FormulaAnchor { row: 45, column: 0 });

        let history = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 43, 0);
        assert!(state.complete_pending_from_history(&history));

        let overlays = state.visible_overlays(&visible);
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].source.as_ref(), "x^2");
        assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-2));

        // The attempt is consumed: the same orphan never re-triggers, even
        // though the scanner keeps reporting it as unmatched every frame.
        assert!(state.scan_visible_grid(&visible, None).is_none());
        assert!(state.scan_visible_grid(&visible, None).is_none());
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

    /// Cell geometry proportional to a real terminal font: monospace advance
    /// is ~0.6 em and line height ~1.3 em, so budgets in these tests mean the
    /// same thing they mean on screen.
    fn test_size(columns: f32, lines: f32) -> SizeInfo {
        SizeInfo::new(columns * 12.0, lines * 26.0, 12.0, 26.0, 0.0, 0.0, false)
    }

    /// Nominal terminal font size behind [`test_size`].
    const TEST_FONT_PX: f32 = 20.0;

    fn fitted_size(rows: &[&str]) -> f32 {
        let grid = TextGrid::from_rows(rows);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 1, "expected exactly one formula in {rows:?}");
        let size = test_size(grid.columns as f32, grid.rows.len() as f32);
        let mut state = TerminalMathState::default();
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
        prepared[0].expect("formula must render").fitted_pixel_size
    }

    #[test]
    fn display_formula_with_blank_neighbours_keeps_the_terminal_font_size() {
        let fitted = fitted_size(&[
            "                    ",
            "$$                  ",
            r"\sum_{i=1}^{n} i^2  ",
            "$$                  ",
            "                    ",
        ]);
        assert_eq!(fitted, TEST_FONT_PX, "blank-neighbour display math must not shrink");
    }

    /// The whole point of the blank-row budget: the same source keeps one size
    /// whether the emitter put it on one line or spread it over three.
    #[test]
    fn display_math_size_does_not_depend_on_how_many_rows_the_source_used() {
        let one_line = fitted_size(&[
            "                              ",
            r"$$\sum_{i=1}^{n} i^2$$        ",
            "                              ",
        ]);
        let three_lines = fitted_size(&[
            "                              ",
            "$$                            ",
            r"\sum_{i=1}^{n} i^2            ",
            "$$                            ",
            "                              ",
        ]);
        assert_eq!(one_line, three_lines);
        assert_eq!(one_line, TEST_FONT_PX);
    }

    /// A row hemmed in on both sides by prose that reaches under the formula
    /// is the one case where the budget really is a single line gap. The
    /// formula must then trade layout style — and only as a last resort a few
    /// percent of size — for staying inside it, because the clip crops exactly
    /// what the fit could not absorb.
    #[test]
    fn tall_formula_hemmed_in_by_prose_stays_inside_the_line_gap_budget() {
        let rows = &[
            "prose above that runs under the formula",
            r"$$\frac{x^2+1}{y-1}$$                  ",
            "prose below that runs under the formula",
        ];
        let grid = TextGrid::from_rows(rows);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 1);
        let size = test_size(grid.columns as f32, 8.0);

        let mut state = TerminalMathState::default();
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
        let prepared = prepared[0].expect("formula must render");
        assert_eq!(
            prepared.bleed_top,
            size.cell_height() * DISPLAY_BLEED_INTO_PROSE,
            "display math should leave a small prose clearance above",
        );
        assert_eq!(
            prepared.bleed_bottom,
            size.cell_height() * DISPLAY_BLEED_INTO_PROSE,
            "display math should leave a small prose clearance below",
        );
        assert!(
            !prepared.display_style,
            "the compact style must be tried before any size is given up",
        );
        assert!(
            prepared.fitted_pixel_size >= TEST_FONT_PX * 0.9,
            "compact style should cost at most a few percent, got {}",
            prepared.fitted_pixel_size,
        );

        // The invariant behind the whole scheme: re-laid-out ink stays inside
        // bounds plus the prose-side budgets and the deliberate overrun
        // tolerance, so the clip only ever trims an antialiasing edge.
        let layout = state
            .layout(
                overlays[0].formula_id,
                &overlays[0].source,
                prepared.fitted_pixel_size,
                1.0,
                prepared.display_style,
            )
            .expect("fitted layout");
        let budget = size.cell_height()
            * (1.0 + DISPLAY_BLEED_INTO_PROSE * 2.0)
            * (1.0 + HEIGHT_OVERRUN_TOLERANCE);
        assert!(
            layout.metrics.height + layout.metrics.depth <= budget + 0.5,
            "fitted ink {} must fit the prose budget {}",
            layout.metrics.height + layout.metrics.depth,
            budget,
        );
    }

    #[test]
    fn display_math_uses_a_smaller_prose_bleed_than_inline_math() {
        let display_grid = TextGrid::from_rows(&["above", "$$x^2$$", "below"]);
        let display_overlays = scan_grid_with_hints(&display_grid);
        let display_size = test_size(display_grid.columns as f32, display_grid.rows.len() as f32);
        let display_overlay = &display_overlays[0];
        let display_bounds = display_overlay.bounds(&display_size).expect("display bounds");
        let display_bleed = display_overlay.vertical_bleed(&display_size, (0, 0));

        let inline_grid = TextGrid::from_rows(&["value $x^2$ grows"]);
        let inline_overlays = scan_grid_with_hints(&inline_grid);
        let inline_size = test_size(inline_grid.columns as f32, inline_grid.rows.len() as f32);
        let inline_overlay = &inline_overlays[0];
        let inline_bleed = inline_overlay.vertical_bleed(&inline_size, (0, 0));

        assert!(display_overlay.display);
        assert!(!inline_overlay.display);
        assert!(display_bleed.0 < inline_bleed.0);
        assert!(display_bleed.1 < inline_bleed.1);
        assert_eq!(display_bleed.0, display_bounds.height() * DISPLAY_BLEED_INTO_PROSE);
    }

    /// Prose that stops well before the formula's columns is not in the way:
    /// the ink of a centred block lands where those rows are empty, so it may
    /// use their height and keep both the display style and the font size.
    #[test]
    fn short_prose_neighbours_do_not_shrink_a_centred_block() {
        let rows = &[
            "其中：                                  ",
            r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$",
            "下面继续说明。                            ",
        ];
        let grid = TextGrid::from_rows(rows);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 1);

        let mut state = TerminalMathState::default();
        let size = test_size(grid.columns as f32, 8.0);
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
        let prepared = prepared[0].expect("formula must render");
        assert_eq!(prepared.fitted_pixel_size, TEST_FONT_PX);
        assert!(prepared.display_style, "there is room for the block style here");
    }

    #[test]
    fn inline_formula_between_prose_keeps_the_terminal_font_size() {
        let fitted = fitted_size(&["value $x^2$ grows   "]);
        assert_eq!(
            fitted, TEST_FONT_PX,
            "a superscript must fit the line-gap budget without shrinking",
        );
    }

    #[test]
    fn adjacent_formula_rows_do_not_share_vertical_clip_budget() {
        let rows = [
            r"left $\frac{a_1+b^2}{c_i-d^3}+x_i^2$",
            r"mid $\int_0^\infty e^{-x^2}dx+\sum_{k=1}^n k^2$",
            r"right $\frac{\sum_{i=1}^n x_i}{\sqrt{1+x_{j-1}^2}}+y_{m+1}$",
        ];
        let grid = TextGrid::from_rows(&rows);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 3);
        let size = test_size(grid.columns as f32, grid.rows.len() as f32);
        let mut state = TerminalMathState::default();
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);

        for (index, overlay) in overlays.iter().enumerate() {
            let bounds = overlay.bounds(&size).expect("formula bounds");
            let metrics = TerminalMathState::default()
                .layout(overlay.formula_id, &overlay.source, TEST_FONT_PX, 1.0, false)
                .expect("formula layout")
                .metrics;
            let columns = ink_columns(overlay, &size, bounds, bounds.right, metrics.width);
            let absorbed = overlay.absorbable_rows(columns);
            let (above, below) = overlay.vertical_bleed(&size, absorbed);
            if index > 0 {
                assert_eq!(above, 0.0, "formula row {index} must not bleed into row above");
            }
            if index + 1 < overlays.len() {
                assert_eq!(below, 0.0, "formula row {index} must not bleed into row below");
            }

            let fitted = prepared[index].expect("dense formula must render");
            let layout = state
                .layout(
                    overlay.formula_id,
                    &overlay.source,
                    fitted.fitted_pixel_size,
                    1.0,
                    fitted.display_style,
                )
                .expect("fitted formula layout");
            let available_height = bounds.height() + fitted.bleed_top + fitted.bleed_bottom;
            assert!(
                layout.metrics.height + layout.metrics.depth <= available_height + 0.5,
                "formula row {index} ink {} exceeds its clip budget {}",
                layout.metrics.height + layout.metrics.depth,
                available_height,
            );
        }
    }

    /// The acceptance rule: formulas with the same delimiter should read at one size;
    /// at one size. Groups may differ from each other — an inline fraction is
    /// meant to be more compact than a block one — but within a group a reader
    /// scanning an answer must not see one formula shrunk against the others.
    #[test]
    fn formulas_of_the_same_kind_render_at_one_size() {
        let rows = &[
            "核心想法：与其硬选一个 prompt，不如软组合一堆 prompt 组件。      ",
            "                                                              ",
            "- 用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：   ",
            r"$$P_i = \sum_j a_{i,j} \cdot c_j$$                            ",
            "- 这样每个任务的 prompt 是所有组件的连续加权组合。              ",
            "                                                              ",
            "单行块紧贴正文：                                                ",
            r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$                       ",
            "下面继续说明。                                                  ",
            "                                                              ",
            r"高斯积分：$$\int_{-\infty}^{\infty} e^{-x^2}dx = \sqrt{\pi}$$   ",
            "                                                              ",
            "$$                                                            ",
            r"A = \begin{pmatrix}1 & 2 \\ 3 & 4\end{pmatrix}                ",
            "$$                                                            ",
            "                                                              ",
            r"内联的分式 $\frac{a+b}{c-d}$ 与根号 $\sqrt{x^2+y^2}$ 混排收尾。 ",
            r"行内的求和 $\sum_{i=1}^{n} i$ 和上标 $x^2$ 也在同一段里。       ",
        ];
        let grid = TextGrid::from_rows(rows);
        let overlays = scan_grid_with_hints(&grid);
        let size = test_size(grid.columns as f32, grid.rows.len() as f32);
        let mut state = TerminalMathState::default();
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);

        let mut sizes: BTreeMap<bool, Vec<(String, f32)>> = BTreeMap::new();
        for (overlay, prepared) in overlays.iter().zip(&prepared) {
            let prepared = prepared.expect("every formula in the sample must render");
            sizes
                .entry(overlay.display)
                .or_default()
                .push((overlay.source.to_string(), prepared.fitted_pixel_size));
        }
        assert_eq!(sizes.len(), 2, "sample must exercise both kinds");
        for (display, group) in sizes {
            assert!(group.len() >= 4, "each kind needs several samples, got {group:?}");
            // A hard formula boundary may require a small local reduction to
            // keep a script intact; unconstrained neighbours remain at the
            // terminal size and no formula is allowed to become unreadably small.
            assert!(
                group.iter().all(|(_, size)| *size >= TEST_FONT_PX * 0.9),
                "{} formulas must stay readable: {group:?}",
                if display { "block" } else { "inline" },
            );
        }
    }

    /// The ceiling behind the raised script sizes: a formula sharing its row
    /// with prose may grow past one row, but never past two — beyond that it
    /// stops being ink in a line gap and starts being ink on the neighbours.
    #[test]
    fn inline_math_never_grows_past_two_rows() {
        let sources = [
            r"$\frac{a+b}{c-d}$",
            r"$\int_0^\infty e^{-x^2}dx$",
            r"$\sqrt{\frac{a+b}{c+d}}$",
            r"$x_{i+1}^2+y_{j-1}^2$",
            r"$\sum_{i=1}^{n} i$",
        ];
        let size = test_size(60.0, 4.0);
        for source in sources {
            let row = format!("值 {source} 收敛                        ");
            let grid = TextGrid::from_rows(&[&row]);
            let overlays = scan_grid_with_hints(&grid);
            assert_eq!(overlays.len(), 1, "{source}");

            let mut state = TerminalMathState::default();
            let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
            let prepared = prepared[0].expect("formula must render");
            let layout = state
                .layout(
                    overlays[0].formula_id,
                    &overlays[0].source,
                    prepared.fitted_pixel_size,
                    1.0,
                    prepared.display_style,
                )
                .expect("fitted layout");
            let rows = (layout.metrics.height + layout.metrics.depth) / size.cell_height();
            assert!(rows <= 2.0, "{source} rendered {rows} rows tall");
        }
    }

    #[test]
    fn display_math_mask_only_covers_the_formula_source() {
        let grid = TextGrid::from_rows(&[
            "     explanation in the main pane                                  $0.00 spent      ",
            "     $$E = mc^2$$                                                                    ",
        ]);
        let overlays = scan_grid_with_hints(&grid);
        let coverage = CoverageMask::build(&overlays, &[Some(())]);

        assert!(coverage.covers(Point::new(1, Column(5))), "formula source is hidden");
        assert!(coverage.covers(Point::new(1, Column(16))), "formula source is hidden");
        assert!(
            !coverage.covers(Point::new(1, Column(17))),
            "centering space keeps its TUI background"
        );
        assert!(
            !coverage.covers(Point::new(1, Column(41))),
            "trailing TUI background is preserved"
        );
    }

    #[test]
    fn display_math_without_a_right_pane_uses_the_viewport() {
        let grid = TextGrid::from_rows(&[
            "     ordinary prose across one terminal row                                      ",
            "     $$E = mc^2$$                                                                  ",
        ]);
        let overlays = scan_grid_with_hints(&grid);

        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].widen_right_to, Some(grid.columns));
    }

    /// 诊断用：打印各场景的缩放比例，`cargo test diagnose -- --nocapture`。
    #[test]
    fn diagnose_sizes() {
        let blank = " ".repeat(80);
        let blank = blank.as_str();
        let long = r"\pm\quad \mp\quad \times\quad \div\quad \cdot\quad \ast\quad \circ\quad \bullet\quad \oplus\quad \otimes";
        let cases: Vec<(&str, Vec<&str>, bool)> = vec![
            ("display 3行 sum", vec![blank, "$$", r"\sum_{i=1}^{n} i^2", "$$", blank], false),
            ("display 1行 sum", vec![blank, r"$$\sum_{i=1}^{n} i^2$$", blank], false),
            ("display 1行 frac", vec![blank, r"$$\frac{x^2+1}{y-1}$$", blank], false),
            (
                "display 1行 矩阵",
                vec![blank, r"$$A=\begin{pmatrix}1&2\\3&4\end{pmatrix}$$", blank],
                false,
            ),
            (
                "display 3行 矩阵",
                vec![blank, "$$", r"A=\begin{pmatrix}1&2\\3&4\end{pmatrix}", "$$", blank],
                false,
            ),
            (
                "display 3行 大矩阵",
                vec![
                    blank,
                    "$$",
                    r"A=\begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix}",
                    "$$",
                    blank,
                ],
                false,
            ),
            ("display 3行 长串", vec![blank, "$$", long, "$$", blank], false),
            ("display 3行 紧贴正文", vec!["前文", "$$", r"\frac{x^2+1}{y-1}", "$$", "后文"], false),
            (
                "display 1行 紧贴正文",
                vec![
                    "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                    r"$$P_i = \sum_j attention_{i,j} \cdot component_j$$",
                    "- 这样每个任务的 prompt 不是从池子里挑一个，而是所有组件的连续加权组合。",
                ],
                false,
            ),
            (
                "display 1行 sum 夹住",
                vec![
                    "前面的说明文字：",
                    r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$",
                    "下面继续说明。",
                ],
                false,
            ),
            // 用户 08-05 截图的真实排版：上下都是跑满整行的长正文，公式的
            // 墨迹列被两侧的字覆盖，一行的高度就是全部预算。
            (
                "display 长正文夹住 sum",
                vec![
                    "单行块紧贴正文：- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                    r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$",
                    "下面继续说明。- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                ],
                false,
            ),
            (
                "display 长正文夹住 int",
                vec![
                    "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                    r"高斯积分：$$\int_{-\infty}^{\infty} e^{-x^2}dx = \sqrt{\pi}$$",
                    "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                ],
                false,
            ),
            (
                "display 1行 矩阵夹住",
                vec!["前面的说明：", r"$$A=\begin{pmatrix}1&2\\3&4\end{pmatrix}$$", "下面继续。"],
                false,
            ),
            ("inline x^2", vec!["value $x^2$ grows"], true),
            ("inline frac", vec![r"value $\frac{a}{b}$ grows"], true),
            ("inline 大 frac", vec![r"value $\frac{x^2+1}{y-1}$ grows"], true),
            ("inline sqrt", vec![r"value $\sqrt{x^2+y^2}$ grows"], true),
            ("inline sum", vec![r"value $\sum_{i=1}^{n} i$ grows"], true),
            ("inline int", vec![r"value $\int_0^\infty e^{-x^2}dx$ ok"], true),
            ("inline 短源码", vec![r"值 $x_{i+1}^2+y_{j-1}^2$ 收敛"], true),
        ];
        println!(
            "\n{:<22} {:>6} {:>6} {:>16} {:>16}",
            "场景", "比例", "样式", "墨迹 宽×高", "预算 宽×高"
        );
        for (name, rows, expected_inline) in cases {
            let grid = TextGrid::from_rows(&rows);
            let overlays = scan_grid_with_hints(&grid);
            assert_eq!(overlays.len(), 1, "{name}: expected one formula");
            let overlay = &overlays[0];
            assert_eq!(!overlay.display, expected_inline, "{name}: unexpected formula style");
            let size = test_size(grid.columns as f32, grid.rows.len() as f32);
            let mut state = TerminalMathState::default();

            let bounds = overlay.bounds(&size).expect("bounds");
            let right = overlay.widen_right_to.map_or(bounds.right, |column| {
                (size.padding_x() + column as f32 * size.cell_width()).max(bounds.right)
            });
            let base = state
                .layout(overlay.formula_id, &overlay.source, TEST_FONT_PX, 1.0, overlay.display)
                .expect("layout")
                .metrics;
            let absorbed =
                overlay.absorbable_rows(ink_columns(overlay, &size, bounds, right, base.width));
            let (bleed_top, bleed_bottom) = overlay.vertical_bleed(&size, absorbed);
            let budget_width = right - bounds.left - FORMULA_INSET * 2.0;
            let budget_height = bounds.height() + bleed_top + bleed_bottom;

            let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
            let prepared = prepared[0].expect("formula must render");
            println!(
                "{name:<22} {:>5.0}% {:>6} {:>7.0}×{:<8.0} {:>7.0}×{:<8.0}",
                prepared.fitted_pixel_size / TEST_FONT_PX * 100.0,
                if prepared.display_style { "块级" } else { "行内" },
                base.width,
                base.height + base.depth,
                budget_width,
                budget_height,
            );
        }
    }
}
