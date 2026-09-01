//! Shared background-color picker state and color conversion helpers.

use super::color::Rgb;

/// Background swatches shown by both shell implementations.
pub(crate) const BACKGROUND_SWATCHES: [Rgb; 12] = [
    Rgb::new(8, 10, 24),
    Rgb::new(12, 16, 28),
    Rgb::new(18, 14, 32),
    Rgb::new(24, 24, 37),
    Rgb::new(0, 43, 54),
    Rgb::new(6, 26, 28),
    Rgb::new(40, 42, 54),
    Rgb::new(30, 30, 30),
    Rgb::new(12, 12, 12),
    Rgb::new(0, 0, 0),
    Rgb::new(253, 246, 227),
    Rgb::new(255, 255, 255),
];

/// HSV to RGB. Hue is expressed in degrees and normalized to `[0, 360)`;
/// saturation and value are clamped to `[0, 1]`.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h {
        _ if h < 60.0 => (c, x, 0.0),
        _ if h < 120.0 => (x, c, 0.0),
        _ if h < 180.0 => (0.0, c, x),
        _ if h < 240.0 => (0.0, x, c),
        _ if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |component: f32| ((component + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(to(r), to(g), to(b))
}

/// RGB to HSV. Grayscale colors use hue zero; callers preserving a picker hue
/// while saturation is zero should retain their previous hue separately.
pub(crate) fn rgb_to_hsv(color: Rgb) -> (f32, f32, f32) {
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { delta / max };
    (h, s, max)
}

/// Draggable region within the background color picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgPickerPart {
    Sv,
    Hue,
}

#[cfg(test)]
mod tests {
    use super::{hsv_to_rgb, rgb_to_hsv};
    use crate::display::color::Rgb;

    #[test]
    fn hsv_rgb_round_trip_and_gray_hue_convention() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), Rgb::new(255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), Rgb::new(0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), Rgb::new(0, 0, 255));
        assert_eq!(hsv_to_rgb(0.0, 0.0, 1.0), Rgb::new(255, 255, 255));
        assert_eq!(hsv_to_rgb(123.0, 0.7, 0.0), Rgb::new(0, 0, 0));
        assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), hsv_to_rgb(0.0, 1.0, 1.0));
        assert_eq!(hsv_to_rgb(-120.0, 1.0, 1.0), hsv_to_rgb(240.0, 1.0, 1.0));

        for color in [Rgb::new(8, 10, 24), Rgb::new(253, 246, 227), Rgb::new(40, 42, 54)] {
            let (h, s, v) = rgb_to_hsv(color);
            let back = hsv_to_rgb(h, s, v);
            assert!(
                (back.r as i32 - color.r as i32).abs() <= 1
                    && (back.g as i32 - color.g as i32).abs() <= 1
                    && (back.b as i32 - color.b as i32).abs() <= 1,
                "{color:?} -> ({h},{s},{v}) -> {back:?}"
            );
        }
        assert_eq!(rgb_to_hsv(Rgb::new(128, 128, 128)).0, 0.0);
        assert_eq!(rgb_to_hsv(Rgb::new(128, 128, 128)).1, 0.0);
    }
}
