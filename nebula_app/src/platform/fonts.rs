use std::path::Path;
use std::sync::Arc;

use crate::font_install::SystemFontFamily;

pub fn system_families(text: Arc<gpui::TextSystem>) -> Vec<SystemFontFamily> {
    #[cfg(windows)]
    {
        let _ = text;
        crate::font_install::enumerate_system_font_families()
    }
    #[cfg(not(windows))]
    {
        text.all_font_names()
            .into_iter()
            .filter(|name| !name.starts_with('.'))
            .map(|name| {
                let font = text.resolve_font(&gpui::font(name.clone()));
                let widths: Option<Vec<f32>> = (' '..='~')
                    .map(|character| {
                        text.advance(font, gpui::px(16.0), character)
                            .ok()
                            .map(|size| f32::from(size.width))
                    })
                    .collect();
                let monospaced = widths.is_some_and(|widths| {
                    widths[0] > 0.0 && widths.iter().all(|width| (width - widths[0]).abs() < 0.01)
                });
                SystemFontFamily { name, monospaced }
            })
            .collect()
    }
}

pub fn file_families(path: &Path) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        crate::font_install::probe_font_file_families(path)
    }
    #[cfg(not(windows))]
    {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        let mut families = Vec::new();
        for index in 0..count {
            let face = ttf_parser::Face::parse(&bytes, index).map_err(|error| error.to_string())?;
            for name in face.names() {
                if matches!(
                    name.name_id,
                    ttf_parser::name_id::FAMILY | ttf_parser::name_id::TYPOGRAPHIC_FAMILY
                ) {
                    if let Some(name) = name.to_string() {
                        families.push(name);
                    }
                }
            }
        }
        families.sort();
        families.dedup();
        if families.is_empty() { Err("No font family names in file".into()) } else { Ok(families) }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_font_can_be_identified_without_installing_it() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("bundled.ttf");
        std::fs::write(&font, crate::font_install::REQUIRED_FONT_BYTES).unwrap();
        let families = super::file_families(&font).unwrap();
        assert!(families.iter().any(|name| name == crate::font_install::REQUIRED_FONT_FAMILY));
    }
}
