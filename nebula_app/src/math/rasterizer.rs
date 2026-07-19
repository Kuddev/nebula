//! 固定数学字体 glyph ID 的紧边界 CPU 栅格化。

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use ttf_parser::{GlyphId, OutlineBuilder};

use super::font::MathFont;
use super::{MathError, MathErrorKind};

const GLYPH_PADDING: f32 = 1.0;
const MAX_GLYPH_DIMENSION: u32 = 512;
/// Grayscale math glyphs do not receive DirectWrite's subpixel contrast.
/// A modest coverage curve keeps thin Latin Modern strokes legible at the
/// document's normal text size without allocating a supersampled bitmap.
const COVERAGE_GAMMA: f32 = 0.75;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RasterizedMathGlyph {
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// glyph 原点到位图左边界的像素偏移。
    pub(crate) left: i16,
    /// glyph 基线到位图顶边界的像素偏移，向上为正。
    pub(crate) top: i16,
    pub(crate) rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MathGlyphRasterizer {
    font: MathFont,
}

impl MathGlyphRasterizer {
    pub(crate) fn new() -> Result<Self, MathError> {
        let font = MathFont::load().map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
        Ok(Self { font })
    }

    pub(crate) fn rasterize(
        &self,
        glyph_id: u16,
        pixel_size: f32,
    ) -> Result<RasterizedMathGlyph, MathError> {
        if !pixel_size.is_finite() || pixel_size <= 0.0 {
            return Err(MathError::new(MathErrorKind::Font, 0));
        }
        let glyph = GlyphId(glyph_id);
        let metrics = self
            .font
            .glyph_metrics(glyph, pixel_size)
            .map_err(|_| MathError::new(MathErrorKind::MissingGlyph, 0))?;
        let left = (metrics.x_min - GLYPH_PADDING).floor();
        let right = (metrics.x_max + GLYPH_PADDING).ceil();
        let top = (metrics.height + GLYPH_PADDING).ceil();
        let bottom = (-metrics.depth - GLYPH_PADDING).floor();
        let width = (right - left).max(0.0) as u32;
        let height = (top - bottom).max(0.0) as u32;
        if width == 0 || height == 0 {
            return Ok(RasterizedMathGlyph::default());
        }
        if width > MAX_GLYPH_DIMENSION || height > MAX_GLYPH_DIMENSION {
            return Err(MathError::new(MathErrorKind::GlyphTooLarge, 0));
        }
        let left_i16 = checked_i16(left)?;
        let top_i16 = checked_i16(top)?;
        let mut outline =
            RasterOutline::new(width as usize, height as usize, pixel_size, left, top, self.font)?;
        let bounds = self
            .font
            .outline(glyph, &mut outline)
            .map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
        if bounds.is_none() {
            return Err(MathError::new(MathErrorKind::MissingGlyph, 0));
        }

        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| MathError::new(MathErrorKind::GlyphTooLarge, 0))?;
        let mut rgba = vec![0u8; pixel_count * 4];
        outline.rasterizer.for_each_pixel(|index, alpha| {
            let offset = index * 4;
            rgba[offset] = 255;
            rgba[offset + 1] = 255;
            rgba[offset + 2] = 255;
            rgba[offset + 3] = coverage_alpha(alpha);
        });
        Ok(RasterizedMathGlyph {
            width: width as u16,
            height: height as u16,
            left: left_i16,
            top: top_i16,
            rgba,
        })
    }
}

fn coverage_alpha(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0).powf(COVERAGE_GAMMA) * 255.0).round() as u8
}

fn checked_i16(value: f32) -> Result<i16, MathError> {
    if value < i16::MIN as f32 || value > i16::MAX as f32 {
        Err(MathError::new(MathErrorKind::GlyphTooLarge, 0))
    } else {
        Ok(value as i16)
    }
}

struct RasterOutline {
    rasterizer: Rasterizer,
    scale: f32,
    left: f32,
    top: f32,
    current: Point,
    contour_start: Point,
}

impl RasterOutline {
    fn new(
        width: usize,
        height: usize,
        pixel_size: f32,
        left: f32,
        top: f32,
        font: MathFont,
    ) -> Result<Self, MathError> {
        let units_per_em =
            font.units_per_em().map_err(|_| MathError::new(MathErrorKind::Font, 0))?;
        Ok(Self {
            rasterizer: Rasterizer::new(width, height),
            scale: pixel_size / units_per_em as f32,
            left,
            top,
            current: point(0.0, 0.0),
            contour_start: point(0.0, 0.0),
        })
    }

    fn transform(&self, x: f32, y: f32) -> Point {
        point(x * self.scale - self.left, self.top - y * self.scale)
    }
}

impl OutlineBuilder for RasterOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = self.transform(x, y);
        self.current = point;
        self.contour_start = point;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let next = self.transform(x, y);
        self.rasterizer.draw_line(self.current, next);
        self.current = next;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.transform(x1, y1);
        let next = self.transform(x, y);
        self.rasterizer.draw_quad(self.current, control, next);
        self.current = next;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let control_1 = self.transform(x1, y1);
        let control_2 = self.transform(x2, y2);
        let next = self.transform(x, y);
        self.rasterizer.draw_cubic(self.current, control_1, control_2, next);
        self.current = next;
    }

    fn close(&mut self) {
        self.rasterizer.draw_line(self.current, self.contour_start);
        self.current = self.contour_start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_math_glyph_rasterizes_to_bounded_rgba() {
        let font = MathFont::load().unwrap();
        let glyph = font.glyph_id('∫').unwrap();
        let rasterizer = MathGlyphRasterizer::new().unwrap();
        let image = rasterizer.rasterize(glyph.0, 32.0).unwrap();
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(image.rgba.len(), image.width as usize * image.height as usize * 4);
        assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
        assert!(image.rgba.chunks_exact(4).all(|pixel| pixel[..3] == [255, 255, 255]));
    }

    #[test]
    fn rasterization_is_deterministic_and_rejects_oversized_requests() {
        let font = MathFont::load().unwrap();
        let glyph = font.glyph_id('x').unwrap();
        let rasterizer = MathGlyphRasterizer::new().unwrap();
        let first = rasterizer.rasterize(glyph.0, 18.0).unwrap();
        let second = rasterizer.rasterize(glyph.0, 18.0).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            rasterizer.rasterize(glyph.0, 4096.0).unwrap_err().kind,
            MathErrorKind::GlyphTooLarge
        );
    }

    #[test]
    fn coverage_curve_preserves_endpoints_and_strengthens_thin_edges() {
        assert_eq!(coverage_alpha(0.0), 0);
        assert_eq!(coverage_alpha(1.0), 255);
        assert!(coverage_alpha(0.25) > 64);
        assert!(coverage_alpha(0.25) < coverage_alpha(0.5));
    }
}
