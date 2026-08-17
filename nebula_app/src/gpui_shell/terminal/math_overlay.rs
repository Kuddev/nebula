//! GPUI 壳终端的公式覆盖层。
//!
//! 探测/持久化/fit/几何全部复用旧壳 `display::terminal_math`（单一权威，
//! 两壳像素级同源）；本文件只做三件事：在 term 锁内组织一帧的扫描与
//! 绘制计划、把覆盖掩码交给格子绘制循环跳过源格、按计划用
//! `math_view` 的位图管线上屏。
//!
//! 与旧壳 `draw_pane` 相同的门控：alt screen / vi 模式 / 存在选区时整帧
//! 不覆盖（源码可正常选择复制）；光标所在逻辑行的排除在 `scan_visible`
//! 内部完成。旧壳按 `RenderableCell` 背景 alpha 过滤围栏代码块，本壳的
//! 等价数据源是快照的 `bg_runs`（默认背景不产生 run）。

use std::sync::Arc;

use gpui::{App, Bounds, Pixels, Rgba, SharedString, Window, point, px, size};
use nebula_terminal::Term;
use nebula_terminal::event::EventListener;
use nebula_terminal::index::{Column, Point};
use nebula_terminal::term::TermMode;

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::terminal_math::{
    self, CoverageMask, OverlayDrawPlan, TerminalMathState,
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
    pub fn observe_program(&mut self, program: Option<&str>) {
        self.state.observe_program(program);
    }

    /// 每帧的扫描 + 预编 + 几何计划。必须在 term 锁内调用。
    ///
    /// `size` 以网格左上角为原点（padding = 0）；`bg_covered` 回答"该行的
    /// 列区间内是否存在非默认背景"（围栏代码过滤）。
    #[allow(clippy::too_many_arguments)]
    pub fn plan_frame<T: EventListener>(
        &mut self,
        term: &Term<T>,
        size: &SizeInfo,
        has_selection: bool,
        cursor: Option<(usize, usize)>,
        default_foreground: Rgb,
        font_pixel_size: f32,
        pixels_per_point: f32,
        bg_covered: impl Fn(usize, usize, usize) -> bool,
    ) -> MathFrame {
        // 旧壳 draw_pane 的整帧门控（搜索态本壳尚不存在，故不在此列）。
        if term.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI) || has_selection {
            return MathFrame::default();
        }

        let allow_inline_dollar = self.state.inline_dollar_enabled();
        let cursor = cursor.map(|(row, column)| Point::new(row, Column(column)));
        let mut overlays = terminal_math::scan_visible(
            &mut self.state,
            term,
            size,
            // 本壳不走 RenderableCell 管线：fallback 重画与围栏过滤各有
            // 等价实现（覆盖失败不跳格 / bg_runs 过滤）。
            &[],
            allow_inline_dollar,
            cursor,
            default_foreground,
        );
        // 围栏代码里的公式源码保持原文（旧壳按单元格背景 alpha 判定）。
        overlays.retain(|overlay| {
            !overlay.covered_ranges().any(|(row, start, end)| {
                usize::try_from(row).is_ok_and(|row| bg_covered(row, start, end))
            })
        });

        let prepared = terminal_math::prepare_overlays(
            &mut self.state,
            &overlays,
            size,
            font_pixel_size,
            pixels_per_point,
        );
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
