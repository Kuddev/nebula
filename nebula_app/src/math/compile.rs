//! Shared high-level math compilation used by every presentation surface.
//!
//! Markdown and terminal code may locate formulas differently, but neither is
//! allowed to normalize, parse or lay them out independently. Keeping that
//! boundary here prevents source-specific matrix/cases fixes from diverging.

use std::borrow::Cow;

use super::layout::{MathLayout, layout_formula};
use super::parser::parse_formula;
use super::{MathError, MathErrorKind, MathLimits};

const MAX_ENTITY_DECODE_PASSES: usize = 4;

/// Compile normalized TeX source into backend-independent drawing operations.
pub(crate) fn compile_formula(
    source: &str,
    display: bool,
    pixel_size: f32,
    pixels_per_point: f32,
    limits: MathLimits,
) -> Result<MathLayout, MathError> {
    let formula = parse_formula(source, display, limits)?;
    // 光学补偿在这一个入口做：调用方传名义字号（终端/正文字号），缓存键也用
    // 名义字号，返回的 metrics 已是补偿后的真实几何，fit 逻辑自然吸收。
    layout_formula(&formula, pixel_size * super::OPTICAL_SCALE, pixels_per_point, limits)
}

/// Normalize transport-level text without inferring mathematical structure.
///
/// HTML named/numeric references are decoded generically, CRLF is normalized,
/// and Unicode horizontal whitespace becomes an ordinary TeX space. A row
/// separator is repaired only when transport left an otherwise invalid single
/// backslash at a physical line ending; bare newlines or entities never invent
/// mathematical structure by themselves.
pub(super) fn normalize_formula_source<'a>(
    source: &'a str,
    limits: MathLimits,
) -> Result<Cow<'a, str>, MathError> {
    if source.len() > limits.max_source_bytes {
        return Err(MathError::new(MathErrorKind::SourceTooLong, limits.max_source_bytes));
    }

    let mut normalized = Cow::Borrowed(source);
    // 终端输出可能先经过 Markdown/HTML，再经过终端协议；有限次解码既覆盖
    // 真实的双重转义，也避免恶意深层实体造成无界重复扫描。
    for _ in 0..MAX_ENTITY_DECODE_PASSES {
        let Some(decoded) = decode_entities_once(normalized.as_ref()) else {
            break;
        };
        if decoded.len() > limits.max_source_bytes {
            return Err(MathError::new(MathErrorKind::SourceTooLong, limits.max_source_bytes));
        }
        normalized = Cow::Owned(decoded);
    }

    if let Some(text) = normalize_line_endings_and_spaces(normalized.as_ref()) {
        normalized = Cow::Owned(text);
    }
    if let Some(text) = repair_collapsed_row_breaks(normalized.as_ref()) {
        normalized = Cow::Owned(text);
    }
    for _ in 0..MAX_ENTITY_DECODE_PASSES {
        let Some(text) = brace_unbraced_fraction_arguments(normalized.as_ref()) else {
            break;
        };
        normalized = Cow::Owned(text);
    }

    if normalized.len() > limits.max_source_bytes {
        return Err(MathError::new(MathErrorKind::SourceTooLong, limits.max_source_bytes));
    }
    Ok(normalized)
}

/// `pulldown-latex` requires braces around `\frac` arguments, while TeX also
/// accepts a single token (`\frac13`, `\frac\pi2`). Canonicalize that standard
/// shorthand before parsing without changing already-grouped arguments.
fn brace_unbraced_fraction_arguments(source: &str) -> Option<String> {
    let mut normalized = String::with_capacity(source.len());
    let mut copied_until = 0usize;
    let mut offset = 0usize;
    let mut changed = false;

    while offset < source.len() {
        if source.as_bytes()[offset] != b'\\' {
            offset += source[offset..].chars().next()?.len_utf8();
            continue;
        }

        let command_start = offset + 1;
        let mut command_end = command_start;
        while source.as_bytes().get(command_end).is_some_and(u8::is_ascii_alphabetic) {
            command_end += 1;
        }
        if !matches!(&source[command_start..command_end], "frac" | "dfrac" | "tfrac") {
            offset = command_end.max(offset + 1);
            continue;
        }

        let Some((first, second)) = two_tex_arguments(source, command_end) else {
            offset = command_end;
            continue;
        };
        if first.2 && second.2 {
            // Keep scanning inside grouped arguments: they may contain another
            // shorthand fraction even though this outer command is canonical.
            offset = command_end;
            continue;
        }

        normalized.push_str(&source[copied_until..command_end]);
        let mut cursor = command_end;
        for (start, end, grouped) in [first, second] {
            normalized.push_str(&source[cursor..start]);
            if grouped {
                normalized.push_str(&source[start..end]);
            } else {
                normalized.push('{');
                normalized.push_str(&source[start..end]);
                normalized.push('}');
            }
            cursor = end;
        }
        copied_until = cursor;
        offset = cursor;
        changed = true;
    }

    changed.then(|| {
        normalized.push_str(&source[copied_until..]);
        normalized
    })
}

fn two_tex_arguments(
    source: &str,
    command_end: usize,
) -> Option<((usize, usize, bool), (usize, usize, bool))> {
    let first = tex_argument(source, command_end)?;
    let second = tex_argument(source, first.1)?;
    Some((first, second))
}

fn tex_argument(source: &str, mut offset: usize) -> Option<(usize, usize, bool)> {
    while let Some(character) = source[offset..].chars().next() {
        if !character.is_whitespace() {
            break;
        }
        offset += character.len_utf8();
    }
    let first = source[offset..].chars().next()?;
    if first == '{' {
        return grouped_argument_end(source, offset).map(|end| (offset, end, true));
    }
    if first != '\\' {
        return Some((offset, offset + first.len_utf8(), false));
    }

    let mut end = offset + 1;
    let command_start = end;
    while source.as_bytes().get(end).is_some_and(u8::is_ascii_alphabetic) {
        end += 1;
    }
    if end == command_start {
        end += source[end..].chars().next()?.len_utf8();
    }
    Some((offset, end, false))
}

fn grouped_argument_end(source: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (relative, character) in source[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + relative + character.len_utf8());
                }
            },
            _ => {},
        }
    }
    None
}

fn decode_entities_once(source: &str) -> Option<String> {
    match html_escape::decode_html_entities(source) {
        Cow::Borrowed(_) => None,
        Cow::Owned(decoded) => Some(decoded),
    }
}

fn normalize_line_endings_and_spaces(source: &str) -> Option<String> {
    let needs_normalization = source.contains('\r')
        || source
            .chars()
            .any(|character| character != '\n' && character != '\r' && character.is_whitespace());
    if !needs_normalization {
        return None;
    }

    let mut normalized = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push('\n');
            },
            '\n' => normalized.push('\n'),
            character if character.is_whitespace() => normalized.push(' '),
            character => normalized.push(character),
        }
    }
    Some(normalized)
}

fn repair_collapsed_row_breaks(source: &str) -> Option<String> {
    let mut line_start = 0usize;
    let repairs: Vec<_> = source
        .split_inclusive('\n')
        .filter_map(|line| {
            let offset = line_start;
            line_start += line.len();
            line.strip_suffix('\n')
                .and_then(collapsed_row_break_offset)
                .map(|offset_in_line| offset + offset_in_line)
        })
        .collect();
    if repairs.is_empty() {
        return None;
    }

    let mut repaired = String::with_capacity(source.len() + repairs.len());
    let mut start = 0usize;
    for offset in repairs {
        repaired.push_str(&source[start..offset]);
        repaired.push('\\');
        start = offset;
    }
    repaired.push_str(&source[start..]);
    Some(repaired)
}

fn collapsed_row_break_offset(line: &str) -> Option<usize> {
    let content = line.trim_end_matches(' ');
    if content.ends_with('\\') {
        let slash_start = content.trim_end_matches('\\').len();
        return (content.len() - slash_start == 1).then_some(slash_start);
    }

    let spacing_start = content.rfind('[')?;
    let spacing = content.get(spacing_start + 1..)?.strip_suffix(']')?;
    if !is_tex_dimension(spacing) {
        return None;
    }
    let slash_start = content[..spacing_start].trim_end_matches('\\').len();
    (spacing_start - slash_start == 1).then_some(slash_start)
}

fn is_tex_dimension(source: &str) -> bool {
    let source = source.trim();
    let source = source.strip_prefix(['+', '-']).unwrap_or(source);
    let number_end = source
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(source.len());
    let (number, unit) = source.split_at(number_end);
    number.chars().any(|character| character.is_ascii_digit())
        && number.chars().filter(|character| *character == '.').count() <= 1
        && !unit.trim().is_empty()
        && unit.trim().chars().all(|character| character.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::{compile_formula, normalize_formula_source};
    use crate::math::DEFAULT_LIMITS;

    const QUADRATIC: &str = r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}";
    const CASES: &str = r"f(x)=\begin{cases}x^2,&x\geq 0\\-x,&x<0\end{cases}";
    const MATRIX: &str = r"A=\begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix}";

    #[test]
    fn screenshot_formulas_share_one_successful_compile_path() {
        for source in [QUADRATIC, CASES, MATRIX] {
            let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
                .unwrap_or_else(|error| panic!("screenshot formula failed: {source:?}: {error:?}"));
            assert!(layout.metrics.width.is_finite() && layout.metrics.width > 0.0);
            assert!(layout.metrics.height.is_finite() && layout.metrics.height > 0.0);
            assert!(!layout.glyphs.is_empty());
        }
    }

    #[test]
    fn transport_entities_are_decoded_without_inventing_matrix_rows() {
        let encoded = r"A=\begin{pmatrix}1&amp;2&amp;3\\&nbsp;4&amp;5&amp;6\\&#160;7&amp;8&amp;9\end{pmatrix}";
        let normalized = normalize_formula_source(encoded, DEFAULT_LIMITS).unwrap();

        assert_eq!(normalized, r"A=\begin{pmatrix}1&2&3\\ 4&5&6\\ 7&8&9\end{pmatrix}");
        let layout = compile_formula(encoded, true, 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        assert!(!layout.glyphs.is_empty());
        assert!(layout.text.iter().all(|operation| operation.character != '&'));
    }

    #[test]
    fn an_entity_cannot_replace_a_missing_tex_row_separator() {
        let damaged = r"\begin{pmatrix}1&2&3&nbsp;4&5&6\end{pmatrix}";
        let normalized = normalize_formula_source(damaged, DEFAULT_LIMITS).unwrap();

        assert_eq!(normalized, r"\begin{pmatrix}1&2&3 4&5&6\end{pmatrix}");
        assert!(!normalized.contains(r"\\"));
    }

    #[test]
    fn transport_repairs_only_row_breaks_with_a_remaining_backslash_signal() {
        let damaged = concat!(
            "\\begin{aligned}\n",
            "a&=b \\[6pt]\n",
            "c&=d \\\n",
            "e&=f\n",
            "\\end{aligned}",
        );
        let normalized = normalize_formula_source(damaged, DEFAULT_LIMITS).unwrap();

        assert_eq!(
            normalized,
            concat!(
                "\\begin{aligned}\n",
                "a&=b \\\\[6pt]\n",
                "c&=d \\\\\n",
                "e&=f\n",
                "\\end{aligned}",
            )
        );
        compile_formula(damaged, true, 18.0, 1.0, DEFAULT_LIMITS).unwrap();
    }

    #[test]
    fn nested_transport_entities_decode_before_row_break_recovery() {
        let damaged = concat!(
            "A=\\begin{pmatrix}\n",
            "1&2&3\\&amp;nbsp;\n",
            "4&5&6\\&amp;#160;\n",
            "7&8&9\n",
            "\\end{pmatrix}",
        );
        let normalized = normalize_formula_source(damaged, DEFAULT_LIMITS).unwrap();

        assert!(!normalized.contains("nbsp"));
        assert!(!normalized.contains("&#160;"));
        assert!(normalized.contains("3\\\\ \n4"));
        assert!(normalized.contains("6\\\\ \n7"));

        let layout = compile_formula(damaged, true, 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        assert!(layout.text.iter().all(|operation| operation.character != '&'));
    }

    #[test]
    fn unbraced_fraction_arguments_are_canonicalized_before_parsing() {
        let source = r"\displaystyle \int_0^1 x^2\,dx=\frac13+\frac\pi2+\dfrac1{n}";
        let normalized = normalize_formula_source(source, DEFAULT_LIMITS).unwrap();

        assert_eq!(
            normalized,
            r"\displaystyle \int_0^1 x^2\,dx=\frac{1}{3}+\frac{\pi}{2}+\dfrac{1}{n}"
        );
        compile_formula(source, false, 18.0, 1.0, DEFAULT_LIMITS).unwrap();

        let nested = normalize_formula_source(r"\frac{\frac12}{3}", DEFAULT_LIMITS).unwrap();
        assert_eq!(nested, r"\frac{\frac{1}{2}}{3}");
    }

    /// 光学补偿必须真正到达 metrics：Latin Modern 的 x-height 是 0.431 em，
    /// 名义 20px 下补偿后应为 0.431 × 20 × [`super::super::OPTICAL_SCALE`]。
    /// 补偿只在 compile_formula 单点生效；缓存键、fit、draw 都持名义字号。
    #[test]
    fn optical_scale_reaches_layout_metrics() {
        let layout = compile_formula("x", false, 20.0, 1.0, DEFAULT_LIMITS).expect("compile");
        let expected = 0.431 * 20.0 * crate::math::OPTICAL_SCALE;
        assert!(
            (layout.metrics.height - expected).abs() < 0.5,
            "x-height {} should be ≈ {expected}",
            layout.metrics.height,
        );
    }

    /// 每个字形都必须能落到位图上：合成器（`math_view::compose_image`）遇到
    /// 一个失败字形就整张放弃，而那时源格已经被覆盖掩码藏掉，屏幕上只剩空洞。
    /// `\text{ToT Search}` 里那个空格就是这么把一整条流水线公式吃掉的。
    #[test]
    fn every_glyph_of_a_text_annotated_formula_rasterizes() {
        let source = concat!(
            r"(F, D_{\text{few}}) \xrightarrow{\text{Prompting}} \boxed{C} ",
            r"\xrightarrow{\text{MultiAgent}}",
            "\n",
            r"  \boxed{(F_{\text{ref}}, F_{\text{neg}})} \xrightarrow{\text{ToT Search}}",
        );
        let rasterizer = crate::math::rasterizer::MathGlyphRasterizer::new().expect("math font");
        for pixel_size in [20.0_f32, 14.0, 9.0] {
            let layout = compile_formula(source, true, pixel_size, 1.0, DEFAULT_LIMITS)
                .unwrap_or_else(|error| panic!("compile failed at {pixel_size}: {error:?}"));
            assert!(!layout.glyphs.is_empty());
            for scale in [1.0_f32, 1.5, 2.0] {
                for op in &layout.glyphs {
                    rasterizer.rasterize(op.glyph_id, op.pixel_size * scale).unwrap_or_else(
                        |error| {
                            panic!(
                                "glyph {} at {:.1}px (scale {scale}) failed: {error:?}",
                                op.glyph_id, op.pixel_size
                            )
                        },
                    );
                }
            }
        }
    }
}
