//! OpenType MATH 盒布局与后端无关绘制指令。

use pulldown_latex::event::{Dimension, DimensionUnit};
use ttf_parser::GlyphId;
use unicode_width::UnicodeWidthChar;

use super::font::{GlyphMetrics, MathConstant, MathFont, StretchGlyph};
use super::ir::{
    AtomClass, ChildRange, ColumnAlign, DelimiterScale, MathNode, MathStyle, NodeId, ParsedFormula,
    ScriptPlacement,
};
use super::spacing::{atom_spacing, normalize_binary};
use super::{MathError, MathErrorKind, MathLimits};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MathMetrics {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) depth: f32,
    pub(crate) axis: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MathGlyphOp {
    pub(crate) glyph_id: u16,
    pub(crate) x: f32,
    /// 相对公式基线，正值向下。
    pub(crate) baseline_y: f32,
    pub(crate) pixel_size: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MathRuleOp {
    pub(crate) x: f32,
    /// 相对公式基线的矩形顶边，正值向下。
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

/// Unicode character absent from the compact math font. The document renderer
/// draws it through the application's existing cross-platform text cache.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MathTextOp {
    pub(crate) character: char,
    pub(crate) x: f32,
    /// Relative to the formula baseline, positive values point down.
    pub(crate) baseline_y: f32,
    pub(crate) pixel_size: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MathLayout {
    pub(crate) metrics: MathMetrics,
    pub(crate) glyphs: Vec<MathGlyphOp>,
    pub(crate) rules: Vec<MathRuleOp>,
    pub(crate) text: Vec<MathTextOp>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Segment {
    start: u32,
    len: u32,
}

impl Segment {
    fn new(start: usize, end: usize, kind: MathErrorKind) -> Result<Self, MathError> {
        let len = end.saturating_sub(start);
        if start > u32::MAX as usize || len > u32::MAX as usize {
            return Err(MathError::new(kind, 0));
        }
        Ok(Self { start: start as u32, len: len as u32 })
    }

    fn bounds(self) -> std::ops::Range<usize> {
        let start = self.start as usize;
        start..start + self.len as usize
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BoxPlan {
    metrics: MathMetrics,
    children: Segment,
    glyphs: Segment,
    rules: Segment,
    text: Segment,
    class: Option<AtomClass>,
    italic_correction: f32,
}

#[derive(Clone, Copy, Debug)]
struct PlacedChild {
    node: NodeId,
    x: f32,
    baseline_y: f32,
}

#[derive(Clone, Copy, Debug)]
struct LocalGlyph {
    glyph_id: GlyphId,
    x: f32,
    baseline_y: f32,
    pixel_size: f32,
}

#[derive(Clone, Copy, Debug)]
struct LocalRule {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Copy, Debug)]
struct LocalText {
    character: char,
    x: f32,
    baseline_y: f32,
    pixel_size: f32,
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    metrics: MathMetrics,
    class: Option<AtomClass>,
    italic_correction: f32,
}

pub(crate) fn layout_formula(
    formula: &ParsedFormula,
    pixel_size: f32,
    pixels_per_point: f32,
    limits: MathLimits,
) -> Result<MathLayout, MathError> {
    if !pixel_size.is_finite()
        || pixel_size <= 0.0
        || !pixels_per_point.is_finite()
        || pixels_per_point <= 0.0
    {
        return Err(MathError::new(MathErrorKind::Font, 0));
    }

    let font = MathFont::load().map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
    let styles = effective_styles(formula)?;
    let mut builder = LayoutBuilder {
        formula,
        font,
        base_pixel_size: pixel_size,
        pixels_per_point,
        limits,
        styles,
        plans: Vec::with_capacity(formula.arena.nodes.len()),
        placements: Vec::with_capacity(formula.arena.nodes.len()),
        glyphs: Vec::new(),
        rules: Vec::new(),
        text: Vec::new(),
    };
    builder.build_all()?;
    builder.flatten()
}

/// 样式先自根向下传播，随后节点可按 arena 后序顺序一次完成测量。
fn effective_styles(formula: &ParsedFormula) -> Result<Vec<MathStyle>, MathError> {
    let mut styles = vec![formula.style; formula.arena.nodes.len()];
    let mut stack = vec![(formula.root, formula.style, None::<u16>)];
    while let Some((node_id, inherited, active_scope)) = stack.pop() {
        let node = formula.arena.node(node_id);
        let (style, active_scope) = match node.style_override() {
            Some(style_override) if Some(style_override.scope) != active_scope => {
                (style_override.style, Some(style_override.scope))
            },
            _ => (inherited, active_scope),
        };
        let Some(slot) = styles.get_mut(node_id.0 as usize) else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        *slot = style;

        let mut push = |child: NodeId, child_style: MathStyle| {
            stack.push((child, child_style, active_scope));
        };
        match node {
            MathNode::Glyph { .. } | MathNode::Operator { .. } | MathNode::Space { .. } => {},
            MathNode::OperatorName { body, .. }
            | MathNode::Fenced { body, .. }
            | MathNode::Accent { body, .. }
            | MathNode::Negation { body, .. } => push(*body, style),
            MathNode::Row(range) => {
                for child in formula.arena.children(*range).iter().rev() {
                    push(*child, style);
                }
            },
            MathNode::Fraction { numerator, denominator, .. } => {
                let child_style = fraction_child_style(style);
                push(*denominator, child_style);
                push(*numerator, child_style);
            },
            MathNode::Scripts { base, subscript, superscript, .. } => {
                let script_style = script_child_style(style);
                if let Some(superscript) = superscript {
                    push(*superscript, script_style);
                }
                if let Some(subscript) = subscript {
                    push(*subscript, script_style);
                }
                push(*base, style);
            },
            MathNode::Radical { degree, body, .. } => {
                if let Some(degree) = degree {
                    push(*degree, MathStyle::ScriptScript);
                }
                push(*body, style);
            },
            MathNode::Matrix { rows, .. } => {
                let cell_style = if style == MathStyle::Display { MathStyle::Text } else { style };
                for row in formula.arena.rows(*rows).iter().rev() {
                    for cell in formula.arena.children(row.cells).iter().rev() {
                        push(*cell, cell_style);
                    }
                }
            },
        }
    }
    Ok(styles)
}

fn fraction_child_style(style: MathStyle) -> MathStyle {
    match style {
        MathStyle::Display => MathStyle::Text,
        MathStyle::Text => MathStyle::Script,
        MathStyle::Script | MathStyle::ScriptScript => MathStyle::ScriptScript,
    }
}

fn script_child_style(style: MathStyle) -> MathStyle {
    match style {
        MathStyle::Display | MathStyle::Text => MathStyle::Script,
        MathStyle::Script | MathStyle::ScriptScript => MathStyle::ScriptScript,
    }
}

struct LayoutBuilder<'a> {
    formula: &'a ParsedFormula,
    font: MathFont,
    base_pixel_size: f32,
    pixels_per_point: f32,
    limits: MathLimits,
    styles: Vec<MathStyle>,
    plans: Vec<BoxPlan>,
    placements: Vec<PlacedChild>,
    glyphs: Vec<LocalGlyph>,
    rules: Vec<LocalRule>,
    text: Vec<LocalText>,
}

impl LayoutBuilder<'_> {
    fn build_all(&mut self) -> Result<(), MathError> {
        for index in 0..self.formula.arena.nodes.len() {
            let node = self.formula.arena.nodes[index].clone();
            let child_start = self.placements.len();
            let glyph_start = self.glyphs.len();
            let rule_start = self.rules.len();
            let text_start = self.text.len();
            let geometry = match node {
                MathNode::Glyph {
                    character, class, variant, stretchy: _, delimiter_scale, ..
                } => self.layout_glyph(index, character, variant, class, delimiter_scale)?,
                MathNode::Operator { character, variant, small, .. } => {
                    self.layout_operator(index, character, variant, small)?
                },
                MathNode::OperatorName { body, limits: _, .. } => {
                    self.layout_passthrough(body, AtomClass::Op)
                },
                MathNode::Row(range) => self.layout_row(index, range)?,
                MathNode::Fraction { numerator, denominator, bar, .. } => {
                    self.layout_fraction(index, numerator, denominator, bar)?
                },
                MathNode::Scripts { base, subscript, superscript, placement, .. } => {
                    self.layout_scripts(index, base, subscript, superscript, placement)?
                },
                MathNode::Radical { degree, body, .. } => {
                    self.layout_radical(index, degree, body)?
                },
                MathNode::Fenced { open, close, body, .. } => {
                    self.layout_fenced(index, open, close, body)?
                },
                MathNode::Accent { accent, body, .. } => self.layout_accent(index, accent, body)?,
                MathNode::Matrix { rows, alignments, .. } => {
                    self.layout_matrix(index, rows, alignments)?
                },
                MathNode::Space { width, height, .. } => self.layout_space(index, width, height)?,
                MathNode::Negation { body, .. } => self.layout_negation(index, body)?,
            };
            self.plans.push(BoxPlan {
                metrics: geometry.metrics,
                children: Segment::new(
                    child_start,
                    self.placements.len(),
                    MathErrorKind::NodeLimit,
                )?,
                glyphs: Segment::new(glyph_start, self.glyphs.len(), MathErrorKind::OpLimit)?,
                rules: Segment::new(rule_start, self.rules.len(), MathErrorKind::OpLimit)?,
                text: Segment::new(text_start, self.text.len(), MathErrorKind::OpLimit)?,
                class: geometry.class,
                italic_correction: geometry.italic_correction,
            });
        }
        Ok(())
    }

    fn flatten(self) -> Result<MathLayout, MathError> {
        let root_plan = *self
            .plans
            .get(self.formula.root.0 as usize)
            .ok_or_else(|| MathError::new(MathErrorKind::Parse, 0))?;
        let mut layout = MathLayout {
            metrics: root_plan.metrics,
            glyphs: Vec::new(),
            rules: Vec::new(),
            text: Vec::new(),
        };
        let mut stack = vec![(self.formula.root, 0.0f32, 0.0f32)];
        while let Some((node, offset_x, offset_y)) = stack.pop() {
            let plan = self.plans[node.0 as usize];
            for glyph in &self.glyphs[plan.glyphs.bounds()] {
                ensure_op_budget(
                    layout.glyphs.len(),
                    layout.rules.len(),
                    layout.text.len(),
                    self.limits,
                )?;
                layout.glyphs.push(MathGlyphOp {
                    glyph_id: glyph.glyph_id.0,
                    x: offset_x + glyph.x,
                    baseline_y: offset_y + glyph.baseline_y,
                    pixel_size: glyph.pixel_size,
                });
            }
            for rule in &self.rules[plan.rules.bounds()] {
                ensure_op_budget(
                    layout.glyphs.len(),
                    layout.rules.len(),
                    layout.text.len(),
                    self.limits,
                )?;
                layout.rules.push(MathRuleOp {
                    x: offset_x + rule.x,
                    y: offset_y + rule.y,
                    width: rule.width,
                    height: rule.height,
                });
            }
            for text in &self.text[plan.text.bounds()] {
                ensure_op_budget(
                    layout.glyphs.len(),
                    layout.rules.len(),
                    layout.text.len(),
                    self.limits,
                )?;
                layout.text.push(MathTextOp {
                    character: text.character,
                    x: offset_x + text.x,
                    baseline_y: offset_y + text.baseline_y,
                    pixel_size: text.pixel_size,
                });
            }
            for child in self.placements[plan.children.bounds()].iter().rev() {
                stack.push((child.node, offset_x + child.x, offset_y + child.baseline_y));
            }
        }
        Ok(layout)
    }

    fn layout_glyph(
        &mut self,
        index: usize,
        character: char,
        variant: super::ir::FontVariant,
        class: AtomClass,
        delimiter_scale: Option<DelimiterScale>,
    ) -> Result<Geometry, MathError> {
        let pixel_size = self.pixel_size(index)?;
        let glyph = match self.font.styled_glyph(character, variant) {
            Ok(glyph) => glyph,
            Err(_) => return self.layout_text_character(index, character, class),
        };
        let mut geometry = if let Some(scale) = delimiter_scale {
            let target = delimiter_scale_factor(scale) * pixel_size;
            self.vertical_glyph_box(glyph, target, pixel_size, self.axis(index)?)?
        } else {
            self.natural_glyph_box(glyph, pixel_size)?
        };
        geometry.class = Some(class);
        Ok(geometry)
    }

    fn layout_operator(
        &mut self,
        index: usize,
        character: char,
        variant: super::ir::FontVariant,
        small: bool,
    ) -> Result<Geometry, MathError> {
        let pixel_size = self.pixel_size(index)?;
        let glyph = match self.font.styled_glyph(character, variant) {
            Ok(glyph) => glyph,
            Err(_) => return self.layout_text_character(index, character, AtomClass::Op),
        };
        let mut geometry = if self.styles[index] == MathStyle::Display && !small {
            let target = self
                .font
                .display_operator_min_height(pixel_size)
                .map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
            self.vertical_glyph_box(glyph, target, pixel_size, self.axis(index)?)?
        } else {
            self.natural_glyph_box(glyph, pixel_size)?
        };
        geometry.class = Some(AtomClass::Op);
        Ok(geometry)
    }

    fn layout_passthrough(&mut self, child: NodeId, class: AtomClass) -> Geometry {
        let plan = self.plans[child.0 as usize];
        self.place(child, 0.0, 0.0);
        Geometry {
            metrics: plan.metrics,
            class: Some(class),
            italic_correction: plan.italic_correction,
        }
    }

    fn layout_text_character(
        &mut self,
        index: usize,
        character: char,
        class: AtomClass,
    ) -> Result<Geometry, MathError> {
        let pixel_size = self.pixel_size(index)?;
        let columns = character.width().unwrap_or(1).max(1) as f32;
        // Match the normal terminal font's usual half-em Latin and full-em
        // wide-character advances without retaining another font/shaper here.
        let width = pixel_size * 0.52 * columns;
        let height = pixel_size * 0.78;
        let depth = pixel_size * 0.22;
        self.push_text(LocalText { character, x: 0.0, baseline_y: 0.0, pixel_size })?;
        Ok(Geometry {
            metrics: MathMetrics { width, height, depth, axis: self.axis(index)? },
            class: Some(class),
            italic_correction: 0.0,
        })
    }

    fn layout_row(&mut self, index: usize, range: ChildRange) -> Result<Geometry, MathError> {
        let children = self.formula.arena.children(range).to_vec();
        let mut classes: Vec<Option<AtomClass>> =
            children.iter().map(|child| self.plans[child.0 as usize].class).collect();
        normalize_binary(&mut classes);

        let em = self.pixel_size(index)?;
        let mut metrics = self.empty_metrics(index)?;
        let mut x = 0.0;
        let mut previous = None;
        let mut significant = 0usize;
        let mut single_class = None;
        let mut italic_correction = 0.0;
        for (child, class) in children.into_iter().zip(classes) {
            let plan = self.plans[child.0 as usize];
            if let (Some(left), Some(right)) = (previous, class) {
                x += atom_spacing(left, right, self.styles[index], em);
            }
            self.place(child, x, 0.0);
            include_child(&mut metrics, plan.metrics, x, 0.0);
            x += plan.metrics.width;
            if let Some(class) = class {
                previous = Some(class);
                significant += 1;
                single_class = Some(class);
            }
            italic_correction = plan.italic_correction;
        }
        metrics.width = x.max(metrics.width);
        Ok(Geometry {
            metrics,
            class: match significant {
                0 => None,
                1 => single_class,
                _ => Some(AtomClass::Ord),
            },
            italic_correction,
        })
    }

    fn layout_fraction(
        &mut self,
        index: usize,
        numerator: NodeId,
        denominator: NodeId,
        bar: bool,
    ) -> Result<Geometry, MathError> {
        let numerator_plan = self.plans[numerator.0 as usize];
        let denominator_plan = self.plans[denominator.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let display = self.styles[index] == MathStyle::Display;
        let axis = self.axis(index)?;
        let rule =
            if bar { self.constant(MathConstant::FractionRuleThickness, pixel_size)? } else { 0.0 };
        let numerator_shift = self.constant(
            if display {
                MathConstant::FractionNumeratorDisplayShiftUp
            } else {
                MathConstant::FractionNumeratorShiftUp
            },
            pixel_size,
        )?;
        let denominator_shift = self.constant(
            if display {
                MathConstant::FractionDenominatorDisplayShiftDown
            } else {
                MathConstant::FractionDenominatorShiftDown
            },
            pixel_size,
        )?;
        let numerator_gap = self.constant(
            if display {
                MathConstant::FractionNumeratorDisplayGapMin
            } else {
                MathConstant::FractionNumeratorGapMin
            },
            pixel_size,
        )?;
        let denominator_gap = self.constant(
            if display {
                MathConstant::FractionDenominatorDisplayGapMin
            } else {
                MathConstant::FractionDenominatorGapMin
            },
            pixel_size,
        )?;
        let rule_y = -axis - rule * 0.5;
        let numerator_shift =
            numerator_shift.max(numerator_plan.metrics.depth - rule_y + numerator_gap);
        let denominator_shift = denominator_shift
            .max(rule_y + rule + denominator_gap + denominator_plan.metrics.height);
        let padding = pixel_size / 18.0;
        let width =
            numerator_plan.metrics.width.max(denominator_plan.metrics.width) + 2.0 * padding;
        let numerator_x = (width - numerator_plan.metrics.width) * 0.5;
        let denominator_x = (width - denominator_plan.metrics.width) * 0.5;
        self.place(numerator, numerator_x, -numerator_shift);
        self.place(denominator, denominator_x, denominator_shift);
        if bar {
            self.push_rule(LocalRule { x: 0.0, y: rule_y, width, height: rule })?;
        }
        let mut metrics = self.empty_metrics(index)?;
        include_child(&mut metrics, numerator_plan.metrics, numerator_x, -numerator_shift);
        include_child(&mut metrics, denominator_plan.metrics, denominator_x, denominator_shift);
        include_rule(&mut metrics, 0.0, rule_y, width, rule);
        metrics.width = width;
        Ok(Geometry { metrics, class: Some(AtomClass::Inner), italic_correction: 0.0 })
    }

    fn layout_scripts(
        &mut self,
        index: usize,
        base: NodeId,
        subscript: Option<NodeId>,
        superscript: Option<NodeId>,
        placement: ScriptPlacement,
    ) -> Result<Geometry, MathError> {
        let above_below = matches!(placement, ScriptPlacement::AboveBelow)
            || matches!(placement, ScriptPlacement::Movable)
                && self.styles[index] == MathStyle::Display;
        if above_below {
            self.layout_limits(index, base, subscript, superscript)
        } else {
            self.layout_side_scripts(index, base, subscript, superscript)
        }
    }

    fn layout_side_scripts(
        &mut self,
        index: usize,
        base: NodeId,
        subscript: Option<NodeId>,
        superscript: Option<NodeId>,
    ) -> Result<Geometry, MathError> {
        let base_plan = self.plans[base.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let mut metrics = self.empty_metrics(index)?;
        self.place(base, 0.0, 0.0);
        include_child(&mut metrics, base_plan.metrics, 0.0, 0.0);
        let script_x = base_plan.metrics.width + base_plan.italic_correction.max(0.0);
        let mut sup_shift = self.constant(MathConstant::SuperscriptShiftUp, pixel_size)?;
        let mut sub_shift = self.constant(MathConstant::SubscriptShiftDown, pixel_size)?;
        if let Some(superscript) = superscript {
            let sup = self.plans[superscript.0 as usize].metrics;
            sup_shift = sup_shift
                .max(
                    base_plan.metrics.height
                        - self.constant(MathConstant::SuperscriptBaselineDropMax, pixel_size)?,
                )
                .max(sup.depth + self.constant(MathConstant::SuperscriptBottomMin, pixel_size)?);
        }
        if let Some(subscript) = subscript {
            let sub = self.plans[subscript.0 as usize].metrics;
            sub_shift = sub_shift
                .max(
                    base_plan.metrics.depth
                        + self.constant(MathConstant::SubscriptBaselineDropMin, pixel_size)?,
                )
                .max(sub.height - self.constant(MathConstant::SubscriptTopMax, pixel_size)?);
        }
        if let (Some(subscript), Some(superscript)) = (subscript, superscript) {
            let sub = self.plans[subscript.0 as usize].metrics;
            let sup = self.plans[superscript.0 as usize].metrics;
            sup_shift = sup_shift.max(
                sup.depth
                    + self.constant(MathConstant::SuperscriptBottomMaxWithSubscript, pixel_size)?,
            );
            let gap = sup_shift + sub_shift - sup.depth - sub.height;
            let gap_min = self.constant(MathConstant::SubSuperscriptGapMin, pixel_size)?;
            if gap < gap_min {
                sub_shift += gap_min - gap;
            }
        }
        let mut script_width: f32 = 0.0;
        if let Some(superscript) = superscript {
            let plan = self.plans[superscript.0 as usize];
            self.place(superscript, script_x, -sup_shift);
            include_child(&mut metrics, plan.metrics, script_x, -sup_shift);
            script_width = script_width.max(plan.metrics.width);
        }
        if let Some(subscript) = subscript {
            let plan = self.plans[subscript.0 as usize];
            self.place(subscript, script_x, sub_shift);
            include_child(&mut metrics, plan.metrics, script_x, sub_shift);
            script_width = script_width.max(plan.metrics.width);
        }
        if subscript.is_some() || superscript.is_some() {
            metrics.width = metrics.width.max(
                script_x
                    + script_width
                    + self.constant(MathConstant::SpaceAfterScript, pixel_size)?,
            );
        }
        Ok(Geometry { metrics, class: base_plan.class, italic_correction: 0.0 })
    }

    fn layout_limits(
        &mut self,
        index: usize,
        base: NodeId,
        subscript: Option<NodeId>,
        superscript: Option<NodeId>,
    ) -> Result<Geometry, MathError> {
        let base_plan = self.plans[base.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let mut width = base_plan.metrics.width;
        if let Some(node) = subscript {
            width = width.max(self.plans[node.0 as usize].metrics.width);
        }
        if let Some(node) = superscript {
            width = width.max(self.plans[node.0 as usize].metrics.width);
        }
        let base_x = (width - base_plan.metrics.width) * 0.5;
        let mut metrics = self.empty_metrics(index)?;
        self.place(base, base_x, 0.0);
        include_child(&mut metrics, base_plan.metrics, base_x, 0.0);
        if let Some(superscript) = superscript {
            let plan = self.plans[superscript.0 as usize];
            let gap = self.constant(MathConstant::UpperLimitGapMin, pixel_size)?;
            let rise = self.constant(MathConstant::UpperLimitBaselineRiseMin, pixel_size)?;
            let shift = (base_plan.metrics.height + gap + plan.metrics.depth).max(rise);
            let x = (width - plan.metrics.width) * 0.5 + base_plan.italic_correction * 0.5;
            self.place(superscript, x, -shift);
            include_child(&mut metrics, plan.metrics, x, -shift);
        }
        if let Some(subscript) = subscript {
            let plan = self.plans[subscript.0 as usize];
            let gap = self.constant(MathConstant::LowerLimitGapMin, pixel_size)?;
            let drop = self.constant(MathConstant::LowerLimitBaselineDropMin, pixel_size)?;
            let shift = (base_plan.metrics.depth + gap + plan.metrics.height).max(drop);
            let x = (width - plan.metrics.width) * 0.5 - base_plan.italic_correction * 0.5;
            self.place(subscript, x.max(0.0), shift);
            include_child(&mut metrics, plan.metrics, x.max(0.0), shift);
        }
        metrics.width = metrics.width.max(width);
        Ok(Geometry { metrics, class: base_plan.class, italic_correction: 0.0 })
    }

    fn layout_radical(
        &mut self,
        index: usize,
        degree: Option<NodeId>,
        body: NodeId,
    ) -> Result<Geometry, MathError> {
        let body_plan = self.plans[body.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let display = self.styles[index] == MathStyle::Display;
        let gap = self.constant(
            if display {
                MathConstant::RadicalDisplayVerticalGap
            } else {
                MathConstant::RadicalVerticalGap
            },
            pixel_size,
        )?;
        let rule = self.constant(MathConstant::RadicalRuleThickness, pixel_size)?;
        let extra = self.constant(MathConstant::RadicalExtraAscender, pixel_size)?;
        let root =
            self.font.glyph_id('√').map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
        let glyph_start = self.glyphs.len();
        let body_total = body_plan.metrics.height + body_plan.metrics.depth;
        let target = body_total + gap + rule;
        let mut radical = self.vertical_glyph_box(root, target, pixel_size, self.axis(index)?)?;
        let radical_total = radical.metrics.height + radical.metrics.depth;
        // OpenType 将 extra ascender 定义为根号上方留白。若离散的伸展
        // 字形高于最低目标，按 TeX 的规则把余量分到上下，横线才会与
        // 根号字形顶端处在同一高度，而不是悬在它的下方。
        let gap = gap.max((radical_total - rule - body_total + gap) * 0.5);
        let radical_ascent = body_plan.metrics.height + gap + rule;
        let radical_depth = (radical_total - radical_ascent).max(0.0);
        let shift_y = radical_depth - radical.metrics.depth;
        self.shift_glyphs(glyph_start, 0.0, shift_y);
        radical.metrics.height = (radical.metrics.height - shift_y).max(0.0);
        radical.metrics.depth = (radical.metrics.depth + shift_y).max(0.0);
        let body_x = radical.metrics.width;
        self.place(body, body_x, 0.0);
        // The rasterized surd and the UI rule are snapped by separate render
        // paths. Overlap the rule by half a physical pixel so rounding cannot
        // expose a hairline gap at the surd/radicand junction.
        let rule_overlap = 0.5;
        let rule_y = -body_plan.metrics.height - gap - rule;
        self.push_rule(LocalRule {
            x: (body_x - rule_overlap).max(0.0),
            y: rule_y,
            width: body_plan.metrics.width + rule_overlap,
            height: rule,
        })?;
        let mut min_x = 0.0;
        let mut degree_placement = None;
        if let Some(degree) = degree {
            let degree_plan = self.plans[degree.0 as usize];
            let before = self.constant(MathConstant::RadicalKernBeforeDegree, pixel_size)?;
            let after = self.constant(MathConstant::RadicalKernAfterDegree, pixel_size)?;
            let raise = self
                .font
                .radical_degree_raise()
                .map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
            let x = before - degree_plan.metrics.width;
            let baseline = -(radical.metrics.height * raise) + degree_plan.metrics.depth;
            min_x = x.min(0.0);
            degree_placement = Some((degree, x, baseline));
            let _ = after;
        }
        let x_shift = -min_x;
        if x_shift > 0.0 {
            self.shift_glyphs(glyph_start, x_shift, 0.0);
            self.shift_recent_rule(x_shift);
            if let Some(last) = self.placements.last_mut() {
                last.x += x_shift;
            }
        }
        if let Some((degree, x, baseline)) = degree_placement {
            self.place(degree, x + x_shift, baseline);
        }
        let mut metrics = self.empty_metrics(index)?;
        metrics.width = x_shift + body_x + body_plan.metrics.width;
        metrics.height =
            (radical.metrics.height + extra).max(body_plan.metrics.height + gap + rule + extra);
        metrics.depth = radical.metrics.depth.max(body_plan.metrics.depth);
        if let Some((degree, x, baseline)) = degree_placement {
            include_child(
                &mut metrics,
                self.plans[degree.0 as usize].metrics,
                x + x_shift,
                baseline,
            );
        }
        Ok(Geometry { metrics, class: Some(AtomClass::Ord), italic_correction: 0.0 })
    }

    fn layout_fenced(
        &mut self,
        index: usize,
        open: Option<char>,
        close: Option<char>,
        body: NodeId,
    ) -> Result<Geometry, MathError> {
        let body_plan = self.plans[body.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let min_height = self.constant(MathConstant::DelimitedSubFormulaMinHeight, pixel_size)?;
        let target = min_height.max((body_plan.metrics.height + body_plan.metrics.depth) * 1.01);
        let axis = self.axis(index)?;
        let mut x = 0.0;
        let mut metrics = self.empty_metrics(index)?;
        if let Some(open) = open {
            let glyph = self
                .font
                .glyph_id(open)
                .map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
            let delimiter = self.vertical_glyph_box(glyph, target, pixel_size, axis)?;
            include_metrics(&mut metrics, delimiter.metrics, x, 0.0);
            x += delimiter.metrics.width;
        }
        self.place(body, x, 0.0);
        include_child(&mut metrics, body_plan.metrics, x, 0.0);
        x += body_plan.metrics.width;
        if let Some(close) = close {
            let glyph_start = self.glyphs.len();
            let glyph = self
                .font
                .glyph_id(close)
                .map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
            let delimiter = self.vertical_glyph_box(glyph, target, pixel_size, axis)?;
            self.shift_glyphs(glyph_start, x, 0.0);
            include_metrics(&mut metrics, delimiter.metrics, x, 0.0);
            x += delimiter.metrics.width;
        }
        metrics.width = metrics.width.max(x);
        Ok(Geometry { metrics, class: Some(AtomClass::Inner), italic_correction: 0.0 })
    }

    fn layout_accent(
        &mut self,
        index: usize,
        accent: char,
        body: NodeId,
    ) -> Result<Geometry, MathError> {
        let body_plan = self.plans[body.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let glyph = self
            .font
            .glyph_id(accent)
            .map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
        let glyph_start = self.glyphs.len();
        let accent_box = self.horizontal_glyph_box(glyph, body_plan.metrics.width, pixel_size)?;
        let gap = (self.constant(MathConstant::AccentBaseHeight, pixel_size)?
            - body_plan.metrics.height)
            .max(0.0);
        let x = ((body_plan.metrics.width - accent_box.metrics.width) * 0.5).max(0.0);
        let baseline = -body_plan.metrics.height - gap - accent_box.metrics.depth;
        self.shift_glyphs(glyph_start, x, baseline);
        self.place(body, 0.0, 0.0);
        let mut metrics = self.empty_metrics(index)?;
        include_child(&mut metrics, body_plan.metrics, 0.0, 0.0);
        include_metrics(&mut metrics, accent_box.metrics, x, baseline);
        metrics.width = metrics.width.max(body_plan.metrics.width);
        Ok(Geometry {
            metrics,
            class: Some(AtomClass::Ord),
            italic_correction: body_plan.italic_correction,
        })
    }

    fn layout_matrix(
        &mut self,
        index: usize,
        rows: super::ir::RowRange,
        alignments: super::ir::AlignRange,
    ) -> Result<Geometry, MathError> {
        let rows = self.formula.arena.rows(rows).to_vec();
        let alignments = self.formula.arena.row_alignments(alignments).to_vec();
        let cells: Vec<Vec<NodeId>> =
            rows.iter().map(|row| self.formula.arena.children(row.cells).to_vec()).collect();
        let column_count = cells.iter().map(Vec::len).max().unwrap_or(0).max(alignments.len());
        let mut column_widths = vec![0.0f32; column_count];
        let mut row_heights = vec![0.0f32; cells.len()];
        let mut row_depths = vec![0.0f32; cells.len()];
        for (row_index, row) in cells.iter().enumerate() {
            for (column, cell) in row.iter().enumerate() {
                let metrics = self.plans[cell.0 as usize].metrics;
                column_widths[column] = column_widths[column].max(metrics.width);
                row_heights[row_index] = row_heights[row_index].max(metrics.height);
                row_depths[row_index] = row_depths[row_index].max(metrics.depth);
            }
        }
        let pixel_size = self.pixel_size(index)?;
        let column_gap = pixel_size;
        let row_gap = pixel_size * 0.2;
        let width: f32 =
            column_widths.iter().sum::<f32>() + column_gap * column_count.saturating_sub(1) as f32;
        let total_height: f32 = row_heights.iter().sum::<f32>()
            + row_depths.iter().sum::<f32>()
            + row_gap * cells.len().saturating_sub(1) as f32;
        let axis = self.axis(index)?;
        let mut top = -axis - total_height * 0.5;
        let mut metrics = self.empty_metrics(index)?;
        for (row_index, row) in cells.iter().enumerate() {
            let baseline = top + row_heights[row_index];
            let mut column_x = 0.0;
            for (column, cell) in row.iter().enumerate() {
                let plan = self.plans[cell.0 as usize];
                let alignment = alignments.get(column).copied().unwrap_or(ColumnAlign::Center);
                let x = match alignment {
                    ColumnAlign::Left => column_x,
                    ColumnAlign::Center => {
                        column_x + (column_widths[column] - plan.metrics.width) * 0.5
                    },
                    ColumnAlign::Right => column_x + column_widths[column] - plan.metrics.width,
                };
                self.place(*cell, x, baseline);
                include_child(&mut metrics, plan.metrics, x, baseline);
                column_x += column_widths[column] + column_gap;
            }
            top = baseline + row_depths[row_index] + row_gap;
        }
        metrics.width = metrics.width.max(width);
        Ok(Geometry { metrics, class: Some(AtomClass::Inner), italic_correction: 0.0 })
    }

    fn layout_space(
        &self,
        index: usize,
        width: Option<Dimension>,
        height: Option<Dimension>,
    ) -> Result<Geometry, MathError> {
        let em = self.pixel_size(index)?;
        Ok(Geometry {
            metrics: MathMetrics {
                width: width
                    .map_or(0.0, |value| dimension_pixels(value, em, self.pixels_per_point)),
                height: height
                    .map_or(0.0, |value| dimension_pixels(value, em, self.pixels_per_point))
                    .max(0.0),
                depth: 0.0,
                axis: self.axis(index)?,
            },
            class: None,
            italic_correction: 0.0,
        })
    }

    fn layout_negation(&mut self, index: usize, body: NodeId) -> Result<Geometry, MathError> {
        let body_plan = self.plans[body.0 as usize];
        let pixel_size = self.pixel_size(index)?;
        let glyph = self
            .font
            .glyph_id('\u{0338}')
            .or_else(|_| self.font.glyph_id('/'))
            .map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
        let glyph_start = self.glyphs.len();
        let slash = self.natural_glyph_box(glyph, pixel_size)?;
        let width = body_plan.metrics.width.max(slash.metrics.width);
        let body_x = (width - body_plan.metrics.width) * 0.5;
        let slash_x = (width - slash.metrics.width) * 0.5;
        self.place(body, body_x, 0.0);
        self.shift_glyphs(glyph_start, slash_x, 0.0);
        let mut metrics = self.empty_metrics(index)?;
        include_child(&mut metrics, body_plan.metrics, body_x, 0.0);
        include_metrics(&mut metrics, slash.metrics, slash_x, 0.0);
        metrics.width = width;
        Ok(Geometry {
            metrics,
            class: body_plan.class,
            italic_correction: body_plan.italic_correction,
        })
    }

    fn natural_glyph_box(
        &mut self,
        glyph: GlyphId,
        pixel_size: f32,
    ) -> Result<Geometry, MathError> {
        let metrics = self.glyph_metrics(glyph, pixel_size)?;
        let x = (-metrics.x_min).max(0.0);
        self.push_glyph(LocalGlyph { glyph_id: glyph, x, baseline_y: 0.0, pixel_size })?;
        Ok(Geometry {
            metrics: MathMetrics {
                width: (x + metrics.advance.max(metrics.x_max)).max(0.0),
                height: metrics.height.max(0.0),
                depth: metrics.depth.max(0.0),
                axis: 0.0,
            },
            class: None,
            italic_correction: metrics.italic_correction.max(0.0),
        })
    }

    fn vertical_glyph_box(
        &mut self,
        glyph: GlyphId,
        target: f32,
        pixel_size: f32,
        axis: f32,
    ) -> Result<Geometry, MathError> {
        let glyph_start = self.glyphs.len();
        let stretch = self
            .font
            .vertical_stretch(glyph, target, pixel_size)
            .map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
        let Some(stretch) = stretch else {
            let geometry = self.natural_glyph_box(glyph, pixel_size)?;
            return Ok(self.center_on_axis(glyph_start, geometry, axis));
        };
        match stretch {
            StretchGlyph::Single { glyph_id, .. } => {
                let geometry = self.natural_glyph_box(glyph_id, pixel_size)?;
                Ok(self.center_on_axis(glyph_start, geometry, axis))
            },
            StretchGlyph::Assembly { parts, advance } => {
                let scale = pixel_size
                    / self
                        .font
                        .units_per_em()
                        .map_err(|_| MathError::new(MathErrorKind::Font, 0))?
                        as f32;
                let total = advance * scale;
                let bottom = -axis + total * 0.5;
                let measured: Vec<(GlyphId, f32, GlyphMetrics)> = parts
                    .into_iter()
                    .map(|part| {
                        self.glyph_metrics(part.glyph_id, pixel_size)
                            .map(|metrics| (part.glyph_id, part.origin * scale, metrics))
                    })
                    .collect::<Result<_, _>>()?;
                let min_x = measured.iter().map(|(_, _, m)| m.x_min).fold(0.0f32, f32::min);
                let x_shift = -min_x;
                let mut geometry = Geometry {
                    metrics: MathMetrics { width: 0.0, height: 0.0, depth: 0.0, axis },
                    class: None,
                    italic_correction: 0.0,
                };
                for (glyph_id, origin, metrics) in measured {
                    let baseline = bottom - origin - metrics.depth;
                    self.push_glyph(LocalGlyph {
                        glyph_id,
                        x: x_shift,
                        baseline_y: baseline,
                        pixel_size,
                    })?;
                    geometry.metrics.width =
                        geometry.metrics.width.max(x_shift + metrics.advance.max(metrics.x_max));
                    geometry.metrics.height =
                        geometry.metrics.height.max(metrics.height - baseline);
                    geometry.metrics.depth = geometry.metrics.depth.max(metrics.depth + baseline);
                }
                Ok(geometry)
            },
        }
    }

    fn horizontal_glyph_box(
        &mut self,
        glyph: GlyphId,
        target: f32,
        pixel_size: f32,
    ) -> Result<Geometry, MathError> {
        let stretch = self
            .font
            .horizontal_stretch(glyph, target, pixel_size)
            .map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
        let Some(stretch) = stretch else { return self.natural_glyph_box(glyph, pixel_size) };
        match stretch {
            StretchGlyph::Single { glyph_id, .. } => self.natural_glyph_box(glyph_id, pixel_size),
            StretchGlyph::Assembly { parts, advance } => {
                let scale = pixel_size
                    / self
                        .font
                        .units_per_em()
                        .map_err(|_| MathError::new(MathErrorKind::Font, 0))?
                        as f32;
                let measured: Vec<(GlyphId, f32, GlyphMetrics)> = parts
                    .into_iter()
                    .map(|part| {
                        self.glyph_metrics(part.glyph_id, pixel_size)
                            .map(|metrics| (part.glyph_id, part.origin * scale, metrics))
                    })
                    .collect::<Result<_, _>>()?;
                let min_x = measured
                    .iter()
                    .map(|(_, origin, metrics)| origin + metrics.x_min)
                    .fold(0.0f32, f32::min);
                let x_shift = -min_x;
                let mut geometry = Geometry {
                    metrics: MathMetrics {
                        width: advance * scale + x_shift,
                        height: 0.0,
                        depth: 0.0,
                        axis: 0.0,
                    },
                    class: None,
                    italic_correction: 0.0,
                };
                for (glyph_id, origin, metrics) in measured {
                    self.push_glyph(LocalGlyph {
                        glyph_id,
                        x: x_shift + origin,
                        baseline_y: 0.0,
                        pixel_size,
                    })?;
                    geometry.metrics.width = geometry
                        .metrics
                        .width
                        .max(x_shift + origin + metrics.advance.max(metrics.x_max));
                    geometry.metrics.height = geometry.metrics.height.max(metrics.height);
                    geometry.metrics.depth = geometry.metrics.depth.max(metrics.depth);
                }
                Ok(geometry)
            },
        }
    }

    fn glyph_metrics(&self, glyph: GlyphId, pixel_size: f32) -> Result<GlyphMetrics, MathError> {
        self.font
            .glyph_metrics(glyph, pixel_size)
            .map_err(|_| MathError::new(MathErrorKind::Font, 0))
    }

    fn pixel_size(&self, index: usize) -> Result<f32, MathError> {
        let scale = match self.styles[index] {
            MathStyle::Display | MathStyle::Text => 1.0,
            MathStyle::Script => {
                self.font.script_scale(false).map_err(|_| MathError::new(MathErrorKind::Font, 0))?
            },
            MathStyle::ScriptScript => {
                self.font.script_scale(true).map_err(|_| MathError::new(MathErrorKind::Font, 0))?
            },
        };
        Ok(self.base_pixel_size * scale)
    }

    fn axis(&self, index: usize) -> Result<f32, MathError> {
        self.constant(MathConstant::AxisHeight, self.pixel_size(index)?)
    }

    fn constant(&self, constant: MathConstant, pixel_size: f32) -> Result<f32, MathError> {
        self.font.constant(constant, pixel_size).map_err(|_| MathError::new(MathErrorKind::Font, 0))
    }

    fn empty_metrics(&self, index: usize) -> Result<MathMetrics, MathError> {
        Ok(MathMetrics { axis: self.axis(index)?, ..MathMetrics::default() })
    }

    fn place(&mut self, node: NodeId, x: f32, baseline_y: f32) {
        self.placements.push(PlacedChild { node, x, baseline_y });
    }

    fn push_glyph(&mut self, glyph: LocalGlyph) -> Result<(), MathError> {
        ensure_op_budget(self.glyphs.len(), self.rules.len(), self.text.len(), self.limits)?;
        self.glyphs.push(glyph);
        Ok(())
    }

    fn push_rule(&mut self, rule: LocalRule) -> Result<(), MathError> {
        ensure_op_budget(self.glyphs.len(), self.rules.len(), self.text.len(), self.limits)?;
        self.rules.push(rule);
        Ok(())
    }

    fn push_text(&mut self, text: LocalText) -> Result<(), MathError> {
        ensure_op_budget(self.glyphs.len(), self.rules.len(), self.text.len(), self.limits)?;
        self.text.push(text);
        Ok(())
    }

    fn shift_glyphs(&mut self, start: usize, x: f32, baseline_y: f32) {
        for glyph in &mut self.glyphs[start..] {
            glyph.x += x;
            glyph.baseline_y += baseline_y;
        }
    }

    fn shift_recent_rule(&mut self, x: f32) {
        if let Some(rule) = self.rules.last_mut() {
            rule.x += x;
        }
    }

    fn center_on_axis(
        &mut self,
        glyph_start: usize,
        mut geometry: Geometry,
        axis: f32,
    ) -> Geometry {
        let center = (geometry.metrics.depth - geometry.metrics.height) * 0.5;
        let baseline_shift = -axis - center;
        self.shift_glyphs(glyph_start, 0.0, baseline_shift);
        geometry.metrics.height = (geometry.metrics.height - baseline_shift).max(0.0);
        geometry.metrics.depth = (geometry.metrics.depth + baseline_shift).max(0.0);
        geometry.metrics.axis = axis;
        geometry
    }
}

fn ensure_op_budget(
    glyphs: usize,
    rules: usize,
    text: usize,
    limits: MathLimits,
) -> Result<(), MathError> {
    if glyphs.saturating_add(rules).saturating_add(text) >= limits.max_ops {
        Err(MathError::new(MathErrorKind::OpLimit, 0))
    } else {
        Ok(())
    }
}

fn include_child(metrics: &mut MathMetrics, child: MathMetrics, x: f32, baseline_y: f32) {
    include_metrics(metrics, child, x, baseline_y);
}

fn include_metrics(metrics: &mut MathMetrics, child: MathMetrics, x: f32, baseline_y: f32) {
    metrics.width = metrics.width.max(x + child.width);
    metrics.height = metrics.height.max(child.height - baseline_y);
    metrics.depth = metrics.depth.max(child.depth + baseline_y);
}

fn include_rule(metrics: &mut MathMetrics, x: f32, y: f32, width: f32, height: f32) {
    metrics.width = metrics.width.max(x + width);
    metrics.height = metrics.height.max(-y);
    metrics.depth = metrics.depth.max(y + height);
}

fn delimiter_scale_factor(scale: DelimiterScale) -> f32 {
    match scale {
        DelimiterScale::Big => 1.2,
        DelimiterScale::BigUpper => 1.8,
        DelimiterScale::Bigg => 2.4,
        DelimiterScale::BiggUpper => 3.0,
    }
}

fn dimension_pixels(dimension: Dimension, em: f32, pixels_per_point: f32) -> f32 {
    let value = dimension.value;
    match dimension.unit {
        DimensionUnit::Em => value * em,
        DimensionUnit::Mu => value * em / 18.0,
        DimensionUnit::Ex => value * em * 0.431,
        DimensionUnit::Pt => value * pixels_per_point,
        DimensionUnit::Pc => value * 12.0 * pixels_per_point,
        DimensionUnit::In => value * 72.27 * pixels_per_point,
        DimensionUnit::Bp => value * (72.27 / 72.0) * pixels_per_point,
        DimensionUnit::Cm => value * (72.27 / 2.54) * pixels_per_point,
        DimensionUnit::Mm => value * (72.27 / 25.4) * pixels_per_point,
        DimensionUnit::Dd => value * (1238.0 / 1157.0) * pixels_per_point,
        DimensionUnit::Cc => value * 12.0 * (1238.0 / 1157.0) * pixels_per_point,
        DimensionUnit::Sp => value * pixels_per_point / 65_536.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{DEFAULT_LIMITS, parse_formula};

    fn formula(source: &str, display: bool) -> ParsedFormula {
        parse_formula(source, display, DEFAULT_LIMITS).unwrap()
    }

    fn glyph_style(formula: &ParsedFormula, styles: &[MathStyle], character: char) -> MathStyle {
        formula
            .arena
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                MathNode::Glyph { character: found, .. } if *found == character => {
                    Some(styles[index])
                },
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn inherited_fraction_and_script_styles_scale_down_once() {
        let fraction = formula(r"\frac{x}{y}", false);
        let styles = effective_styles(&fraction).unwrap();
        assert_eq!(glyph_style(&fraction, &styles, 'x'), MathStyle::Script);
        assert_eq!(glyph_style(&fraction, &styles, 'y'), MathStyle::Script);

        let scripts = formula("x_{y_z}", false);
        let styles = effective_styles(&scripts).unwrap();
        assert_eq!(glyph_style(&scripts, &styles, 'x'), MathStyle::Text);
        assert_eq!(glyph_style(&scripts, &styles, 'y'), MathStyle::Script);
        assert_eq!(glyph_style(&scripts, &styles, 'z'), MathStyle::ScriptScript);
    }

    #[test]
    fn explicit_display_style_inside_fraction_overrides_inherited_script_style() {
        let formula = formula(r"\frac{\displaystyle x}{y}", false);
        let styles = effective_styles(&formula).unwrap();
        assert_eq!(glyph_style(&formula, &styles, 'x'), MathStyle::Display);
        assert_eq!(glyph_style(&formula, &styles, 'y'), MathStyle::Script);
    }

    #[test]
    fn fraction_layout_emits_positioned_glyphs_and_rule() {
        let layout =
            layout_formula(&formula(r"\frac{x+1}{y}", true), 20.0, 96.0 / 72.27, DEFAULT_LIMITS)
                .unwrap();
        assert_eq!(layout.rules.len(), 1);
        assert_eq!(layout.glyphs.len(), 4);
        assert!(layout.metrics.width > 0.0);
        assert!(layout.metrics.height > 0.0 && layout.metrics.depth > 0.0);
        assert!(layout.glyphs.iter().all(|glyph| glyph.pixel_size.is_finite()));
    }

    #[test]
    fn radical_rule_meets_the_stretched_surd_without_a_rounding_gap() {
        let layout =
            layout_formula(&formula(r"\sqrt{x^2}", false), 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        assert_eq!(layout.rules.len(), 1);
        let radical = layout.glyphs[0];
        let glyph_metrics = MathFont::load()
            .unwrap()
            .glyph_metrics(GlyphId(radical.glyph_id), radical.pixel_size)
            .unwrap();
        let rule = layout.rules[0];
        let glyph_top = radical.baseline_y - glyph_metrics.height;
        let glyph_right = radical.x + glyph_metrics.x_max;

        assert!((rule.y - glyph_top).abs() < 0.01);
        assert!(rule.x <= glyph_right && glyph_right - rule.x <= 0.51);
        assert!(rule.x + rule.width <= layout.metrics.width + 0.01);
        assert!(layout.metrics.height > -rule.y);
    }

    #[test]
    fn atom_spacing_and_complex_boxes_change_measured_geometry() {
        let compact = layout_formula(&formula("xy", false), 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        let spaced = layout_formula(&formula("x+y", false), 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        assert!(spaced.metrics.width > compact.metrics.width);

        let complex = layout_formula(
            &formula(r"\left(\begin{matrix}a&b\\c&\sqrt{d}\end{matrix}\right)", true),
            18.0,
            1.0,
            DEFAULT_LIMITS,
        )
        .unwrap();
        assert!(complex.glyphs.len() >= 7);
        assert!(complex.metrics.height + complex.metrics.depth > 18.0);
    }

    #[test]
    fn final_draw_op_budget_is_enforced() {
        let limits = MathLimits { max_ops: 2, ..DEFAULT_LIMITS };
        let error = layout_formula(&formula("abc", false), 18.0, 1.0, limits).unwrap_err();
        assert_eq!(error.kind, MathErrorKind::OpLimit);
    }
}
