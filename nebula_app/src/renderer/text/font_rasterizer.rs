#[cfg(windows)]
use std::collections::HashMap;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use crossfont::{
    BitmapBuffer, Error, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph, Size,
    Slant, Style, Weight,
};
#[cfg(windows)]
use dwrote::{
    CustomFontCollectionLoaderImpl, DWRITE_GLYPH_RUN, FontCollection, FontFace, FontFile,
    FontStretch, FontStyle, FontWeight, GlyphOffset, GlyphRunAnalysis,
};

#[cfg(windows)]
pub(super) static EMBEDDED_FONT_BYTES: &[u8] =
    include_bytes!("../../../../assets/fonts/MapleMonoNormal-NF-CN-Regular.ttf");

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FontSource {
    System,
    Embedded,
}

#[cfg(windows)]
pub(super) fn preferred_font_source(
    system_available: bool,
    embedded_available: bool,
) -> Option<FontSource> {
    if system_available {
        Some(FontSource::System)
    } else if embedded_available {
        Some(FontSource::Embedded)
    } else {
        None
    }
}

#[cfg(windows)]
pub(super) struct StaticFontData(&'static [u8]);

#[cfg(windows)]
impl StaticFontData {
    pub(super) fn new() -> Self {
        Self(EMBEDDED_FONT_BYTES)
    }
}

#[cfg(windows)]
impl AsRef<[u8]> for StaticFontData {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

#[cfg(windows)]
pub(crate) struct Rasterizer {
    system: crossfont::Rasterizer,
    embedded_collection: FontCollection,
    embedded_fonts: HashMap<FontKey, EmbeddedFont>,
    embedded_keys: HashMap<FontDesc, FontKey>,
}

#[cfg(windows)]
struct EmbeddedFont {
    face: FontFace,
    fallback_key: Option<FontKey>,
}

#[cfg(windows)]
impl Rasterizer {
    pub(crate) fn load_preferred_font(
        &mut self,
        description: &FontDesc,
        family: &str,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<FontKey, Error> {
        let system = self.system.load_font(description, size);
        if preferred_font_source(system.is_ok(), true) == Some(FontSource::System) {
            return system;
        }

        let embedded = self.load_embedded_font(family, slant, weight, size);
        match preferred_font_source(false, embedded.is_ok()) {
            Some(FontSource::Embedded) => embedded,
            _ => system,
        }
    }

    pub(super) fn load_embedded_font(
        &mut self,
        family: &str,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<FontKey, Error> {
        let description = FontDesc::new(family, Style::Description { slant, weight });
        if let Some(key) = self.embedded_keys.get(&description) {
            return Ok(*key);
        }

        let family = self
            .embedded_collection
            .font_family_by_name(family)
            .map_err(directwrite_error)?
            .ok_or_else(|| Error::FontNotFound(description.clone()))?;
        let font = family
            .first_matching_font(font_weight(weight), FontStretch::Normal, font_style(slant))
            .map_err(directwrite_error)?;
        let fallback_key = ["Cascadia Code", "Consolas"].into_iter().find_map(|name| {
            let fallback = FontDesc::new(
                name,
                Style::Description { slant: Slant::Normal, weight: Weight::Normal },
            );
            self.system.load_font(&fallback, size).ok()
        });
        let key = FontKey::next();
        self.embedded_fonts
            .insert(key, EmbeddedFont { face: font.create_font_face(), fallback_key });
        self.embedded_keys.insert(description, key);
        Ok(key)
    }

    pub(super) fn is_embedded_font(&self, key: FontKey) -> bool {
        self.embedded_fonts.contains_key(&key)
    }

    fn embedded_metrics(font: &EmbeddedFont, size: Size) -> Result<Metrics, Error> {
        let vertical = font.face.metrics().metrics0();
        let scale = size.as_px() / f32::from(vertical.designUnitsPerEm);
        let glyph_index = font
            .face
            .glyph_indices(&['!' as u32])
            .map_err(directwrite_error)?
            .first()
            .copied()
            .unwrap_or(0);
        let horizontal = font
            .face
            .design_glyph_metrics(&[glyph_index], false)
            .map_err(directwrite_error)?
            .into_iter()
            .next()
            .ok_or(Error::MetricsNotFound)?;

        let ascent = f32::from(vertical.ascent) * scale;
        let descent = -f32::from(vertical.descent) * scale;
        let line_gap = f32::from(vertical.lineGap) * scale;
        Ok(Metrics {
            average_advance: f64::from(horizontal.advanceWidth) * f64::from(scale),
            line_height: f64::from(ascent - descent + line_gap),
            descent,
            underline_position: f32::from(vertical.underlinePosition) * scale,
            underline_thickness: f32::from(vertical.underlineThickness) * scale,
            strikeout_position: f32::from(vertical.strikethroughPosition) * scale,
            strikeout_thickness: f32::from(vertical.strikethroughThickness) * scale,
        })
    }

    fn rasterize_embedded(
        font: &EmbeddedFont,
        size: Size,
        character: char,
        glyph_index: u16,
    ) -> Result<RasterizedGlyph, Error> {
        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace: unsafe { font.face.as_ptr() },
            fontEmSize: size.as_px(),
            glyphCount: 1,
            glyphIndices: &glyph_index,
            glyphAdvances: &0.0,
            glyphOffsets: &GlyphOffset::default(),
            isSideways: 0,
            bidiLevel: 0,
        };
        let rendering_mode = font.face.get_recommended_rendering_mode_default_params(
            size.as_px(),
            1.0,
            dwrote::DWRITE_MEASURING_MODE_NATURAL,
        );
        let analysis = GlyphRunAnalysis::create(
            &glyph_run,
            1.0,
            None,
            rendering_mode,
            dwrote::DWRITE_MEASURING_MODE_NATURAL,
            0.0,
            0.0,
        )
        .map_err(directwrite_error)?;
        let bounds = analysis
            .get_alpha_texture_bounds(dwrote::DWRITE_TEXTURE_CLEARTYPE_3x1)
            .map_err(directwrite_error)?;
        let buffer = analysis
            .create_alpha_texture(dwrote::DWRITE_TEXTURE_CLEARTYPE_3x1, bounds)
            .map_err(directwrite_error)?;
        Ok(RasterizedGlyph {
            character,
            width: bounds.right - bounds.left,
            height: bounds.bottom - bounds.top,
            top: -bounds.top,
            left: bounds.left,
            advance: (0, 0),
            buffer: BitmapBuffer::Rgb(buffer),
        })
    }
}

#[cfg(not(windows))]
pub(crate) type Rasterizer = crossfont::Rasterizer;

#[cfg(windows)]
impl Rasterize for Rasterizer {
    fn new() -> Result<Self, Error> {
        let system = crossfont::Rasterizer::new()?;
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(StaticFontData::new());
        let file = FontFile::new_from_buffer(data).ok_or_else(|| {
            Error::PlatformError("DirectWrite rejected the embedded Maple Mono font".to_owned())
        })?;
        let loader = CustomFontCollectionLoaderImpl::new(&[file]);
        let embedded_collection = FontCollection::from_loader(loader);
        Ok(Self {
            system,
            embedded_collection,
            embedded_fonts: HashMap::new(),
            embedded_keys: HashMap::new(),
        })
    }

    fn metrics(&self, key: FontKey, size: Size) -> Result<Metrics, Error> {
        match self.embedded_fonts.get(&key) {
            Some(font) => Self::embedded_metrics(font, size),
            None => self.system.metrics(key, size),
        }
    }

    fn load_font(&mut self, description: &FontDesc, size: Size) -> Result<FontKey, Error> {
        self.system.load_font(description, size)
    }

    fn get_glyph(&mut self, glyph: GlyphKey) -> Result<RasterizedGlyph, Error> {
        let Some(font) = self.embedded_fonts.get(&glyph.font_key) else {
            return self.system.get_glyph(glyph);
        };
        let glyph_index = font
            .face
            .glyph_indices(&[glyph.character as u32])
            .map_err(directwrite_error)?
            .first()
            .copied()
            .unwrap_or(0);
        if glyph_index == 0 {
            if let Some(font_key) = font.fallback_key {
                return self.system.get_glyph(GlyphKey { font_key, ..glyph });
            }
        }
        let rasterized = Self::rasterize_embedded(font, glyph.size, glyph.character, glyph_index)?;
        if glyph_index == 0 { Err(Error::MissingGlyph(rasterized)) } else { Ok(rasterized) }
    }

    fn kerning(&mut self, left: GlyphKey, right: GlyphKey) -> (f32, f32) {
        if self.is_embedded_font(left.font_key) || self.is_embedded_font(right.font_key) {
            (0.0, 0.0)
        } else {
            self.system.kerning(left, right)
        }
    }
}

#[cfg(windows)]
fn directwrite_error(error: i32) -> Error {
    Error::PlatformError(format!("DirectWrite error: {error:#X}"))
}

#[cfg(windows)]
fn font_weight(weight: Weight) -> FontWeight {
    match weight {
        Weight::Normal => FontWeight::Regular,
        Weight::Bold => FontWeight::Bold,
    }
}

#[cfg(windows)]
fn font_style(slant: Slant) -> FontStyle {
    match slant {
        Slant::Normal => FontStyle::Normal,
        Slant::Italic => FontStyle::Italic,
        Slant::Oblique => FontStyle::Oblique,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::mem;

    use crossfont::{GlyphKey, Rasterize, Size, Slant, Weight};

    use super::{
        EMBEDDED_FONT_BYTES, FontSource, Rasterizer, StaticFontData, preferred_font_source,
    };

    #[test]
    fn system_font_is_preferred_before_the_embedded_fallback() {
        assert_eq!(preferred_font_source(true, true), Some(FontSource::System));
        assert_eq!(preferred_font_source(false, true), Some(FontSource::Embedded));
        assert_eq!(preferred_font_source(false, false), None);
    }

    #[test]
    fn embedded_font_storage_borrows_the_static_pe_bytes() {
        let storage = StaticFontData::new();
        assert_eq!(mem::size_of_val(&storage), mem::size_of::<&'static [u8]>());
        assert_eq!(storage.as_ref().as_ptr(), EMBEDDED_FONT_BYTES.as_ptr());
        assert_eq!(storage.as_ref().len(), EMBEDDED_FONT_BYTES.len());
    }

    #[test]
    fn embedded_directwrite_collection_rasterizes_maple_glyphs() {
        let mut rasterizer = Rasterizer::new().expect("DirectWrite rasterizer");
        let size = Size::new(11.25);
        let key = rasterizer
            .load_embedded_font(
                crate::font_install::REQUIRED_FONT_FAMILY,
                Slant::Normal,
                Weight::Normal,
                size,
            )
            .expect("embedded Maple Mono");

        assert!(rasterizer.is_embedded_font(key));
        for character in ['A', '\u{ea83}', '\u{4e2d}'] {
            let glyph = rasterizer
                .get_glyph(GlyphKey { character, font_key: key, size })
                .unwrap_or_else(|error| panic!("embedded glyph {character:?}: {error}"));
            assert!(glyph.width > 0);
            assert!(glyph.height > 0);
        }
    }
}
