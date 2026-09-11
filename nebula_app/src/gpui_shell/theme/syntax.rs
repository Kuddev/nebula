//! Adapt reviewed theme colors to code highlighting once when the theme changes.
use std::sync::Arc;

use gpui_component::{Theme, highlighter::SyntaxColors};
use nebula_settings::ReviewedPalette;

use crate::display::color::Rgb;
use crate::display::terminal_color::ensure_contrast;

pub(super) fn apply(theme: &mut Theme, colors: ReviewedPalette) {
    let background = colors.code_background();
    let mut highlighted = (*theme.highlight_theme).clone();
    let color = |[r, g, b]: [u8; 3]| super::to_hsla(r, g, b);
    highlighted.style.editor_background = Some(color(background));
    highlighted.style.editor_foreground = Some(color(colors.foreground));
    highlighted.style.editor_line_number = Some(color(colors.muted));
    let mut syntax =
        serde_json::to_value(&highlighted.style.syntax).expect("syntax theme is serializable");
    if let Some(entries) = syntax.as_object_mut() {
        for (name, style) in entries {
            let source = match name.as_str() {
                "comment" | "comment_doc" | "comment.doc" | "hint" => colors.muted,
                "keyword" | "boolean" | "enum" | "preproc" => colors.purple,
                "function" | "constructor" | "link_text" | "link_uri" => colors.blue,
                "number" | "constant" => colors.yellow,
                "type" | "attribute" | "tag" => colors.cyan,
                name if name.starts_with("string") => colors.green,
                _ => colors.foreground,
            };
            let rgb = |[r, g, b]: [u8; 3]| Rgb::new(r, g, b);
            let resolved = ensure_contrast(
                rgb(source),
                rgb(background),
                rgb(colors.foreground),
                rgb(colors.background),
                4.5,
            );
            if style.is_null() {
                *style = serde_json::json!({});
            }
            if let Some(style) = style.as_object_mut() {
                style.insert(
                    "color".to_owned(),
                    serde_json::json!(format!(
                        "#{:02x}{:02x}{:02x}",
                        resolved.r, resolved.g, resolved.b,
                    )),
                );
            }
        }
    }
    highlighted.style.syntax = serde_json::from_value::<SyntaxColors>(syntax)
        .expect("reviewed syntax colors keep the schema");
    highlighted.name = format!("Pebrel {}", theme.is_dark());
    theme.highlight_theme = Arc::new(highlighted);
}
