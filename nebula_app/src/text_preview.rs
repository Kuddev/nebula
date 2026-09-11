//! Display-only text bounds. Original document/input text stays authoritative.

use std::borrow::Cow;

pub(crate) const MAX_SOURCE_PREVIEW_LINES: usize = 32;
const MAX_SOURCE_PREVIEW_BYTES: usize = 4096;

pub(crate) fn multiline_source(source: &str) -> Cow<'_, str> {
    let normalized = if source.contains('\r') {
        Cow::Owned(source.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(source)
    };
    let text = normalized.as_ref();
    let mut end = text.floor_char_boundary(text.len().min(MAX_SOURCE_PREVIEW_BYTES));
    if text.match_indices('\n').nth(MAX_SOURCE_PREVIEW_LINES - 1).is_some() {
        end = end.min(text.match_indices('\n').nth(MAX_SOURCE_PREVIEW_LINES - 2).unwrap().0);
    }
    if end < text.len() {
        Cow::Owned(format!("{}\n…", text[..end].trim_end_matches('\n')))
    } else {
        normalized
    }
}

/// A single-line surface such as an IME bubble or link tooltip must never
/// forward a line terminator to the font system's single-line API.
pub(crate) fn single_line_label(source: &str) -> Cow<'_, str> {
    if source.contains(['\r', '\n']) {
        Cow::Owned(source.replace(['\r', '\n'], " "))
    } else {
        Cow::Borrowed(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_formula_keeps_rows_without_rewriting_the_source() {
        let source = "A=\r\n\\begin{pmatrix}\r\n1&2\\\\\r\n3&4\r\n\\end{pmatrix}";
        let preview = multiline_source(source);
        assert_eq!(preview.lines().count(), 5);
        assert!(preview.contains("1&2\\\\\n3&4"));
        assert!(source.contains("\r\n"));
        assert!(matches!(multiline_source("x=1\ny=2"), Cow::Borrowed(_)));
    }

    #[test]
    fn pending_preview_is_bounded_in_lines_and_unicode_bytes() {
        let source = "公式\n".repeat(1000);
        let preview = multiline_source(&source);
        assert!(preview.lines().count() <= MAX_SOURCE_PREVIEW_LINES);
        assert!(preview.ends_with('…'));
        let source = "α".repeat(4096);
        let preview = multiline_source(&source);
        assert!(preview.len() <= MAX_SOURCE_PREVIEW_BYTES + 4);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn single_line_bubbles_preserve_committed_input_separately() {
        let source = "第一行\r\nsecond line\n";
        let label = single_line_label(source);
        assert!(!label.contains(['\r', '\n']));
        assert_eq!(label, "第一行  second line ");
        assert!(source.contains('\n'));
        assert!(matches!(single_line_label("普通输入"), Cow::Borrowed(_)));
    }
}
