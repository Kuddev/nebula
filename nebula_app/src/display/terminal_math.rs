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
#[cfg(feature = "legacy-shell")]
use crate::renderer::math::MathClip;
#[cfg(feature = "legacy-shell")]
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
/// How far below its opener a standalone display block may still close once
/// the search has had to step over a TUI paragraph gap (see
/// [`TextGrid::find_closing`]). Bridging a gap is a guess, so unlike the plain
/// search it is not allowed to run to the bottom of the grid.
const DISPLAY_BLOCK_GAP_SEARCH_ROWS: usize = BARE_BRACKET_SEARCH_ROWS;
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
    fn find_closing(
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
    fn delimiter_owns_row(&self, position: GridPosition, length: usize) -> bool {
        self.span_is_blank(position.row, 0, position.column)
            && self.span_is_blank(position.row, position.column + length, self.columns)
    }

    fn row_text(&self, row: usize) -> String {
        self.rows.get(row).into_iter().flat_map(|cells| cells.iter().flatten()).collect()
    }

    /// Whether `row` holds nothing but `delimiter`.
    fn row_is_only(&self, row: usize, delimiter: &[char]) -> bool {
        self.row_text(row).trim().chars().eq(delimiter.iter().copied())
    }

    /// Row-level version of the source test, for rows a bridged search has to
    /// vouch for on their own. In the Codex shape the row after the gap is a
    /// list item in the renderer's eyes, so its leading sign is also read as
    /// the list marker it became: `- 2xy` has no evidence as a whole, but a
    /// compact operand behind the marker is one, while `- 修复 SSH 重连` (a
    /// real list item) is not.
    fn row_has_math_evidence(&self, row: usize) -> bool {
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
#[cfg(feature = "legacy-shell")]
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
#[path = "terminal_math/tests.rs"]
mod tests;
