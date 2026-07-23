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
    layout_formula(&formula, pixel_size, pixels_per_point, limits)
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

    if normalized.len() > limits.max_source_bytes {
        return Err(MathError::new(MathErrorKind::SourceTooLong, limits.max_source_bytes));
    }
    Ok(normalized)
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
}
