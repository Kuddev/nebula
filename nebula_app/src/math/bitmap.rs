//! Bounded CPU composition shared by all formula presentation surfaces.
use super::layout::MathLayout;
use super::rasterizer::MathGlyphRasterizer;

const MAX_IMAGE_EDGE_PX: u32 = 8192;
const MAX_IMAGE_BYTES: usize = 24 * 1024 * 1024;
const CANVAS_PAD: u32 = 2;

pub(crate) struct MathBitmap {
    pub(crate) pixels: Vec<u8>,
    pub(crate) baseline: u32,
    pub(crate) pad: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Shared preflight for admission and allocation; never reserve a differently
/// rounded size from the buffer the worker will actually create.
pub(crate) fn required_bytes(layout: &MathLayout, scale: f32) -> Option<usize> {
    let (width, height, _) = geometry(layout, scale)?;
    (width as usize).checked_mul(height as usize)?.checked_mul(4)
}

fn geometry(layout: &MathLayout, scale: f32) -> Option<(u32, u32, u32)> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let metrics = layout.metrics;
    if [metrics.width, metrics.height, metrics.depth].iter().any(|value| !value.is_finite()) {
        return None;
    }
    let content_width = (metrics.width * scale).ceil().max(1.0) as u32;
    let ascent = (metrics.height * scale).ceil().max(0.0) as u32;
    let descent = (metrics.depth * scale).ceil().max(0.0) as u32;
    let width = content_width.checked_add(CANVAS_PAD * 2)?;
    let height = ascent.checked_add(descent)?.checked_add(CANVAS_PAD * 2)?.max(1);
    let bytes = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    (width <= MAX_IMAGE_EDGE_PX && height <= MAX_IMAGE_EDGE_PX && bytes <= MAX_IMAGE_BYTES)
        .then_some((width, height, ascent + CANVAS_PAD))
}

/// 把整条公式（字形 + 分数线等矩形）按物理像素合成为一张直通 alpha 的
/// BGRA 位图。字形位图原点吸附整数物理像素（旧壳同款），advance 保持
/// 全精度。
pub(crate) fn compose(
    rasterizer: &MathGlyphRasterizer,
    layout: &MathLayout,
    raster_scale: f32,
    color: [u8; 3],
) -> Option<MathBitmap> {
    let (width, height, baseline) = geometry(layout, raster_scale)?;
    let bytes = width as usize * height as usize * 4;
    let baseline = baseline as i64;

    // Accumulate coverage in the final BGRA buffer's alpha channel. A separate
    // full-size alpha plane would remain live alongside these same pixels.
    let mut bgra = vec![0u8; bytes];
    for op in &layout.glyphs {
        let glyph = rasterizer.rasterize(op.glyph_id, op.pixel_size * raster_scale).ok()?;
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let origin_x = (op.x * raster_scale).round() as i64 + glyph.left as i64 + CANVAS_PAD as i64;
        let origin_y = baseline + (op.baseline_y * raster_scale).round() as i64 - glyph.top as i64;
        blit_over(
            &mut bgra,
            width,
            height,
            origin_x,
            origin_y,
            glyph.width as u32,
            glyph.height as u32,
            |x, y| glyph.rgba[(y * glyph.width as usize + x) * 4 + 3],
        );
    }
    for rule in &layout.rules {
        let x0 = (rule.x * raster_scale).round() as i64 + CANVAS_PAD as i64;
        let y0 = baseline + (rule.y * raster_scale).round() as i64;
        let w = (rule.width * raster_scale).round().max(1.0) as u32;
        let h = (rule.height * raster_scale).round().max(1.0) as u32;
        blit_over(&mut bgra, width, height, x0, y0, w, h, |_, _| u8::MAX);
    }

    // gpui 图像管线吃直通 alpha 的 BGRA（与 DirectWrite 字形同一约定）。
    let [r, g, b] = color;
    for pixel in bgra.chunks_exact_mut(4) {
        pixel[0] = b;
        pixel[1] = g;
        pixel[2] = r;
    }
    Some(MathBitmap { pixels: bgra, baseline: baseline as u32, pad: CANVAS_PAD, width, height })
}

/// alpha 平面上的 Porter-Duff over 合成；目标越界的像素按行列裁剪。
#[allow(clippy::too_many_arguments)]
fn blit_over(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    origin_x: i64,
    origin_y: i64,
    width: u32,
    height: u32,
    sample: impl Fn(usize, usize) -> u8,
) {
    for row in 0..height as i64 {
        let y = origin_y + row;
        if y < 0 || y >= canvas_height as i64 {
            continue;
        }
        for column in 0..width as i64 {
            let x = origin_x + column;
            if x < 0 || x >= canvas_width as i64 {
                continue;
            }
            let source = sample(column as usize, row as usize) as u32;
            if source == 0 {
                continue;
            }
            let target = &mut canvas[(y as usize * canvas_width as usize + x as usize) * 4 + 3];
            let existing = *target as u32;
            *target = (existing + source * (255 - existing) / 255).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_blends_only_alpha_and_clips_to_the_canvas() {
        let mut pixels = vec![0; 2 * 2 * 4];
        blit_over(&mut pixels, 2, 2, -1, -1, 2, 2, |_, _| 128);
        blit_over(&mut pixels, 2, 2, 0, 0, 1, 1, |_, _| 128);
        assert_eq!(pixels[3], 191);
        assert!(pixels.iter().enumerate().all(|(i, value)| i == 3 || *value == 0));
    }

    #[test]
    fn preflight_matches_allocated_pixels_at_fractional_scales() {
        let rasterizer = MathGlyphRasterizer::new().unwrap();
        let layout = crate::math::compile_formula_source(
            r"\frac{x^2}{\sqrt{y}}",
            true,
            18.0,
            1.0,
            crate::math::DEFAULT_LIMITS,
        )
        .unwrap();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let bitmap = compose(&rasterizer, &layout, scale, [11, 22, 33]).unwrap();
            assert_eq!(required_bytes(&layout, scale), Some(bitmap.pixels.len()));
            assert!(bitmap.pixels.chunks_exact(4).any(|p| p[3] != 0));
            assert!(bitmap.pixels.chunks_exact(4).all(|p| p[..3] == [33, 22, 11]));
        }
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, 10000.0] {
            assert!(required_bytes(&layout, scale).is_none());
            assert!(compose(&rasterizer, &layout, scale, [0, 0, 0]).is_none());
        }
    }
}
