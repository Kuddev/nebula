//! Renderer-agnostic terminal render contract (model → viewport → frontend).
//!
//! Layering: `Term`/grid (terminal model) → this module (viewport protocol +
//! plain-data snapshot) → a frontend renderer (today the GPUI element; later
//! possibly a self-managed glyph atlas or another backend). Frontends depend
//! on these types; nothing here may depend on a UI framework.
//!
//! Hard rules encoded here:
//! - Terminal content only exists as a cell grid (column × fixed step). There
//!   is no "string flow" representation; typography must never move a glyph.
//! - A viewport is immutable once issued and carries a monotonically
//!   increasing `revision`: a stale resize must never override a newer one.
//! - Pixel sizes reported to the PTY / applications are always the exact
//!   `columns × cell_width` product, never leftover pixels.
//! - A pixel-size change is reported even when rows/cols are unchanged:
//!   applications may care about pixel metrics.

pub mod boxdraw;

use crate::event::{EventListener, WindowSize};
use crate::term::cell::Flags;
use crate::term::color::Colors;
use crate::term::{Term, point_to_viewport_from};
use crate::vte::ansi::{Color, CursorShape, NamedColor};

/// Cell metrics measured by the frontend (logical pixels + device scale).
///
/// Contract: once issued, every glyph in the grid lands at
/// `column × cell_width`; no layout engine may change the step.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CellMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub scale: f32,
}

impl CellMetrics {
    pub fn device_cell_width(&self) -> u16 {
        (self.cell_width * self.scale).round().max(1.0) as u16
    }

    pub fn device_cell_height(&self) -> u16 {
        (self.cell_height * self.scale).round().max(1.0) as u16
    }
}

pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 1;

/// One immutable grid decision derived from a pane's content rect.
///
/// The caller must pass the *content* rect in logical pixels — paddings,
/// dividers, tab bars and side bars already subtracted. UI-framework logical
/// points must be converted before they reach this type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TerminalViewport {
    pub cols: u16,
    pub rows: u16,
    /// Device-pixel cell size: the unit used when reporting to the PTY and
    /// answering text-area size queries.
    pub cell_width_px: u16,
    pub cell_height_px: u16,
    pub revision: u64,
}

impl TerminalViewport {
    /// Floor-divide a content rect into a grid; clamps to the minimum grid.
    pub fn from_content_size(
        width: f32,
        height: f32,
        metrics: &CellMetrics,
        revision: u64,
    ) -> Self {
        let cols = (width / metrics.cell_width.max(1.0)).floor().max(MIN_COLS as f32) as u16;
        let rows = (height / metrics.cell_height.max(1.0)).floor().max(MIN_ROWS as f32) as u16;
        Self {
            cols,
            rows,
            cell_width_px: metrics.device_cell_width(),
            cell_height_px: metrics.device_cell_height(),
            revision,
        }
    }

    /// Exact text-area width: `cols × cell_width`.
    pub fn text_area_width_px(&self) -> u32 {
        u32::from(self.cols) * u32::from(self.cell_width_px)
    }

    /// Exact text-area height: `rows × cell_height`.
    pub fn text_area_height_px(&self) -> u32 {
        u32::from(self.rows) * u32::from(self.cell_height_px)
    }

    /// The size reported to the PTY and to text-area size queries.
    pub fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell_width_px,
            cell_height: self.cell_height_px,
        }
    }

    pub fn grid_eq(&self, other: &Self) -> bool {
        self.cols == other.cols && self.rows == other.rows
    }

    pub fn pixel_eq(&self, other: &Self) -> bool {
        self.cell_width_px == other.cell_width_px && self.cell_height_px == other.cell_height_px
    }
}

/// Outcome of [`ViewportTracker::observe`].
#[derive(Copy, Clone, Debug)]
pub struct ViewportChange {
    pub viewport: TerminalViewport,
    /// Rows/cols changed: the grid and the PTY must be resized.
    pub grid_changed: bool,
    /// Device cell size changed: the PTY must be re-informed even when the
    /// grid stayed the same.
    pub pixel_changed: bool,
}

/// Coalesces layout observations into viewport revisions.
///
/// `observe` returns `None` while nothing changed, so per-frame layout code
/// can call it unconditionally. Every returned viewport carries a strictly
/// increasing revision; consumers must drop events whose revision is older
/// than the latest they have seen.
#[derive(Default)]
pub struct ViewportTracker {
    current: Option<TerminalViewport>,
    issued: u64,
}

impl ViewportTracker {
    pub fn current(&self) -> Option<&TerminalViewport> {
        self.current.as_ref()
    }

    pub fn observe(
        &mut self,
        width: f32,
        height: f32,
        metrics: &CellMetrics,
    ) -> Option<ViewportChange> {
        let candidate =
            TerminalViewport::from_content_size(width, height, metrics, self.issued + 1);
        let (grid_changed, pixel_changed) = match &self.current {
            Some(current) => (!current.grid_eq(&candidate), !current.pixel_eq(&candidate)),
            None => (true, true),
        };
        if !grid_changed && !pixel_changed {
            return None;
        }
        self.issued += 1;
        self.current = Some(candidate);
        Some(ViewportChange { viewport: candidate, grid_changed, pixel_changed })
    }
}

/// One renderable cell: text plus unresolved style. Colors stay as vte
/// [`Color`] values; the frontend resolves them against its palette and the
/// snapshot's [`RenderSnapshot::color_overrides`].
pub struct SnapCell {
    pub col: u16,
    /// Base character plus any zero-width combining characters.
    pub text: String,
    pub fg: Color,
    /// This cell's background, INVERSE already applied — the same value the
    /// matching [`BgRun`] carries, repeated here because default backgrounds
    /// are suppressed at the source and so have no run at all.
    ///
    /// 前端要它来算对比度：应用写死的前景色是否可读，取决于它**这一格**底下
    /// 是什么颜色，不是取决于主题底色。
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// A run of column-contiguous cells sharing one width class. `wide`
/// segments step two columns per cell; narrow segments step one.
pub struct TextSegment {
    pub row: u16,
    pub start_col: u16,
    pub wide: bool,
    pub cells: Vec<SnapCell>,
}

impl TextSegment {
    pub fn step(&self) -> u16 {
        if self.wide { 2 } else { 1 }
    }
}

/// Geometry-only cell run (selection highlight).
pub struct CellRun {
    pub row: u16,
    pub start: u16,
    pub end: u16,
}

/// Background run with its unresolved color.
pub struct BgRun {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub color: Color,
}

pub struct CursorSnapshot {
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
    /// Cursor sits on a wide char: draw it two cells wide.
    pub wide: bool,
    /// Glyph under the cursor. Claude Code / Codex hide DECSCUSR and draw a
    /// reverse-video space or block element; the frontend needs the cell to
    /// recolor that fake cursor (old shell `is_application_cursor_cell`).
    pub cell_ch: char,
    pub cell_flags: Flags,
    pub cell_bg: Color,
}

/// A cell rendered by built-in geometry ([`boxdraw`]) instead of a font:
/// box-drawing, block elements and Powerline separators must cover the cell
/// exactly and join seamlessly across cells — no font (CJK fonts above all)
/// guarantees that, so these never enter a text segment.
pub struct BoxGlyph {
    pub row: u16,
    pub col: u16,
    /// The terminal marked this cell wide: geometry spans two columns.
    pub wide: bool,
    pub ch: char,
    pub fg: Color,
    pub bold: bool,
}

/// Everything a frontend needs to paint one frame, as plain data. Built in a
/// single pass under the `Term` lock; owning no references, it lets the lock
/// drop before any painting or color resolution happens.
pub struct RenderSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub display_offset: usize,
    /// OSC 4/10/11… override table copied out of the terminal.
    pub color_overrides: Colors,
    pub bg_runs: Vec<BgRun>,
    pub selection_runs: Vec<CellRun>,
    pub segments: Vec<TextSegment>,
    /// Cells routed to built-in geometry; disjoint from `segments`.
    pub box_glyphs: Vec<BoxGlyph>,
    pub cursor: Option<CursorSnapshot>,
}

pub struct SnapshotConfig {
    pub rows: u16,
    pub cols: u16,
}

impl RenderSnapshot {
    pub fn capture<T: EventListener>(term: &Term<T>, cfg: &SnapshotConfig) -> Self {
        let rows = cfg.rows as usize;
        let cols = cfg.cols as usize;
        let content = term.renderable_content_with_viewport(rows, cols);
        let display_offset = content.display_offset;
        let viewport_origin = content.viewport_origin;
        let selection_range = content.selection;
        let cursor_vp = point_to_viewport_from(viewport_origin, content.cursor.point);
        let cursor_shape = content.cursor.shape;

        let mut snap = Self {
            cols: cfg.cols,
            rows: cfg.rows,
            display_offset,
            color_overrides: *content.colors,
            bg_runs: Vec::new(),
            selection_runs: Vec::new(),
            segments: Vec::new(),
            box_glyphs: Vec::new(),
            cursor: None,
        };

        fn push_bg(runs: &mut Vec<BgRun>, row: u16, col: u16, color: Color) {
            if let Some(last) = runs.last_mut() {
                if last.row == row && last.end == col && last.color == color {
                    last.end = col + 1;
                    return;
                }
            }
            runs.push(BgRun { row, start: col, end: col + 1, color });
        }

        fn push_sel(runs: &mut Vec<CellRun>, row: u16, col: u16) {
            if let Some(last) = runs.last_mut() {
                if last.row == row && last.end == col {
                    last.end = col + 1;
                    return;
                }
            }
            runs.push(CellRun { row, start: col, end: col + 1 });
        }

        // Segment assembly state: (row, wide, next expected col).
        let mut open: Option<TextSegment> = None;
        let flush = |segments: &mut Vec<TextSegment>, open: &mut Option<TextSegment>| {
            if let Some(seg) = open.take() {
                segments.push(seg);
            }
        };

        for indexed in content.display_iter {
            let Some(vp) = point_to_viewport_from(viewport_origin, indexed.point) else { continue };
            let (row, col) = (vp.line, vp.column.0);
            if row >= rows || col >= cols {
                continue;
            }
            let (row, col) = (row as u16, col as u16);
            let flags = indexed.cell.flags;
            let bold = flags.intersects(Flags::BOLD);
            let mut fg = indexed.cell.fg;
            let mut bg = indexed.cell.bg;
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            // Default background is suppressed at the source; anything else
            // is emitted and resolved by the frontend.
            if bg != Color::Named(NamedColor::Background) {
                push_bg(&mut snap.bg_runs, row, col, bg);
            }
            if selection_range.is_some_and(|range| range.contains(indexed.point)) {
                push_sel(&mut snap.selection_runs, row, col);
            }

            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                || flags.contains(Flags::HIDDEN)
            {
                continue;
            }

            let c = indexed.cell.c;
            if boxdraw::is_builtin(c) {
                // Built-in geometry cells never enter a text segment — the
                // typography engine has no authority over them. Combining
                // characters on box glyphs are meaningless and dropped.
                snap.box_glyphs.push(BoxGlyph {
                    row,
                    col,
                    wide: flags.contains(Flags::WIDE_CHAR),
                    ch: c,
                    fg,
                    bold,
                });
                continue;
            }
            if c == ' '
                && indexed.cell.extra.is_none()
                && !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
            {
                continue;
            }

            let mut text = String::new();
            text.push(c);
            if let Some(zerowidth) = indexed.cell.zerowidth() {
                text.extend(zerowidth);
            }

            let wide = flags.contains(Flags::WIDE_CHAR);
            let cell = SnapCell {
                col,
                text,
                fg,
                bg,
                bold,
                italic: flags.intersects(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: flags.contains(Flags::STRIKEOUT),
            };

            // 分段只由内容与宽度类决定，任何光标/焦点/闪烁状态都不得掺入：
            // 前端按 cell 原点逐字起笔，反色只是换色；若分段随光标状态变化，
            // 行缓存键会跟着抖动，重塑形本身就是可见的跳字。
            let continues = open.as_ref().is_some_and(|seg| {
                seg.row == row
                    && seg.wide == wide
                    && seg.start_col + seg.step() * seg.cells.len() as u16 == col
            });
            if !continues {
                flush(&mut snap.segments, &mut open);
            }
            match &mut open {
                Some(seg) => seg.cells.push(cell),
                None => {
                    open = Some(TextSegment { row, start_col: col, wide, cells: vec![cell] });
                },
            }
        }
        flush(&mut snap.segments, &mut open);

        if let Some(vp) = cursor_vp {
            if vp.line < rows && vp.column.0 < cols {
                let cell = &term.grid()[content.cursor.point];
                snap.cursor = Some(CursorSnapshot {
                    row: vp.line as u16,
                    col: vp.column.0 as u16,
                    shape: cursor_shape,
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                    cell_ch: cell.c,
                    cell_flags: cell.flags,
                    cell_bg: cell.bg,
                });
            }
        }

        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::grid::Dimensions;
    use crate::index::{Column, Line};
    use crate::term::Config;

    struct TestSize {
        cols: usize,
        rows: usize,
    }

    impl Dimensions for TestSize {
        fn total_lines(&self) -> usize {
            self.rows
        }

        fn screen_lines(&self) -> usize {
            self.rows
        }

        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn metrics() -> CellMetrics {
        CellMetrics { cell_width: 9.0, cell_height: 18.0, scale: 2.0 }
    }

    #[test]
    fn viewport_floor_division_and_clamps() {
        let vp = TerminalViewport::from_content_size(93.0, 40.0, &metrics(), 1);
        assert_eq!((vp.cols, vp.rows), (10, 2));
        // Below one cell: clamps to the minimum grid instead of zero.
        let vp = TerminalViewport::from_content_size(3.0, 3.0, &metrics(), 2);
        assert_eq!((vp.cols, vp.rows), (MIN_COLS, MIN_ROWS));
    }

    #[test]
    fn viewport_reports_exact_text_area_pixels() {
        let vp = TerminalViewport::from_content_size(95.0, 41.0, &metrics(), 1);
        // 95/9 → 10 cols; device cell = 18px → 180, never the leftover 190.
        assert_eq!(vp.text_area_width_px(), u32::from(vp.cols) * 18);
        assert_eq!(vp.window_size().cell_width, 18);
        assert_eq!(vp.window_size().cell_height, 36);
    }

    #[test]
    fn tracker_coalesces_and_advances_revision() {
        let mut tracker = ViewportTracker::default();
        let first = tracker.observe(900.0, 360.0, &metrics()).expect("initial viewport");
        assert!(first.grid_changed && first.pixel_changed);

        // Sub-cell jitter: same grid, same pixels → no event.
        assert!(tracker.observe(902.0, 361.0, &metrics()).is_none());

        // One more column: grid change, revision strictly increases.
        let second = tracker.observe(911.0, 360.0, &metrics()).expect("grid change");
        assert!(second.grid_changed);
        assert!(second.viewport.revision > first.viewport.revision);

        // Same grid but larger glyphs (font size change on a fluke-equal
        // grid): pixel change alone must still be reported.
        let mut larger = metrics();
        larger.scale = 3.0;
        let third = tracker.observe(911.0 / 9.0 * 9.0, 360.0, &larger).expect("pixel change");
        assert!(third.pixel_changed);
    }

    fn term_with(content: &[&str]) -> Term<VoidListener> {
        let size = TestSize { cols: 8, rows: 4 };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        for (line, text) in content.iter().enumerate() {
            let mut col = 0usize;
            for ch in text.chars() {
                let line = Line(line as i32);
                let cell = &mut term.grid_mut()[line][Column(col)];
                cell.c = ch;
                if unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) == 2 {
                    cell.flags.insert(Flags::WIDE_CHAR);
                    col += 1;
                    term.grid_mut()[line][Column(col)].flags.insert(Flags::WIDE_CHAR_SPACER);
                }
                col += 1;
            }
        }
        term
    }

    fn cfg(rows: u16, cols: u16) -> SnapshotConfig {
        SnapshotConfig { rows, cols }
    }

    #[test]
    fn capture_splits_segments_on_width_class() {
        let term = term_with(&["ab中c"]);
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));
        let rows: Vec<_> =
            snap.segments.iter().map(|s| (s.row, s.start_col, s.wide, s.cells.len())).collect();
        // "ab" narrow at col 0, "中" wide at col 2 (spacer col 3 skipped),
        // "c" narrow resumes at col 4.
        assert_eq!(rows, vec![(0, 0, false, 2), (0, 2, true, 1), (0, 4, false, 1)]);
    }

    #[test]
    fn capture_segments_ignore_cursor_position() {
        // 分段随光标状态变化曾导致整行按闪烁相位重塑形（可见跳字）；
        // 光标只能以 CursorSnapshot 形式出现，绝不改变文本分段。
        let mut term = term_with(&["abc"]);
        term.grid_mut().cursor.point = crate::index::Point::new(Line(0), Column(1));
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));
        let shape: Vec<_> = snap.segments.iter().map(|s| (s.start_col, s.cells.len())).collect();
        assert_eq!(shape, vec![(0, 3)]);
        let cursor = snap.cursor.expect("cursor visible");
        assert_eq!((cursor.row, cursor.col, cursor.wide), (0, 1, false));
    }

    #[test]
    fn capture_maps_a_deferred_resize_crop_to_visual_rows() {
        let size = TestSize { cols: 8, rows: 4 };
        let config = Config { conpty_resize: true, ..Config::default() };
        let mut term = Term::new(config, &size, VoidListener);
        for (line, ch) in ['a', 'b', 'c', 'd'].into_iter().enumerate() {
            term.grid_mut()[Line(line as i32)][Column(0)].c = ch;
        }
        term.grid_mut().cursor.point = crate::index::Point::new(Line(2), Column(0));

        // A two-row visual viewport previews the same bottom crop that the
        // settled ConPTY resize will commit: grid rows 2..4 become visual 0..2.
        let snap = RenderSnapshot::capture(&term, &cfg(2, 8));
        let rows: Vec<_> = snap
            .segments
            .iter()
            .map(|segment| (segment.row, segment.cells[0].text.as_str()))
            .collect();
        assert_eq!(rows, vec![(0, "c"), (1, "d")]);
        assert_eq!(snap.cursor.expect("cropped cursor").row, 0);
    }

    #[test]
    fn capture_marks_wide_cursor() {
        let mut term = term_with(&["中"]);
        term.grid_mut().cursor.point = crate::index::Point::new(Line(0), Column(0));
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));
        assert!(snap.cursor.expect("cursor").wide);
    }

    #[test]
    fn capture_keeps_a_hidden_cursor_and_its_cell() {
        use crate::vte::ansi::{Handler, NamedPrivateMode};

        let mut term = term_with(&[" "]);
        term.grid_mut().cursor.point = crate::index::Point::new(Line(0), Column(0));
        term.grid_mut()[Line(0)][Column(0)].flags.insert(Flags::INVERSE);
        term.unset_private_mode(NamedPrivateMode::ShowCursor.into());
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));
        let cursor = snap.cursor.expect("hidden cursor still has a cell");
        assert_eq!(cursor.shape, CursorShape::Hidden);
        assert_eq!((cursor.row, cursor.col), (0, 0));
        assert!(cursor.cell_flags.contains(Flags::INVERSE));
        assert_eq!(cursor.cell_ch, ' ');
        assert!(
            snap.bg_runs.iter().any(|run| run.row == 0 && run.start <= 0 && 0 < run.end),
            "inverse space still emits a bg run so the frontend can skip the black cell"
        );
    }

    #[test]
    fn capture_keeps_decscusr_hidden_block_glyph() {
        use crate::vte::ansi::Handler;

        let mut term = term_with(&["█"]);
        term.grid_mut().cursor.point = crate::index::Point::new(Line(0), Column(0));
        term.set_cursor_shape(CursorShape::Hidden);
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));
        let cursor = snap.cursor.expect("DECSCUSR hidden cursor still has a cell");
        assert_eq!(cursor.shape, CursorShape::Hidden);
        assert_eq!(cursor.cell_ch, '█');
        assert_eq!(snap.box_glyphs.iter().map(|g| g.ch).collect::<Vec<_>>(), vec!['█']);
    }

    #[test]
    fn capture_routes_builtin_glyphs_to_geometry() {
        let term = term_with(&["a─█b"]);
        let snap = RenderSnapshot::capture(&term, &cfg(4, 8));

        let boxes: Vec<_> = snap.box_glyphs.iter().map(|b| (b.row, b.col, b.ch, b.wide)).collect();
        assert_eq!(boxes, vec![(0, 1, '─', false), (0, 2, '█', false)]);

        // Text segments keep only 'a' and 'b', split at the geometry cells.
        let segments: Vec<_> = snap.segments.iter().map(|s| (s.start_col, s.cells.len())).collect();
        assert_eq!(segments, vec![(0, 1), (3, 1)]);
    }
}
