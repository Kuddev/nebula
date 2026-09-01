//! Display-width-aware text and local-link helpers shared by both UI shells.

use unicode_width::UnicodeWidthChar;

pub(crate) fn strip_file_scheme(uri: &str) -> String {
    match uri.strip_prefix("file:///").or_else(|| uri.strip_prefix("file://")) {
        Some(rest) => percent_decode_lossy(rest),
        None => uri.to_owned(),
    }
}

pub(crate) fn percent_decode_lossy(value: &str) -> String {
    if !value.contains('%') {
        return value.to_owned();
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let decoded = (bytes[index] == b'%' && index + 2 < bytes.len())
            .then(|| {
                let high = (bytes[index + 1] as char).to_digit(16)?;
                let low = (bytes[index + 2] as char).to_digit(16)?;
                Some((high * 16 + low) as u8)
            })
            .flatten();
        match decoded {
            Some(byte) => {
                output.push(byte);
                index += 3;
            },
            None => {
                output.push(bytes[index]);
                index += 1;
            },
        }
    }
    String::from_utf8(output).unwrap_or_else(|_| value.to_owned())
}

pub(crate) fn fit_tail(value: &str, budget: usize) -> String {
    let width = |character: char| character.width().unwrap_or(0);
    let total: usize = value.chars().map(width).sum();
    if total <= budget {
        return value.to_owned();
    }
    if budget == 0 {
        return String::new();
    }

    let room = budget.saturating_sub(1);
    let mut kept = std::collections::VecDeque::new();
    let mut used = 0;
    for character in value.chars().rev() {
        let character_width = width(character);
        if used + character_width > room {
            break;
        }
        used += character_width;
        kept.push_front(character);
    }
    let mut output = String::with_capacity(kept.len() + 1);
    output.push('\u{2026}');
    output.extend(kept);
    output
}

pub(crate) fn truncate_tab_label(label: &str, max_columns: usize) -> String {
    let total: usize = label.chars().map(|character| character.width().unwrap_or(0)).sum();
    if total <= max_columns {
        return label.to_owned();
    }
    if max_columns <= 1 {
        return "\u{2026}".to_owned();
    }

    let budget = max_columns - 1;
    let mut used = 0;
    let mut text = String::new();
    for character in label.chars() {
        let character_width = character.width().unwrap_or(0);
        if used + character_width > budget {
            break;
        }
        used += character_width;
        text.push(character);
    }
    text.push('\u{2026}');
    text
}

#[cfg(test)]
mod tests {
    use super::{fit_tail, strip_file_scheme, truncate_tab_label};

    #[test]
    fn local_uri_is_decoded_before_tail_fitting() {
        let decoded = strip_file_scheme("file:///D:/%E6%98%9F%E9%9B%B2/read%20me.txt");
        assert_eq!(decoded, "D:/\u{661f}\u{96f2}/read me.txt");
        assert_eq!(fit_tail(&decoded, 13), "\u{2026}/read me.txt");
    }

    #[test]
    fn tab_label_respects_wide_character_columns() {
        assert_eq!(truncate_tab_label("ab\u{661f}\u{96f2}cd", 6), "ab\u{661f}\u{2026}");
    }
}
