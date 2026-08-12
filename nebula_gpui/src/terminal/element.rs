//! 终端网格的自定义 GPUI Element。
//!
//! 热路径约定：paint 内只对 `Term` 加一次锁，把可见网格快照成纯数据后立刻放
//! 锁，再用快照分三层绘制（背景块 → 选区/光标底 → 文字段），文字按
//! `force_width` 锁定单元格步进，保证 CJK 宽字符与网格对齐。

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, HitboxBehavior, Hsla, InspectorElementId,
    LayoutId, Pixels, Rgba, SharedString, Style, TextRun, UnderlineStyle, Window, fill, outline,
    point, px, relative, size,
};
use gpui_component::PixelsExt as _;
use nebula_terminal::index::Point as TermPoint;
use nebula_terminal::term::cell::Flags;
use nebula_terminal::term::point_to_viewport;
use nebula_terminal::vte::ansi::CursorShape;

use super::colors;
use super::view::TerminalView;

pub struct TerminalElement {
    view: gpui::Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: gpui::Entity<TerminalView>) -> Self {
        Self { view }
    }
}

pub struct TermLayout {
    cell_width: Pixels,
    line_height: Pixels,
    rows: usize,
}

struct CellSnap {
    col: usize,
    text: String,
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    wide: bool,
}

struct BgRun {
    row: usize,
    start: usize,
    end: usize,
    color: Rgba,
}

struct CursorSnap {
    row: usize,
    col: usize,
    shape: CursorShape,
    wide: bool,
}

struct Snapshot {
    rows: Vec<Vec<CellSnap>>,
    bgs: Vec<BgRun>,
    selection: Vec<BgRun>,
    cursor: Option<CursorSnap>,
}

fn rgba_eq(a: Rgba, b: Rgba) -> bool {
    a.r == b.r && a.g == b.g && a.b == b.b && a.a == b.a
}

impl TerminalElement {
    /// 在一次锁内把可见内容快照成绘制数据。
    fn snapshot(&self, rows: usize, cols: usize, focused: bool, cx: &App) -> Option<Snapshot> {
        let view = self.view.read(cx);
        let session = view.session.as_ref()?;
        let term = session.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let palette = content.colors;
        let selection_range = content.selection;

        let cursor_vp = point_to_viewport(display_offset, content.cursor.point);
        let cursor_shape = content.cursor.shape;

        let mut snap = Snapshot {
            rows: (0..rows).map(|_| Vec::new()).collect(),
            bgs: Vec::new(),
            selection: Vec::new(),
            cursor: None,
        };

        let push_bg = |bgs: &mut Vec<BgRun>, row: usize, col: usize, color: Rgba| {
            if let Some(last) = bgs.last_mut() {
                if last.row == row && last.end == col && rgba_eq(last.color, color) {
                    last.end = col + 1;
                    return;
                }
            }
            bgs.push(BgRun { row, start: col, end: col + 1, color });
        };

        for indexed in content.display_iter {
            let Some(vp) = point_to_viewport(display_offset, indexed.point) else { continue };
            let (row, col) = (vp.line, vp.column.0);
            if row >= rows || col >= cols {
                continue;
            }
            let flags = indexed.cell.flags;
            let bold = flags.intersects(Flags::BOLD);
            let mut fg = colors::resolve(indexed.cell.fg, palette, bold);
            let mut bg = colors::resolve(indexed.cell.bg, palette, false);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            if !rgba_eq(bg, colors::BACKGROUND) {
                push_bg(&mut snap.bgs, row, col, bg);
            }
            if selection_range.is_some_and(|range| range.contains(indexed.point)) {
                push_bg(&mut snap.selection, row, col, colors::SELECTION);
            }

            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                || flags.contains(Flags::HIDDEN)
            {
                continue;
            }

            let is_cursor_cell = focused
                && matches!(cursor_shape, CursorShape::Block)
                && cursor_vp.is_some_and(|c| c.line == row && c.column.0 == col);

            let c = indexed.cell.c;
            if c == ' '
                && indexed.cell.extra.is_none()
                && !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
                && !is_cursor_cell
            {
                continue;
            }

            let mut text = String::new();
            text.push(c);
            if let Some(zerowidth) = indexed.cell.zerowidth() {
                text.extend(zerowidth);
            }

            snap.rows[row].push(CellSnap {
                col,
                text,
                fg: if is_cursor_cell { colors::BACKGROUND.into() } else { fg.into() },
                bold,
                italic: flags.intersects(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: flags.contains(Flags::STRIKEOUT),
                wide: flags.contains(Flags::WIDE_CHAR),
            });
        }

        if let Some(vp) = cursor_vp {
            if vp.line < rows && vp.column.0 < cols && cursor_shape != CursorShape::Hidden {
                let wide = term.grid()
                    [TermPoint::new(content.cursor.point.line, content.cursor.point.column)]
                .flags
                .contains(Flags::WIDE_CHAR);
                snap.cursor =
                    Some(CursorSnap { row: vp.line, col: vp.column.0, shape: cursor_shape, wide });
            }
        }

        Some(snap)
    }
}

impl gpui::IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TermLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (font, font_size) = {
            let view = self.view.read(cx);
            (view.font.clone(), view.font_size)
        };
        let sample = window.text_system().shape_line(
            SharedString::new_static("M"),
            font_size,
            &[TextRun {
                len: 1,
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let cell_width = px(sample.width.as_f32().max(1.0));
        let view = self.view.read(cx);
        let line_height = px((font_size.as_f32() * view.line_height_mul).round().max(1.0));

        let cols = ((bounds.size.width.as_f32() / cell_width.as_f32()).floor().max(2.0)) as usize;
        let rows = ((bounds.size.height.as_f32() / line_height.as_f32()).floor().max(1.0)) as usize;

        let scale = window.scale_factor();
        self.view.update(cx, |view, _| {
            view.set_layout(bounds.origin, cell_width, line_height, cols, rows, scale);
        });

        window.insert_hitbox(bounds, HitboxBehavior::Normal);

        TermLayout { cell_width, line_height, rows }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(fill(bounds, colors::BACKGROUND));

        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            gpui::ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let focused = focus_handle.is_focused(window);
        let cols =
            ((bounds.size.width.as_f32() / layout.cell_width.as_f32()).floor().max(2.0)) as usize;
        let Some(snap) = self.snapshot(layout.rows, cols, focused, cx) else {
            return;
        };

        let cell_rect = |row: usize, start: usize, count: usize| -> Bounds<Pixels> {
            Bounds::new(
                point(
                    bounds.origin.x + layout.cell_width * start as f32,
                    bounds.origin.y + layout.line_height * row as f32,
                ),
                size(layout.cell_width * count as f32, layout.line_height),
            )
        };

        for run in &snap.bgs {
            window.paint_quad(fill(cell_rect(run.row, run.start, run.end - run.start), run.color));
        }
        for run in &snap.selection {
            window.paint_quad(fill(cell_rect(run.row, run.start, run.end - run.start), run.color));
        }

        // 聚焦时的块状光标垫在文字下面，让字形以反色盖在上面。
        if let Some(cursor) = &snap.cursor {
            if focused && matches!(cursor.shape, CursorShape::Block) {
                let width = if cursor.wide { 2 } else { 1 };
                window.paint_quad(fill(cell_rect(cursor.row, cursor.col, width), colors::CURSOR));
            }
        }

        let (font, bold_font, italic_font, bold_italic_font, font_size) = {
            let view = self.view.read(cx);
            (
                view.font.clone(),
                view.font_bold.clone(),
                view.font_italic.clone(),
                view.font_bold_italic.clone(),
                view.font_size,
            )
        };
        let pick_font = |bold: bool, italic: bool| match (bold, italic) {
            (false, false) => font.clone(),
            (true, false) => bold_font.clone(),
            (false, true) => italic_font.clone(),
            (true, true) => bold_italic_font.clone(),
        };

        for (row, cells) in snap.rows.iter().enumerate() {
            let mut i = 0;
            while i < cells.len() {
                // 一段：列连续且宽度类别一致的单元格，样式差异用多个 TextRun 表达。
                let seg_wide = cells[i].wide;
                let seg_start_col = cells[i].col;
                let step = if seg_wide { 2 } else { 1 };
                let mut text = String::new();
                let mut runs: Vec<TextRun> = Vec::new();
                let mut expected = seg_start_col;
                let mut cell_count = 0usize;
                while i < cells.len() && cells[i].wide == seg_wide && cells[i].col == expected {
                    let cell = &cells[i];
                    let bytes = cell.text.len();
                    let underline = cell.underline.then(|| UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(cell.fg),
                        wavy: false,
                    });
                    let strikethrough = cell.strikethrough.then(|| gpui::StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(cell.fg),
                    });
                    let mergeable = runs.last().is_some_and(|run: &TextRun| {
                        run.color == cell.fg
                            && run.font == pick_font(cell.bold, cell.italic)
                            && run.underline == underline
                            && run.strikethrough == strikethrough
                    });
                    if mergeable {
                        runs.last_mut().unwrap().len += bytes;
                    } else {
                        runs.push(TextRun {
                            len: bytes,
                            font: pick_font(cell.bold, cell.italic),
                            color: cell.fg,
                            background_color: None,
                            underline,
                            strikethrough,
                        });
                    }
                    text.push_str(&cell.text);
                    expected += step;
                    cell_count += 1;
                    i += 1;
                }
                if cell_count == 0 {
                    i += 1;
                    continue;
                }
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    font_size,
                    &runs,
                    Some(layout.cell_width * step as f32),
                );
                let origin = point(
                    bounds.origin.x + layout.cell_width * seg_start_col as f32,
                    bounds.origin.y + layout.line_height * row as f32,
                );
                let _ = shaped.paint(origin, layout.line_height, window, cx);
            }
        }

        if let Some(cursor) = &snap.cursor {
            let width = if cursor.wide { 2 } else { 1 };
            let rect = cell_rect(cursor.row, cursor.col, width);
            match (focused, cursor.shape) {
                (true, CursorShape::Block) => {}, // 已在文字层下方填充
                (false, CursorShape::Block | CursorShape::HollowBlock)
                | (true, CursorShape::HollowBlock) => {
                    window.paint_quad(outline(rect, colors::CURSOR, gpui::BorderStyle::Solid));
                },
                (_, CursorShape::Beam) => {
                    window.paint_quad(fill(
                        Bounds::new(rect.origin, size(px(2.0), layout.line_height)),
                        colors::CURSOR,
                    ));
                },
                (_, CursorShape::Underline) => {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(rect.origin.x, rect.origin.y + layout.line_height - px(2.0)),
                            size(rect.size.width, px(2.0)),
                        ),
                        colors::CURSOR,
                    ));
                },
                (_, CursorShape::Hidden) => {},
            }

            // IME 组合文本锚点与预编辑串：跟随光标单元格。
            let anchor = cell_rect(cursor.row, cursor.col, 1);
            let marked = self.view.read(cx).marked_text.clone();
            self.view.update(cx, |view, _| view.ime_bounds = anchor);
            if let Some(marked) = marked.filter(|m| !m.is_empty()) {
                let run = TextRun {
                    len: marked.len(),
                    font: font.clone(),
                    color: colors::FOREGROUND.into(),
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(colors::FOREGROUND.into()),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(marked),
                    font_size,
                    &[run],
                    None,
                );
                let bg = Bounds::new(anchor.origin, size(shaped.width, layout.line_height));
                window.paint_quad(fill(bg, colors::BACKGROUND));
                let _ = shaped.paint(anchor.origin, layout.line_height, window, cx);
            }
        }
    }
}
