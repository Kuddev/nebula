//! GPUI 壳终端的公式覆盖层。
//!
//! 探测/持久化/fit/几何全部复用旧壳 `display::terminal_math`（单一权威，
//! 两壳像素级同源）；本文件只做三件事：在 term 锁内组织一帧的扫描与
//! 绘制计划、把覆盖掩码交给格子绘制循环跳过源格、按计划用
//! `math_view` 的位图管线上屏。
//!
//! 与旧壳 `draw_pane` 相同的门控：alt screen / vi 模式 / 存在选区时整帧
//! 不覆盖（源码可正常选择复制）；光标所在逻辑行的排除在 `scan_visible`
//! 内部完成。围栏过滤与前景色走同一条 `scan_visible(..., rendered_cells)`
//! 合同：从 term 网格构造等价 cell 列表（`bg_alpha` = 非默认 ANSI 背景或
//! 反色），不再把空切片丢进去再另用 `bg_runs` 事后过滤。

use std::sync::Arc;

use gpui::{App, Bounds, Pixels, Rgba, SharedString, Window, point, px, size};
use nebula_terminal::Term;
use nebula_terminal::event::EventListener;
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Point};
use nebula_terminal::term::TermMode;
use nebula_terminal::term::cell::Flags;
use nebula_terminal::vte::ansi::{Color, NamedColor};

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::display::terminal_math::{self, CoverageMask, OverlayDrawPlan, TerminalMathState};
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
}

/// 一帧的覆盖产物：绘制清单 + 源格跳过掩码。
#[derive(Default)]
pub struct MathFrame {
    formulas: Vec<PlannedFormula>,
    coverage: CoverageMask,
}

impl MathFrame {
    /// 该格是否被某条公式覆盖（覆盖格不画原文）。
    pub fn covers(&self, row: usize, column: usize) -> bool {
        !self.coverage.is_empty() && self.coverage.covers(Point::new(row, Column(column)))
    }

    pub fn is_empty(&self) -> bool {
        self.formulas.is_empty()
    }
}

impl MathOverlay {
    /// shell 集成上报的前台程序（AI CLI 名单解锁行内 `$…$`，与旧壳同门）。
    ///
    /// 身份先经 `extract_program` + `AgentKind` 归一成 slug（`claude-code` →
    /// `claude`），再交给共享的 `is_ai_cli`。CommandStart / `NEBULA|` / hook
    /// 三条路只要把同一串交给这里即可。
    pub fn observe_program(&mut self, program: Option<&str>) {
        let normalized = program.map(normalize_ai_program);
        self.state.observe_program(normalized.as_deref());
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
    ) -> MathFrame {
        // 旧壳 draw_pane：alt / vi / 选区整帧不覆盖。空选区 `to_range` 为
        // None，不会因为鼠标按下残留的零宽 Selection 把公式关掉。
        if term.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI)
            || term.selection.as_ref().and_then(|selection| selection.to_range(term)).is_some()
        {
            return MathFrame::default();
        }

        let allow_inline_dollar = self.state.inline_dollar_enabled();
        let origin = term.viewport_origin_for(size.screen_lines());
        let cursor = nebula_terminal::term::point_to_viewport_from(origin, term.grid().cursor.point)
            .filter(|point| point.line < size.screen_lines() && point.column.0 < size.columns());
        let rendered_cells = scan_cells_from_term(term, size, default_foreground);
        let overlays = terminal_math::scan_visible(
            &mut self.state,
            term,
            size,
            &rendered_cells,
            allow_inline_dollar,
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
        // 不调用 `update_projection`：GPUI 格子绘制不按 compact 公式左移
        // 后续字形；若这里重建投影，`plan_overlay_draw` 会把公式原点挪到
        // 源码左侧，叠到前一段正文上。
        let mut plans: Vec<Option<OverlayDrawPlan>> = Vec::with_capacity(overlays.len());
        for (overlay, prepared) in overlays.iter().zip(&prepared) {
            plans.push(prepared.as_ref().and_then(|prepared| {
                terminal_math::plan_overlay_draw(
                    &mut self.state,
                    overlay,
                    prepared,
                    size,
                    pixels_per_point,
                )
            }));
        }
        // 掩码按"真的会画"的公式构建：计划失败的公式保留原文，不留洞。
        let coverage = CoverageMask::build(&overlays, &plans);
        let formulas = overlays
            .iter()
            .zip(&plans)
            .filter_map(|(overlay, plan)| {
                plan.map(|plan| PlannedFormula { source: overlay.source_arc(), plan })
            })
            .collect();
        MathFrame { formulas, coverage }
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

fn normalize_ai_program(program: &str) -> String {
    let extracted =
        crate::display::extract_program(program).unwrap_or_else(|| program.trim().to_owned());
    crate::ai_agents::AgentKind::parse(&extracted)
        .map(|agent| agent.slug().to_owned())
        .unwrap_or(extracted)
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
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
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
        let color = Rgba {
            r: plan.foreground.r as f32 / 255.0,
            g: plan.foreground.g as f32 / 255.0,
            b: plan.foreground.b as f32 / 255.0,
            a: 1.0,
        };
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
                let _ = line.paint(target, line.ascent + line.descent, window, cx);
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

    use super::{MathOverlay, grid_size_info, normalize_ai_program, scan_cells_from_term};
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
        overlay.plan_frame(term, &size, Rgb::new(0xd6, 0xda, 0xea), 16.0, 1.0)
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
    fn normalize_ai_program_maps_claude_code_alias_to_is_ai_cli_slug() {
        assert_eq!(normalize_ai_program("claude-code"), "claude");
        assert_eq!(normalize_ai_program(r"D:\tools\Claude.EXE"), "claude");
        assert_eq!(normalize_ai_program("codex-cli"), "codex");
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
        assert!(!frame.is_empty(), "\\(x\\) is inline and does not need inline_dollar");
        assert!(frame.covers(0, 0));
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(!frame.formulas[0].plan.display_style);
    }

    #[test]
    fn inline_dollar_needs_ai_cli_identity() {
        let mut overlay = MathOverlay::default();
        let term = term_with(16, 4, &["$x$", "", "prompt"]);
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(frame.is_empty(), "bare $x$ must stay literal without AI CLI");

        overlay.observe_program(Some("claude-code"));
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "claude-code must normalize to claude and unlock $");
        assert_eq!(frame.formulas[0].source.as_ref(), "x");
        assert!(!frame.formulas[0].plan.display_style);
    }

    #[test]
    fn confirmed_display_formula_unlocks_inline_dollar() {
        let mut overlay = MathOverlay::default();
        let display = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        let _ = plan(&mut overlay, &display, 16, 4);

        let inline = term_with(16, 4, &["$x$", "", "prompt"]);
        let frame = plan(&mut overlay, &inline, 16, 4);
        assert!(!frame.is_empty(), "a seen \\[ must unlock inline dollar like the old shell");
    }

    #[test]
    fn ansi_background_fence_keeps_formula_source() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        for col in 0..5 {
            term.grid_mut()[Line(0)][Column(col)].bg = Color::Indexed(4);
        }
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(frame.is_empty(), "fenced ANSI background must not overlay, matching bg_alpha>0");
    }

    #[test]
    fn empty_selection_does_not_disable_overlays() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(2), Column(0)),
            Side::Left,
        ));
        let frame = plan(&mut overlay, &term, 16, 4);
        assert!(!frame.is_empty(), "zero-width leftover selection must not gate the frame");
    }

    #[test]
    fn alt_screen_disables_overlays() {
        let mut overlay = MathOverlay::default();
        let mut term = term_with(16, 4, &[r"\[x\]", "", "prompt"]);
        assert!(!plan(&mut overlay, &term, 16, 4).is_empty());
        term.swap_alt();
        assert!(plan(&mut overlay, &term, 16, 4).is_empty());
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
        let term =
            term_with(40, 6, &["[", r"\int_0^1 x^2,dx = \frac{1}{3}", "]", "", "prompt"]);
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
