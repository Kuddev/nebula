//! 固定数学字体的零拷贝访问层。

use std::sync::OnceLock;

use ttf_parser::{Face, GlyphId};

pub(crate) static FONT_BYTES: &[u8] =
    include_bytes!("../../../assets/fonts/LatinModernMath.otf");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MathFontError;

/// Latin Modern Math 在进程内只解析一次；Face 只借用静态字体字节，不复制约 716 KiB 资源。
fn face() -> Result<&'static Face<'static>, MathFontError> {
    static FACE: OnceLock<Option<Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| Face::parse(FONT_BYTES, 0).ok()).as_ref().ok_or(MathFontError)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MathFont;

impl MathFont {
    pub(crate) fn load() -> Result<Self, MathFontError> {
        face().map(|_| Self)
    }

    pub(crate) fn units_per_em(self) -> Result<u16, MathFontError> {
        Ok(face()?.units_per_em())
    }

    pub(crate) fn glyph_id(self, character: char) -> Result<GlyphId, MathFontError> {
        face()?.glyph_index(character).ok_or(MathFontError)
    }

    pub(crate) fn has_math_table(self) -> Result<bool, MathFontError> {
        Ok(face()?.tables().math.is_some())
    }

    pub(crate) fn has_vertical_construction(
        self,
        character: char,
    ) -> Result<bool, MathFontError> {
        let glyph = self.glyph_id(character)?;
        let Some(variants) = face()?.tables().math.and_then(|math| math.variants) else {
            return Ok(false);
        };
        let Some(construction) = variants.vertical_constructions.get(glyph) else {
            return Ok(false);
        };
        Ok(!construction.variants.is_empty() || construction.assembly.is_some())
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{FONT_BYTES, MathFont};

    const EXPECTED_SHA256: [u8; 32] = [
        0x60, 0x75, 0x56, 0x2b, 0x77, 0x1f, 0x8b, 0x82, 0xf0, 0xc1, 0x79, 0xe3, 0x63, 0x38,
        0x96, 0x84, 0xf2, 0xdd, 0x09, 0xde, 0x30, 0x03, 0x82, 0x69, 0xe2, 0x62, 0x8e, 0x50,
        0x4b, 0xd7, 0xbe, 0x0f,
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
}
