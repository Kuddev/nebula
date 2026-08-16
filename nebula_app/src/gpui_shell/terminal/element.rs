//! 终端网格的自定义 GPUI Element。
//!
//! 热路径约定：paint 内只对 `Term` 加一次锁，通过渲染合同
//! （`nebula_terminal::render::RenderSnapshot`）把可见网格快照成纯数据后立刻
//! 放锁；颜色解析（调色板 + OSC 覆盖表）与绘制都在锁外进行。
//!
//! 定位合同（与旧壳 `Renderer::draw_string` 相同）：每个 cell 的字形单独
//! 塑形、从 `列号 × cell_width` 的整数 cell 原点起笔。绝不把整段文本交给
//! `force_width` 批量塑形——GPUI 只在字形偏离目标格超过 1px 时才吸附
//! （line_layout.rs），1px 内保留字体自然 advance；删除字符或分段变化引发
//! 整行重塑形时，每个字形都可能在"自然位置/吸附位置"间翻转，肉眼即为
//! 字符左右跳动。逐 cell 塑形时首字形天然落在 x=0，排版引擎没有移动
//! 字形的权力；单字符行在 GPUI 行缓存中按 (字符, 字体) 去重，命中率极高。

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, HitboxBehavior, Hsla, InspectorElementId,
    LayoutId, Pixels, Rgba, SharedString, Style, TextRun, UnderlineStyle, Window, fill, outline,
    point, px, relative, size,
};
use gpui_component::{ActiveTheme as _, PixelsExt as _};
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::render::{RenderSnapshot, SnapshotConfig, boxdraw};
#[cfg(windows)]
use nebula_terminal::term::TermMode;
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
    /// 在一次锁内通过渲染合同截取快照；锁外解析颜色与绘制。第三项是回滚
    /// 历史行数——滚动条拇指的分母，顺着这一次锁一起取回，免得绘制路径
    /// 为了它再抢一次 `Term` 锁。
    fn snapshot(
        &self,
        rows: usize,
        cols: usize,
        cx: &App,
    ) -> Option<(RenderSnapshot, Option<String>, usize)> {
        let view = self.view.read(cx);
        let session = view.session.as_ref()?;
        let term = session.term.lock();
        #[cfg(windows)]
        let prompt_line = if term.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI) {
            None
        } else {
            let cursor = term.grid().cursor.point;
            crate::display::Display::nebula_input_from_raw_grid(&term, cursor)
        };
        #[cfg(not(windows))]
        let prompt_line = None;
        // 分段只反映内容与宽度类，绝不掺入光标状态：per-cell 绘制下反色只是
        // 换色，若让闪烁相位改变分段，整行会随闪烁重塑形而跳字。
        let snapshot = RenderSnapshot::capture(
            &term,
            &SnapshotConfig { rows: rows as u16, cols: cols as u16 },
        );
        let history = term.history_size();
        Some((snapshot, prompt_line, history))
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
        let scale = window.scale_factor();
        let view = self.view.read(cx);
        let cell_width = view.cell_width_for_advance(sample.width.as_f32(), scale);
        let line_height =
            view.line_height_for_metrics(sample.ascent.as_f32() + sample.descent.as_f32(), scale);

        // 网格裁定（floor、最小网格、resize 合流）全部由渲染合同的
        // ViewportTracker 在 view 内完成；元素只上报内容矩形与度量。
        self.view.update(cx, |view, cx| {
            view.set_layout(bounds.origin, cell_width, line_height, bounds.size, scale, cx);
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
        // 元素不画底色：卡容器统一负责"卡底色（带窗口透明度）→ 壁纸"
        // 两层（旧壳 draw_window_backdrop 同模型），元素只画单元格背景、
        // 文字与光标——默认背景的格子保持透明，让模糊/壁纸透进来。

        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            gpui::ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let focused = focus_handle.is_focused(window);
        // 旧壳只让光标本身参与闪烁；ghost、弹窗补齐和 IME 仍复用同一个坐标锚点。
        let cursor_visible = self.view.read(cx).cursor_visible();
        let Some((snap, prompt_line, history)) = self.snapshot(layout.rows, layout.cols, cx) else {
            return;
        };
        let suggest_anchor =
            snap.cursor.as_ref().map(|cursor| (cursor.row as usize, cursor.col as usize));
        self.view.update(cx, |view, _| {
            view.refresh_suggestion_from_snapshot(prompt_line, suggest_anchor);
        });
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
            if focused && cursor_visible && matches!(cursor.shape, CursorShape::Block) {
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
                && cursor_visible
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

        // 旧壳定位合同：每个 cell 单独塑形，从自己的整数 cell 原点起笔。
        // 不传 force_width——单 cell 首字形天然落在 x=0，位置完全由列号决定，
        // 组合字符（零宽）跟随基字自然排布，不会被按字形序号吸附到邻格。
        // 光标反色在这里只是换色，不影响任何字形位置。
        for seg in &snap.segments {
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
                let run = TextRun {
                    len: cell.text.len(),
                    font: pick_font(cell.bold, cell.italic),
                    color: fg,
                    background_color: None,
                    underline,
                    strikethrough,
                };
                let origin = point(
                    bounds.origin.x + layout.cell_width * cell.col as f32,
                    bounds.origin.y + layout.line_height * seg.row as f32,
                );
                paint_cell_text(
                    window,
                    cx,
                    SharedString::from(cell.text.clone()),
                    run,
                    font_size,
                    origin,
                    layout.line_height,
                );
            }
        }

        if let Some(cursor) = &snap.cursor {
            // 内联 ghost 余量与弹窗补全：结果由 view 在本帧 render 时算好
            // （引擎与旧壳共享），元素只负责画。IME 组合中让位给 preedit，
            // 与旧壳同一抑制条件；窗口失焦不灭（对齐旧壳 draw_pane）。
            let (ghost_text, popup_items, popup_selected) = {
                let view = self.view.read(cx);
                let ime_active = view.marked_text.as_deref().is_some_and(|m| !m.is_empty());
                let same_frame =
                    view.suggest_anchor == Some((cursor.row as usize, cursor.col as usize));
                if ime_active || !same_frame {
                    (None, Vec::new(), 0)
                } else {
                    (
                        (!view.suggest.suggestion.is_empty())
                            .then(|| view.suggest.suggestion.clone()),
                        view.suggest.completion_items.clone(),
                        view.suggest.completion_selected,
                    )
                }
            };
            if ghost_text.is_some() || !popup_items.is_empty() {
                let colors = crate::gpui_shell::theme::completion_colors(cx, theme.background);
                if let Some(ghost) = &ghost_text {
                    let avail = layout.cols.saturating_sub(cursor.col as usize);
                    if avail > 0 {
                        grid_text(
                            window,
                            cx,
                            ghost,
                            &font,
                            font_size,
                            colors.ghost,
                            point(
                                bounds.origin.x + layout.cell_width * cursor.col as f32,
                                bounds.origin.y + layout.line_height * cursor.row as f32,
                            ),
                            layout.cell_width,
                            layout.line_height,
                            avail,
                        );
                    }
                }
                if !popup_items.is_empty() {
                    paint_completion_popup(
                        window,
                        cx,
                        &popup_items,
                        popup_selected,
                        cursor.row as usize,
                        cursor.col as usize,
                        layout,
                        &bounds,
                        &colors,
                        &font,
                        font_size,
                    );
                }
            }

            // ghost 从光标单元格起笔；GPUI 的字形抗锯齿可能盖住同一位置的
            // beam/underline 边缘，所以非块状光标的前景必须在 ghost 后补画。
            // 聚焦块状光标仍在文字层下方填充，保持旧壳的反色合同。
            let width = if cursor.wide { 2 } else { 1 };
            let rect = cell_rect(cursor.row as usize, cursor.col as usize, width);
            if !focused || cursor_visible {
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

        // Overlay 滚动条：贴着底部时拇指为 None 不画，滚进历史才浮在网格右缘。
        // 几何来自 view 的单一真值源——命中测试拿的是同一个矩形（旧壳
        // `draw_scrollbar` / `scrollbar_grab` 共用 `scrollbar_geometry` 同构）。
        let (dragging, thumb) = {
            let view = self.view.read(cx);
            (view.scrollbar_dragging(), view.scrollbar_thumb(snap.display_offset, history))
        };
        if let Some(thumb) = thumb {
            let color = if dragging {
                cx.theme().scrollbar_thumb_hover
            } else {
                cx.theme().scrollbar_thumb
            };
            let radius = thumb.size.width / 2.0;
            window.paint_quad(fill(thumb, color).corner_radii(radius));
        }
    }
}

/// 唯一的 cell 文本落笔原语：单 cell 文本塑形后从调用方给定的整数 cell
/// 原点起笔，不传 `force_width`（首字形天然在 x=0，位置只由列号决定）。
/// 网格、ghost、弹窗全部经由此处——定位合同只此一份，禁止绕开它直接
/// `shape_line` 网格对齐文本。
fn paint_cell_text(
    window: &mut Window,
    cx: &mut App,
    text: SharedString,
    run: TextRun,
    font_size: Pixels,
    origin: gpui::Point<Pixels>,
    line_height: Pixels,
) {
    let shaped = window.text_system().shape_line(text, font_size, &[run], None);
    let _ = shaped.paint(origin, line_height, window, cx);
}

/// 网格对齐的浮层文本（ghost/弹窗共用），与网格文字同一定位合同：每个可见
/// 字符单独塑形、从 `起始格 + 已用格数` 的整数 cell 原点起笔，超出
/// `max_cells` 截断。绝不批量 `force_width`——其 1px 容差会沿字符累积并在
/// 重塑形时翻转吸附，删除 `bypassPermission` 一类提示时就会逐字向左挤。
/// 单字符行命中 GPUI 行缓存，逐字塑形没有额外热路径成本。
#[allow(clippy::too_many_arguments)]
fn grid_text(
    window: &mut Window,
    cx: &mut App,
    text: &str,
    font: &gpui::Font,
    font_size: Pixels,
    color: Hsla,
    origin: gpui::Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    max_cells: usize,
) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    let mut used = 0usize;
    for character in text.chars() {
        // 与旧壳 draw_string 一致：零宽字符不单占格，宽字符占两格。
        let width = character.width().unwrap_or(0);
        if width == 0 {
            continue;
        }
        if used + width > max_cells {
            break;
        }
        let text = character.to_string();
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        paint_cell_text(
            window,
            cx,
            SharedString::from(text),
            run,
            font_size,
            point(origin.x + cell_width * used as f32, origin.y),
            line_height,
        );
        used += width;
    }
    used
}

/// 绘制与命中测试共用的弹窗几何。旧壳的候选列表是在 cell 网格中绘制的，
/// GPUI 也必须用同一套偏移，否则鼠标看到的行和实际接受的行会错位。
#[derive(Clone, Copy)]
pub(super) struct CompletionPopupLayout {
    pub start_line: usize,
    pub start_col: usize,
    pub offset: usize,
    pub rows: usize,
    pub width: usize,
    pub label_width: usize,
    pub tag_width: usize,
    pub selected: usize,
}

pub(super) fn completion_popup_layout(
    items: &[crate::display::NebulaCompletionItem],
    selected: usize,
    cursor_row: usize,
    cursor_col: usize,
    screen_lines: usize,
    columns: usize,
) -> Option<CompletionPopupLayout> {
    use unicode_width::UnicodeWidthChar as _;

    if columns < 12 || screen_lines < 2 || items.is_empty() {
        return None;
    }

    // 行位：优先光标下方，不够放则上方；最多显示八项。
    let below = screen_lines.saturating_sub(cursor_row + 1);
    let above = cursor_row;
    let want = items.len().min(8);
    let (rows, start_line) = if below >= want || below >= above {
        (want.min(below), cursor_row + 1)
    } else {
        (want.min(above), cursor_row - want.min(above))
    };
    if rows == 0 {
        return None;
    }

    let selected = selected.min(items.len() - 1);
    let offset = if selected >= rows { selected + 1 - rows } else { 0 };
    let visible = &items[offset..(offset + rows).min(items.len())];
    let tag = |kind: crate::display::NebulaCompletionKind| -> &'static str {
        match kind {
            crate::display::NebulaCompletionKind::History => "历史",
            crate::display::NebulaCompletionKind::Command => "命令",
            crate::display::NebulaCompletionKind::Dir => "目录",
            crate::display::NebulaCompletionKind::File => "文件",
        }
    };
    let cells_of = |text: &str| -> usize { text.chars().map(|c| c.width().unwrap_or(0)).sum() };
    let tag_width = visible.iter().map(|item| cells_of(tag(item.kind))).max().unwrap_or(0);
    let label_width_max = visible.iter().map(|item| cells_of(&item.label)).max().unwrap_or(0);

    let mut start_col = cursor_col.min(columns - 1);
    let mut avail = columns - start_col;
    let full_width = label_width_max + tag_width + 4;
    if full_width > avail {
        let slide = (full_width - avail).min(start_col);
        start_col -= slide;
        avail += slide;
    }
    let width = full_width.min(avail);
    let label_width = width.saturating_sub(tag_width + 4);
    // 这里的 4 是窄窗口裁切后“至少还能读”的标签宽度，不是候选本身的
    // 最小长度。`cat` 这类没有更长邻居的短命令，其自然宽度只有 3；旧判
    // 定会让引擎已生成的精确候选在布局阶段直接消失。只拒绝完全没有标签
    // 空间的布局，短标签照常按自然宽度显示。
    (label_width > 0).then_some(CompletionPopupLayout {
        start_line,
        start_col,
        offset,
        rows,
        width,
        label_width,
        tag_width,
        selected,
    })
}

/// 弹窗补全列表：下方优先、放不下上移、宽度不足先左滑再截 label；
/// 面板底和行内容都对齐主题色，选中态使用柔和 accent 水洗。
#[allow(clippy::too_many_arguments)]
fn paint_completion_popup(
    window: &mut Window,
    cx: &mut App,
    items: &[crate::display::NebulaCompletionItem],
    selected: usize,
    cursor_row: usize,
    cursor_col: usize,
    layout: &TermLayout,
    bounds: &Bounds<Pixels>,
    colors: &crate::gpui_shell::theme::CompletionColors,
    font: &gpui::Font,
    font_size: Pixels,
) {
    let columns = layout.cols;
    let screen_lines = layout.rows;
    let Some(popup) = completion_popup_layout(
        items,
        selected,
        cursor_row,
        cursor_col,
        screen_lines,
        columns,
    ) else {
        return;
    };
    let tag = |kind: crate::display::NebulaCompletionKind| -> &'static str {
        match kind {
            crate::display::NebulaCompletionKind::History => "历史",
            crate::display::NebulaCompletionKind::Command => "命令",
            crate::display::NebulaCompletionKind::Dir => "目录",
            crate::display::NebulaCompletionKind::File => "文件",
        }
    };
    let visible = &items[popup.offset..(popup.offset + popup.rows).min(items.len())];

    // 四周留出少量呼吸边，圆角和边框不会压到候选文字。
    let panel_pad = px(4.0);
    let panel_origin = point(
        bounds.origin.x + layout.cell_width * popup.start_col as f32 - panel_pad,
        bounds.origin.y + layout.line_height * popup.start_line as f32 - panel_pad,
    );
    let panel_bounds = Bounds::new(
        panel_origin,
        size(
            layout.cell_width * popup.width as f32 + panel_pad * 2.0,
            layout.line_height * popup.rows as f32 + panel_pad * 2.0,
        ),
    );
    window.paint_quad(fill(panel_bounds, colors.panel_bg).corner_radii(px(8.0)));
    window.paint_quad(
        outline(panel_bounds, colors.panel_border, gpui::BorderStyle::Solid).corner_radii(px(8.0)),
    );

    for (row, item) in visible.iter().enumerate() {
        let line = popup.start_line + row;
        if line >= screen_lines {
            break;
        }
        let is_selected = popup.offset + row == popup.selected;
        let (bg, label_fg, tag_fg) = if is_selected {
            (colors.selected_bg, colors.selected_fg, colors.selected_fg)
        } else {
            (colors.row_bg, colors.row_fg, colors.tag_fg)
        };
        let origin = point(
            bounds.origin.x + layout.cell_width * popup.start_col as f32,
            bounds.origin.y + layout.line_height * line as f32,
        );
        window.paint_quad(fill(
            Bounds::new(origin, size(layout.cell_width * popup.width as f32, layout.line_height)),
            bg,
        ));
        grid_text(
            window,
            cx,
            &item.label,
            font,
            font_size,
            label_fg,
            point(origin.x + layout.cell_width, origin.y),
            layout.cell_width,
            layout.line_height,
            popup.label_width,
        );
        let tag_start = popup.width.saturating_sub(1 + popup.tag_width);
        grid_text(
            window,
            cx,
            tag(item.kind),
            font,
            font_size,
            tag_fg,
            point(origin.x + layout.cell_width * tag_start as f32, origin.y),
            layout.cell_width,
            layout.line_height,
            popup.tag_width,
        );
    }

    if items.len() > popup.rows {
        // 滚动条覆在面板右侧 4px 呼吸边内，不占终端字符列，也不遮住 tag。
        // 拇指跟随可见窗口的 offset；Tab、hover、滚轮三条路径共享同一个
        // 可视位置，不会出现键盘已翻页而滚动条仍停在顶部。
        let track_h = layout.line_height * popup.rows as f32 - px(4.0);
        let track_top =
            bounds.origin.y + layout.line_height * popup.start_line as f32 + px(2.0);
        let content_right =
            bounds.origin.x + layout.cell_width * (popup.start_col + popup.width) as f32;
        let track_bounds = Bounds::new(
            point(content_right + px(1.0), track_top),
            size(px(2.0), track_h),
        );
        window.paint_quad(fill(track_bounds, colors.scroll_track).corner_radii(px(1.0)));

        let visible_ratio = popup.rows as f32 / items.len() as f32;
        let thumb_h = (track_h * visible_ratio).max(px(14.0)).min(track_h);
        let max_offset = items.len() - popup.rows;
        let progress = popup.offset.min(max_offset) as f32 / max_offset as f32;
        let thumb_top = track_top + (track_h - thumb_h) * progress;
        window.paint_quad(
            fill(
                Bounds::new(point(content_right + px(1.0), thumb_top), size(px(2.0), thumb_h)),
                colors.scroll_thumb,
            )
            .corner_radii(px(1.0)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::completion_popup_layout;
    use crate::display::{NebulaCompletionItem, NebulaCompletionKind};

    #[test]
    fn popup_keeps_a_single_short_exact_command_visible() {
        let items = [NebulaCompletionItem {
            label: "cat".to_owned(),
            insert: " ".to_owned(),
            kind: NebulaCompletionKind::Command,
        }];

        let popup = completion_popup_layout(&items, 0, 2, 7, 24, 80)
            .expect("短命令的精确候选也必须形成可见面板");
        assert_eq!(popup.rows, 1);
        assert_eq!(popup.label_width, 3);
    }
}
