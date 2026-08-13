//! 终端网格的自定义 GPUI Element。
//!
//! 热路径约定：paint 内只对 `Term` 加一次锁，通过渲染合同
//! （`nebula_terminal::render::RenderSnapshot`）把可见网格快照成纯数据后立刻
//! 放锁；颜色解析（调色板 + OSC 覆盖表）与绘制都在锁外进行。文字按
//! `force_width` 锁定单元格步进，保证 CJK 宽字符与网格对齐 —— 排版引擎在
//! 这里只承担栅格化与批处理，没有移动字形的权力。

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, HitboxBehavior, Hsla, InspectorElementId,
    LayoutId, Pixels, Rgba, SharedString, Style, TextRun, UnderlineStyle, Window, fill, outline,
    point, px, relative, size,
};
use gpui_component::PixelsExt as _;
use nebula_terminal::render::{RenderSnapshot, SnapshotConfig, boxdraw};
use nebula_terminal::vte::ansi::CursorShape;

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
    cols: usize,
}

impl TerminalElement {
    /// 在一次锁内通过渲染合同截取快照；锁外解析颜色与绘制。
    fn snapshot(&self, rows: usize, cols: usize, focused: bool, cx: &App) -> Option<RenderSnapshot> {
        let view = self.view.read(cx);
        let session = view.session.as_ref()?;
        let term = session.term.lock();
        Some(RenderSnapshot::capture(
            &term,
            &SnapshotConfig {
                rows: rows as u16,
                cols: cols as u16,
                // 聚焦时把光标格隔离成单格段，方便下面反色绘制。
                isolate_cursor_cell: focused,
            },
        ))
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

        // 网格裁定（floor、最小网格、resize 合流）全部由渲染合同的
        // ViewportTracker 在 view 内完成；元素只上报内容矩形与度量。
        let scale = window.scale_factor();
        self.view.update(cx, |view, _| {
            view.set_layout(bounds.origin, cell_width, line_height, bounds.size, scale);
        });

        window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let view = self.view.read(cx);
        TermLayout { cell_width, line_height, rows: view.grid_rows(), cols: view.grid_cols() }
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
        let theme = self.view.read(cx).palette.clone();
        window.paint_quad(fill(bounds, theme.background));

        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            gpui::ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let focused = focus_handle.is_focused(window);
        let Some(snap) = self.snapshot(layout.rows, layout.cols, focused, cx) else {
            return;
        };
        let overrides = snap.color_overrides;

        let cell_rect = |row: usize, start: usize, count: usize| -> Bounds<Pixels> {
            Bounds::new(
                point(
                    bounds.origin.x + layout.cell_width * start as f32,
                    bounds.origin.y + layout.line_height * row as f32,
                ),
                size(layout.cell_width * count as f32, layout.line_height),
            )
        };

        for run in &snap.bg_runs {
            let color = theme.resolve(run.color, &overrides, false);
            window.paint_quad(fill(
                cell_rect(run.row as usize, run.start as usize, (run.end - run.start) as usize),
                color,
            ));
        }
        for run in &snap.selection_runs {
            window.paint_quad(fill(
                cell_rect(run.row as usize, run.start as usize, (run.end - run.start) as usize),
                theme.selection,
            ));
        }

        // 聚焦时的块状光标垫在文字下面，让字形以反色盖在上面。
        if let Some(cursor) = &snap.cursor {
            if focused && matches!(cursor.shape, CursorShape::Block) {
                let width = if cursor.wide { 2 } else { 1 };
                window.paint_quad(fill(
                    cell_rect(cursor.row as usize, cursor.col as usize, width),
                    theme.cursor,
                ));
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
        // 聚焦块光标下的字形反色（合同保证该格是独立段）。
        let cursor_inverts = |row: u16, col: u16| {
            focused
                && snap.cursor.as_ref().is_some_and(|c| {
                    matches!(c.shape, CursorShape::Block) && c.row == row && c.col == col
                })
        };

        // 内建几何字形（框线/块元素/Powerline）：不走字体，直接以图元填充。
        // 几何由渲染合同裁定（含设备像素吸附），保证盖满单元格、相邻无缝
        // —— CJK 字体下的框线错位就此根治。
        let scale = window.scale_factor();
        for glyph in &snap.box_glyphs {
            let fg = if cursor_inverts(glyph.row, glyph.col) {
                theme.background
            } else {
                theme.resolve(glyph.fg, &overrides, glyph.bold)
            };
            let span = if glyph.wide { 2.0 } else { 1.0 };
            let Some(prims) = boxdraw::primitives(
                glyph.ch,
                layout.cell_width.as_f32() * span,
                layout.line_height.as_f32(),
                scale,
            ) else {
                continue;
            };
            let origin = point(
                bounds.origin.x + layout.cell_width * glyph.col as f32,
                bounds.origin.y + layout.line_height * glyph.row as f32,
            );
            let at = |p: &[f32; 2]| point(origin.x + px(p[0]), origin.y + px(p[1]));
            for prim in prims {
                match prim {
                    boxdraw::Primitive::Rect { rect, alpha } => {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + px(rect.x), origin.y + px(rect.y)),
                                size(px(rect.w), px(rect.h)),
                            ),
                            Rgba { a: fg.a * alpha, ..fg },
                        ));
                    },
                    boxdraw::Primitive::Poly { points } => {
                        // 合同保证顶点为凸序，首点三角扇填充正确。
                        let Some((first, rest)) = points.split_first() else { continue };
                        let mut path = gpui::Path::new(at(first));
                        for p in rest {
                            path.line_to(at(p));
                        }
                        window.paint_path(path, fg);
                    },
                }
            }
        }

        for seg in &snap.segments {
            let step = seg.step() as usize;
            let mut text = String::new();
            let mut runs: Vec<TextRun> = Vec::new();
            for cell in &seg.cells {
                let fg: Hsla = if cursor_inverts(seg.row, cell.col) {
                    theme.background.into()
                } else {
                    theme.resolve(cell.fg, &overrides, cell.bold).into()
                };
                let underline = cell.underline.then(|| UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                });
                let strikethrough = cell
                    .strikethrough
                    .then(|| gpui::StrikethroughStyle { thickness: px(1.0), color: Some(fg) });
                let bytes = cell.text.len();
                let mergeable = runs.last().is_some_and(|run: &TextRun| {
                    run.color == fg
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
                        color: fg,
                        background_color: None,
                        underline,
                        strikethrough,
                    });
                }
                text.push_str(&cell.text);
            }
            if runs.is_empty() {
                continue;
            }
            let shaped = window.text_system().shape_line(
                SharedString::from(text),
                font_size,
                &runs,
                Some(layout.cell_width * step as f32),
            );
            let origin = point(
                bounds.origin.x + layout.cell_width * seg.start_col as f32,
                bounds.origin.y + layout.line_height * seg.row as f32,
            );
            let _ = shaped.paint(origin, layout.line_height, window, cx);
        }

        if let Some(cursor) = &snap.cursor {
            let width = if cursor.wide { 2 } else { 1 };
            let rect = cell_rect(cursor.row as usize, cursor.col as usize, width);
            match (focused, cursor.shape) {
                (true, CursorShape::Block) => {}, // 已在文字层下方填充
                (false, CursorShape::Block | CursorShape::HollowBlock)
                | (true, CursorShape::HollowBlock) => {
                    window.paint_quad(outline(rect, theme.cursor, gpui::BorderStyle::Solid));
                },
                (_, CursorShape::Beam) => {
                    window.paint_quad(fill(
                        Bounds::new(rect.origin, size(px(2.0), layout.line_height)),
                        theme.cursor,
                    ));
                },
                (_, CursorShape::Underline) => {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(rect.origin.x, rect.origin.y + layout.line_height - px(2.0)),
                            size(rect.size.width, px(2.0)),
                        ),
                        theme.cursor,
                    ));
                },
                (_, CursorShape::Hidden) => {},
            }

            // IME 组合文本锚点与预编辑串：跟随光标单元格。
            let anchor = cell_rect(cursor.row as usize, cursor.col as usize, 1);
            let marked = self.view.read(cx).marked_text.clone();
            self.view.update(cx, |view, _| view.ime_bounds = anchor);
            if let Some(marked) = marked.filter(|m| !m.is_empty()) {
                let run = TextRun {
                    len: marked.len(),
                    font: font.clone(),
                    color: theme.foreground.into(),
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(theme.foreground.into()),
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
                window.paint_quad(fill(bg, theme.background));
                let _ = shaped.paint(anchor.origin, layout.line_height, window, cx);
            }
        }
    }
}
