//! GPUI 壳终端的公式覆盖层。
//!
//! 探测/持久化/fit/几何全部复用旧壳 `display::terminal_math`（单一权威，
//! 两壳像素级同源）；本文件只做三件事：在 term 锁内组织一帧的扫描与
//! 绘制计划、把覆盖掩码交给格子绘制循环跳过源格、按计划用
//! `math_view` 的位图管线上屏。
//!
//! 与旧壳 `draw_pane` 相同的门控：vi 模式 / 存在选区时整帧不覆盖（源码可
//! 正常选择复制）。备用屏幕程序可能把光标留在静态内容末行，因此不能把
//! 那一行当作正在编辑而排除。围栏过滤与前景色走同一条
//! `scan_visible(..., rendered_cells)`
//! 合同：从 term 网格构造等价 cell 列表（`bg_alpha` = 非默认 ANSI 背景或
//! 反色），不再把空切片丢进去再另用 `bg_runs` 事后过滤。

use std::{ops::Range, sync::Arc};

use gpui::{App, Bounds, Pixels, Rgba, SharedString, Window, point, px, size};
use nebula_terminal::Term;
use nebula_terminal::event::EventListener;
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Line, Point, Side};
use nebula_terminal::term::TermMode;
use nebula_terminal::term::cell::Flags;
use nebula_terminal::vte::ansi::{Color, NamedColor};

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::display::terminal_math::{
    self, CoverageMask, FormulaOverlay, LineProjection, OverlayDrawPlan, PreparedFormula,
    TerminalMathState,
};
use crate::gpui_shell::math_view;

/// 每 pane 一份的覆盖层状态（`TerminalView` 持有）。内部的持久锚点/缓存
/// 语义（滚动、reflow、清屏失效）全部由共享的 [`TerminalMathState`] 决定。
#[derive(Default)]
pub struct MathOverlay {
    state: TerminalMathState,
}

/// 一条通过几何计划的公式：源码 + 绘制计划（坐标系 = 网格局部逻辑 px）。
pub struct PlannedFormula {
    source: Arc<str>,
    plan: OverlayDrawPlan,
    /// 本条公式盖住的源格跨度 `(行, 起, 止)`：位图预检剔除公式之后要按存活
    /// 的公式重建掩码。
    spans: Vec<(usize, usize, usize)>,
}

/// 扫描/fit 已通过、等待位图预检的公式。预检只依赖公式内容、字号和颜色，
/// 不依赖最终屏幕原点；因此先在空投影下生成候选，过滤完成后再统一重算位置。
struct FormulaCandidate {
    overlay: FormulaOverlay,
    prepared: PreparedFormula,
    plan: OverlayDrawPlan,
}

/// term 锁内产生、锁外消费的候选帧。`MathAssets` 不能在 term 锁内访问，
/// 否则字体/位图缓存的工作会把 PTY 网格锁拖进渲染慢路径。
pub struct PendingMathFrame {
    candidates: Vec<FormulaCandidate>,
    size: SizeInfo,
    pixels_per_point: f32,
    reflow_inline: bool,
}

impl PendingMathFrame {
    fn empty(size: SizeInfo, pixels_per_point: f32) -> Self {
        Self { candidates: Vec::new(), size, pixels_per_point, reflow_inline: false }
    }
}

/// 一帧的覆盖产物：绘制清单 + 源格跳过掩码。
#[derive(Default)]
pub struct MathFrame {
    formulas: Vec<PlannedFormula>,
    coverage: CoverageMask,
    projection: LineProjection,
}

impl MathFrame {
    /// 该格是否被某条公式覆盖（覆盖格不画原文）。
    pub fn covers(&self, row: usize, column: usize) -> bool {
        !self.coverage.is_empty() && self.coverage.covers(Point::new(row, Column(column)))
    }

    pub fn is_empty(&self) -> bool {
        self.formulas.is_empty()
    }

    /// 普通格的源列映射到这一帧的视觉列；公式源格以及越过右边界的格返回
    /// `None`。调用方仍以源坐标决定颜色、链接和光标语义。
    pub fn project_cell(&self, row: usize, source_column: usize, columns: usize) -> Option<usize> {
        self.projection
            .project_cell(Point::new(row, Column(source_column)), columns)
            .map(|point| point.column.0)
    }

    /// 公式源码格的 ANSI 背景映射。视觉公式盒之外的多余源码格会被丢弃，
    /// 避免压缩后仍留下一条源宽度的背景色带。
    pub fn project_formula_background(
        &self,
        row: usize,
        source_column: usize,
        columns: usize,
    ) -> Option<usize> {
        self.projection
            .project_formula_background(Point::new(row, Column(source_column)), columns)
            .map(|point| point.column.0)
    }

    /// 光标/浮层锚点优先按普通格投影；极端情况下锚点落进公式源区，则退到
    /// 公式视觉背景能承载的位置。活动编辑行本来就不投影，这个分支主要防御
    /// 历史链接锚点。
    pub fn visual_column(&self, row: usize, source_column: usize, columns: usize) -> usize {
        self.project_cell(row, source_column, columns)
            .or_else(|| self.project_formula_background(row, source_column, columns))
            .unwrap_or_else(|| source_column.min(columns.saturating_sub(1)))
    }

    /// 把源 run 投影成若干连续视觉 run。背景跨过公式时必须逐源格映射，
    /// 但最终仍合并相邻视觉格，避免给每个背景 cell 单独提交一个 quad。
    pub fn projected_runs(
        &self,
        row: usize,
        source: Range<usize>,
        columns: usize,
        formula_background: bool,
    ) -> Vec<Range<usize>> {
        let mut runs = Vec::new();
        let mut current: Option<Range<usize>> = None;
        for source_column in source {
            let target = if formula_background && self.covers(row, source_column) {
                self.project_formula_background(row, source_column, columns)
            } else {
                self.project_cell(row, source_column, columns)
            };
            match target {
                Some(column) if current.as_ref().is_some_and(|run| run.end == column) => {
                    current.as_mut().expect("current visual run").end += 1;
                },
                Some(column) => {
                    if let Some(run) = current.take() {
                        runs.push(run);
                    }
                    current = Some(column..column + 1);
                },
                None => {
                    if let Some(run) = current.take() {
                        runs.push(run);
                    }
                },
            }
        }
        if let Some(run) = current {
            runs.push(run);
        }
        runs
    }
}

/// 计划里的前景色（`paint_frame` 与位图预检必须用同一个值，否则缓存键不同、
/// 预检的那张图白合成一遍）。
fn plan_color(plan: &OverlayDrawPlan) -> Rgba {
    Rgba {
        r: plan.foreground.r as f32 / 255.0,
        g: plan.foreground.g as f32 / 255.0,
        b: plan.foreground.b as f32 / 255.0,
        a: 1.0,
    }
}

impl MathOverlay {
    /// session 暂不可用时也要立即清掉上一帧投影；输入命中发生在 paint 之外，
    /// 不能等下一次有 term 的扫描再修正旧坐标。
    pub fn clear_frame(&mut self, size: &SizeInfo, pixels_per_point: f32) -> PendingMathFrame {
        self.state.update_projection(&[], &[], false);
        PendingMathFrame::empty(*size, pixels_per_point)
    }

    /// 每帧的扫描 + 预编 + 几何计划。必须在 term 锁内调用。
    ///
    /// `size` 以网格左上角为原点（padding = 0），行列必须与可见网格一致，
    /// 这样 `TextGrid::from_term` 的 history lookback 仍按视口顶锚定。
    pub fn plan_frame<T: EventListener>(
        &mut self,
        term: &Term<T>,
        size: &SizeInfo,
        default_foreground: Rgb,
        font_pixel_size: f32,
        pixels_per_point: f32,
    ) -> PendingMathFrame {
        let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        // Vi 模式与有效选择由终端自身接管，避免覆盖层遮住光标或选区。
        // 空选区 `to_range` 为 None，不会因为鼠标按下残留的零宽 Selection
        // 把公式关掉。
        if term.mode().intersects(TermMode::VI)
            || term.selection.as_ref().and_then(|selection| selection.to_range(term)).is_some()
        {
            return self.clear_frame(size, pixels_per_point);
        }

        let origin = term.viewport_origin_for(size.screen_lines());
        // 光标所在行是活动输入，必须保留原文。备用屏幕里同样放过：编辑器
        // 的光标压在你要改的那一行上，换成渲染图就没法编辑源码了。
        let cursor =
            nebula_terminal::term::point_to_viewport_from(origin, term.grid().cursor.point).filter(
                |point| point.line < size.screen_lines() && point.column.0 < size.columns(),
            );
        let rendered_cells = scan_cells_from_term(term, size, default_foreground);
        let overlays = terminal_math::scan_visible(
            &mut self.state,
            term,
            size,
            &rendered_cells,
            alt_screen,
            cursor,
            default_foreground,
        );

        let prepared = terminal_math::prepare_overlays(
            &mut self.state,
            &overlays,
            size,
            font_pixel_size,
            pixels_per_point,
        );
        // 候选计划必须在空投影下生成。位图预检会淘汰无法合成的公式；若先把
        // 它们计入累计 shift，后续存活公式的原点就会沿用一个不存在的压缩量。
        self.state.update_projection(&[], &[], false);
        let candidates = overlays
            .into_iter()
            .zip(prepared)
            .filter_map(|(overlay, prepared)| {
                let prepared = prepared?;
                let plan = terminal_math::plan_overlay_draw(
                    &mut self.state,
                    &overlay,
                    &prepared,
                    size,
                    pixels_per_point,
                )?;
                Some(FormulaCandidate { overlay, prepared, plan })
            })
            .collect();
        PendingMathFrame {
            candidates,
            size: *size,
            pixels_per_point,
            // 与旧壳一致：TUI/备用屏继续拥有固定列坐标，但公式覆盖本身仍可用。
            reflow_inline: !alt_screen,
        }
    }

    /// 位图预检并完成一帧。必须在 term 锁外调用；成功图像会在这里进入缓存，
    /// [`paint_frame`] 随后消费相同键，不会重复合成。
    pub fn finalize_frame(
        &mut self,
        pending: PendingMathFrame,
        raster_scale: f32,
        cx: &mut App,
    ) -> MathFrame {
        let can_rasterize = cx
            .try_global::<math_view::MathAssets>()
            .is_some_and(math_view::MathAssets::can_rasterize);
        let pixels_per_point = pending.pixels_per_point;
        self.finalize_frame_with(pending, |source, plan| {
            if !can_rasterize {
                return false;
            }
            let source: SharedString = source.to_string().into();
            cx.global_mut::<math_view::MathAssets>().can_compose(
                &source,
                plan.display_style,
                plan.fitted_pixel_size,
                pixels_per_point,
                raster_scale,
                plan_color(plan),
            )
        })
    }

    /// 预检策略与几何收敛拆开，测试可以用确定的存活集合验证 projection，
    /// 不需要启动 GPUI 字体后端。
    fn finalize_frame_with(
        &mut self,
        mut pending: PendingMathFrame,
        mut paintable: impl FnMut(&Arc<str>, &OverlayDrawPlan) -> bool,
    ) -> MathFrame {
        pending.candidates.retain(|candidate| {
            let source = candidate.overlay.source_arc();
            paintable(&source, &candidate.plan)
        });

        // plan 也可能在最终横向 shift 后退化。每淘汰一条就重新建立共享
        // LineProjection，直到 projection、coverage、draw plan 使用同一集合。
        loop {
            let overlays: Vec<FormulaOverlay> =
                pending.candidates.iter().map(|candidate| candidate.overlay.clone()).collect();
            let prepared: Vec<Option<PreparedFormula>> =
                pending.candidates.iter().map(|candidate| Some(candidate.prepared)).collect();
            let retained = self.state.update_projection_with_survivors(
                &overlays,
                &prepared,
                pending.reflow_inline,
            );

            if retained.iter().any(|keep| !keep) {
                let mut index = 0usize;
                pending.candidates.retain(|_| {
                    let keep = retained[index];
                    index += 1;
                    keep
                });
                continue;
            }

            let before = pending.candidates.len();
            pending.candidates.retain_mut(|candidate| {
                let Some(plan) = terminal_math::plan_overlay_draw(
                    &mut self.state,
                    &candidate.overlay,
                    &candidate.prepared,
                    &pending.size,
                    pending.pixels_per_point,
                ) else {
                    return false;
                };
                candidate.plan = plan;
                true
            });
            if pending.candidates.len() == before {
                break;
            }
        }

        let formulas: Vec<PlannedFormula> = pending
            .candidates
            .into_iter()
            .map(|candidate| PlannedFormula {
                source: candidate.overlay.source_arc(),
                plan: candidate.plan,
                spans: candidate.overlay.covered_spans().collect(),
            })
            .collect();
        let spans: Vec<(usize, usize, usize)> =
            formulas.iter().flat_map(|formula| formula.spans.iter().copied()).collect();
        MathFrame {
            coverage: CoverageMask::from_spans(spans),
            projection: self.state.projection_snapshot(),
            formulas,
        }
    }

    /// GPUI 所有 terminal-cell 输入的反投影入口。调用方传入与当前 term 锁
    /// 同源的 viewport origin，避免滚动或 resize commit 时把视觉行映错。
    pub fn source_point(&self, point: Point, side: Side, viewport_origin: Line) -> (Point, Side) {
        self.state.source_point(point, side, viewport_origin)
    }
}

/// 可见网格的 SizeInfo：padding = 0，行列与元素布局一致。
///
/// 半格余量是为了 `SizeInfo::assemble` 里 `(width / cell_width) as usize`
/// 不会把 `8.399994 * 120 / 8.399994 = 119.999` 截成少一列，从而丢掉行尾
/// 的 `\]` / `\)`。
pub(crate) fn grid_size_info(
    columns: usize,
    rows: usize,
    cell_width: f32,
    cell_height: f32,
) -> SizeInfo {
    SizeInfo::new(
        cell_width * columns as f32 + cell_width * 0.5,
        cell_height * rows as f32 + cell_height * 0.5,
        cell_width,
        cell_height,
        0.0,
        0.0,
        false,
    )
}

/// 旧壳 `RenderableContent` 迭代器的 GPUI 等价物：给 `scan_visible` 填
/// fallback / `bg_alpha` / 前景。`bg_alpha` 对齐 `compute_bg_alpha`（无
/// transparent_background_colors）：默认 Named 背景为 0，反色或其它 ANSI
/// 背景为 1。空格+默认背景跳过，与旧壳 `is_empty` 一致。
fn scan_cells_from_term<T: EventListener>(
    term: &Term<T>,
    size: &SizeInfo,
    default_foreground: Rgb,
) -> Vec<RenderableCell> {
    let grid = term.grid();
    let origin = term.viewport_origin_for(size.screen_lines());
    let columns = size.columns().min(grid.columns());
    let screen_lines = size
        .screen_lines()
        .min(((grid.bottommost_line() - origin).0.max(0) as usize).saturating_add(1));
    let mut cells = Vec::new();
    for row in 0..screen_lines {
        let line = origin + row as i32;
        for column in 0..columns {
            let cell = &grid[line][Column(column)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let bg_alpha = if cell.flags.contains(Flags::INVERSE) {
                1.0
            } else if cell.bg == Color::Named(NamedColor::Background) {
                0.0
            } else {
                1.0
            };
            if bg_alpha == 0.0
                && cell.c == ' '
                && !cell.flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
            {
                continue;
            }
            let fg = cell_foreground(cell, default_foreground);
            cells.push(RenderableCell {
                character: cell.c,
                point: Point::new(row, Column(column)),
                fg,
                bg: Rgb::default(),
                bg_alpha,
                underline: fg,
                flags: cell.flags,
                extra: None,
            });
        }
    }
    cells
}

fn cell_foreground(cell: &nebula_terminal::term::cell::Cell, default: Rgb) -> Rgb {
    let color = if cell.flags.contains(Flags::INVERSE) { cell.bg } else { cell.fg };
    match color {
        Color::Spec(rgb) => Rgb::new(rgb.r, rgb.g, rgb.b),
        _ => default,
    }
}

/// 按计划绘制一帧的公式（在格子文本之后调用）。`origin` 是网格左上角的
/// 窗口坐标；`cell` 是 (宽, 高)，text ops 的"整字符可见才画"合同用它。
pub fn paint_frame(
    frame: &MathFrame,
    origin: gpui::Point<Pixels>,
    cell: (f32, f32),
    pixels_per_point: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for formula in &frame.formulas {
        let plan = &formula.plan;
        let color = plan_color(plan);
        let source: SharedString = formula.source.to_string().into();

        // 裁剪跟随 bleed 预算（与旧壳 MathClip 同一矩形），交给 GPUI 的
        // content mask 执行像素级裁剪。
        let clip = Bounds::new(
            point(origin.x + px(plan.clip_left), origin.y + px(plan.clip_top)),
            size(
                px((plan.clip_right - plan.clip_left).max(0.0)),
                px((plan.clip_bottom - plan.clip_top).max(0.0)),
            ),
        );
        let left = origin.x + px(plan.origin_x);
        let baseline = origin.y + px(plan.baseline_y);
        window.with_content_mask(Some(gpui::ContentMask { bounds: clip }), |window| {
            math_view::paint_formula_image(
                &source,
                plan.display_style,
                plan.fitted_pixel_size,
                pixels_per_point,
                left,
                baseline,
                color,
                window,
                cx,
            );

            // 数学字体缺字的字符（公式里的中文等）：旧壳合同是整字符落在
            // 裁剪窗内才画，避免半个汉字。
            let layout = cx.global_mut::<math_view::MathAssets>().layout(
                &source,
                plan.display_style,
                plan.fitted_pixel_size,
                pixels_per_point,
            );
            let Some(layout) = layout else { return };
            let text_style = window.text_style();
            for op in &layout.text {
                let scale = op.pixel_size / plan.fitted_pixel_size.max(f32::EPSILON);
                let x = plan.origin_x + op.x;
                let width = cell.0 * scale;
                let height = cell.1 * scale;
                let top = plan.baseline_y + op.baseline_y - height * 0.8;
                if x < plan.clip_left
                    || x + width > plan.clip_right
                    || top < plan.clip_top
                    || top + height > plan.clip_bottom
                {
                    continue;
                }
                let mut buffer = [0u8; 4];
                let text: SharedString = op.character.encode_utf8(&mut buffer).to_string().into();
                let run = text_style.to_run(text.len());
                let line = window.text_system().shape_line(
                    text,
                    px(op.pixel_size),
                    std::slice::from_ref(&run),
                    None,
                );
                let target = point(
                    origin.x + px(x),
                    origin.y + px(plan.baseline_y + op.baseline_y) - line.ascent,
                );
                let _ = line.paint(
                    target,
                    line.ascent + line.descent,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use nebula_terminal::event::VoidListener;
    use nebula_terminal::grid::Dimensions;
    use nebula_terminal::index::{Column, Line, Point};
    use nebula_terminal::selection::{Selection, SelectionType};
    use nebula_terminal::term::Config;
    use nebula_terminal::vte::ansi::Color;
    use nebula_terminal::{Term, index::Side};

    use super::{MathOverlay, grid_size_info, scan_cells_from_term};
    use crate::display::color::Rgb;

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

    fn term_with(cols: usize, rows: usize, lines: &[&str]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TestSize { cols, rows }, VoidListener);
        for (row, text) in lines.iter().enumerate() {
            let mut col = 0usize;
            for ch in text.chars() {
                if col >= cols {
                    break;
                }
                let cell = &mut term.grid_mut()[Line(row as i32)][Column(col)];
                cell.c = ch;
                col += 1;
            }
        }
        // 公式行不能与光标逻辑行重合，否则 scan_visible 会整段排除。
        term.grid_mut().cursor.point = Point::new(Line(rows as i32 - 1), Column(0));
        term
    }

    fn plan(
        overlay: &mut MathOverlay,
        term: &Term<VoidListener>,
        cols: usize,
        rows: usize,
    ) -> super::MathFrame {
        let size = grid_size_info(cols, rows, 8.0, 16.0);
        let pending = overlay.plan_frame(term, &size, Rgb::new(0xd6, 0xda, 0xea), 16.0, 1.0);
        overlay.finalize_frame_with(pending, |_, _| true)
    }

    #[test]
    fn grid_size_info_does_not_drop_a_column_to_float_truncation() {
        let cell_w = 8.399994;
        let size = grid_size_info(120, 30, cell_w, 16.8);
        assert_eq!(size.columns(), 120);
        assert_eq!(size.screen_lines(), 30);
        assert_eq!(size.padding_x(), 0.0);
        assert_eq!(size.padding_y(), 0.0);
    }

    #[test]
    fn scan_cells_keep_backslash_and_skip_default_background_spaces() {
        let term = term_with(16, 3, &[r"\[x\]", "tail"]);
        let size = grid_size_info(16, 3, 8.0, 16.0);
        let cells = scan_cells_from_term(&term, &size, Rgb::new(255, 255, 255));
        let row0: String =
            cells.iter().filter(|cell| cell.point.line == 0).map(|cell| cell.character).collect();
        assert_eq!(row0, r"\[x\]");
        assert!(cells.iter().all(|cell| cell.bg_alpha == 0.0));
    }

    #[test]
    fn bracket_display_scans_without_inline_dollar() {
        let mut overlay = MathOverlay::default();
        let term = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "\\[x\\] is display math and must overlay without AI CLI");
        assert!(frame.covers(0, 0));
        assert!(frame.covers(0, 4), "closing ] must be covered");
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(frame.formulas[0].plan.display_style, "\\[ must typeset in display style");
    }

    #[test]
    fn parenthesized_inline_scans_without_ai_cli() {
        let mut overlay = MathOverlay::default();
        let term = term_with(16, 4, &[r"\(x\)", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "\\(x\\) is standard inline math");
        assert!(frame.covers(0, 0));
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(!frame.formulas[0].plan.display_style);
    }

    #[test]
    fn inline_dollar_uses_the_standard_formula_rule() {
        let mut overlay = MathOverlay::default();
        let term = term_with(16, 4, &["$x$", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "$x$ is standard inline math without AI context");
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(!frame.formulas[0].plan.display_style);
    }

    #[test]
    fn main_screen_compacts_inline_suffix_without_changing_source_columns() {
        let mut overlay = MathOverlay::default();
        let term = term_with(20, 4, &["$x$ suffix", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 20, 4);
        let source_suffix = 3;
        let visual_suffix =
            frame.project_cell(0, source_suffix, 20).expect("suffix remains inside the viewport");
        assert!(visual_suffix < source_suffix, "inline suffix must move into the compact gap");
        assert!(frame.covers(0, 0));
        assert!(frame.covers(0, 2));
        assert_eq!(term.grid()[Line(0)][Column(source_suffix)].c, ' ');
    }

    #[test]
    fn failed_formula_does_not_shift_later_survivors() {
        let line = "a $x$ b $y^2$ c";
        let term = term_with(32, 4, &[line, "", "prompt"]);
        let size = grid_size_info(32, 4, 8.0, 16.0);

        let mut all_overlay = MathOverlay::default();
        let all_pending =
            all_overlay.plan_frame(&term, &size, Rgb::new(0xd6, 0xda, 0xea), 16.0, 1.0);
        let all = all_overlay.finalize_frame_with(all_pending, |_, _| true);

        let mut filtered_overlay = MathOverlay::default();
        let filtered_pending =
            filtered_overlay.plan_frame(&term, &size, Rgb::new(0xd6, 0xda, 0xea), 16.0, 1.0);
        let filtered = filtered_overlay
            .finalize_frame_with(filtered_pending, |source, _| source.as_ref() != "x");

        assert!(!filtered.covers(0, 2), "failed first formula must leave its source visible");
        assert!(filtered.covers(0, 8), "later paintable formula still overlays");
        assert_eq!(filtered.formulas.len(), 1);
        assert_eq!(
            filtered.project_formula_background(0, 8, 32),
            Some(8),
            "the surviving formula must not inherit the failed formula's shift"
        );
        let source_suffix = 13;
        let filtered_suffix =
            filtered.project_cell(0, source_suffix, 32).expect("filtered suffix remains visible");
        let all_suffix =
            all.project_cell(0, source_suffix, 32).expect("unfiltered suffix remains visible");
        assert!(
            filtered_suffix > all_suffix,
            "the failed formula must not contribute a stale cumulative shift"
        );
    }

    #[test]
    fn ansi_background_keeps_formula_source_literal() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(16, 4, &["$x$", "", "prompt"]);
        for col in 0..3 {
            term.grid_mut()[Line(0)][Column(col)].bg = Color::Indexed(4);
        }
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(frame.is_empty(), "TUI- or code-owned backgrounds stay untouched");
    }

    #[test]
    fn markdown_stripped_bare_delimiter_still_needs_ansi_context() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(24, 4, &[r"(\sqrt{x})", "", "prompt"]);
        for col in 0..10 {
            term.grid_mut()[Line(0)][Column(col)].bg = Color::Indexed(4);
        }
        let frame = plan(&mut overlay, &term, 24, 4);
        assert!(frame.is_empty(), "bare parentheses remain ambiguous on an ANSI code surface");
    }

    #[test]
    fn empty_selection_does_not_disable_overlays() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        term.selection =
            Some(Selection::new(SelectionType::Simple, Point::new(Line(2), Column(0)), Side::Left));
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "zero-width leftover selection must not gate the frame");
    }

    #[test]
    fn effective_selection_clears_existing_projection() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(20, 4, &["$x^2$ suffix", "", "prompt"]);
        let source_suffix = 5;
        let projected = plan(&mut overlay, &term, 20, 4);
        assert!(projected.project_cell(0, source_suffix, 20).unwrap() < source_suffix);

        let mut selection =
            Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        selection.update(Point::new(Line(0), Column(2)), Side::Right);
        term.selection = Some(selection);
        let selected = plan(&mut overlay, &term, 20, 4);

        assert!(selected.is_empty(), "source text must stay visible while selecting");
        assert_eq!(selected.project_cell(0, source_suffix, 20), Some(source_suffix));
    }

    #[test]
    fn explicit_empty_frame_clears_existing_projection() {
        let mut overlay = MathOverlay::default();
        let term = term_with(20, 4, &["$x^2$ suffix", "", "prompt"]);
        let source_suffix = 5;
        let projected = plan(&mut overlay, &term, 20, 4);
        assert!(projected.project_cell(0, source_suffix, 20).unwrap() < source_suffix);

        let size = grid_size_info(20, 4, 8.0, 16.0);
        let pending = overlay.clear_frame(&size, 1.0);
        let cleared = overlay.finalize_frame_with(pending, |_, _| true);

        assert!(cleared.is_empty());
        assert_eq!(cleared.project_cell(0, source_suffix, 20), Some(source_suffix));
    }

    #[test]
    fn alt_screen_renders_agent_math() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(
            96,
            4,
            &[r"$n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n$", "", "prompt"],
        );
        let main_frame = plan(&mut overlay, &term, 96, 4);
        let source_end = r"$n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n$".chars().count();
        assert!(main_frame.project_cell(0, source_end, 96).unwrap() < source_end);
        term.swap_alt();
        // `swap_alt` 按终端语义先清空备用网格；测试内容必须写进交换后的活动
        // 网格，才能验证 ALT_SCREEN 的覆盖与固定列合同。
        for (column, character) in
            r"$n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n$".chars().enumerate()
        {
            term.grid_mut()[Line(0)][Column(column)].c = character;
        }
        let frame = plan(&mut overlay, &term, 96, 4);
        assert!(!frame.is_empty(), "alternate-screen output may use the common TeX path");
        assert_eq!(
            frame.formulas[0].source.as_ref(),
            r"n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n"
        );
        assert_eq!(
            frame.project_cell(0, source_end, 96),
            Some(source_end),
            "alternate-screen formulas may overlay but must keep fixed TUI columns"
        );
    }

    #[test]
    fn bracket_display_can_cross_hard_rows() {
        let mut overlay = MathOverlay::default();
        let term = term_with(16, 6, &[r"\[", "x", r"\]", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 16, 6);
        assert!(!frame.is_empty(), "\\[ across hard rows must overlay");
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(frame.formulas[0].plan.display_style);
        assert!(frame.covers(0, 0));
        assert!(frame.covers(2, 0));
    }

    #[test]
    fn markdown_unescaped_bare_bracket_block_overlays() {
        // AI CLI 的 markdown 渲染吃掉 `\[` `\]` `\,` 的反斜杠后屏幕上只剩
        // 裸 `[` 块（`\int`/`\frac` 幸存）；全链路必须照常出覆盖层。
        let mut overlay = MathOverlay::default();
        let term = term_with(40, 6, &["[", r"\int_0^1 x^2,dx = \frac{1}{3}", "]", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 40, 6);
        assert!(!frame.is_empty(), "markdown-unescaped bare bracket block must overlay");
        assert_eq!(frame.formulas[0].source.as_ref(), r"\int_0^1 x^2,dx = \frac{1}{3}");
        assert!(frame.formulas[0].plan.display_style);
        assert!(frame.covers(0, 0));
        assert!(frame.covers(2, 0));
    }

    #[test]
    fn markdown_unescaped_bare_paren_inline_overlays() {
        let mut overlay = MathOverlay::default();
        let term = term_with(40, 4, &[r"also (\sqrt{x^2+y^2}) works", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 40, 4);
        assert!(!frame.is_empty(), "bare paren with whitelisted TeX command must overlay");
        assert_eq!(frame.formulas[0].source.as_ref(), r"\sqrt{x^2+y^2}");
        assert!(!frame.formulas[0].plan.display_style);
    }
}
