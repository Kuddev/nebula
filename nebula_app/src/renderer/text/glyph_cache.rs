use std::collections::HashMap;

use ahash::RandomState;
use crossfont::{
    Error as RasterizerError, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph,
    Size, Slant, Style, Weight,
};
use log::{error, info};
use unicode_width::UnicodeWidthChar;

use crate::config::font::{Font, FontDescription};
use crate::config::ui_config::Delta;
use crate::gl::types::*;

use super::builtin_font;
use super::font_rasterizer::Rasterizer;

/// `LoadGlyph` allows for copying a rasterized glyph into graphics memory.
pub trait LoadGlyph {
    /// Load the rasterized glyph into GPU memory.
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph;

    /// Clear any state accumulated from previous loaded glyphs.
    ///
    /// This can, for instance, be used to reset the texture Atlas.
    fn clear(&mut self);
}

#[derive(Copy, Clone, Debug)]
pub struct Glyph {
    pub tex_id: GLuint,
    pub multicolor: bool,
    pub top: i16,
    pub left: i16,
    pub width: i16,
    pub height: i16,
    pub uv_bot: f32,
    pub uv_left: f32,
    pub uv_width: f32,
    pub uv_height: f32,
}

/// Naïve glyph cache.
///
/// Currently only keyed by `char`, and thus not possible to hold different
/// representations of the same code point.
pub struct GlyphCache {
    /// Cache of buffered glyphs.
    cache: HashMap<GlyphKey, Glyph, RandomState>,

    /// Rasterizer for loading new glyphs.
    rasterizer: Rasterizer,

    /// Regular font.
    pub font_key: FontKey,

    /// Bold font.
    pub bold_key: FontKey,

    /// Italic font.
    pub italic_key: FontKey,

    /// Bold italic font.
    pub bold_italic_key: FontKey,

    /// Embedded Maple face reserved for UI and terminal icon codepoints.
    pub symbol_key: FontKey,

    /// Font size.
    pub font_size: crossfont::Size,

    /// Font offset.
    font_offset: Delta<i8>,

    /// Glyph offset.
    glyph_offset: Delta<i8>,

    /// Font metrics.
    metrics: Metrics,

    /// Whether to use the built-in font for box drawing characters.
    builtin_box_drawing: bool,
}

impl GlyphCache {
    #[cfg(windows)]
    pub fn private_font_families(&self) -> Vec<String> {
        self.rasterizer.private_font_families()
    }

    #[cfg(windows)]
    pub fn add_private_font(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Vec<String>, crossfont::Error> {
        self.rasterizer.add_private_font(path)
    }

    #[cfg(windows)]
    pub fn refresh_private_fonts(&mut self) -> Vec<String> {
        self.rasterizer.refresh_private_fonts()
    }

    /// Check a system font family without changing the configured terminal font.
    pub fn font_family_available(rasterizer: &mut Rasterizer, family: &str, size: Size) -> bool {
        let description = FontDesc::new(
            family,
            Style::Description { slant: Slant::Normal, weight: Weight::Normal },
        );
        rasterizer.load_font(&description, size).is_ok()
    }

    pub fn new(mut rasterizer: Rasterizer, font: &Font) -> Result<GlyphCache, crossfont::Error> {
        let (regular, bold, italic, bold_italic) = Self::compute_font_keys(font, &mut rasterizer)?;
        #[cfg(windows)]
        let symbol_key = rasterizer.load_embedded_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            Slant::Normal,
            Weight::Normal,
            font.size(),
        )?;
        #[cfg(not(windows))]
        let symbol_key = regular;

        let metrics = GlyphCache::load_font_metrics(&mut rasterizer, font, regular)?;
        Ok(Self {
            cache: Default::default(),
            rasterizer,
            font_size: font.size(),
            font_key: regular,
            bold_key: bold,
            italic_key: italic,
            bold_italic_key: bold_italic,
            symbol_key,
            font_offset: font.offset,
            glyph_offset: font.glyph_offset,
            metrics,
            builtin_box_drawing: font.builtin_box_drawing,
        })
    }

    // Load font metrics and adjust for glyph offset.
    fn load_font_metrics(
        rasterizer: &mut Rasterizer,
        font: &Font,
        key: FontKey,
    ) -> Result<Metrics, crossfont::Error> {
        // Need to load at least one glyph for the face before calling metrics.
        // The glyph requested here ('m' at the time of writing) has no special
        // meaning.
        rasterizer.get_glyph(GlyphKey { font_key: key, character: 'm', size: font.size() })?;

        let mut metrics = rasterizer.metrics(key, font.size())?;
        metrics.strikeout_position += font.glyph_offset.y as f32;
        Ok(metrics)
    }

    fn load_glyphs_for_font<L: LoadGlyph>(&mut self, font: FontKey, loader: &mut L) {
        let size = self.font_size;

        // Cache all ascii characters.
        for i in 32u8..=126u8 {
            self.get(GlyphKey { font_key: font, character: i as char, size }, loader, true);
        }
    }

    /// Computes font keys for (Regular, Bold, Italic, Bold Italic).
    fn compute_font_keys(
        font: &Font,
        rasterizer: &mut Rasterizer,
    ) -> Result<(FontKey, FontKey, FontKey, FontKey), crossfont::Error> {
        let size = font.size();

        // Load regular font.
        let regular_desc = Self::make_desc(font.normal(), Slant::Normal, Weight::Normal);

        let regular = Self::load_regular_font(
            rasterizer,
            &regular_desc,
            &font.normal().family,
            Slant::Normal,
            Weight::Normal,
            size,
        )?;

        // Helper to load a description if it is not the `regular_desc`.
        let mut load_or_regular = |desc: FontDesc, family: &str, slant: Slant, weight: Weight| {
            if desc == regular_desc {
                regular
            } else {
                Self::load_regular_font(rasterizer, &desc, family, slant, weight, size)
                    .unwrap_or(regular)
            }
        };

        // Load bold font.
        let bold_font = font.bold();
        let bold_desc = Self::make_desc(&bold_font, Slant::Normal, Weight::Bold);
        let bold = load_or_regular(bold_desc, &bold_font.family, Slant::Normal, Weight::Bold);

        // Load italic font.
        let italic_font = font.italic();
        let italic_desc = Self::make_desc(&italic_font, Slant::Italic, Weight::Normal);
        let italic =
            load_or_regular(italic_desc, &italic_font.family, Slant::Italic, Weight::Normal);

        // Load bold italic font.
        let bold_italic_font = font.bold_italic();
        let bold_italic_desc = Self::make_desc(&bold_italic_font, Slant::Italic, Weight::Bold);
        let bold_italic = load_or_regular(
            bold_italic_desc,
            &bold_italic_font.family,
            Slant::Italic,
            Weight::Bold,
        );

        Ok((regular, bold, italic, bold_italic))
    }

    fn load_regular_font(
        rasterizer: &mut Rasterizer,
        description: &FontDesc,
        family: &str,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<FontKey, crossfont::Error> {
        #[cfg(windows)]
        let preferred = rasterizer.load_preferred_font(description, family, slant, weight, size);
        #[cfg(not(windows))]
        let preferred = rasterizer.load_font(description, size);

        match preferred {
            Ok(font) => Ok(font),
            Err(err) => {
                error!("{err}");

                #[cfg(windows)]
                let fallback_desc = FontDesc::new(
                    "Cascadia Code",
                    Style::Description { slant: Slant::Normal, weight: Weight::Normal },
                );
                #[cfg(not(windows))]
                let fallback_desc =
                    Self::make_desc(Font::default().normal(), Slant::Normal, Weight::Normal);

                rasterizer.load_font(&fallback_desc, size)
            },
        }
    }

    fn make_desc(desc: &FontDescription, slant: Slant, weight: Weight) -> FontDesc {
        let style = if let Some(ref spec) = desc.style {
            Style::Specific(spec.to_owned())
        } else {
            Style::Description { slant, weight }
        };
        FontDesc::new(desc.family.clone(), style)
    }

    #[inline]
    pub fn font_key_for(&self, character: char, text_key: FontKey) -> FontKey {
        if is_private_use(character) { self.symbol_key } else { text_key }
    }

    /// Get a glyph from the font.
    ///
    /// If the glyph has never been loaded before, it will be rasterized and inserted into the
    /// cache.
    ///
    /// # Errors
    ///
    /// This will fail when the glyph could not be rasterized. Usually this is due to the glyph
    /// not being present in any font.
    pub fn get<L>(&mut self, glyph_key: GlyphKey, loader: &mut L, show_missing: bool) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        // Try to load glyph from cache.
        if let Some(glyph) = self.cache.get(&glyph_key) {
            return *glyph;
        };

        // Rasterize the glyph using the built-in font for special characters or the user's font
        // for everything else.
        let rasterized = self
            .builtin_box_drawing
            .then(|| {
                builtin_font::builtin_glyph(
                    glyph_key.character,
                    &self.metrics,
                    &self.font_offset,
                    &self.glyph_offset,
                )
            })
            .flatten()
            .map_or_else(|| self.rasterizer.get_glyph(glyph_key), Ok);

        let glyph = match rasterized {
            Ok(rasterized) => self.load_glyph(loader, rasterized),
            // Load fallback glyph.
            Err(RasterizerError::MissingGlyph(rasterized)) if show_missing => {
                // Use `\0` as "missing" glyph to cache it only once.
                let missing_key = GlyphKey { character: '\0', ..glyph_key };
                if let Some(glyph) = self.cache.get(&missing_key) {
                    *glyph
                } else {
                    // If no missing glyph was loaded yet, insert it as `\0`.
                    let glyph = self.load_glyph(loader, rasterized);
                    self.cache.insert(missing_key, glyph);

                    glyph
                }
            },
            Err(_) => self.load_glyph(loader, Default::default()),
        };

        // Cache rasterized glyph.
        *self.cache.entry(glyph_key).or_insert(glyph)
    }

    /// Load glyph into the atlas.
    ///
    /// This will apply all transforms defined for the glyph cache to the rasterized glyph before
    pub fn load_glyph<L>(&self, loader: &mut L, mut glyph: RasterizedGlyph) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        glyph.left += i32::from(self.glyph_offset.x);
        glyph.top += i32::from(self.glyph_offset.y);
        glyph.top -= self.metrics.descent as i32;

        // The metrics of zero-width characters are based on rendering
        // the character after the current cell, with the anchor at the
        // right side of the preceding character. Since we render the
        // zero-width characters inside the preceding character, the
        // anchor has been moved to the right by one cell.
        if glyph.character.width() == Some(0) {
            glyph.left += self.metrics.average_advance as i32;
        }

        // Add glyph to cache.
        loader.load_glyph(&glyph)
    }

    /// Reset currently cached data in both GL and the registry to default state.
    pub fn reset_glyph_cache<L: LoadGlyph>(&mut self, loader: &mut L) {
        loader.clear();
        self.cache = Default::default();

        self.load_common_glyphs(loader);
    }

    /// Update the inner font size.
    ///
    /// NOTE: To reload the renderers's fonts [`Self::reset_glyph_cache`] should be called
    /// afterwards.
    pub fn update_font_size(&mut self, font: &Font) -> Result<(), crossfont::Error> {
        // Update dpi scaling.
        self.font_offset = font.offset;
        self.glyph_offset = font.glyph_offset;

        // Recompute font keys.
        let (regular, bold, italic, bold_italic) =
            Self::compute_font_keys(font, &mut self.rasterizer)?;
        #[cfg(windows)]
        let symbol_key = self.rasterizer.load_embedded_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            Slant::Normal,
            Weight::Normal,
            font.size(),
        )?;
        #[cfg(not(windows))]
        let symbol_key = regular;

        let metrics = GlyphCache::load_font_metrics(&mut self.rasterizer, font, regular)?;

        info!("Font size changed to {:?} px", font.size().as_px());

        self.font_size = font.size();
        self.font_key = regular;
        self.bold_key = bold;
        self.italic_key = italic;
        self.bold_italic_key = bold_italic;
        self.symbol_key = symbol_key;
        self.metrics = metrics;
        self.builtin_box_drawing = font.builtin_box_drawing;

        Ok(())
    }

    pub fn font_metrics(&self) -> crossfont::Metrics {
        self.metrics
    }

    /// Prefetch glyphs that are almost guaranteed to be loaded anyways.
    pub fn load_common_glyphs<L: LoadGlyph>(&mut self, loader: &mut L) {
        self.load_glyphs_for_font(self.font_key, loader);
        self.load_glyphs_for_font(self.bold_key, loader);
        self.load_glyphs_for_font(self.italic_key, loader);
        self.load_glyphs_for_font(self.bold_italic_key, loader);
    }
}

#[inline]
fn is_private_use(character: char) -> bool {
    matches!(character as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn missing_font_family_is_not_reported_as_available() {
        let mut rasterizer = Rasterizer::new().expect("DirectWrite rasterizer");
        assert!(!GlyphCache::font_family_available(
            &mut rasterizer,
            "Nebula Missing Font Probe 8C0A651D",
            Size::new(11.25),
        ));
    }

    #[test]
    fn private_use_symbols_always_use_the_embedded_maple_key() {
        let rasterizer = Rasterizer::new().expect("DirectWrite rasterizer");
        let font = Font::default().with_family("Consolas");
        let cache = GlyphCache::new(rasterizer, &font).expect("glyph cache");

        assert_ne!(cache.font_key, cache.symbol_key);
        assert_eq!(cache.font_key_for('A', cache.font_key), cache.font_key);
        assert_eq!(cache.font_key_for('\u{ea83}', cache.font_key), cache.symbol_key);
        assert_eq!(cache.font_key_for('\u{f0000}', cache.font_key), cache.symbol_key);
    }
}
