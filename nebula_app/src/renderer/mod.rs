use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt, ptr};

use ahash::RandomState;
use crossfont::Metrics;
use glutin::context::{ContextApi, GlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use log::{LevelFilter, debug, info};
use unicode_width::UnicodeWidthChar;

use nebula_terminal::grid::Dimensions;
use nebula_terminal::index::{Column, Point};
use nebula_terminal::term::cell::Flags;

use crate::config::debug::RendererPreference;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::gl;
use crate::renderer::image::ImageRenderer;
use crate::renderer::math::{MathClip, MathRenderer};
use crate::renderer::rects::{RectRenderer, RenderRect};
use crate::renderer::shader::ShaderError;
use crate::renderer::ui::{UiQuad, UiRenderer};

pub mod image;
pub(crate) mod math;
pub mod platform;
pub mod rects;
mod shader;
mod text;
pub mod ui;
#[cfg(feature = "wgpu-foundation")]
pub mod wgpu_foundation;
pub(crate) use text::Rasterizer;

pub use text::{GlyphCache, LoaderApi};

use shader::ShaderVersion;
use text::{Gles2Renderer, Glsl3Renderer, TextRenderer, TextShader};

/// Whether the OpenGL functions have been loaded.
pub static GL_FUNS_LOADED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum Error {
    /// Shader error.
    Shader(ShaderError),

    /// Other error.
    Other(String),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Shader(err) => err.source(),
            Error::Other(_) => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Shader(err) => {
                write!(f, "There was an error initializing the shaders: {err}")
            },
            Error::Other(err) => {
                write!(f, "{err}")
            },
        }
    }
}

impl From<ShaderError> for Error {
    fn from(val: ShaderError) -> Self {
        Error::Shader(val)
    }
}

impl From<String> for Error {
    fn from(val: String) -> Self {
        Error::Other(val)
    }
}

#[derive(Debug)]
enum TextRendererProvider {
    Gles2(Gles2Renderer),
    Glsl3(Glsl3Renderer),
}

#[derive(Debug)]
pub struct Renderer {
    text_renderer: TextRendererProvider,
    rect_renderer: RectRenderer,
    ui_renderer: UiRenderer,
    image_renderer: ImageRenderer,
    math_renderer: MathRenderer,
    robustness: bool,
    /// Full window height in physical pixels, used to flip pane viewports from
    /// top-down `SizeInfo` coordinates into OpenGL's bottom-left origin. Set
    /// each frame from the window's `SizeInfo`.
    window_height: std::cell::Cell<f32>,
}

/// Wrapper around gl::GetString with error checking and reporting.
fn gl_get_string(
    string_id: gl::types::GLenum,
    description: &str,
) -> Result<Cow<'static, str>, Error> {
    unsafe {
        let string_ptr = gl::GetString(string_id);
        match gl::GetError() {
            gl::NO_ERROR if !string_ptr.is_null() => {
                Ok(CStr::from_ptr(string_ptr as *const _).to_string_lossy())
            },
            gl::INVALID_ENUM => {
                Err(format!("OpenGL error requesting {description}: invalid enum").into())
            },
            error_id => Err(format!("OpenGL error {error_id} requesting {description}").into()),
        }
    }
}

impl Renderer {
    /// Create a new renderer.
    ///
    /// This will automatically pick between the GLES2 and GLSL3 renderer based on the GPU's
    /// supported OpenGL version.
    pub fn new(
        context: &PossiblyCurrentContext,
        renderer_preference: Option<RendererPreference>,
    ) -> Result<Self, Error> {
        // We need to load OpenGL functions once per instance, but only after we make our context
        // current due to WGL limitations.
        if !GL_FUNS_LOADED.swap(true, Ordering::Relaxed) {
            let gl_display = context.display();
            gl::load_with(|symbol| {
                let symbol = CString::new(symbol).unwrap();
                gl_display.get_proc_address(symbol.as_c_str()).cast()
            });
        }

        let shader_version = gl_get_string(gl::SHADING_LANGUAGE_VERSION, "shader version")?;
        let gl_version = gl_get_string(gl::VERSION, "OpenGL version")?;
        let renderer = gl_get_string(gl::RENDERER, "renderer version")?;

        info!("Running on {renderer}");
        info!("OpenGL version {gl_version}, shader_version {shader_version}");

        // Check if robustness is supported.
        let robustness = Self::supports_robustness();

        let is_gles_context = matches!(context.context_api(), ContextApi::Gles(_));

        // Use the config option to enforce a particular renderer configuration.
        let (use_glsl3, allow_dsb) = match renderer_preference {
            Some(RendererPreference::Glsl3) => (true, true),
            Some(RendererPreference::Gles2) => (false, true),
            Some(RendererPreference::Gles2Pure) => (false, false),
            None => (shader_version.as_ref() >= "3.3" && !is_gles_context, true),
        };

        let (text_renderer, rect_renderer, ui_renderer, image_renderer, math_renderer) =
            if use_glsl3 {
                let text_renderer = TextRendererProvider::Glsl3(Glsl3Renderer::new()?);
                let rect_renderer = RectRenderer::new(ShaderVersion::Glsl3)?;
                let ui_renderer = UiRenderer::new(ShaderVersion::Glsl3)?;
                let image_renderer = ImageRenderer::new(ShaderVersion::Glsl3)?;
                let math_renderer = MathRenderer::new(ShaderVersion::Glsl3)?;
                (text_renderer, rect_renderer, ui_renderer, image_renderer, math_renderer)
            } else {
                let text_renderer =
                    TextRendererProvider::Gles2(Gles2Renderer::new(allow_dsb, is_gles_context)?);
                let rect_renderer = RectRenderer::new(ShaderVersion::Gles2)?;
                let ui_renderer = UiRenderer::new(ShaderVersion::Gles2)?;
                let image_renderer = ImageRenderer::new(ShaderVersion::Gles2)?;
                let math_renderer = MathRenderer::new(ShaderVersion::Gles2)?;
                (text_renderer, rect_renderer, ui_renderer, image_renderer, math_renderer)
            };

        // Enable debug logging for OpenGL as well.
        if log::max_level() >= LevelFilter::Debug && GlExtensions::contains("GL_KHR_debug") {
            debug!("Enabled debug logging for OpenGL");
            unsafe {
                gl::Enable(gl::DEBUG_OUTPUT);
                gl::Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);
                gl::DebugMessageCallback(Some(gl_debug_log), ptr::null_mut());
            }
        }

        Ok(Self {
            text_renderer,
            rect_renderer,
            ui_renderer,
            image_renderer,
            math_renderer,
            robustness,
            window_height: std::cell::Cell::new(0.0),
        })
    }

    pub fn draw_cells<I: Iterator<Item = RenderableCell>>(
        &mut self,
        size_info: &SizeInfo,
        glyph_cache: &mut GlyphCache,
        cells: I,
    ) {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => {
                renderer.draw_cells(size_info, glyph_cache, cells)
            },
            TextRendererProvider::Glsl3(renderer) => {
                renderer.draw_cells(size_info, glyph_cache, cells)
            },
        }
    }

    /// Draw a string in a variable location. Used for printing the render timer, warnings and
    /// errors.
    pub fn draw_string(
        &mut self,
        point: Point<usize>,
        fg: Rgb,
        bg: Rgb,
        string_chars: impl Iterator<Item = char>,
        size_info: &SizeInfo,
        glyph_cache: &mut GlyphCache,
    ) {
        // Lay out by display width: a wide char occupies two columns and is
        // flagged so the glyph rasterizes double-width. The input is a plain
        // string — nothing here consumes "spacer" characters; treating the
        // character AFTER a wide char as its spacer used to swallow every
        // second CJK glyph in ghost hints and chrome labels.
        let columns = size_info.columns();
        let mut offset = 0usize;
        let cells = string_chars.filter_map(move |character| {
            let width = character.width().unwrap_or(0);
            // Zero-width has nothing to lay out; anything past the row's
            // right edge clips instead of bleeding out of the grid.
            if width == 0 || point.column.0 + offset + width > columns {
                return None;
            }
            let flags = if width == 2 { Flags::WIDE_CHAR } else { Flags::empty() };
            let cell = RenderableCell {
                point: Point::new(point.line, point.column + offset),
                character,
                extra: None,
                flags,
                bg_alpha: 1.0,
                fg,
                bg,
                underline: fg,
            };
            offset += width;
            Some(cell)
        });

        self.draw_cells(size_info, glyph_cache, cells);
    }

    pub fn with_loader<F, T>(&mut self, func: F) -> T
    where
        F: FnOnce(LoaderApi<'_>) -> T,
    {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.with_loader(func),
            TextRendererProvider::Glsl3(renderer) => renderer.with_loader(func),
        }
    }

    /// Set up the text program to draw glyphs at an arbitrary pixel anchor over
    /// the full window, for chrome labels living outside the terminal grid.
    fn begin_chrome_text(&self, size_info: &SizeInfo, anchor_x: f32, anchor_y: f32) {
        self.begin_chrome_text_scaled(size_info, anchor_x, anchor_y, 1.0);
    }

    /// Like [`Self::begin_chrome_text`] but magnifies glyph geometry by `mult`
    /// from the top-left anchor, for oversized chrome titles. The glyph bitmaps
    /// are scaled by the GPU (they're rasterized at the terminal font size), so
    /// keep `mult` modest — roughly ≤ 1.6 — to stay crisp.
    fn begin_chrome_text_scaled(
        &self,
        size_info: &SizeInfo,
        anchor_x: f32,
        anchor_y: f32,
        mult: f32,
    ) {
        // Snap the anchor to whole physical pixels. Chrome anchors come out of
        // layout math full of `* scale` and `/ 2.0` fractions; a fractional
        // anchor shifts every glyph off the texel grid and the atlas's LINEAR
        // sampling turns that into blur plus bleed from neighbouring atlas
        // entries (the "dirty hairline on characters" artifact).
        let (anchor_x, anchor_y) = (anchor_x.round(), anchor_y.round());
        let w = size_info.width();
        let h = size_info.height();

        // Map full-window pixel space to NDC, with the cell origin at the anchor.
        // Scaling the NDC delta (not the offset) grows glyphs and advances alike
        // away from the fixed anchor.
        let offset_x = -1.0 + 2.0 * anchor_x / w;
        let offset_y = 1.0 - 2.0 * anchor_y / h;
        let scale_x = 2.0 / w * mult;
        let scale_y = -2.0 / h * mult;

        let (id, u_projection) = match &self.text_renderer {
            TextRendererProvider::Gles2(r) => (r.program().id(), r.program().projection_uniform()),
            TextRendererProvider::Glsl3(r) => (r.program().id(), r.program().projection_uniform()),
        };

        unsafe {
            gl::Viewport(0, 0, w as i32, h as i32);
            gl::UseProgram(id);
            gl::Uniform4f(u_projection, offset_x, offset_y, scale_x, scale_y);
            gl::UseProgram(0);
        }
    }

    /// Restore the grid projection and inset viewport after chrome text.
    fn end_chrome_text(&self, size_info: &SizeInfo) {
        self.resize(size_info);
    }

    /// Draw a chrome label at an arbitrary pixel position (top-left of the first
    /// cell), with a transparent background so the underlying pill shows.
    pub fn draw_chrome_text(
        &mut self,
        size_info: &SizeInfo,
        x: f32,
        y: f32,
        fg: Rgb,
        text: &str,
        glyph_cache: &mut GlyphCache,
    ) {
        self.draw_chrome_text_styled(size_info, x, y, fg, Flags::empty(), text, glyph_cache);
    }

    /// [`Self::draw_chrome_text`] with extra cell style flags. `BOLD`/`ITALIC`
    /// select the real bold/italic faces in `draw_cells` — the document viewer
    /// uses this so emphasis is carried by the typeface, not just ink color.
    pub fn draw_chrome_text_styled(
        &mut self,
        size_info: &SizeInfo,
        x: f32,
        y: f32,
        fg: Rgb,
        style: Flags,
        text: &str,
        glyph_cache: &mut GlyphCache,
    ) {
        self.begin_chrome_text(size_info, x, y);

        // Lay characters out by their display width. Unlike `draw_string` (which
        // mirrors the terminal grid, where a wide char is already followed by a
        // spacer cell), a chrome label is a plain `&str` with no spacer chars —
        // so we must NOT skip the char after a CJK glyph. We instead advance the
        // column cursor by the glyph's width (2 cells for wide, 1 otherwise) and
        // tag wide glyphs with `WIDE_CHAR` so the shader draws them double-width.
        let mut col = 0usize;
        let cells = text.chars().filter_map(|character| {
            let width = character.width().unwrap_or(0);
            if width == 0 {
                return None; // combining/zero-width marks have no cell of their own
            }
            let flags = if width == 2 { Flags::WIDE_CHAR | style } else { style };
            let cell = RenderableCell {
                point: Point::new(0, Column(col)),
                character,
                extra: None,
                flags,
                bg_alpha: 0.0,
                fg,
                bg: Rgb::new(0, 0, 0),
                underline: fg,
            };
            col += width;
            Some(cell)
        });

        self.draw_cells(size_info, glyph_cache, cells);

        self.end_chrome_text(size_info);
    }

    /// Like [`draw_chrome_text`], but scales the glyphs about the `(x, y)` anchor
    /// by `mult` (1.0 = normal). The atlas glyphs stay cell-sized; the projection
    /// stretches their quads, so a title can be drawn larger than the terminal
    /// font without a second rasterization. Advance width scales too, keeping the
    /// run tightly kerned. Returns the drawn width in pixels so callers can lay
    /// out following content.
    pub fn draw_chrome_text_scaled(
        &mut self,
        size_info: &SizeInfo,
        x: f32,
        y: f32,
        mult: f32,
        fg: Rgb,
        text: &str,
        glyph_cache: &mut GlyphCache,
    ) -> f32 {
        self.begin_chrome_text_scaled(size_info, x, y, mult);

        let mut col = 0usize;
        let cells = text.chars().filter_map(|character| {
            let width = character.width().unwrap_or(0);
            if width == 0 {
                return None;
            }
            let flags = if width == 2 { Flags::WIDE_CHAR } else { Flags::empty() };
            let cell = RenderableCell {
                point: Point::new(0, Column(col)),
                character,
                extra: None,
                flags,
                bg_alpha: 0.0,
                fg,
                bg: Rgb::new(0, 0, 0),
                underline: fg,
            };
            col += width;
            Some(cell)
        });

        self.draw_cells(size_info, glyph_cache, cells);

        self.end_chrome_text(size_info);
        col as f32 * size_info.cell_width() * mult
    }

    /// Draw document text at `scale` × the terminal font size, rasterized at
    /// the REAL target size — unlike [`Self::draw_chrome_text_scaled`], which
    /// stretches base-size atlas bitmaps on the GPU and goes fuzzy past ~1.2×.
    ///
    /// `GlyphKey.size` carries the font size per glyph, so scaling is just a
    /// temporary bump of `glyph_cache.font_size` (distinct sizes cache as
    /// distinct atlas entries; a handful of heading tiers stays cheap). The
    /// grid pipeline positions glyphs inside a base-metrics cell, so each char
    /// is drawn as its own one-cell run at a pixel anchor, advanced by the
    /// scaled width — headings are a few dozen chars, the extra draws are noise.
    ///
    /// Returns the advance width in pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_doc_text(
        &mut self,
        size_info: &SizeInfo,
        x: f32,
        y: f32,
        scale: f32,
        fg: Rgb,
        style: Flags,
        text: &str,
        glyph_cache: &mut GlyphCache,
    ) -> f32 {
        self.draw_doc_text_tracked(size_info, x, y, scale, 0.0, fg, style, text, glyph_cache)
    }

    /// Document text with explicit physical-pixel tracking between glyphs.
    /// This is reserved for compact UI labels; terminal cells must remain fixed.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_doc_text_tracked(
        &mut self,
        size_info: &SizeInfo,
        x: f32,
        y: f32,
        scale: f32,
        tracking: f32,
        fg: Rgb,
        style: Flags,
        text: &str,
        glyph_cache: &mut GlyphCache,
    ) -> f32 {
        let cell_w = size_info.cell_width();
        if (scale - 1.0).abs() < 0.01 && tracking.abs() < 0.01 {
            self.draw_chrome_text_styled(size_info, x, y, fg, style, text, glyph_cache);
            let cols: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
            return cols as f32 * cell_w;
        }

        let base_size = glyph_cache.font_size;
        // `Size::new` takes POINTS; feeding it `as_px()` (pt × 96/72) inflates
        // every scaled glyph by 1.33× over the advance it's stepped by —
        // headings rendered as one connected smear of touching glyphs.
        glyph_cache.font_size = base_size.scale(scale);

        // The shader anchors a glyph to its cell bottom using BASE metrics
        // (`cellDim.y - glyph.top`, with `top` pre-shifted by the base descent
        // in `load_glyph`). Push the anchor down by the ascent growth so the
        // scaled run's baseline lands where a truly scaled cell would put it:
        // Δ = (cell_h + descent) · (scale − 1) — i.e. ascent · (scale − 1).
        let metrics = glyph_cache.font_metrics();
        let anchor_y = (y + (size_info.cell_height() + metrics.descent) * (scale - 1.0)).round();

        // Advance by the UNfloored design advance, scaled. The grid's
        // `cell_width` is `floor(average_advance)`; at base size the small-ppem
        // hinting squeezes ink to fit it, but rasterized larger the outline's
        // true width comes out — stepping by `cell_w × scale` then crams
        // characters together, a fraction of a pixel per column, cumulatively.
        let advance = metrics.average_advance as f32 * scale;

        let mut pen_x = x;
        for character in text.chars() {
            let width = character.width().unwrap_or(0);
            if width == 0 {
                continue;
            }
            let flags = if width == 2 { Flags::WIDE_CHAR | style } else { style };
            self.begin_chrome_text(size_info, pen_x.round(), anchor_y);
            let cell = RenderableCell {
                point: Point::new(0, Column(0)),
                character,
                extra: None,
                flags,
                bg_alpha: 0.0,
                fg,
                bg: Rgb::new(0, 0, 0),
                underline: fg,
            };
            self.draw_cells(size_info, glyph_cache, std::iter::once(cell));
            pen_x += width as f32 * advance + tracking;
        }

        glyph_cache.font_size = base_size;
        self.end_chrome_text(size_info);
        pen_x - x
    }

    /// Draw all rectangles simultaneously to prevent excessive program swaps.
    pub fn draw_rects(&mut self, size_info: &SizeInfo, metrics: &Metrics, rects: Vec<RenderRect>) {
        if rects.is_empty() {
            return;
        }

        // Prepare rect rendering state.
        unsafe {
            // Remove padding from viewport.
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }

        self.rect_renderer.draw(size_info, metrics, rects, self.window_height.get());

        // Activate regular state again.
        unsafe {
            // Reset blending strategy.
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            // Restore viewport with padding.
            self.set_viewport(size_info);
        }
    }

    /// Draw chrome UI quads (rounded, optionally gradient-filled) for the
    /// window decorations: title bar, tabs, status bar and settings.
    pub fn draw_ui(&mut self, size_info: &SizeInfo, quads: &[UiQuad]) {
        if quads.is_empty() {
            return;
        }

        // Snap every quad's edges to whole physical pixels. Layout math is
        // full of `* scale_factor` fractions, and a 1px hairline whose top
        // edge lands on x.5 renders as a 2px half-alpha smear — the "dirty
        // line" class of artifact. Snapping EDGES (not x/width separately)
        // keeps stacked quads (code-block bands, quote bars) seamless: a
        // shared edge coordinate rounds identically on both sides.
        let quads: Vec<UiQuad> = quads.iter().map(UiQuad::pixel_snapped).collect();

        // Prepare UI rendering state.
        unsafe {
            // Draw over the whole window, ignoring grid padding.
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }

        self.ui_renderer.draw(size_info, &quads);

        // Activate regular state again.
        unsafe {
            // Reset blending strategy.
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            // Restore viewport with padding.
            self.set_viewport(size_info);
        }
    }

    /// Draw one native math layout at a window-pixel baseline. Rules are submitted through the
    /// existing UI quad batch; glyphs use the single fixed-size math atlas.
    pub(crate) fn draw_math(
        &mut self,
        size_info: &SizeInfo,
        layout: &crate::math::layout::MathLayout,
        origin_x: f32,
        baseline_y: f32,
        color: Rgb,
        clip: MathClip,
    ) -> Result<(), crate::math::MathError> {
        let rules: Vec<UiQuad> = layout
            .rules
            .iter()
            .filter_map(|rule| {
                let left = (origin_x + rule.x).max(clip.left);
                let top = (baseline_y + rule.y).max(clip.top);
                let right = (origin_x + rule.x + rule.width).min(clip.right);
                let bottom = (baseline_y + rule.y + rule.height.max(1.0)).min(clip.bottom);
                (right > left && bottom > top).then(|| UiQuad::solid(
                    left,
                    top,
                    right - left,
                    bottom - top,
                    0.0,
                    crate::renderer::ui::Rgba::opaque(color),
                ))
            })
            .collect();
        self.draw_ui(size_info, &rules);
        unsafe {
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }
        let result = self.math_renderer.draw(size_info, layout, origin_x, baseline_y, color, clip);
        self.invalidate_text_texture_cache();
        unsafe {
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);
            self.set_viewport(size_info);
        }
        result
    }

    /// Draw a full-window background image using CSS-like `cover` scaling.
    pub fn draw_background_image(&mut self, size_info: &SizeInfo, path: &Path, opacity: f32) {
        if opacity <= 0.0 {
            return;
        }

        unsafe {
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }

        self.image_renderer.draw(size_info, path, opacity);
        // The image pass rebinds TEXTURE_2D behind the text renderer's back;
        // drop its cached atlas binding or every later glyph goes invisible.
        self.invalidate_text_texture_cache();

        unsafe {
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);
            self.set_viewport(size_info);
        }
    }

    pub fn invalidate_background_image(&mut self) {
        self.image_renderer.invalidate();
    }

    /// Draw one OSC 1337 inline image at a window-pixel rect (blended like the
    /// background image; viewport spans the full window during the call).
    pub fn draw_inline_image(
        &mut self,
        size_info: &SizeInfo,
        id: u64,
        rgba: &std::sync::Arc<Vec<u8>>,
        px: (u32, u32),
        rect: (f32, f32, f32, f32),
    ) {
        unsafe {
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }

        self.image_renderer.draw_inline(size_info, id, rgba, px, rect);
        // Same texture-cache poison risk as the background image path.
        self.invalidate_text_texture_cache();

        unsafe {
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);
            self.set_viewport(size_info);
        }
    }

    /// Drop the text renderers' cached `TEXTURE_2D` binding after an image
    /// draw. Their batch loop elides `glBindTexture` when the cache claims
    /// the atlas is still bound — a stale cache silently blanks all text.
    fn invalidate_text_texture_cache(&mut self) {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.invalidate_active_tex(),
            TextRendererProvider::Glsl3(renderer) => renderer.invalidate_active_tex(),
        }
    }

    /// Drop inline textures for images no pane references anymore.
    pub fn retain_inline_images(&mut self, alive: impl Fn(u64) -> bool) {
        self.image_renderer.retain_inline_images(alive);
    }

    /// Fill the window with `color` and `alpha`.
    pub fn clear(&self, color: Rgb, alpha: f32) {
        unsafe {
            gl::ClearColor(
                (f32::from(color.r) / 255.0).min(1.0) * alpha,
                (f32::from(color.g) / 255.0).min(1.0) * alpha,
                (f32::from(color.b) / 255.0).min(1.0) * alpha,
                alpha,
            );
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }

    /// Get the context reset status.
    pub fn was_context_reset(&self) -> bool {
        // If robustness is not supported, don't use its functions.
        if !self.robustness {
            return false;
        }

        let status = unsafe { gl::GetGraphicsResetStatus() };
        if status == gl::NO_ERROR {
            false
        } else {
            let reason = match status {
                gl::GUILTY_CONTEXT_RESET_KHR => "guilty",
                gl::INNOCENT_CONTEXT_RESET_KHR => "innocent",
                gl::UNKNOWN_CONTEXT_RESET_KHR => "unknown",
                _ => "invalid",
            };

            info!("GPU reset ({reason})");

            true
        }
    }

    fn supports_robustness() -> bool {
        let mut notification_strategy = 0;
        if GlExtensions::contains("GL_KHR_robustness") {
            unsafe {
                gl::GetIntegerv(gl::RESET_NOTIFICATION_STRATEGY_KHR, &mut notification_strategy);
            }
        } else {
            notification_strategy = gl::NO_RESET_NOTIFICATION_KHR as gl::types::GLint;
        }

        if notification_strategy == gl::LOSE_CONTEXT_ON_RESET_KHR as gl::types::GLint {
            info!("GPU reset notifications are enabled");
            true
        } else {
            info!("GPU reset notifications are disabled");
            false
        }
    }

    pub fn finish(&self) {
        unsafe {
            gl::Finish();
        }
    }

    /// Set the viewport for cell rendering.
    #[inline]
    /// Record the full window height (physical px) so pane viewports can be
    /// flipped from top-down `SizeInfo` coords to OpenGL's bottom-left origin.
    pub fn set_window_height(&self, height: f32) {
        self.window_height.set(height);
    }

    pub fn set_viewport(&self, size: &SizeInfo) {
        let (x, y, width, height) = cell_viewport(size, self.window_height.get());
        unsafe {
            gl::Viewport(x, y, width, height);
        }
    }

    /// Resize the renderer.
    pub fn resize(&self, size_info: &SizeInfo) {
        self.set_viewport(size_info);
        match &self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.resize(size_info),
            TextRendererProvider::Glsl3(renderer) => renderer.resize(size_info),
        }
    }
}

/// Calculate the cell viewport in OpenGL's bottom-left coordinate system.
fn cell_viewport(size: &SizeInfo, window_height: f32) -> (i32, i32, i32, i32) {
    let content_width = size.width() - size.padding_x() - size.padding_right();
    let content_height = size.height() - size.padding_y() - size.padding_bottom();

    // `padding_y` is measured from the top. A pane can encode only a subsection
    // of the framebuffer, so flip against the real window height when available.
    // 全窗口路径最终落在独立的 bottom padding 上，不能再镜像顶部标题栏留白。
    let framebuffer_height = if window_height > 0.0 { window_height } else { size.height() };
    let y = framebuffer_height - size.padding_y() - content_height;

    (
        size.padding_x() as i32,
        y as i32,
        content_width.max(0.0) as i32,
        content_height.max(0.0) as i32,
    )
}

struct GlExtensions;

impl GlExtensions {
    /// Check if the given `extension` is supported.
    ///
    /// This function will lazily load OpenGL extensions.
    fn contains(extension: &str) -> bool {
        static OPENGL_EXTENSIONS: OnceLock<HashSet<&'static str, RandomState>> = OnceLock::new();

        OPENGL_EXTENSIONS.get_or_init(Self::load_extensions).contains(extension)
    }

    /// Load available OpenGL extensions.
    fn load_extensions() -> HashSet<&'static str, RandomState> {
        unsafe {
            let extensions = gl::GetString(gl::EXTENSIONS);

            if extensions.is_null() {
                let mut extensions_number = 0;
                gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut extensions_number);

                (0..extensions_number as gl::types::GLuint)
                    .flat_map(|i| {
                        let extension = CStr::from_ptr(gl::GetStringi(gl::EXTENSIONS, i) as *mut _);
                        extension.to_str()
                    })
                    .collect()
            } else {
                match CStr::from_ptr(extensions as *mut _).to_str() {
                    Ok(ext) => ext.split_whitespace().collect(),
                    Err(_) => Default::default(),
                }
            }
        }
    }
}

extern "system" fn gl_debug_log(
    _: gl::types::GLenum,
    _: gl::types::GLenum,
    _: gl::types::GLuint,
    _: gl::types::GLenum,
    _: gl::types::GLsizei,
    msg: *const gl::types::GLchar,
    _: *mut std::os::raw::c_void,
) {
    let msg = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    debug!("[gl_render] {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asymmetric_bottom_padding_is_the_cell_viewport_origin() {
        let size =
            SizeInfo::new_fully_asymmetric(1000.0, 1000.0, 10.0, 20.0, 260.0, 20.0, 64.0, 16.0);

        assert_eq!(cell_viewport(&size, 1000.0), (260, 16, 720, 920));
        // Startup calls resize before the renderer has recorded the window height.
        assert_eq!(cell_viewport(&size, 0.0), (260, 16, 720, 920));
    }

    #[test]
    fn split_viewport_flips_against_the_real_window_height() {
        // Pane content rect: y=150, h=300. Split layout encodes it as
        // height=h+2*y with symmetric local padding.
        let pane = SizeInfo::new(900.0, 600.0, 10.0, 20.0, 100.0, 150.0, false);
        assert_eq!(cell_viewport(&pane, 1000.0), (100, 550, 700, 300));
    }
}
