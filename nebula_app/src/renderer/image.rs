//! Background image renderer for Nebula chrome.
//!
//! This is deliberately separate from the terminal text renderer: wallpapers are
//! a full-window backdrop drawn after `glClear` and before terminal cells, so
//! blank cells stay transparent while real cell backgrounds/cursor overlays keep
//! their normal priority.
//!
//! The same shader also draws OSC 1337 inline images: those arrive as decoded
//! RGBA buffers (not paths) and render at explicit pixel rects above the cells.

use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::info;

use crate::display::SizeInfo;
use crate::gl;
use crate::gl::types::*;
use crate::renderer;
use crate::renderer::shader::{ShaderProgram, ShaderVersion};

const IMAGE_SHADER_F: &str = include_str!("../../res/image.f.glsl");
const IMAGE_SHADER_V: &str = include_str!("../../res/image.v.glsl");

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ImageVertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

#[derive(Debug)]
struct CachedImage {
    path: PathBuf,
    texture: GLuint,
    width: u32,
    height: u32,
}

impl CachedImage {
    fn load(path: &Path) -> Result<Self, String> {
        let decoded = decode_png(path)?;
        let texture = upload_rgba_texture(decoded.width, decoded.height, &decoded.rgba);
        Ok(Self { path: path.to_path_buf(), texture, width: decoded.width, height: decoded.height })
    }
}

/// Upload an RGBA8 buffer as a GL texture (linear filtering, edge clamp).
fn upload_rgba_texture(width: u32, height: u32, rgba: &[u8]) -> GLuint {
    let mut texture: GLuint = 0;
    unsafe {
        gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
        gl::GenTextures(1, &mut texture);
        gl::BindTexture(gl::TEXTURE_2D, texture);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RGBA as i32,
            width as i32,
            height as i32,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            rgba.as_ptr().cast(),
        );
        gl::BindTexture(gl::TEXTURE_2D, 0);
    }
    texture
}

impl Drop for CachedImage {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.texture);
        }
    }
}

#[derive(Debug)]
struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[cfg(all(feature = "png", not(target_os = "macos")))]
fn decode_png(path: &Path) -> Result<DecodedImage, String> {
    use std::fs::File;
    use std::io::BufReader;

    let file = File::open(path).map_err(|err| format!("open {path:?}: {err}"))?;
    decode_png_reader(BufReader::new(file))
}

/// Decode PNG bytes (an OSC 1337 payload) into RGBA8. Public so the event
/// layer can decode off the render path.
#[cfg(all(feature = "png", not(target_os = "macos")))]
pub fn decode_png_bytes(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    decode_png_reader(std::io::Cursor::new(png))
        .map(|decoded| (decoded.width, decoded.height, decoded.rgba))
}

#[cfg(any(not(feature = "png"), target_os = "macos"))]
pub fn decode_png_bytes(_png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    Err("PNG support is not enabled for this build".to_owned())
}

#[cfg(all(feature = "png", not(target_os = "macos")))]
fn decode_png_reader<R: std::io::Read>(reader: R) -> Result<DecodedImage, String> {
    let mut decoder = png::Decoder::new(reader);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|err| format!("decode header: {err}"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|err| format!("decode frame: {err}"))?;
    let bytes = &buffer[..info.buffer_size()];

    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(bytes.len() / 3 * 4);
            for chunk in bytes.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(u8::MAX);
            }
            rgba
        },
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(bytes.len() / 2 * 4);
            for chunk in bytes.chunks_exact(2) {
                let gray = chunk[0];
                rgba.extend_from_slice(&[gray, gray, gray, chunk[1]]);
            }
            rgba
        },
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(bytes.len() * 4);
            for gray in bytes {
                rgba.extend_from_slice(&[*gray, *gray, *gray, u8::MAX]);
            }
            rgba
        },
        png::ColorType::Indexed => {
            return Err("indexed PNG did not expand to RGB/RGBA".to_owned());
        },
    };

    if rgba.len() != info.width as usize * info.height as usize * 4 {
        return Err("decoded PNG size mismatch".to_owned());
    }

    Ok(DecodedImage { width: info.width, height: info.height, rgba })
}

#[cfg(any(not(feature = "png"), target_os = "macos"))]
fn decode_png(_path: &Path) -> Result<DecodedImage, String> {
    Err("PNG background support is not enabled for this build".to_owned())
}

/// A GPU texture for one inline image, keyed by the image's id.
#[derive(Debug)]
struct InlineTexture {
    texture: GLuint,
}

impl Drop for InlineTexture {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.texture);
        }
    }
}

#[derive(Debug)]
pub struct ImageRenderer {
    vao: GLuint,
    vbo: GLuint,
    program: ShaderProgram,
    u_texture: GLint,
    u_opacity: GLint,
    image: Option<CachedImage>,
    failed_path: Option<PathBuf>,
    /// Lazily-uploaded textures for OSC 1337 inline images.
    inline: HashMap<u64, InlineTexture>,
}

impl ImageRenderer {
    pub fn new(shader_version: ShaderVersion) -> Result<Self, renderer::Error> {
        let program = ShaderProgram::new(shader_version, None, IMAGE_SHADER_V, IMAGE_SHADER_F)?;
        let u_texture = program.get_uniform_location(c"uTexture")?;
        let u_opacity = program.get_uniform_location(c"uOpacity")?;

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;

        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let stride = mem::size_of::<ImageVertex>() as i32;
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, std::ptr::null());
            gl::EnableVertexAttribArray(0);

            let uv_offset = (mem::size_of::<f32>() * 2) as *const _;
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, stride, uv_offset);
            gl::EnableVertexAttribArray(1);

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }

        Ok(Self {
            vao,
            vbo,
            program,
            u_texture,
            u_opacity,
            image: None,
            failed_path: None,
            inline: HashMap::new(),
        })
    }

    /// Draw one inline image at a pixel rect, uploading its texture on first
    /// use. `rgba`/`px` describe the decoded pixels; `rect` is x/y/w/h in
    /// window pixels.
    pub fn draw_inline(
        &mut self,
        size_info: &SizeInfo,
        id: u64,
        rgba: &Arc<Vec<u8>>,
        px: (u32, u32),
        rect: (f32, f32, f32, f32),
    ) {
        // Cheap displacement cap: inline textures rarely exceed a handful,
        // but a runaway imgcat loop must not exhaust VRAM.
        if self.inline.len() > 32 {
            self.inline.clear();
        }
        let texture = self
            .inline
            .entry(id)
            .or_insert_with(|| InlineTexture {
                texture: upload_rgba_texture(px.0, px.1, rgba),
            })
            .texture;

        let (x, y, w, h) = rect;
        let vertices = rect_vertices(size_info.width(), size_info.height(), x, y, w, h);

        unsafe {
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::UseProgram(self.program.id());

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::Uniform1i(self.u_texture, 0);
            gl::Uniform1f(self.u_opacity, 1.0);

            gl::BufferData(
                gl::ARRAY_BUFFER,
                (vertices.len() * mem::size_of::<ImageVertex>()) as isize,
                vertices.as_ptr().cast(),
                gl::STREAM_DRAW,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, vertices.len() as i32);

            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::UseProgram(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }
    }

    /// Drop inline textures whose ids are gone from every pane.
    pub fn retain_inline_images(&mut self, alive: impl Fn(u64) -> bool) {
        self.inline.retain(|id, _| alive(*id));
    }

    pub fn draw(&mut self, size_info: &SizeInfo, path: &Path, opacity: f32) {
        if opacity <= 0.0 || !self.ensure_image(path) {
            return;
        }

        let image = match self.image.as_ref() {
            Some(image) => image,
            None => return,
        };

        let vertices = cover_vertices(
            size_info.width(),
            size_info.height(),
            image.width as f32,
            image.height as f32,
        );

        unsafe {
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::UseProgram(self.program.id());

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, image.texture);
            gl::Uniform1i(self.u_texture, 0);
            gl::Uniform1f(self.u_opacity, opacity.clamp(0.0, 1.0));

            gl::BufferData(
                gl::ARRAY_BUFFER,
                (vertices.len() * mem::size_of::<ImageVertex>()) as isize,
                vertices.as_ptr().cast(),
                gl::STREAM_DRAW,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, vertices.len() as i32);

            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::UseProgram(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }
    }

    pub fn invalidate(&mut self) {
        self.image = None;
        self.failed_path = None;
    }

    fn ensure_image(&mut self, path: &Path) -> bool {
        if self.image.as_ref().is_some_and(|image| image.path == path) {
            return true;
        }
        if self.failed_path.as_deref() == Some(path) {
            return false;
        }

        self.image = None;
        match CachedImage::load(path) {
            Ok(image) => {
                info!(
                    "Loaded Nebula background image {:?} ({}x{})",
                    image.path, image.width, image.height
                );
                self.failed_path = None;
                self.image = Some(image);
                true
            },
            Err(err) => {
                info!("Failed to load Nebula background image {:?}: {}", path, err);
                self.failed_path = Some(path.to_path_buf());
                false
            },
        }
    }
}

impl Drop for ImageRenderer {
    fn drop(&mut self) {
        // Drop the texture before deleting the buffers; Display makes the GL
        // context current before dropping Renderer.
        self.image = None;
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
        }
    }
}

fn cover_vertices(window_w: f32, window_h: f32, image_w: f32, image_h: f32) -> [ImageVertex; 6] {
    let window_w = window_w.max(1.0);
    let window_h = window_h.max(1.0);
    let image_w = image_w.max(1.0);
    let image_h = image_h.max(1.0);

    // CSS `background-size: cover`: preserve aspect ratio, crop only at the
    // edges. This avoids the "stretched wallpaper" feel that makes terminals
    // look cheap.
    let scale = (window_w / image_w).max(window_h / image_h);
    let draw_w = image_w * scale;
    let draw_h = image_h * scale;
    let x0 = (window_w - draw_w) * 0.5;
    let y0 = (window_h - draw_h) * 0.5;
    quad_vertices(window_w, window_h, x0, y0, draw_w, draw_h)
}

/// Vertices for an image drawn 1:1 at an explicit pixel rect.
fn rect_vertices(window_w: f32, window_h: f32, x: f32, y: f32, w: f32, h: f32) -> [ImageVertex; 6] {
    quad_vertices(window_w.max(1.0), window_h.max(1.0), x, y, w.max(1.0), h.max(1.0))
}

fn quad_vertices(
    window_w: f32,
    window_h: f32,
    x0: f32,
    y0: f32,
    draw_w: f32,
    draw_h: f32,
) -> [ImageVertex; 6] {
    let x1 = x0 + draw_w;
    let y1 = y0 + draw_h;

    let ndc = |x: f32, y: f32| ImageVertex {
        x: x / (window_w * 0.5) - 1.0,
        y: 1.0 - y / (window_h * 0.5),
        u: 0.0,
        v: 0.0,
    };

    let mut tl = ndc(x0, y0);
    tl.u = 0.0;
    tl.v = 0.0;
    let mut bl = ndc(x0, y1);
    bl.u = 0.0;
    bl.v = 1.0;
    let mut tr = ndc(x1, y0);
    tr.u = 1.0;
    tr.v = 0.0;
    let mut br = ndc(x1, y1);
    br.u = 1.0;
    br.v = 1.0;

    [tl, bl, tr, tr, br, bl]
}
