//! Hand-rolled drawing of unicode characters that need to fully cover their character area.

use std::{cmp, mem, ops};

use crossfont::{Metrics, RasterizedGlyph};

use crate::config::ui_config::Delta;

// Colors which are used for filling shade variants.
const COLOR_FILL_ALPHA_STEP_1: Pixel = Pixel { _r: 192, _g: 192, _b: 192 };
const COLOR_FILL_ALPHA_STEP_2: Pixel = Pixel { _r: 128, _g: 128, _b: 128 };
const COLOR_FILL_ALPHA_STEP_3: Pixel = Pixel { _r: 64, _g: 64, _b: 64 };

/// Default color used for filling.
const COLOR_FILL: Pixel = Pixel { _r: 255, _g: 255, _b: 255 };

const POWERLINE_TRIANGLE_LTR: char = '\u{e0b0}';
const POWERLINE_ARROW_LTR: char = '\u{e0b1}';
const POWERLINE_TRIANGLE_RTL: char = '\u{e0b2}';
const POWERLINE_ARROW_RTL: char = '\u{e0b3}';
const POWERLINE_ROUND_RTL: char = '\u{e0b4}';
const POWERLINE_ROUND_LTR: char = '\u{e0b6}';

/// Returns the rasterized glyph if the character is part of the built-in font.
pub fn builtin_glyph(
    character: char,
    metrics: &Metrics,
    offset: &Delta<i8>,
    glyph_offset: &Delta<i8>,
) -> Option<RasterizedGlyph> {
    let mut glyph = match character {
        // Box drawing characters and block elements.
        '\u{2500}'..='\u{259f}' | '\u{1fb00}'..='\u{1fb3b}' | '\u{1fb82}'..='\u{1fb8b}' => {
            box_drawing(character, metrics, offset)
        },
        // Powerline symbols: '','','',''
        POWERLINE_TRIANGLE_LTR..=POWERLINE_ARROW_RTL => {
            powerline_drawing(character, metrics, offset)?
        },
        // Powerline extra rounded caps: '',''. 这些字符经常由 Nerd Font 提供，
        // 但不同字体的垂直 metrics 不一致；内建绘制能保证它们和三角分隔符同高。
        POWERLINE_ROUND_RTL | POWERLINE_ROUND_LTR => {
            powerline_round_drawing(character, metrics, offset)?
        },
        _ => return None,
    };

    // Since we want to ignore `glyph_offset` for the built-in font, subtract it to compensate its
    // addition when loading glyphs in the renderer.
    glyph.left -= glyph_offset.x as i32;
    glyph.top -= glyph_offset.y as i32;

    Some(glyph)
}

mod glyphs;
use glyphs::{box_drawing, powerline_drawing, powerline_round_drawing};

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct Pixel {
    _r: u8,
    _g: u8,
    _b: u8,
}

impl Pixel {
    fn gray(color: u8) -> Self {
        Self { _r: color, _g: color, _b: color }
    }
}

impl ops::Add for Pixel {
    type Output = Pixel;

    fn add(self, rhs: Pixel) -> Self::Output {
        let _r = self._r.saturating_add(rhs._r);
        let _g = self._g.saturating_add(rhs._g);
        let _b = self._b.saturating_add(rhs._b);
        Pixel { _r, _g, _b }
    }
}

impl ops::Div<u8> for Pixel {
    type Output = Pixel;

    fn div(self, rhs: u8) -> Self::Output {
        let _r = self._r / rhs;
        let _g = self._g / rhs;
        let _b = self._b / rhs;
        Pixel { _r, _g, _b }
    }
}

/// Canvas which is used for simple line drawing operations.
///
/// The coordinate system is the following:
///
///  0             x
///  --------------→
///  |
///  |
///  |
///  |
///  |
///  |
/// y↓
struct Canvas {
    /// Canvas width.
    width: usize,

    /// Canvas height.
    height: usize,

    /// Canvas buffer we draw on.
    buffer: Vec<Pixel>,
}

impl Canvas {
    /// Builds new `Canvas` for line drawing with the given `width` and `height` with default color.
    fn new(width: usize, height: usize) -> Self {
        let buffer = vec![Pixel::default(); width * height];
        Self { width, height, buffer }
    }

    /// Vertical center of the `Canvas`.
    fn y_center(&self) -> f32 {
        self.height as f32 / 2.
    }

    /// Horizontal center of the `Canvas`.
    fn x_center(&self) -> f32 {
        self.width as f32 / 2.
    }

    /// Canvas underlying buffer for direct manipulation
    fn buffer_mut(&mut self) -> &mut [Pixel] {
        &mut self.buffer
    }

    /// Gives bounds for horizontal straight line on `y` with `stroke_size`.
    fn h_line_bounds(&self, y: f32, stroke_size: usize) -> (f32, f32) {
        let start_y = cmp::max((y - stroke_size as f32 / 2.) as i32, 0) as f32;
        let end_y = cmp::min((y + stroke_size as f32 / 2.) as i32, self.height as i32) as f32;

        (start_y, end_y)
    }

    /// Gives bounds for vertical straight line on `y` with `stroke_size`.
    fn v_line_bounds(&self, x: f32, stroke_size: usize) -> (f32, f32) {
        let start_x = cmp::max((x - stroke_size as f32 / 2.) as i32, 0) as f32;
        let end_x = cmp::min((x + stroke_size as f32 / 2.) as i32, self.width as i32) as f32;

        (start_x, end_x)
    }

    /// Flip horizontally.
    fn flip_horizontal(&mut self) {
        for row in 0..self.height {
            for col in 0..self.width / 2 {
                let index = row * self.width;
                self.buffer.swap(index + col, index + self.width - col - 1)
            }
        }
    }

    /// Draws a horizontal straight line from (`x`, `y`) of `size` with the given `stroke_size`.
    fn draw_h_line(&mut self, x: f32, y: f32, size: f32, stroke_size: usize) {
        let (start_y, end_y) = self.h_line_bounds(y, stroke_size);
        self.draw_rect(x, start_y, size, end_y - start_y, COLOR_FILL);
    }

    /// Draws a vertical straight line from (`x`, `y`) of `size` with the given `stroke_size`.
    fn draw_v_line(&mut self, x: f32, y: f32, size: f32, stroke_size: usize) {
        let (start_x, end_x) = self.v_line_bounds(x, stroke_size);
        self.draw_rect(start_x, y, end_x - start_x, size, COLOR_FILL);
    }

    /// Draws a rect from the (`x`, `y`) of the given `width` and `height` using `color`.
    fn draw_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Pixel) {
        let start_x = x as usize;
        let end_x = cmp::min((x + width) as usize, self.width);
        let start_y = y as usize;
        let end_y = cmp::min((y + height) as usize, self.height);
        for y in start_y..end_y {
            let y = y * self.width;
            self.buffer[start_x + y..end_x + y].fill(color);
        }
    }

    /// Put pixel into buffer with the given color if the color is brighter than the one buffer
    /// already has in place.
    #[inline]
    fn put_pixel(&mut self, x: f32, y: f32, color: Pixel) {
        if x < 0. || y < 0. || x > self.width as f32 - 1. || y > self.height as f32 - 1. {
            return;
        }
        let index = x as usize + y as usize * self.width;
        if color._r > self.buffer[index]._r {
            self.buffer[index] = color;
        }
    }

    /// Xiaolin Wu's line drawing from (`from_x`, `from_y`) to (`to_x`, `to_y`).
    fn draw_line(&mut self, mut from_x: f32, mut from_y: f32, mut to_x: f32, mut to_y: f32) {
        let steep = (to_y - from_y).abs() > (to_x - from_x).abs();
        if steep {
            mem::swap(&mut from_x, &mut from_y);
            mem::swap(&mut to_x, &mut to_y);
        }
        if from_x > to_x {
            mem::swap(&mut from_x, &mut to_x);
            mem::swap(&mut from_y, &mut to_y);
        }

        let delta_x = to_x - from_x;
        let delta_y = to_y - from_y;
        let gradient = if delta_x.abs() <= f32::EPSILON { 1. } else { delta_y / delta_x };

        let x_end = f32::round(from_x);
        let y_end = from_y + gradient * (x_end - from_x);
        let x_gap = 1. - (from_x + 0.5).fract();

        let xpxl1 = x_end;
        let ypxl1 = y_end.trunc();

        let color_1 = Pixel::gray(((1. - y_end.fract()) * x_gap * COLOR_FILL._r as f32) as u8);
        let color_2 = Pixel::gray((y_end.fract() * x_gap * COLOR_FILL._r as f32) as u8);
        if steep {
            self.put_pixel(ypxl1, xpxl1, color_1);
            self.put_pixel(ypxl1 + 1., xpxl1, color_2);
        } else {
            self.put_pixel(xpxl1, ypxl1, color_1);
            self.put_pixel(xpxl1 + 1., ypxl1, color_2);
        }

        let mut intery = y_end + gradient;

        let x_end = f32::round(to_x);
        let y_end = to_y + gradient * (x_end - to_x);
        let x_gap = (to_x + 0.5).fract();
        let xpxl2 = x_end;
        let ypxl2 = y_end.trunc();

        let color_1 = Pixel::gray(((1. - y_end.fract()) * x_gap * COLOR_FILL._r as f32) as u8);
        let color_2 = Pixel::gray((y_end.fract() * x_gap * COLOR_FILL._r as f32) as u8);
        if steep {
            self.put_pixel(ypxl2, xpxl2, color_1);
            self.put_pixel(ypxl2 + 1., xpxl2, color_2);
        } else {
            self.put_pixel(xpxl2, ypxl2, color_1);
            self.put_pixel(xpxl2, ypxl2 + 1., color_2);
        }

        if steep {
            for x in xpxl1 as i32 + 1..xpxl2 as i32 {
                let color_1 = Pixel::gray(((1. - intery.fract()) * COLOR_FILL._r as f32) as u8);
                let color_2 = Pixel::gray((intery.fract() * COLOR_FILL._r as f32) as u8);
                self.put_pixel(intery.trunc(), x as f32, color_1);
                self.put_pixel(intery.trunc() + 1., x as f32, color_2);
                intery += gradient;
            }
        } else {
            for x in xpxl1 as i32 + 1..xpxl2 as i32 {
                let color_1 = Pixel::gray(((1. - intery.fract()) * COLOR_FILL._r as f32) as u8);
                let color_2 = Pixel::gray((intery.fract() * COLOR_FILL._r as f32) as u8);
                self.put_pixel(x as f32, intery.trunc(), color_1);
                self.put_pixel(x as f32, intery.trunc() + 1., color_2);
                intery += gradient;
            }
        }
    }

    /// Draws a quarter of a circle centered in `(0., self.height - radius)` with radius
    /// `self.width` and an attached rectangle to form a "╭" using a given `stroke_size` in the
    /// bottom-right quadrant of the `Canvas` coordinate system.
    fn draw_rounded_corner(&mut self, stroke_size: usize) {
        let radius = (self.width.min(self.height) + stroke_size) as f32 / 2.;
        let stroke_size_f = stroke_size as f32;

        let mut x_offset = 0.;
        let mut y_offset = 0.;
        let (long_side, short_side, offset) = if self.height > self.width {
            (&self.height, &self.width, &mut y_offset)
        } else {
            (&self.width, &self.height, &mut x_offset)
        };
        let distance_bias = if short_side % 2 == stroke_size % 2 { 0. } else { 0.5 };
        *offset = *long_side as f32 / 2. - radius + stroke_size_f / 2.;
        if (self.width % 2 != self.height % 2) && (long_side % 2 == stroke_size % 2) {
            *offset += 1.;
        }

        let radius_i = (short_side + stroke_size).div_ceil(2);
        for y in 0..radius_i {
            for x in 0..radius_i {
                let y = y as f32;
                let x = x as f32;
                let distance = x.hypot(y) + distance_bias;
                let value = if distance < radius - stroke_size_f - 1. {
                    // Inside the circle.
                    0.
                } else if distance < radius - stroke_size_f {
                    // On the inner border.
                    1. + distance - (radius - stroke_size_f)
                } else if distance < radius - 1. {
                    // Inside the stroke.
                    1.
                } else if distance < radius {
                    // On the outer border.
                    radius - distance
                } else {
                    // Outside of the circle.
                    0.
                };

                self.put_pixel(
                    x + x_offset,
                    y + y_offset,
                    Pixel::gray((COLOR_FILL._r as f32 * value) as u8),
                );
            }
        }

        if self.height > self.width {
            self.draw_rect(
                self.x_center() - stroke_size_f * 0.5,
                0.,
                stroke_size_f,
                y_offset,
                COLOR_FILL,
            );
        } else {
            self.draw_rect(
                0.,
                self.y_center() - stroke_size_f * 0.5,
                x_offset,
                stroke_size_f,
                COLOR_FILL,
            );
        }
    }

    /// Fills the `Canvas` with the given `Color`.
    fn fill(&mut self, color: Pixel) {
        self.buffer.fill(color);
    }

    /// Consumes `Canvas` and returns its underlying storage as raw byte vector.
    fn into_raw(self) -> Vec<u8> {
        // SAFETY This is safe since we use `repr(packed)` on `Pixel` struct for underlying storage
        // of the `Canvas` buffer which consists of three u8 values.
        unsafe {
            let capacity = self.buffer.capacity() * mem::size_of::<Pixel>();
            let len = self.buffer.len() * mem::size_of::<Pixel>();
            let buf = self.buffer.as_ptr() as *mut u8;
            mem::forget(self.buffer);
            Vec::from_raw_parts(buf, len, capacity)
        }
    }
}

/// Compute line width.
fn calculate_stroke_size(cell_width: usize) -> usize {
    // Use one eight of the cell width, since this is used as a step size for block elements.
    cmp::max((cell_width as f32 / 8.).round() as usize, 1)
}

/// `f(x) = slope * x + offset` equation.
fn line_equation(slope: i32, x: i32, offset: i32) -> (f32, f32) {
    (x as f32, (slope * x + offset) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossfont::Metrics;

    // Dummy metrics values to test builtin glyphs coverage.
    const METRICS: Metrics = Metrics {
        average_advance: 6.,
        line_height: 16.,
        descent: 4.,
        underline_position: 2.,
        underline_thickness: 2.,
        strikeout_position: 2.,
        strikeout_thickness: 2.,
    };

    #[test]
    fn builtin_line_drawing_glyphs_coverage() {
        let offset = Default::default();
        let glyph_offset = Default::default();

        // Test coverage of box drawing characters.
        for character in ('\u{2500}'..='\u{259f}').chain('\u{1fb00}'..='\u{1fb3b}') {
            assert!(builtin_glyph(character, &METRICS, &offset, &glyph_offset).is_some());
        }

        for character in ('\u{2450}'..'\u{2500}').chain('\u{25a0}'..'\u{2600}') {
            assert!(builtin_glyph(character, &METRICS, &offset, &glyph_offset).is_none());
        }
    }

    #[test]
    fn builtin_powerline_glyphs_coverage() {
        let offset = Default::default();
        let glyph_offset = Default::default();

        // Test coverage of box drawing characters.
        for character in '\u{e0b0}'..='\u{e0b3}' {
            assert!(builtin_glyph(character, &METRICS, &offset, &glyph_offset).is_some());
        }

        for character in ('\u{e0a0}'..'\u{e0b0}').chain('\u{e0b4}'..'\u{e0c0}') {
            assert!(builtin_glyph(character, &METRICS, &offset, &glyph_offset).is_none());
        }
    }
}
