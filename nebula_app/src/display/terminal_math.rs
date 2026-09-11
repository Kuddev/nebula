//! Native TeX overlays for formula delimiters emitted into a terminal grid.
//!
//! The terminal remains the source of truth: this module never mutates cells,
//! scrollback, cursor positions, selections, or copied text. It only replaces
//! visible delimiter spans during the final paint pass.

#[path = "terminal_math/scan.rs"]
mod scan;
pub(crate) use scan::scan_visible;
use scan::*;

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
    layout_resolver: Option<LayoutResolver>,
    last_scan: Option<(TextGrid, GridScanResult)>,
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
            layout_resolver: None,
            last_scan: None,
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

pub(crate) type LayoutResolver =
    Arc<dyn Fn(Arc<str>, f32, f32, bool) -> Option<Arc<MathLayout>> + Send + Sync>;

impl TerminalMathState {
    pub(crate) fn set_layout_resolver(&mut self, resolver: LayoutResolver) {
        self.layout_resolver = Some(resolver);
    }

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
        let scan = match &self.last_scan {
            Some((previous, scan)) if previous == grid => scan.clone(),
            _ => {
                let scan = scan_grid_result(grid);
                self.last_scan = Some((grid.clone(), scan.clone()));
                scan
            },
        };
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
        source: &Arc<str>,
        pixel_size: f32,
        pixels_per_point: f32,
        display: bool,
    ) -> Result<Arc<MathLayout>, crate::math::MathError> {
        if let Some(resolve) = &self.layout_resolver {
            return resolve(source.clone(), pixel_size, pixels_per_point, display)
                .ok_or_else(|| crate::math::MathError::new(crate::math::MathErrorKind::Parse, 0));
        }
        let key = FormulaCacheKey::new(formula_id, pixel_size, pixels_per_point, display);
        self.cache
            .get_or_insert_with(key, || {
                compile_formula(source, display, pixel_size, pixels_per_point, DEFAULT_LIMITS)
            })
            .map(|layout| Arc::new(layout.clone()))
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
