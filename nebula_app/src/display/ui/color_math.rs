//! Backend-neutral RGBA composition used by both shell renderers.

use crate::renderer::ui::Rgba;

/// Fade a color by `progress`, preserving its RGB channels.
#[inline]
pub fn fade(color: Rgba, progress: f32) -> Rgba {
    if progress >= 1.0 {
        return color;
    }
    Rgba::new(color.r, color.g, color.b, (color.a as f32 * progress.clamp(0.0, 1.0)).round() as u8)
}

/// Composite `top` over `base`, retaining the base alpha channel.
#[inline]
pub fn over(top: Rgba, base: Rgba) -> Rgba {
    let alpha = top.a as f32 / 255.0;
    let mix = |top: u8, base: u8| {
        (top as f32 * alpha + base as f32 * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
    };
    Rgba::new(mix(top.r, base.r), mix(top.g, base.g), mix(top.b, base.b), base.a)
}
