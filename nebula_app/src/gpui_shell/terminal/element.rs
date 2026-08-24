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

use std::collections::HashSet;

use gpui::{
    App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, LayoutId, PathBuilder, Pixels, Rgba, SharedString, Style, TextRun,
    UnderlineStyle, Window, fill, outline, point, px, relative, size,
};
use gpui_component::ActiveTheme as _;
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::render::{RenderSnapshot, SnapshotConfig, boxdraw};
#[cfg(windows)]
use nebula_terminal::term::TermMode;
use nebula_terminal::vte::ansi::{Color, CursorShape};

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
    hitbox: Hitbox,
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
    ) -> Option<(RenderSnapshot, Option<String>, usize, HashSet<(u16, u16)>)> {
        let view = self.view.read(cx);
        let session = view.session.as_ref()?;
        let hint_config = view.hint_config.clone();
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
        let dashed = super::osc_links::dashed_cells(&term, &hint_config, rows, cols);
        Some((snapshot, prompt_line, history, dashed))
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

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let view = self.view.read(cx);
        TermLayout {
            cell_width,
            line_height,
            rows: view.grid_rows(),
            cols: view.grid_cols(),
            hitbox,
        }
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
        let Some((snap, prompt_line, history, dashed)) =
            self.snapshot(layout.rows, layout.cols, cx)
        else {
            return;
        };
        let suggest_anchor =
            snap.cursor.as_ref().map(|cursor| (cursor.row as usize, cursor.col as usize));
        self.view.update(cx, |view, cx| {
            view.refresh_suggestion_from_snapshot(prompt_line, suggest_anchor);
            // 补齐只会**登记**要问哪个来宾/远端目录（按键路径上不做 IO），
            // 真正的往返在这里派出去。
            view.drive_pending_remote_dir(cx);
        });
        let overrides = snap.color_overrides;
        let (theme_anchor, theme_is_light) = themed_anchor(&theme, cx);
        let host_cursor_follows_theme = is_default_host_cursor(&theme);
        let app_cursor = snap.cursor.as_ref().filter(|cursor| {
            cursor.shape == CursorShape::Hidden
                && crate::display::content::is_application_cursor_glyph(
                    cursor.cell_ch,
                    cursor.cell_flags,
                    crate::display::content::cell_background_is_fixed(cursor.cell_bg, &overrides),
                )
        });
        let themed_block = host_cursor_follows_theme
            && snap
                .cursor
                .as_ref()
                .is_some_and(|cursor| matches!(cursor.shape, CursorShape::Block));

        // 公式覆盖层：与本帧快照同一份网格状态（term 锁内扫描 + 计划），
        // 探测/fit/几何合同全部由共享的 display::terminal_math 裁定。
        let scale_factor = window.scale_factor();
        let math_pixels_per_point = crate::math::pixels_per_point(scale_factor);
        let math_frame = {
            let cell_w = layout.cell_width.as_f32();
            let line_h = layout.line_height.as_f32();
            let font_px = f32::from(self.view.read(cx).font_size);
            let fg = self.view.read(cx).palette.foreground;
            let foreground = crate::display::color::Rgb::new(
                (fg.r * 255.0).round() as u8,
                (fg.g * 255.0).round() as u8,
                (fg.b * 255.0).round() as u8,
            );
            self.view.update(cx, |view, _| {
                let Some(term) = view.session.as_ref().map(|session| session.term.clone()) else {
                    return super::math_overlay::MathFrame::default();
                };
                // CommandStart / NEBULA| / hook 都写入 running_program；回合
                // 结束后 hook 身份仍可能留在 ai_session.source（旧壳同事实源）。
                view.math.observe_program(
                    view.running_program
                        .as_deref()
                        .or(view.ai_session.as_ref().map(|session| session.source.as_str())),
                );
                let size_info =
                    super::math_overlay::grid_size_info(layout.cols, layout.rows, cell_w, line_h);
                let term = term.lock();
                view.math.plan_frame(&term, &size_info, foreground, font_px, math_pixels_per_point)
            })
        };
        // 栅格器不可用时不能跳源格：旧壳 draw_math 失败会补画 fallback，
        // GPUI 格子已经画过，只能整帧不覆盖，让 `\[` 源码留在屏幕上。
        let math_frame = if cx
            .try_global::<crate::gpui_shell::math_view::MathAssets>()
            .is_some_and(|assets| assets.can_rasterize())
        {
            math_frame
        } else {
            super::math_overlay::MathFrame::default()
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

        for run in &snap.bg_runs {
            let mut paint = |start: u16, end: u16, color: Color| {
                if start >= end {
                    return;
                }
                let color = theme.resolve(color, &overrides, false);
                window.paint_quad(fill(
                    cell_rect(run.row as usize, start as usize, (end - start) as usize),
                    color,
                ));
            };
            if let Some(cursor) = app_cursor {
                if run.row == cursor.row {
                    let skip_end = cursor.col.saturating_add(if cursor.wide { 2 } else { 1 });
                    if run.start < skip_end && cursor.col < run.end {
                        paint(run.start, cursor.col, run.color);
                        paint(skip_end, run.end, run.color);
                        continue;
                    }
                }
            }
            paint(run.start, run.end, run.color);
        }
        let selection_fill = if is_default_selection(&theme) {
            let alpha = if theme_is_light {
                crate::display::ui::tokens::terminal_feedback::SELECTION_ALPHA_LIGHT
            } else {
                crate::display::ui::tokens::terminal_feedback::SELECTION_ALPHA_DARK
            };
            rgba_rgb(theme_anchor, alpha)
        } else {
            theme.selection
        };
        for run in &snap.selection_runs {
            window.paint_quad(fill(
                cell_rect(run.row as usize, run.start as usize, (run.end - run.start) as usize),
                selection_fill,
            ));
        }

        // 聚焦时的块状光标：旧壳默认主题走半透明 theme_anchor（浅色 0.20），
        // 叠在格子/壁纸上，不反色文字；用户显式配置光标才用实心色。
        if let Some(cursor) = &snap.cursor {
            if focused && cursor_visible && matches!(cursor.shape, CursorShape::Block) {
                let width = if cursor.wide { 2 } else { 1 };
                let fill_color = if host_cursor_follows_theme {
                    let alpha = if theme_is_light {
                        crate::display::ui::tokens::terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT
                    } else {
                        crate::display::ui::tokens::terminal_feedback::BLOCK_CURSOR_ALPHA_DARK
                    };
                    rgba_rgb(theme_anchor, alpha)
                } else {
                    theme.cursor
                };
                window.paint_quad(fill(
                    cell_rect(cursor.row as usize, cursor.col as usize, width),
                    fill_color,
                ));
            }
        }
        // CC/Codex：DECSCUSR Hidden 后自己画反色空格。旧壳把那格从反色黑
        // 改成叠在主题底上的 theme_anchor；块元素则只换前景，保留字形。
        let app_cursor_color = app_cursor.map(|cursor| {
            let alpha = if theme_is_light {
                crate::display::ui::tokens::terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT
            } else {
                crate::display::ui::tokens::terminal_feedback::BLOCK_CURSOR_ALPHA_DARK
            };
            let base = if cursor.cell_ch == ' ' {
                rgb_from_rgba(theme.background)
            } else {
                rgb_from_rgba(theme.resolve(cursor.cell_bg, &overrides, false))
            };
            let (color, _) =
                crate::display::content::composite_overlay(theme_anchor, alpha, base, 1.0);
            rgba_rgb(color, 1.0)
        });
        if let Some(cursor) = app_cursor {
            if cursor.cell_ch == ' ' {
                if let Some(color) = app_cursor_color {
                    let width = if cursor.wide { 2 } else { 1 };
                    window.paint_quad(fill(
                        cell_rect(cursor.row as usize, cursor.col as usize, width),
                        color,
                    ));
                }
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
        // 全宽（CJK 等）bold run 的字形策略（设置 `cjk_bold_regular`，默认
        // 开）：bold 只提亮颜色（resolve 已做），字形保持 Regular——旧壳
        // `wide_bold_use_regular` 的同义移植。CJK 假粗体在小字号糊成一团，
        // 这是旧壳早已裁定过的取舍。
        let cjk_bold_regular = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.cjk_bold_regular)
            .unwrap_or(true);
        let pick_font = |bold: bool, italic: bool, wide: bool| {
            let bold = bold && !(wide && cjk_bold_regular);
            match (bold, italic) {
                (false, false) => font.clone(),
                (true, false) => bold_font.clone(),
                (false, true) => italic_font.clone(),
                (true, true) => bold_italic_font.clone(),
            }
        };
        // 聚焦块光标下的字形反色（合同保证该格是独立段）。
        let cursor_inverts = |row: u16, col: u16| {
            focused
                && cursor_visible
                && !themed_block
                && snap.cursor.as_ref().is_some_and(|c| {
                    matches!(c.shape, CursorShape::Block) && c.row == row && c.col == col
                })
        };

        // 内建几何字形（框线/块元素/Powerline）：不走字体，直接以图元填充。
        // 几何由渲染合同裁定（含设备像素吸附），保证盖满单元格、相邻无缝
        // —— CJK 字体下的框线错位就此根治。
        let scale = window.scale_factor();
        for glyph in &snap.box_glyphs {
            let fg = if app_cursor
                .is_some_and(|cursor| cursor.row == glyph.row && cursor.col == glyph.col)
            {
                app_cursor_color.unwrap_or_else(|| theme.resolve(glyph.fg, &overrides, glyph.bold))
            } else if cursor_inverts(glyph.row, glyph.col) {
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
                // 被公式覆盖的源格不画原文（公式直接落在卡底上，与旧壳
                // CoverageMask 合同一致；计划失败的公式不进掩码、原文保留）。
                if math_frame.covers(seg.row as usize, cell.col as usize) {
                    continue;
                }
                let fg: Hsla = if cursor_inverts(seg.row, cell.col) {
                    theme.background.into()
                } else {
                    theme.resolve(cell.fg, &overrides, cell.bold).into()
                };
                let dashed_link = dashed.contains(&(seg.row, cell.col));
                let underline = (cell.underline && !dashed_link).then(|| UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                });
                let strikethrough = cell
                    .strikethrough
                    .then(|| gpui::StrikethroughStyle { thickness: px(1.0), color: Some(fg) });
                let run = TextRun {
                    len: cell.text.len(),
                    font: pick_font(cell.bold, cell.italic, seg.wide),
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
                if dashed_link {
                    paint_dashed_underline(
                        window,
                        origin,
                        layout.cell_width,
                        layout.line_height,
                        fg,
                    );
                }
            }
        }

        // 公式位图画在格子文本之后、装饰（ghost/光标/滚动条）之前，
        // 与旧壳 draw_rects → draw_overlays 的次序一致。
        if !math_frame.is_empty() {
            super::math_overlay::paint_frame(
                &math_frame,
                bounds.origin,
                (layout.cell_width.as_f32(), layout.line_height.as_f32()),
                math_pixels_per_point,
                window,
                cx,
            );
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
                    (None, Vec::new(), None)
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
            let stroke = if host_cursor_follows_theme {
                let alpha = if theme_is_light {
                    crate::display::ui::tokens::terminal_feedback::STROKE_CURSOR_ALPHA_LIGHT
                } else {
                    crate::display::ui::tokens::terminal_feedback::STROKE_CURSOR_ALPHA_DARK
                };
                rgba_rgb(theme_anchor, alpha)
            } else {
                theme.cursor
            };
            let width = if cursor.wide { 2 } else { 1 };
            let rect = cell_rect(cursor.row as usize, cursor.col as usize, width);
            if !focused || cursor_visible {
                match (focused, cursor.shape) {
                    (true, CursorShape::Block) => {}, // 已在文字层下方填充
                    (false, CursorShape::Block | CursorShape::HollowBlock)
                    | (true, CursorShape::HollowBlock) => {
                        window.paint_quad(outline(rect, stroke, gpui::BorderStyle::Solid));
                    },
                    (_, CursorShape::Beam) => {
                        window.paint_quad(fill(
                            Bounds::new(rect.origin, size(px(2.0), layout.line_height)),
                            stroke,
                        ));
                    },
                    (_, CursorShape::Underline) => {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(rect.origin.x, rect.origin.y + layout.line_height - px(2.0)),
                                size(rect.size.width, px(2.0)),
                            ),
                            stroke,
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
                let _ = shaped.paint(
                    anchor.origin,
                    layout.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
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

        let (bell_flash, link_hover) = {
            let view = self.view.read(cx);
            (view.bell_flash, view.link_hover.clone())
        };
        if bell_flash {
            let mut flash = theme.foreground;
            flash.a = 0.12;
            window.paint_quad(fill(bounds, flash));
        }
        if let Some(hover) = link_hover {
            window.set_cursor_style(CursorStyle::PointingHand, &layout.hitbox);
            paint_link_preview(
                window,
                cx,
                &hover.preview,
                hover.anchor_row as usize,
                hover.anchor_col as usize,
                layout,
                &bounds,
                &font,
                font_size,
            );
        }
    }
}

fn paint_dashed_underline(
    window: &mut Window,
    origin: gpui::Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    color: Hsla,
) {
    let y = origin.y + line_height - px(1.0);
    let end: f32 = origin.x.as_f32() + cell_width.as_f32();
    let mut x: f32 = origin.x.as_f32();
    let dash: f32 = 3.0;
    let gap: f32 = 2.0;
    while x < end {
        let width = f32::min(dash, end - x);
        if width > 0.0 {
            window.paint_quad(fill(Bounds::new(point(px(x), y), size(px(width), px(1.0))), color));
        }
        x += dash + gap;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_link_preview(
    window: &mut Window,
    cx: &mut App,
    preview: &str,
    anchor_row: usize,
    anchor_col: usize,
    layout: &TermLayout,
    bounds: &Bounds<Pixels>,
    font: &gpui::Font,
    font_size: Pixels,
) {
    let line =
        if anchor_row + 1 < layout.rows { anchor_row + 1 } else { anchor_row.saturating_sub(1) };
    let label_size = px(font_size.as_f32() * 0.85);
    let run = TextRun {
        len: preview.len(),
        font: font.clone(),
        color: cx.theme().popover_foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window.text_system().shape_line(
        SharedString::from(preview.to_owned()),
        label_size,
        &[run],
        None,
    );
    let pad = px(8.0);
    let bubble_w = shaped.width + pad * 2.0;
    let bubble_h = layout.line_height * 0.85 + pad;
    let max_x = (bounds.origin.x + bounds.size.width - bubble_w).max(bounds.origin.x);
    let x = (bounds.origin.x + layout.cell_width * anchor_col as f32).min(max_x);
    let y =
        bounds.origin.y + layout.line_height * line as f32 + (layout.line_height - bubble_h) * 0.5;
    let bubble = Bounds::new(point(x, y), size(bubble_w, bubble_h));
    window.paint_quad(fill(bubble, cx.theme().popover).corner_radii(px(6.0)));
    window.paint_quad(
        outline(bubble, cx.theme().border, gpui::BorderStyle::Solid).corner_radii(px(6.0)),
    );
    let _ = shaped.paint(
        point(x + pad, y + (bubble_h - layout.line_height * 0.85) * 0.5),
        layout.line_height * 0.85,
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    );
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
    let _ = shaped.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx);
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

const COMPLETION_PANEL_WIDTH: f32 = 520.0;
const COMPLETION_PANEL_PAD: f32 = 4.0;
const COMPLETION_PANEL_GAP: f32 = 6.0;
const COMPLETION_ROW_HEIGHT: f32 = 36.0;
const COMPLETION_ROW_LEFT_PAD: f32 = 8.0;
const COMPLETION_ROW_RIGHT_PAD: f32 = 9.0;
const COMPLETION_ICON_WIDTH: f32 = 22.0;
const COMPLETION_ICON_SIZE: f32 = 17.0;
const COMPLETION_COLUMN_GAP: f32 = 8.0;
const COMPLETION_TAG_WIDTH: f32 = 46.0;

#[derive(Clone, Copy)]
enum CompletionVisualKind {
    History,
    Command,
    Directory,
    Rust,
    Toml,
    Markdown,
    File,
}

fn completion_visual_kind(item: &crate::display::NebulaCompletionItem) -> CompletionVisualKind {
    use crate::display::NebulaCompletionKind;

    match item.kind {
        NebulaCompletionKind::History => CompletionVisualKind::History,
        NebulaCompletionKind::Command => CompletionVisualKind::Command,
        NebulaCompletionKind::Dir => CompletionVisualKind::Directory,
        NebulaCompletionKind::File => {
            // 候选 label 保留真实扩展名；先按 Path 解析，避免把目录名中间的点
            // 或命令参数误认成文件类型。引号只可能包住整段显示文本。
            let label = item.label.trim_matches(|c| c == '"' || c == '\'');
            match std::path::Path::new(label).extension().and_then(|ext| ext.to_str()) {
                Some(ext) if ext.eq_ignore_ascii_case("rs") => CompletionVisualKind::Rust,
                Some(ext) if ext.eq_ignore_ascii_case("toml") => CompletionVisualKind::Toml,
                Some(ext)
                    if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") =>
                {
                    CompletionVisualKind::Markdown
                },
                _ => CompletionVisualKind::File,
            }
        },
    }
}

fn completion_tag(kind: CompletionVisualKind) -> &'static str {
    match kind {
        CompletionVisualKind::History => "HIS",
        CompletionVisualKind::Command => "CMD",
        CompletionVisualKind::Directory => "DIR",
        CompletionVisualKind::Rust => "RS",
        CompletionVisualKind::Toml => "TOML",
        CompletionVisualKind::Markdown => "MD",
        CompletionVisualKind::File => "FILE",
    }
}

fn completion_kind_color(
    colors: &crate::gpui_shell::theme::CompletionColors,
    kind: CompletionVisualKind,
) -> Hsla {
    match kind {
        CompletionVisualKind::History => colors.history,
        CompletionVisualKind::Command => colors.command,
        CompletionVisualKind::Directory => colors.directory,
        CompletionVisualKind::Rust => colors.rust_file,
        CompletionVisualKind::Toml => colors.toml,
        CompletionVisualKind::Markdown => colors.markdown,
        CompletionVisualKind::File => colors.file,
    }
}

fn completion_cells(text: &str) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn completion_label_parts(item: &crate::display::NebulaCompletionItem) -> (&str, &str) {
    if !item.insert.is_empty() && item.label.ends_with(&item.insert) {
        item.label.split_at(item.label.len() - item.insert.len())
    } else {
        ("", &item.label)
    }
}

/// 绘制与命中共用的像素几何。这里刻意不再用终端 cell 拼凑 UI 列：
/// 22px 图标槽、两段 8px 间距和 46px 标签槽必须与 HTML 原型一一对应。
#[derive(Clone, Copy)]
pub(super) struct CompletionPopupLayout {
    content_x: Pixels,
    content_y: Pixels,
    content_width: Pixels,
    pub row_height: Pixels,
    pub offset: usize,
    pub rows: usize,
    pub label_width: usize,
    pub selected: Option<usize>,
}

impl CompletionPopupLayout {
    pub(super) fn content_bounds(self, origin: gpui::Point<Pixels>) -> Bounds<Pixels> {
        Bounds::new(
            point(origin.x + self.content_x, origin.y + self.content_y),
            size(self.content_width, self.row_height * self.rows),
        )
    }

    fn panel_bounds(self, origin: gpui::Point<Pixels>) -> Bounds<Pixels> {
        let content = self.content_bounds(origin);
        let pad = px(COMPLETION_PANEL_PAD);
        Bounds::new(
            point(content.origin.x - pad, content.origin.y - pad),
            size(content.size.width + pad * 2.0, content.size.height + pad * 2.0),
        )
    }

    fn row_bounds(self, origin: gpui::Point<Pixels>, row: usize) -> Bounds<Pixels> {
        let content = self.content_bounds(origin);
        Bounds::new(
            point(content.origin.x, content.origin.y + self.row_height * row),
            size(content.size.width, self.row_height),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn completion_popup_layout(
    items: &[crate::display::NebulaCompletionItem],
    selected: Option<usize>,
    cursor_row: usize,
    cursor_col: usize,
    screen_lines: usize,
    columns: usize,
    cell_width: Pixels,
    line_height: Pixels,
) -> Option<CompletionPopupLayout> {
    if columns < 12 || screen_lines < 2 || items.is_empty() {
        return None;
    }

    let cell_width = cell_width.as_f32();
    let line_height = line_height.as_f32();
    if cell_width <= 0.0 || line_height <= 0.0 {
        return None;
    }

    let viewport_width = cell_width * columns as f32;
    let viewport_height = line_height * screen_lines as f32;
    let panel_width = COMPLETION_PANEL_WIDTH.min(viewport_width);
    let content_width = panel_width - COMPLETION_PANEL_PAD * 2.0;
    let fixed_columns = COMPLETION_ROW_LEFT_PAD
        + COMPLETION_ICON_WIDTH
        + COMPLETION_COLUMN_GAP * 2.0
        + COMPLETION_TAG_WIDTH
        + COMPLETION_ROW_RIGHT_PAD;
    let label_width = ((content_width - fixed_columns) / cell_width).floor() as isize;
    if label_width <= 0 {
        return None;
    }

    let cursor_row = cursor_row.min(screen_lines - 1);
    let cursor_col = cursor_col.min(columns - 1);
    let cursor_top = line_height * cursor_row as f32;
    let cursor_bottom = cursor_top + line_height;
    let below_space = (viewport_height - cursor_bottom - COMPLETION_PANEL_GAP).max(0.0);
    let above_space = (cursor_top - COMPLETION_PANEL_GAP).max(0.0);
    let rows_that_fit = |space: f32| {
        ((space - COMPLETION_PANEL_PAD * 2.0).max(0.0) / COMPLETION_ROW_HEIGHT).floor() as usize
    };
    let below = rows_that_fit(below_space);
    let above = rows_that_fit(above_space);
    let want = items.len().min(8);
    let place_below = below >= want || below >= above;
    let rows = want.min(if place_below { below } else { above });
    if rows == 0 {
        return None;
    }

    let panel_height = COMPLETION_ROW_HEIGHT * rows as f32 + COMPLETION_PANEL_PAD * 2.0;
    let panel_y = if place_below {
        cursor_bottom + COMPLETION_PANEL_GAP
    } else {
        (cursor_top - COMPLETION_PANEL_GAP - panel_height).max(0.0)
    };

    // 光标处优先对齐列表内容；若 520px 面板越过右边界，整块向左滑，
    // 而不是压缩图标/标签列，窄窗里只牺牲中间文字宽度。
    let desired_panel_x = cell_width * cursor_col as f32 - COMPLETION_PANEL_PAD;
    let max_panel_x = (viewport_width - panel_width).max(0.0);
    let panel_x = desired_panel_x.clamp(0.0, max_panel_x);

    let selected = selected.filter(|index| *index < items.len());
    let offset = selected.filter(|index| *index >= rows).map(|index| index + 1 - rows).unwrap_or(0);
    Some(CompletionPopupLayout {
        content_x: px(panel_x + COMPLETION_PANEL_PAD),
        content_y: px(panel_y + COMPLETION_PANEL_PAD),
        content_width: px(content_width),
        row_height: px(COMPLETION_ROW_HEIGHT),
        offset,
        rows,
        label_width: label_width as usize,
        selected,
    })
}

fn completion_icon_point(
    origin: gpui::Point<Pixels>,
    scale: f32,
    x: f32,
    y: f32,
) -> gpui::Point<Pixels> {
    point(origin.x + px(x * scale), origin.y + px(y * scale))
}

fn add_file_outline(path: &mut PathBuilder, origin: gpui::Point<Pixels>, scale: f32) {
    let at = |x, y| completion_icon_point(origin, scale, x, y);
    path.move_to(at(14.5, 2.0));
    path.line_to(at(6.0, 2.0));
    path.curve_to(at(4.0, 4.0), at(4.0, 2.0));
    path.line_to(at(4.0, 20.0));
    path.curve_to(at(6.0, 22.0), at(4.0, 22.0));
    path.line_to(at(18.0, 22.0));
    path.curve_to(at(20.0, 20.0), at(20.0, 22.0));
    path.line_to(at(20.0, 7.5));
    path.line_to(at(14.5, 2.0));
    path.move_to(at(14.0, 2.0));
    path.line_to(at(14.0, 8.0));
    path.line_to(at(20.0, 8.0));
}

/// HTML 原型使用的 Lucide 线框被直接转换为 GPUI stroke path；不再用
/// ↶、›、/、· 这些字符假装图标，字体变化也不会破坏列间距。
fn paint_completion_icon(
    window: &mut Window,
    kind: CompletionVisualKind,
    bounds: Bounds<Pixels>,
    color: Hsla,
) {
    let scale = COMPLETION_ICON_SIZE / 24.0;
    let origin = point(
        bounds.origin.x + (bounds.size.width - px(COMPLETION_ICON_SIZE)) * 0.5,
        bounds.origin.y + (bounds.size.height - px(COMPLETION_ICON_SIZE)) * 0.5,
    );
    let at = |x, y| completion_icon_point(origin, scale, x, y);
    let mut path = PathBuilder::stroke(px(1.35));

    match kind {
        CompletionVisualKind::History => {
            path.move_to(at(3.0, 12.0));
            path.arc_to(
                point(px(9.0 * scale), px(9.0 * scale)),
                px(0.0),
                true,
                false,
                at(6.0, 5.3),
            );
            path.line_to(at(3.0, 8.0));
            path.move_to(at(3.0, 3.0));
            path.line_to(at(3.0, 8.0));
            path.line_to(at(8.0, 8.0));
            path.move_to(at(12.0, 7.0));
            path.line_to(at(12.0, 12.0));
            path.line_to(at(15.0, 14.0));
        },
        CompletionVisualKind::Command => {
            path.move_to(at(5.0, 4.0));
            path.line_to(at(19.0, 4.0));
            path.curve_to(at(21.0, 6.0), at(21.0, 4.0));
            path.line_to(at(21.0, 18.0));
            path.curve_to(at(19.0, 20.0), at(21.0, 20.0));
            path.line_to(at(5.0, 20.0));
            path.curve_to(at(3.0, 18.0), at(3.0, 20.0));
            path.line_to(at(3.0, 6.0));
            path.curve_to(at(5.0, 4.0), at(3.0, 4.0));
            path.move_to(at(7.0, 9.0));
            path.line_to(at(10.0, 12.0));
            path.line_to(at(7.0, 15.0));
            path.move_to(at(13.0, 15.0));
            path.line_to(at(17.0, 15.0));
        },
        CompletionVisualKind::Directory => {
            path.move_to(at(4.0, 3.0));
            path.line_to(at(7.9, 3.0));
            path.curve_to(at(9.6, 3.9), at(9.0, 3.0));
            path.line_to(at(10.4, 5.1));
            path.curve_to(at(12.1, 6.0), at(11.0, 6.0));
            path.line_to(at(20.0, 6.0));
            path.curve_to(at(22.0, 8.0), at(22.0, 6.0));
            path.line_to(at(22.0, 18.0));
            path.curve_to(at(20.0, 20.0), at(22.0, 20.0));
            path.line_to(at(4.0, 20.0));
            path.curve_to(at(2.0, 18.0), at(2.0, 20.0));
            path.line_to(at(2.0, 5.0));
            path.curve_to(at(4.0, 3.0), at(2.0, 3.0));
        },
        CompletionVisualKind::Rust => {
            add_file_outline(&mut path, origin, scale);
            path.move_to(at(10.0, 13.0));
            path.line_to(at(8.0, 15.0));
            path.line_to(at(10.0, 17.0));
            path.move_to(at(14.0, 17.0));
            path.line_to(at(16.0, 15.0));
            path.line_to(at(14.0, 13.0));
        },
        CompletionVisualKind::Toml => {
            add_file_outline(&mut path, origin, scale);
            path.move_to(at(10.0, 13.0));
            path.curve_to(at(9.0, 14.5), at(9.0, 13.0));
            path.curve_to(at(7.0, 16.0), at(9.0, 16.0));
            path.move_to(at(14.0, 13.0));
            path.curve_to(at(15.0, 14.5), at(15.0, 13.0));
            path.curve_to(at(17.0, 16.0), at(15.0, 16.0));
        },
        CompletionVisualKind::Markdown => {
            add_file_outline(&mut path, origin, scale);
            path.move_to(at(8.0, 13.0));
            path.line_to(at(16.0, 13.0));
            path.move_to(at(8.0, 17.0));
            path.line_to(at(14.0, 17.0));
        },
        CompletionVisualKind::File => {
            add_file_outline(&mut path, origin, scale);
            path.move_to(at(8.0, 15.0));
            path.line_to(at(16.0, 15.0));
        },
    }

    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// 弹窗补全列表严格复用 HTML 原型的三列间距；底部空间容不下时整体翻到
/// 光标上方。选中态没有箭头光标，只在行左侧画候选语义色的 2px 竖线。
#[allow(clippy::too_many_arguments)]
fn paint_completion_popup(
    window: &mut Window,
    cx: &mut App,
    items: &[crate::display::NebulaCompletionItem],
    selected: Option<usize>,
    cursor_row: usize,
    cursor_col: usize,
    layout: &TermLayout,
    bounds: &Bounds<Pixels>,
    colors: &crate::gpui_shell::theme::CompletionColors,
    font: &gpui::Font,
    font_size: Pixels,
) {
    let Some(popup) = completion_popup_layout(
        items,
        selected,
        cursor_row,
        cursor_col,
        layout.rows,
        layout.cols,
        layout.cell_width,
        layout.line_height,
    ) else {
        return;
    };
    let visible = &items[popup.offset..(popup.offset + popup.rows).min(items.len())];
    let panel_bounds = popup.panel_bounds(bounds.origin);
    let panel_radius = px(6.0);
    window.paint_drop_shadows(
        panel_bounds,
        panel_radius.into(),
        &[
            gpui::BoxShadow {
                color: colors.panel_shadow,
                offset: point(px(0.0), px(20.0)),
                blur_radius: px(45.0),
                spread_radius: px(0.0),
                inset: false,
            },
            gpui::BoxShadow {
                color: colors.panel_shadow.opacity(0.62),
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(12.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ],
    );
    window.paint_quad(fill(panel_bounds, colors.panel_bg).corner_radii(panel_radius));
    window.paint_quad(
        outline(panel_bounds, colors.panel_border, gpui::BorderStyle::Solid)
            .corner_radii(panel_radius),
    );

    for (row, item) in visible.iter().enumerate() {
        let row_bounds = popup.row_bounds(bounds.origin, row);
        let is_selected = Some(popup.offset + row) == popup.selected;
        let visual_kind = completion_visual_kind(item);
        let semantic = completion_kind_color(colors, visual_kind);
        let row_radius = if is_selected { px(4.0) } else { px(0.0) };
        window.paint_quad(
            fill(row_bounds, if is_selected { colors.selected_bg } else { colors.row_bg })
                .corner_radii(row_radius),
        );
        if is_selected {
            window.paint_quad(fill(
                Bounds::new(row_bounds.origin, size(px(2.0), row_bounds.size.height)),
                semantic,
            ));
        }

        let icon_slot = Bounds::new(
            point(row_bounds.origin.x + px(COMPLETION_ROW_LEFT_PAD), row_bounds.origin.y),
            size(px(COMPLETION_ICON_WIDTH), row_bounds.size.height),
        );
        paint_completion_icon(window, visual_kind, icon_slot, semantic);

        let label_origin = point(
            row_bounds.origin.x
                + px(COMPLETION_ROW_LEFT_PAD + COMPLETION_ICON_WIDTH + COMPLETION_COLUMN_GAP),
            row_bounds.origin.y,
        );
        let (matched, remainder) = completion_label_parts(item);
        let used = grid_text(
            window,
            cx,
            matched,
            font,
            font_size,
            colors.match_fg,
            label_origin,
            layout.cell_width,
            popup.row_height,
            popup.label_width,
        );
        grid_text(
            window,
            cx,
            remainder,
            font,
            font_size,
            if is_selected { colors.selected_fg } else { colors.row_fg },
            point(label_origin.x + layout.cell_width * used, label_origin.y),
            layout.cell_width,
            popup.row_height,
            popup.label_width.saturating_sub(used),
        );

        let tag = completion_tag(visual_kind);
        let tag_font_size = font_size * 0.75;
        let tag_cell_width = layout.cell_width * 0.75;
        let tag_cells = completion_cells(tag);
        let tag_right = row_bounds.origin.x + row_bounds.size.width - px(COMPLETION_ROW_RIGHT_PAD);
        let tag_origin = point(tag_right - tag_cell_width * tag_cells, row_bounds.origin.y);
        grid_text(
            window,
            cx,
            tag,
            font,
            tag_font_size,
            semantic,
            tag_origin,
            tag_cell_width,
            popup.row_height,
            tag_cells,
        );
    }

    if items.len() > popup.rows {
        // 滚动条覆在面板右侧 4px 呼吸边内，不侵入固定 46px 标签列。
        let content = popup.content_bounds(bounds.origin);
        let track_h = content.size.height - px(4.0);
        let track_top = content.origin.y + px(2.0);
        let content_right = content.origin.x + content.size.width;
        let track_bounds =
            Bounds::new(point(content_right + px(1.0), track_top), size(px(2.0), track_h));
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

fn rgba_channels(color: Rgba) -> (u8, u8, u8) {
    (
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    )
}

fn is_default_host_cursor(palette: &super::colors::Palette) -> bool {
    rgba_channels(palette.cursor) == default_cursor_rgb()
}

fn is_default_selection(palette: &super::colors::Palette) -> bool {
    rgba_channels(palette.selection) == default_cursor_rgb()
        && (palette.selection.a - 0.60).abs() < 0.05
}

fn default_cursor_rgb() -> (u8, u8, u8) {
    match crate::config::color::NEBULA_DEFAULT_CURSOR.background {
        crate::display::color::CellRgb::Rgb(rgb) => (rgb.r, rgb.g, rgb.b),
        _ => (0x49, 0x4d, 0x72),
    }
}

fn rgb_from_rgba(color: Rgba) -> crate::display::color::Rgb {
    let (r, g, b) = rgba_channels(color);
    crate::display::color::Rgb::new(r, g, b)
}

fn rgba_rgb(color: crate::display::color::Rgb, alpha: f32) -> Rgba {
    Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: alpha,
    }
}

fn themed_anchor(palette: &super::colors::Palette, cx: &App) -> (crate::display::color::Rgb, bool) {
    let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
    // ANSI magenta = index 5；旧壳 `display.colors[NamedColor::Magenta]`。
    let magenta = rgb_from_rgba(palette.ansi[5]);
    let mix = if sk.is_light {
        crate::display::ui::tokens::terminal_feedback::ANCHOR_NEUTRAL_MIX_LIGHT
    } else {
        crate::display::ui::tokens::terminal_feedback::ANCHOR_NEUTRAL_MIX_DARK
    };
    (crate::display::content::mix_rgb(magenta, sk.ink_dim, mix), sk.is_light)
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

        let popup =
            completion_popup_layout(&items, Some(0), 2, 7, 24, 80, gpui::px(8.0), gpui::px(21.0))
                .expect("短命令的精确候选也必须形成可见面板");
        assert_eq!(popup.rows, 1);
        assert!(popup.label_width >= 3);
    }

    #[test]
    fn popup_layout_preserves_the_unselected_state() {
        let items = [NebulaCompletionItem {
            label: "git pull upstream".to_owned(),
            insert: " upstream".to_owned(),
            kind: NebulaCompletionKind::History,
        }];

        let popup =
            completion_popup_layout(&items, None, 2, 7, 24, 80, gpui::px(8.0), gpui::px(21.0))
                .expect("空选中态仍应显示候选面板");
        assert_eq!(popup.selected, None);
        assert_eq!(popup.offset, 0);
    }

    #[test]
    fn popup_flips_above_the_cursor_near_the_bottom_edge() {
        let items = [NebulaCompletionItem {
            label: "completion.rs".to_owned(),
            insert: "ompletion.rs".to_owned(),
            kind: NebulaCompletionKind::File,
        }];

        let line_height = gpui::px(21.0);
        let cursor_row = 22;
        let popup = completion_popup_layout(
            &items,
            None,
            cursor_row,
            7,
            24,
            80,
            gpui::px(8.0),
            line_height,
        )
        .expect("底部空间不足时仍应在光标上方显示候选");
        let cursor_top = line_height * cursor_row;
        assert!(popup.content_y + popup.row_height < cursor_top);
    }
}
