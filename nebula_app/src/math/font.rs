//! 固定数学字体的零拷贝 OpenType MATH 访问层。

use std::sync::OnceLock;

use ttf_parser::math::{GlyphConstruction, GlyphPart};
use ttf_parser::{Face, GlyphId, OutlineBuilder, Rect};

use super::ir::FontVariant;

pub(crate) static FONT_BYTES: &[u8] = include_bytes!("../../../assets/fonts/LatinModernMath.otf");

const MAX_ASSEMBLY_PARTS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MathFontError;

/// Latin Modern Math 在进程内只解析一次；Face 只借用静态字体字节，不复制约 716 KiB 资源。
fn face() -> Result<&'static Face<'static>, MathFontError> {
    static FACE: OnceLock<Option<Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| Face::parse(FONT_BYTES, 0).ok()).as_ref().ok_or(MathFontError)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MathFont;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GlyphMetrics {
    pub(crate) advance: f32,
    pub(crate) height: f32,
    pub(crate) depth: f32,
    pub(crate) x_min: f32,
    pub(crate) x_max: f32,
    pub(crate) italic_correction: f32,
    pub(crate) top_accent: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MathConstant {
    AxisHeight,
    FractionNumeratorShiftUp,
    FractionNumeratorDisplayShiftUp,
    FractionDenominatorShiftDown,
    FractionDenominatorDisplayShiftDown,
    FractionNumeratorGapMin,
    FractionNumeratorDisplayGapMin,
    FractionRuleThickness,
    FractionDenominatorGapMin,
    FractionDenominatorDisplayGapMin,
    SubscriptShiftDown,
    SubscriptTopMax,
    SubscriptBaselineDropMin,
    SuperscriptShiftUp,
    SuperscriptBottomMin,
    SuperscriptBaselineDropMax,
    SubSuperscriptGapMin,
    SuperscriptBottomMaxWithSubscript,
    SpaceAfterScript,
    UpperLimitGapMin,
    UpperLimitBaselineRiseMin,
    LowerLimitGapMin,
    LowerLimitBaselineDropMin,
    RadicalVerticalGap,
    RadicalDisplayVerticalGap,
    RadicalRuleThickness,
    RadicalExtraAscender,
    RadicalKernBeforeDegree,
    RadicalKernAfterDegree,
    AccentBaseHeight,
    OverbarVerticalGap,
    OverbarRuleThickness,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StretchPart {
    pub(crate) glyph_id: GlyphId,
    /// 沿伸展方向的 design-unit 原点；vertical 为自底向上，horizontal 为自左向右。
    pub(crate) origin: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StretchGlyph {
    Single { glyph_id: GlyphId, advance: f32 },
    Assembly { parts: Vec<StretchPart>, advance: f32 },
}

impl StretchGlyph {
    fn advance(&self) -> f32 {
        match self {
            Self::Single { advance, .. } | Self::Assembly { advance, .. } => *advance,
        }
    }
}

impl MathFont {
    pub(crate) fn load() -> Result<Self, MathFontError> {
        let parsed = face()?;
        if parsed.tables().math.is_none() {
            return Err(MathFontError);
        }
        Ok(Self)
    }

    pub(crate) fn units_per_em(self) -> Result<u16, MathFontError> {
        Ok(face()?.units_per_em())
    }

    pub(crate) fn glyph_id(self, character: char) -> Result<GlyphId, MathFontError> {
        face()?.glyph_index(character).ok_or(MathFontError)
    }

    pub(crate) fn styled_glyph(
        self,
        character: char,
        variant: FontVariant,
    ) -> Result<GlyphId, MathFontError> {
        let styled = map_variant(variant, character);
        face()?
            .glyph_index(styled)
            .or_else(|| face().ok()?.glyph_index(character))
            .ok_or(MathFontError)
    }

    pub(crate) fn has_math_table(self) -> Result<bool, MathFontError> {
        Ok(face()?.tables().math.is_some())
    }

    pub(crate) fn has_vertical_construction(self, character: char) -> Result<bool, MathFontError> {
        let glyph = self.glyph_id(character)?;
        let Some(variants) = face()?.tables().math.and_then(|math| math.variants) else {
            return Ok(false);
        };
        let Some(construction) = variants.vertical_constructions.get(glyph) else {
            return Ok(false);
        };
        Ok(!construction.variants.is_empty() || construction.assembly.is_some())
    }

    pub(crate) fn glyph_metrics(
        self,
        glyph: GlyphId,
        pixel_size: f32,
    ) -> Result<GlyphMetrics, MathFontError> {
        let face = face()?;
        let scale = pixel_size / face.units_per_em() as f32;
        let bounds = face.glyph_bounding_box(glyph);
        let (height, depth, x_min, x_max) = bounds.map_or((0.0, 0.0, 0.0, 0.0), |bounds| {
            (
                bounds.y_max as f32 * scale,
                -(bounds.y_min as f32) * scale,
                bounds.x_min as f32 * scale,
                bounds.x_max as f32 * scale,
            )
        });
        let advance = face.glyph_hor_advance(glyph).unwrap_or(0) as f32 * scale;
        let glyph_info = face.tables().math.and_then(|math| math.glyph_info);
        let italic_correction = glyph_info
            .and_then(|info| info.italic_corrections)
            .and_then(|values| values.get(glyph))
            .map_or(0.0, |value| value.value as f32 * scale);
        let top_accent = glyph_info
            .and_then(|info| info.top_accent_attachments)
            .and_then(|values| values.get(glyph))
            .map_or(advance * 0.5, |value| value.value as f32 * scale);
        Ok(GlyphMetrics { advance, height, depth, x_min, x_max, italic_correction, top_accent })
    }

    pub(crate) fn constant(
        self,
        constant: MathConstant,
        pixel_size: f32,
    ) -> Result<f32, MathFontError> {
        let face = face()?;
        let constants = face.tables().math.and_then(|math| math.constants).ok_or(MathFontError)?;
        let value = match constant {
            MathConstant::AxisHeight => constants.axis_height().value,
            MathConstant::FractionNumeratorShiftUp => constants.fraction_numerator_shift_up().value,
            MathConstant::FractionNumeratorDisplayShiftUp => {
                constants.fraction_numerator_display_style_shift_up().value
            },
            MathConstant::FractionDenominatorShiftDown => {
                constants.fraction_denominator_shift_down().value
            },
            MathConstant::FractionDenominatorDisplayShiftDown => {
                constants.fraction_denominator_display_style_shift_down().value
            },
            MathConstant::FractionNumeratorGapMin => constants.fraction_numerator_gap_min().value,
            MathConstant::FractionNumeratorDisplayGapMin => {
                constants.fraction_num_display_style_gap_min().value
            },
            MathConstant::FractionRuleThickness => constants.fraction_rule_thickness().value,
            MathConstant::FractionDenominatorGapMin => {
                constants.fraction_denominator_gap_min().value
            },
            MathConstant::FractionDenominatorDisplayGapMin => {
                constants.fraction_denom_display_style_gap_min().value
            },
            MathConstant::SubscriptShiftDown => constants.subscript_shift_down().value,
            MathConstant::SubscriptTopMax => constants.subscript_top_max().value,
            MathConstant::SubscriptBaselineDropMin => constants.subscript_baseline_drop_min().value,
            MathConstant::SuperscriptShiftUp => constants.superscript_shift_up().value,
            MathConstant::SuperscriptBottomMin => constants.superscript_bottom_min().value,
            MathConstant::SuperscriptBaselineDropMax => {
                constants.superscript_baseline_drop_max().value
            },
            MathConstant::SubSuperscriptGapMin => constants.sub_superscript_gap_min().value,
            MathConstant::SuperscriptBottomMaxWithSubscript => {
                constants.superscript_bottom_max_with_subscript().value
            },
            MathConstant::SpaceAfterScript => constants.space_after_script().value,
            MathConstant::UpperLimitGapMin => constants.upper_limit_gap_min().value,
            MathConstant::UpperLimitBaselineRiseMin => {
                constants.upper_limit_baseline_rise_min().value
            },
            MathConstant::LowerLimitGapMin => constants.lower_limit_gap_min().value,
            MathConstant::LowerLimitBaselineDropMin => {
                constants.lower_limit_baseline_drop_min().value
            },
            MathConstant::RadicalVerticalGap => constants.radical_vertical_gap().value,
            MathConstant::RadicalDisplayVerticalGap => {
                constants.radical_display_style_vertical_gap().value
            },
            MathConstant::RadicalRuleThickness => constants.radical_rule_thickness().value,
            MathConstant::RadicalExtraAscender => constants.radical_extra_ascender().value,
            MathConstant::RadicalKernBeforeDegree => constants.radical_kern_before_degree().value,
            MathConstant::RadicalKernAfterDegree => constants.radical_kern_after_degree().value,
            MathConstant::AccentBaseHeight => constants.accent_base_height().value,
            MathConstant::OverbarVerticalGap => constants.overbar_vertical_gap().value,
            MathConstant::OverbarRuleThickness => constants.overbar_rule_thickness().value,
        };
        Ok(value as f32 * pixel_size / face.units_per_em() as f32)
    }

    pub(crate) fn script_scale(self, script_script: bool) -> Result<f32, MathFontError> {
        let constants =
            face()?.tables().math.and_then(|math| math.constants).ok_or(MathFontError)?;
        let percent = if script_script {
            constants.script_script_percent_scale_down()
        } else {
            constants.script_percent_scale_down()
        };
        Ok(percent as f32 / 100.0)
    }

    pub(crate) fn display_operator_min_height(self, pixel_size: f32) -> Result<f32, MathFontError> {
        let face = face()?;
        let constants = face.tables().math.and_then(|math| math.constants).ok_or(MathFontError)?;
        Ok(constants.display_operator_min_height() as f32 * pixel_size / face.units_per_em() as f32)
    }

    pub(crate) fn radical_degree_raise(self) -> Result<f32, MathFontError> {
        let constants =
            face()?.tables().math.and_then(|math| math.constants).ok_or(MathFontError)?;
        Ok(constants.radical_degree_bottom_raise_percent() as f32 / 100.0)
    }

    pub(crate) fn vertical_stretch(
        self,
        glyph: GlyphId,
        target_pixels: f32,
        pixel_size: f32,
    ) -> Result<Option<StretchGlyph>, MathFontError> {
        self.stretch(glyph, target_pixels, pixel_size, true)
    }

    pub(crate) fn horizontal_stretch(
        self,
        glyph: GlyphId,
        target_pixels: f32,
        pixel_size: f32,
    ) -> Result<Option<StretchGlyph>, MathFontError> {
        self.stretch(glyph, target_pixels, pixel_size, false)
    }

    fn stretch(
        self,
        glyph: GlyphId,
        target_pixels: f32,
        pixel_size: f32,
        vertical: bool,
    ) -> Result<Option<StretchGlyph>, MathFontError> {
        if !target_pixels.is_finite() || !pixel_size.is_finite() || pixel_size <= 0.0 {
            return Err(MathFontError);
        }
        let face = face()?;
        let math_variants =
            face.tables().math.and_then(|math| math.variants).ok_or(MathFontError)?;
        let construction = if vertical {
            math_variants.vertical_constructions.get(glyph)
        } else {
            math_variants.horizontal_constructions.get(glyph)
        };
        let Some(construction) = construction else { return Ok(None) };
        let target = target_pixels * face.units_per_em() as f32 / pixel_size;
        if let Some(single) = select_variant(construction, target) {
            if single.advance() >= target || construction.assembly.is_none() {
                return Ok(Some(single));
            }
        }
        let Some(assembly) = construction.assembly else {
            return Ok(select_variant(construction, target));
        };
        let parts: Vec<GlyphPart> = assembly.parts.into_iter().collect();
        let recipe = assemble(&parts, math_variants.min_connector_overlap, target);
        Ok(recipe.or_else(|| select_variant(construction, target)))
    }

    pub(crate) fn outline(
        self,
        glyph: GlyphId,
        builder: &mut impl OutlineBuilder,
    ) -> Result<Option<Rect>, MathFontError> {
        Ok(face()?.outline_glyph(glyph, builder))
    }
}

fn select_variant(construction: GlyphConstruction<'static>, target: f32) -> Option<StretchGlyph> {
    let mut largest = None;
    for variant in construction.variants {
        let candidate = StretchGlyph::Single {
            glyph_id: variant.variant_glyph,
            advance: variant.advance_measurement as f32,
        };
        if variant.advance_measurement as f32 >= target {
            return Some(candidate);
        }
        largest = Some(candidate);
    }
    largest
}

fn assemble(parts: &[GlyphPart], min_overlap: u16, target: f32) -> Option<StretchGlyph> {
    if parts.is_empty() {
        return None;
    }
    let mut repeats = vec![1usize; parts.len()];
    let mut total_parts = parts.len();
    let mut advance = assembly_advance(parts);
    let extenders: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| part.part_flags.extender().then_some(index))
        .collect();
    let mut extender_cursor = 0usize;
    while advance < target && total_parts < MAX_ASSEMBLY_PARTS && !extenders.is_empty() {
        let index = extenders[extender_cursor % extenders.len()];
        let part = parts[index];
        let overlap = connector_overlap(part, part, min_overlap) as f32;
        advance += part.full_advance as f32 - overlap;
        repeats[index] += 1;
        total_parts += 1;
        extender_cursor += 1;
    }

    let mut expanded = Vec::with_capacity(total_parts);
    for (part, count) in parts.iter().zip(repeats) {
        expanded.extend(std::iter::repeat_n(*part, count));
    }
    let mut positions = Vec::with_capacity(expanded.len());
    let mut origin = 0.0;
    for (index, part) in expanded.iter().enumerate() {
        positions.push(StretchPart { glyph_id: part.glyph_id, origin });
        if let Some(next) = expanded.get(index + 1) {
            origin +=
                part.full_advance as f32 - connector_overlap(*part, *next, min_overlap) as f32;
        }
    }
    let advance = origin + expanded.last()?.full_advance as f32;
    Some(StretchGlyph::Assembly { parts: positions, advance })
}

fn assembly_advance(parts: &[GlyphPart]) -> f32 {
    let advances: f32 = parts.iter().map(|part| part.full_advance as f32).sum();
    let overlaps: f32 =
        parts.windows(2).map(|pair| connector_overlap(pair[0], pair[1], 0) as f32).sum();
    advances - overlaps
}

fn connector_overlap(first: GlyphPart, second: GlyphPart, minimum: u16) -> u16 {
    let available = first.end_connector_length.min(second.start_connector_length);
    if available >= minimum { minimum } else { available }
}

fn map_variant(variant: FontVariant, character: char) -> char {
    use FontVariant::*;
    let codepoint = match (variant, character) {
        (BoldScript, 'A'..='Z') => character as u32 + 0x1D48F,
        (BoldScript, 'a'..='z') => character as u32 + 0x1D489,
        (BoldItalic, 'A'..='Z') => character as u32 + 0x1D427,
        (BoldItalic, 'a'..='z') => character as u32 + 0x1D421,
        (BoldItalic, '\u{0391}'..='\u{03A1}' | '\u{03A3}'..='\u{03A9}') => {
            character as u32 + 0x1D38B
        },
        (BoldItalic, '\u{03B1}'..='\u{03C9}') => character as u32 + 0x1D385,
        (Bold, 'A'..='Z') => character as u32 + 0x1D3BF,
        (Bold, 'a'..='z') => character as u32 + 0x1D3B9,
        (Bold, '\u{0391}'..='\u{03A1}' | '\u{03A3}'..='\u{03A9}') => character as u32 + 0x1D317,
        (Bold, '\u{03B1}'..='\u{03C9}') => character as u32 + 0x1D311,
        (Bold, '0'..='9') => character as u32 + 0x1D79E,
        (Fraktur, 'A' | 'B' | 'D'..='G' | 'J'..='Q' | 'S'..='Y') => character as u32 + 0x1D4C3,
        (Fraktur, 'C') => character as u32 + 0x20EA,
        (Fraktur, 'H' | 'I') => character as u32 + 0x20C4,
        (Fraktur, 'R') => character as u32 + 0x20CA,
        (Fraktur, 'Z') => character as u32 + 0x20CE,
        (Fraktur, 'a'..='z') => character as u32 + 0x1D4BD,
        (Script, 'A' | 'C' | 'D' | 'G' | 'J' | 'K' | 'N'..='Q' | 'S'..='Z') => {
            character as u32 + 0x1D45B
        },
        (Script, 'B') => character as u32 + 0x20EA,
        (Script, 'E' | 'F') => character as u32 + 0x20EB,
        (Script, 'H') => character as u32 + 0x20C3,
        (Script, 'I') => character as u32 + 0x20C7,
        (Script, 'L') => character as u32 + 0x20C6,
        (Script, 'M') => character as u32 + 0x20E6,
        (Script, 'R') => character as u32 + 0x20C9,
        (Script, 'a'..='d' | 'f' | 'h'..='n' | 'p'..='z') => character as u32 + 0x1D455,
        (Script, 'e') => character as u32 + 0x20CA,
        (Script, 'g') => character as u32 + 0x20A3,
        (Script, 'o') => character as u32 + 0x20C5,
        (Monospace, 'A'..='Z') => character as u32 + 0x1D62F,
        (Monospace, 'a'..='z') => character as u32 + 0x1D629,
        (Monospace, '0'..='9') => character as u32 + 0x1D7C6,
        (SansSerif, 'A'..='Z') => character as u32 + 0x1D55F,
        (SansSerif, 'a'..='z') => character as u32 + 0x1D559,
        (SansSerif, '0'..='9') => character as u32 + 0x1D7B2,
        (DoubleStruck, 'A' | 'B' | 'D'..='G' | 'I'..='M' | 'O' | 'S'..='Y') => {
            character as u32 + 0x1D4F7
        },
        (DoubleStruck, 'C') => character as u32 + 0x20BF,
        (DoubleStruck, 'H') => character as u32 + 0x20C5,
        (DoubleStruck, 'N') => character as u32 + 0x20C7,
        (DoubleStruck, 'P' | 'Q') => character as u32 + 0x20C9,
        (DoubleStruck, 'R') => character as u32 + 0x20CB,
        (DoubleStruck, 'Z') => character as u32 + 0x20CA,
        (DoubleStruck, 'a'..='z') => character as u32 + 0x1D4F1,
        (DoubleStruck, '0'..='9') => character as u32 + 0x1D7A8,
        (Italic, 'A'..='Z') => character as u32 + 0x1D3F3,
        (Italic, 'a'..='g' | 'i'..='z') => character as u32 + 0x1D3ED,
        (Italic, 'h') => character as u32 + 0x20A6,
        (Italic, '\u{0391}'..='\u{03A1}' | '\u{03A3}'..='\u{03A9}') => character as u32 + 0x1D351,
        (Italic, '\u{03B1}'..='\u{03C9}') => character as u32 + 0x1D34B,
        (BoldFraktur, 'A'..='Z') => character as u32 + 0x1D52B,
        (BoldFraktur, 'a'..='z') => character as u32 + 0x1D525,
        (SansSerifBoldItalic, 'A'..='Z') => character as u32 + 0x1D5FB,
        (SansSerifBoldItalic, 'a'..='z') => character as u32 + 0x1D5F5,
        (SansSerifItalic, 'A'..='Z') => character as u32 + 0x1D5D7,
        (SansSerifItalic, 'a'..='z') => character as u32 + 0x1D5C1,
        (BoldSansSerif, 'A'..='Z') => character as u32 + 0x1D593,
        (BoldSansSerif, 'a'..='z') => character as u32 + 0x1D58D,
        (BoldSansSerif, '0'..='9') => character as u32 + 0x1D7BC,
        _ => character as u32,
    };
    char::from_u32(codepoint).unwrap_or(character)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    const EXPECTED_SHA256: [u8; 32] = [
        0x60, 0x75, 0x56, 0x2b, 0x77, 0x1f, 0x8b, 0x82, 0xf0, 0xc1, 0x79, 0xe3, 0x63, 0x38, 0x96,
        0x84, 0xf2, 0xdd, 0x09, 0xde, 0x30, 0x03, 0x82, 0x69, 0xe2, 0x62, 0x8e, 0x50, 0x4b, 0xd7,
        0xbe, 0x0f,
    ];

    #[test]
    fn bundled_font_bytes_are_the_reviewed_asset() {
        let digest = Sha256::digest(FONT_BYTES);
        assert_eq!(&digest[..], &EXPECTED_SHA256);
    }

    #[test]
    fn bundled_font_exposes_math_metrics_and_core_glyphs() {
        let font = MathFont::load().expect("reviewed font must parse");
        assert_eq!(font.units_per_em().unwrap(), 1000);
        assert!(font.has_math_table().unwrap());
        for character in ['x', '0', '+', '=', 'α', '∑', '∫', '√'] {
            assert!(font.glyph_id(character).is_ok(), "missing glyph {character}");
        }
        assert!(font.has_vertical_construction('(').unwrap());
    }

    #[test]
    fn glyph_metrics_and_math_constants_scale_linearly() {
        let font = MathFont::load().unwrap();
        let glyph = font.glyph_id('x').unwrap();
        let small = font.glyph_metrics(glyph, 12.0).unwrap();
        let large = font.glyph_metrics(glyph, 24.0).unwrap();
        assert!((large.advance / small.advance - 2.0).abs() < 0.001);
        let axis = font.constant(MathConstant::AxisHeight, 16.0).unwrap();
        let rule = font.constant(MathConstant::FractionRuleThickness, 16.0).unwrap();
        assert!(axis > 0.0 && axis < 16.0);
        assert!(rule > 0.0 && rule < 2.0);
        assert!(font.script_scale(false).unwrap() > font.script_scale(true).unwrap());
    }

    #[test]
    fn math_alphanumeric_variants_use_distinct_font_glyphs() {
        let font = MathFont::load().unwrap();
        assert_ne!(
            font.styled_glyph('R', FontVariant::DoubleStruck).unwrap(),
            font.glyph_id('R').unwrap()
        );
        assert_ne!(
            font.styled_glyph('L', FontVariant::Script).unwrap(),
            font.glyph_id('L').unwrap()
        );
    }

    #[test]
    fn vertical_stretch_selects_variant_then_bounded_assembly() {
        let font = MathFont::load().unwrap();
        let paren = font.glyph_id('(').unwrap();
        let normal = font.vertical_stretch(paren, 30.0, 16.0).unwrap().unwrap();
        assert!(matches!(normal, StretchGlyph::Single { .. }));

        let huge = font.vertical_stretch(paren, 500.0, 16.0).unwrap().unwrap();
        let StretchGlyph::Assembly { parts, advance } = huge else {
            panic!("huge delimiter should use assembly")
        };
        assert!(parts.len() > 1 && parts.len() <= MAX_ASSEMBLY_PARTS);
        assert!(parts.windows(2).all(|pair| pair[0].origin <= pair[1].origin));
        assert!(advance > 0.0);
    }
}
