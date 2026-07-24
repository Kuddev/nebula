//! Nebula chrome UI renderer.
//!
//! A self-contained immediate-mode quad renderer for the window chrome
//! (title bar, tabs, status bar, settings). It draws rounded, optionally
//! gradient-filled rectangles in screen-pixel coordinates and is fully
//! independent from the terminal grid's [`RectRenderer`], so the terminal
//! rendering path is never touched.

use std::mem;

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::gl::types::*;
use crate::renderer::shader::{ShaderProgram, ShaderVersion};
use crate::{gl, renderer};

/// Shader sources for the chrome UI program.
const UI_SHADER_F: &str = include_str!("../../res/ui.f.glsl");
const UI_SHADER_V: &str = include_str!("../../res/ui.v.glsl");

/// RGBA color with straight (non-premultiplied) alpha, 0-255 per channel.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Opaque color from an `Rgb`.
    #[inline]
    pub fn opaque(rgb: Rgb) -> Self {
        Self { r: rgb.r, g: rgb.g, b: rgb.b, a: 255 }
    }

    /// Same color with a scaled alpha (`alpha` in `0.0..=1.0`).
    #[inline]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self { a: (alpha.clamp(0., 1.) * 255.) as u8, ..self }
    }
}

/// Direction of a linear gradient, expressed in the quad's uv space.
#[derive(Debug, Copy, Clone)]
pub enum Gradient {
    /// Flat fill (no gradient).
    None,
    /// Top (`color0`) to bottom (`color1`).
    Vertical,
    /// Left (`color0`) to right (`color1`).
    Horizontal,
    /// Arbitrary axis in uv space; `color0` at `dot(uv, axis) == 0`.
    Axis([f32; 2]),
}

impl Gradient {
    #[inline]
    fn axis(self) -> [f32; 2] {
        match self {
            Gradient::None => [0., 0.],
            Gradient::Vertical => [0., 1.],
            Gradient::Horizontal => [1., 0.],
            Gradient::Axis(a) => a,
        }
    }
}

/// A single rounded quad to draw, in screen-pixel coordinates with the origin
/// at the top-left of the window.
#[derive(Debug, Copy, Clone)]
pub struct UiQuad {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    /// Per-corner radii in pixels `[top-left, top-right, bottom-right,
    /// bottom-left]`. Defaults to `[radius; 4]`; set via [`UiQuad::with_corners`]
    /// to round only some corners (e.g. the connected top-bar/sidebar L-frame,
    /// where the join corners are square and only the outer corners curve).
    /// `radius` still drives the flat/glow shader flags, so keep it ≥ 0 here.
    pub corner_radii: [f32; 4],
    /// Soft-effect parameter: `0.0` is a crisp rounded rectangle, positive is
    /// the legacy radial glow, and negative is the blur radius of a rounded
    /// outer shadow. Constructors hide this encoding from call sites.
    pub feather: f32,
    /// Explicit corner positions in pixels `[top-left, bottom-left, top-right,
    /// bottom-right]` for slanted/parallelogram shapes (flat-filled). When set,
    /// `x/y/width/height/radius` are ignored for geometry.
    pub corners: Option<[[f32; 2]; 4]>,
    /// Visible vertical sub-range of the quad in its own uv space
    /// (`[0.0, 1.0]` = the whole quad). The geometry is trimmed to this band
    /// while the fragment shader keeps evaluating the rounded-rect/glow SDF in
    /// the quad's ORIGINAL size — i.e. a poor man's scissor for scrolled
    /// content. Set via [`UiQuad::clip_y`].
    pub v_range: [f32; 2],
    pub color0: Rgba,
    pub color1: Rgba,
    pub gradient: Gradient,
}

impl UiQuad {
    /// Flat-filled rounded rectangle.
    #[inline]
    pub fn solid(x: f32, y: f32, width: f32, height: f32, radius: f32, color: Rgba) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radius,
            corner_radii: [radius; 4],
            feather: 0.0,
            corners: None,
            v_range: [0.0, 1.0],
            color0: color,
            color1: color,
            gradient: Gradient::None,
        }
    }

    /// This quad with its edges snapped to whole physical pixels, so thin
    /// fills (hairlines, underlines, 1px dividers) cover exactly the pixel
    /// rows they mean to instead of smearing half-alpha across two. Edges are
    /// snapped as `round(x)..round(x + w)` — two quads sharing an edge stay
    /// flush after snapping — and a sub-pixel sliver keeps a minimum of 1px
    /// rather than vanishing. Glows (feathered) and explicit-corner polygons
    /// are soft/slanted by design and pass through untouched.
    pub fn pixel_snapped(&self) -> Self {
        if self.corners.is_some() || self.feather.abs() > 0.0 {
            return *self;
        }
        let x1 = (self.x + self.width).round();
        let y1 = (self.y + self.height).round();
        let x = self.x.round();
        let y = self.y.round();
        let width = if self.width > 0.0 { (x1 - x).max(1.0) } else { 0.0 };
        let height = if self.height > 0.0 { (y1 - y).max(1.0) } else { 0.0 };
        Self { x, y, width, height, ..*self }
    }

    /// Gradient-filled rounded rectangle.
    #[inline]
    pub fn gradient(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        color0: Rgba,
        color1: Rgba,
        gradient: Gradient,
    ) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radius,
            corner_radii: [radius; 4],
            feather: 0.0,
            corners: None,
            v_range: [0.0, 1.0],
            color0,
            color1,
            gradient,
        }
    }

    /// Soft radial glow centered in the quad, fading to transparent at the edge.
    #[inline]
    pub fn glow(x: f32, y: f32, width: f32, height: f32, color: Rgba) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radius: 0.0,
            corner_radii: [0.0; 4],
            feather: 1.0,
            corners: None,
            v_range: [0.0, 1.0],
            color0: color,
            color1: color,
            gradient: Gradient::None,
        }
    }

    /// Soft shadow following a rounded rectangle instead of radiating from its
    /// center. The generated geometry includes the blur extent; the shader
    /// evaluates the original card shape inside it. `offset_y` provides the
    /// small directional cue expected from a floating surface.
    #[inline]
    pub fn shadow(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        blur: f32,
        offset_y: f32,
        color: Rgba,
    ) -> Self {
        let blur = blur.max(1.0);
        Self {
            x: x - blur,
            y: y - blur + offset_y,
            width: width + blur * 2.0,
            height: height + blur * 2.0,
            radius,
            corner_radii: [radius; 4],
            // 负值区分圆角外阴影与既有的中心径向 glow，避免改变旧调用行为。
            feather: -blur,
            corners: None,
            v_range: [0.0, 1.0],
            color0: color,
            color1: color,
            gradient: Gradient::None,
        }
    }

    /// Flat-filled gradient polygon from explicit pixel corners
    /// `[top-left, bottom-left, top-right, bottom-right]` (for powerline slants).
    #[inline]
    pub fn poly(
        corners: [[f32; 2]; 4],
        color0: Rgba,
        color1: Rgba,
        gradient: Gradient,
    ) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            radius: -1.0,
            corner_radii: [0.0; 4],
            feather: 0.0,
            corners: Some(corners),
            v_range: [0.0, 1.0],
            color0,
            color1,
            gradient,
        }
    }

    /// Shell filler for one corner of a rounded card: an `r × r` block that
    /// paints only the area OUTSIDE the card's quarter-circle corner. Placed
    /// over the card's corner, it lets a square shell frame wrap a rounded
    /// card with complementary antialiasing — same circle center and radius
    /// as the card's convex corner, so coverage sums to exactly one across
    /// the arc (no seam, no double cover). `corner` follows the
    /// `corner_radii` order on the CARD: 0 top-left, 1 top-right,
    /// 2 bottom-right, 3 bottom-left. Callers pass pre-rounded pixel
    /// coordinates; `pixel_snapped` skips feathered quads.
    #[inline]
    pub fn concave_corner(x: f32, y: f32, r: f32, corner: usize, color: Rgba) -> Self {
        // Circle center in quad-local px: the block corner pointing INTO the
        // card (e.g. wrapping the card's top-left arc, the center sits at the
        // block's bottom-right).
        let center = match corner {
            0 => [r, r],
            1 => [0.0, r],
            2 => [0.0, 0.0],
            _ => [r, 0.0],
        };
        Self {
            x,
            y,
            width: r,
            height: r,
            radius: 0.0,
            // Repurposed as `[center_x, center_y, radius, 0]` by the concave
            // fragment branch (`feather == 2.0`).
            corner_radii: [center[0], center[1], r, 0.0],
            feather: 2.0,
            corners: None,
            v_range: [0.0, 1.0],
            color0: color,
            color1: color,
            gradient: Gradient::None,
        }
    }

    /// Rounded rectangle with independent per-corner radii, ordered
    /// `[top-left, top-right, bottom-right, bottom-left]`. Used for the
    /// connected top-bar / sidebar L-frame, where the two join corners stay
    /// square (radius 0) and only the outer corners curve. `radius` is kept as
    /// the max of the four so the shader's flat/glow branch and the geometry's
    /// AA padding still behave; the fragment shader picks the right corner.
    #[inline]
    pub fn with_corners(mut self, corner_radii: [f32; 4]) -> Self {
        self.corner_radii = corner_radii;
        self.radius = corner_radii.iter().copied().fold(0.0_f32, f32::max);
        self
    }

    /// Clip the quad to the vertical band `top..bot` (screen px). The visible
    /// part renders pixel-identical to the unclipped quad — rounded corners,
    /// gradients and glows are cut mid-shape rather than re-rounded — because
    /// only the uv band changes, not the SDF space. Returns `None` when the
    /// quad is entirely outside the band. Polygon quads pass through unclipped
    /// (nothing scrollable uses them).
    #[inline]
    pub fn clip_y(mut self, top: f32, bot: f32) -> Option<Self> {
        if self.corners.is_some() {
            return Some(self);
        }
        if self.height <= 0.0 {
            return None;
        }
        let v0 = ((top - self.y) / self.height).max(self.v_range[0]);
        let v1 = ((bot - self.y) / self.height).min(self.v_range[1]);
        if v1 <= v0 {
            return None;
        }
        self.v_range = [v0, v1];
        Some(self)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UiVertex {
    // Position in normalized device coordinates.
    x: f32,
    y: f32,
    // Local coordinate within the quad, 0..1.
    u: f32,
    v: f32,
    // Quad size in pixels, corner radius and glow feather in pixels.
    w: f32,
    h: f32,
    radius: f32,
    feather: f32,
    // Gradient axis in uv space.
    gx: f32,
    gy: f32,
    // Per-corner radii in pixels: [top-left, top-right, bottom-right, bottom-left].
    corners: [f32; 4],
    // Endpoint colors.
    c0: [u8; 4],
    c1: [u8; 4],
}

#[derive(Debug)]
pub struct UiRenderer {
    vao: GLuint,
    vbo: GLuint,
    program: ShaderProgram,
    vertices: Vec<UiVertex>,
}

impl UiRenderer {
    pub fn new(shader_version: ShaderVersion) -> Result<Self, renderer::Error> {
        let program = ShaderProgram::new(shader_version, None, UI_SHADER_V, UI_SHADER_F)?;

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;

        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let stride = mem::size_of::<UiVertex>() as i32;
            let mut offset = 0i32;

            // aPos (location 0): vec2 position.
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(0);
            offset += (mem::size_of::<f32>() * 2) as i32;

            // aUv (location 1): vec2 local coordinate.
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(1);
            offset += (mem::size_of::<f32>() * 2) as i32;

            // aSizeRadius (location 2): vec4 (width, height, radius, feather).
            gl::VertexAttribPointer(2, 4, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(2);
            offset += (mem::size_of::<f32>() * 4) as i32;

            // aGrad (location 3): vec2 gradient axis.
            gl::VertexAttribPointer(3, 2, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(3);
            offset += (mem::size_of::<f32>() * 2) as i32;

            // aCorners (location 6): vec4 per-corner radii (TL, TR, BR, BL).
            gl::VertexAttribPointer(6, 4, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(6);
            offset += (mem::size_of::<f32>() * 4) as i32;

            // aColor0 (location 4): normalized ubyte4.
            gl::VertexAttribPointer(4, 4, gl::UNSIGNED_BYTE, gl::TRUE, stride, offset as *const _);
            gl::EnableVertexAttribArray(4);
            offset += (mem::size_of::<u8>() * 4) as i32;

            // aColor1 (location 5): normalized ubyte4.
            gl::VertexAttribPointer(5, 4, gl::UNSIGNED_BYTE, gl::TRUE, stride, offset as *const _);
            gl::EnableVertexAttribArray(5);

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }

        Ok(Self { vao, vbo, program, vertices: Vec::new() })
    }

    /// Draw all `quads` in a single batch. Assumes the caller has already set
    /// the viewport to the full window and configured straight-alpha blending.
    pub fn draw(&mut self, size_info: &SizeInfo, quads: &[UiQuad]) {
        if quads.is_empty() {
            return;
        }

        let half_width = size_info.width() / 2.;
        let half_height = size_info.height() / 2.;

        self.vertices.clear();
        for quad in quads {
            self.push_quad(half_width, half_height, quad);
        }

        unsafe {
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);

            gl::UseProgram(self.program.id());

            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertices.len() * mem::size_of::<UiVertex>()) as isize,
                self.vertices.as_ptr() as *const _,
                gl::STREAM_DRAW,
            );

            gl::DrawArrays(gl::TRIANGLES, 0, self.vertices.len() as i32);

            gl::UseProgram(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }
    }

    fn push_quad(&mut self, half_width: f32, half_height: f32, quad: &UiQuad) {
        // Top-left origin pixel rect -> NDC (Y points up). The geometry only
        // spans the visible `v_range` band; uv carries the band through to the
        // fragment shader so the SDF still sees the full-size quad.
        let [v0, v1] = quad.v_range;
        let x = quad.x / half_width - 1.0;
        let y = -(quad.y + v0 * quad.height) / half_height + 1.0;
        let y_bot = -(quad.y + v1 * quad.height) / half_height + 1.0;
        let w = quad.width / half_width;

        let [gx, gy] = quad.gradient.axis();
        let c0 = [quad.color0.r, quad.color0.g, quad.color0.b, quad.color0.a];
        let c1 = [quad.color1.r, quad.color1.g, quad.color1.b, quad.color1.a];

        let vertex = |x: f32, y: f32, u: f32, v: f32| UiVertex {
            x,
            y,
            u,
            v,
            w: quad.width,
            h: quad.height,
            radius: quad.radius,
            feather: quad.feather,
            gx,
            gy,
            corners: quad.corner_radii,
            c0,
            c1,
        };

        let tl;
        let bl;
        let tr;
        let br;
        if let Some(corners) = quad.corners {
            // Explicit pixel corners -> NDC (flat-filled slanted polygon).
            let ndc = |p: [f32; 2]| (p[0] / half_width - 1.0, -p[1] / half_height + 1.0);
            let (tlx, tly) = ndc(corners[0]);
            let (blx, bly) = ndc(corners[1]);
            let (trx, tr_y) = ndc(corners[2]);
            let (brx, bry) = ndc(corners[3]);
            tl = vertex(tlx, tly, 0.0, 0.0);
            bl = vertex(blx, bly, 0.0, 1.0);
            tr = vertex(trx, tr_y, 1.0, 0.0);
            br = vertex(brx, bry, 1.0, 1.0);
        } else {
            tl = vertex(x, y, 0.0, v0);
            bl = vertex(x, y_bot, 0.0, v1);
            tr = vertex(x + w, y, 1.0, v0);
            br = vertex(x + w, y_bot, 1.0, v1);
        }

        self.vertices.push(tl);
        self.vertices.push(bl);
        self.vertices.push(tr);
        self.vertices.push(tr);
        self.vertices.push(br);
        self.vertices.push(bl);
    }
}

impl Drop for UiRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_geometry_includes_blur_and_vertical_offset() {
        let shadow = UiQuad::shadow(10.0, 20.0, 100.0, 80.0, 8.0, 12.0, 4.0, Rgba::new(0, 0, 0, 54));
        assert_eq!((shadow.x, shadow.y), (-2.0, 12.0));
        assert_eq!((shadow.width, shadow.height), (124.0, 104.0));
        assert_eq!(shadow.corner_radii, [8.0; 4]);
        assert_eq!(shadow.feather, -12.0);
    }
}
