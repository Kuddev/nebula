//! 固定 1024×1024 RGBA 数学字形图集和批量 quad 渲染。

use std::collections::HashMap;
use std::mem;

use ahash::RandomState;

use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::gl;
use crate::gl::types::*;
use crate::math::layout::MathLayout;
use crate::math::rasterizer::{MathGlyphRasterizer, RasterizedMathGlyph};
use crate::math::{MathError, MathErrorKind};
use crate::renderer;
use crate::renderer::shader::{ShaderProgram, ShaderVersion};

const MATH_SHADER_F: &str = include_str!("../../res/math.f.glsl");
const MATH_SHADER_V: &str = include_str!("../../res/math.v.glsl");
pub(crate) const MATH_ATLAS_SIZE: u16 = 1024;
const ATLAS_GAP: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    glyph_id: u16,
    pixel_size_bits: u32,
}

#[derive(Clone, Copy, Debug)]
struct AtlasGlyph {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    left: i16,
    top: i16,
    epoch: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AtlasSlot {
    x: u16,
    y: u16,
}

#[derive(Debug)]
struct ShelfAtlas {
    x: u16,
    y: u16,
    row_height: u16,
    epoch: u32,
}

impl Default for ShelfAtlas {
    fn default() -> Self {
        Self { x: 0, y: 0, row_height: 0, epoch: 1 }
    }
}

impl ShelfAtlas {
    fn allocate(&mut self, width: u16, height: u16) -> Option<AtlasSlot> {
        if width == 0 || height == 0 || width > MATH_ATLAS_SIZE || height > MATH_ATLAS_SIZE {
            return None;
        }
        let padded_width = width.checked_add(ATLAS_GAP)?;
        let padded_height = height.checked_add(ATLAS_GAP)?;
        if self.x.checked_add(padded_width)? > MATH_ATLAS_SIZE {
            self.x = 0;
            self.y = self.y.checked_add(self.row_height)?;
            self.row_height = 0;
        }
        if self.y.checked_add(padded_height)? > MATH_ATLAS_SIZE {
            return None;
        }
        let slot = AtlasSlot { x: self.x, y: self.y };
        self.x += padded_width;
        self.row_height = self.row_height.max(padded_height);
        Some(slot)
    }

    fn reset(&mut self) {
        self.x = 0;
        self.y = 0;
        self.row_height = 0;
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MathVertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MathClip {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
}

#[derive(Debug)]
pub(crate) struct MathRenderer {
    vao: GLuint,
    vbo: GLuint,
    texture: GLuint,
    program: ShaderProgram,
    u_texture: GLint,
    u_color: GLint,
    atlas: ShelfAtlas,
    cache: HashMap<GlyphKey, AtlasGlyph, RandomState>,
    rasterizer: MathGlyphRasterizer,
    vertices: Vec<MathVertex>,
}

impl MathRenderer {
    pub(crate) fn new(shader_version: ShaderVersion) -> Result<Self, renderer::Error> {
        let program = ShaderProgram::new(shader_version, None, MATH_SHADER_V, MATH_SHADER_F)?;
        let u_texture = program.get_uniform_location(c"uTexture")?;
        let u_color = program.get_uniform_location(c"uColor")?;
        let rasterizer = MathGlyphRasterizer::new()
            .map_err(|_| renderer::Error::Other("failed to load bundled math font".to_owned()))?;
        let mut vao = 0;
        let mut vbo = 0;
        let mut texture = 0;
        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);
            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            let stride = mem::size_of::<MathVertex>() as i32;
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, std::ptr::null());
            gl::EnableVertexAttribArray(0);
            let uv_offset = (mem::size_of::<f32>() * 2) as *const _;
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, stride, uv_offset);
            gl::EnableVertexAttribArray(1);
            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);

            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            allocate_texture();
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        Ok(Self {
            vao,
            vbo,
            texture,
            program,
            u_texture,
            u_color,
            atlas: ShelfAtlas::default(),
            cache: HashMap::with_hasher(RandomState::default()),
            rasterizer,
            vertices: Vec::new(),
        })
    }

    pub(crate) fn draw(
        &mut self,
        size: &SizeInfo,
        layout: &MathLayout,
        origin_x: f32,
        baseline_y: f32,
        color: Rgb,
        clip: MathClip,
    ) -> Result<(), MathError> {
        self.vertices.clear();
        for op in &layout.glyphs {
            let key = GlyphKey { glyph_id: op.glyph_id, pixel_size_bits: op.pixel_size.to_bits() };
            let glyph = match self.cache.get(&key).copied() {
                Some(glyph) if glyph.epoch == self.atlas.epoch => glyph,
                _ => {
                    let rasterized = self.rasterizer.rasterize(op.glyph_id, op.pixel_size)?;
                    if rasterized.width == 0 || rasterized.height == 0 {
                        continue;
                    }
                    match self.upload(key, &rasterized) {
                        Ok(glyph) => glyph,
                        Err(MathError { kind: MathErrorKind::AtlasFull, .. }) => {
                            self.flush(size, color);
                            self.clear_atlas();
                            self.upload(key, &rasterized)?
                        },
                        Err(error) => return Err(error),
                    }
                },
            };
            // The atlas already contains antialiased coverage. Sampling it
            // again from fractional screen coordinates softens every edge a
            // second time, so snap only the final bitmap origin while keeping
            // TeX advances and rule geometry at full precision.
            let x = (origin_x + op.x + glyph.left as f32).round();
            let y = (baseline_y + op.baseline_y - glyph.top as f32).round();
            self.push_clipped_quad(size, x, y, glyph, clip);
        }
        self.flush(size, color);
        Ok(())
    }

    fn upload(
        &mut self,
        key: GlyphKey,
        rasterized: &RasterizedMathGlyph,
    ) -> Result<AtlasGlyph, MathError> {
        let slot = self
            .atlas
            .allocate(rasterized.width, rasterized.height)
            .ok_or_else(|| MathError::new(MathErrorKind::AtlasFull, 0))?;
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                slot.x as i32,
                slot.y as i32,
                rasterized.width as i32,
                rasterized.height as i32,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                rasterized.rgba.as_ptr().cast(),
            );
        }
        let glyph = AtlasGlyph {
            x: slot.x,
            y: slot.y,
            width: rasterized.width,
            height: rasterized.height,
            left: rasterized.left,
            top: rasterized.top,
            epoch: self.atlas.epoch,
        };
        self.cache.insert(key, glyph);
        Ok(glyph)
    }

    fn clear_atlas(&mut self) {
        self.cache.clear();
        self.atlas.reset();
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            allocate_texture();
        }
    }

    fn push_clipped_quad(
        &mut self,
        size: &SizeInfo,
        x: f32,
        y: f32,
        glyph: AtlasGlyph,
        clip: MathClip,
    ) {
        let width = glyph.width as f32;
        let height = glyph.height as f32;
        let left = x.max(clip.left);
        let top = y.max(clip.top);
        let right = (x + width).min(clip.right);
        let bottom = (y + height).min(clip.bottom);
        if right <= left || bottom <= top {
            return;
        }
        let u0 = (glyph.x as f32 + left - x) / MATH_ATLAS_SIZE as f32;
        let v0 = (glyph.y as f32 + top - y) / MATH_ATLAS_SIZE as f32;
        let u1 = (glyph.x as f32 + right - x) / MATH_ATLAS_SIZE as f32;
        let v1 = (glyph.y as f32 + bottom - y) / MATH_ATLAS_SIZE as f32;
        let ndc = |px: f32, py: f32, u: f32, v: f32| MathVertex {
            x: px / (size.width() * 0.5) - 1.0,
            y: 1.0 - py / (size.height() * 0.5),
            u,
            v,
        };
        let tl = ndc(left, top, u0, v0);
        let bl = ndc(left, bottom, u0, v1);
        let tr = ndc(right, top, u1, v0);
        let br = ndc(right, bottom, u1, v1);
        self.vertices.extend_from_slice(&[tl, bl, tr, tr, br, bl]);
    }

    fn flush(&mut self, size: &SizeInfo, color: Rgb) {
        if self.vertices.is_empty() {
            return;
        }
        unsafe {
            gl::Viewport(0, 0, size.width() as i32, size.height() as i32);
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::UseProgram(self.program.id());
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            gl::Uniform1i(self.u_texture, 0);
            gl::Uniform4f(
                self.u_color,
                color.r as f32 / 255.0,
                color.g as f32 / 255.0,
                color.b as f32 / 255.0,
                1.0,
            );
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertices.len() * mem::size_of::<MathVertex>()) as isize,
                self.vertices.as_ptr().cast(),
                gl::STREAM_DRAW,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, self.vertices.len() as i32);
            gl::UseProgram(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }
        self.vertices.clear();
    }
}

unsafe fn allocate_texture() {
    unsafe {
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RGBA as i32,
            MATH_ATLAS_SIZE as i32,
            MATH_ATLAS_SIZE as i32,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );
    }
}

impl Drop for MathRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.texture);
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_allocator_is_bounded_and_epoch_changes_on_reset() {
        let mut atlas = ShelfAtlas::default();
        let epoch = atlas.epoch;
        let mut slots = 0;
        while atlas.allocate(127, 127).is_some() {
            slots += 1;
        }
        assert!(slots > 0);
        assert!(atlas.allocate(MATH_ATLAS_SIZE, 1).is_none());
        atlas.reset();
        assert_ne!(atlas.epoch, epoch);
        assert_eq!(atlas.allocate(127, 127), Some(AtlasSlot { x: 0, y: 0 }));
    }
}
