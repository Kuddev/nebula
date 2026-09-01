//! Read-only image preview tab.

use std::path::{Path, PathBuf};

#[cfg(feature = "legacy-shell")]
use crate::display::SizeInfo;
#[cfg(feature = "legacy-shell")]
use crate::renderer::Renderer;
#[cfg(feature = "legacy-shell")]
use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
const ZOOM_PER_STEP: f32 = 1.18;

/// Common raster formats supported by the in-app file tree preview.
pub fn viewable_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp")
    )
}

#[derive(Clone)]
pub struct ImageView {
    pub path: PathBuf,
    pub title: String,
    dimensions: Option<(u32, u32)>,
    zoom: f32,
    pan: (f32, f32),
    drag_last: Option<(f32, f32)>,
}

impl ImageView {
    pub fn open(path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let dimensions = image_dimensions(&path).ok();
        Self { path, title, dimensions, zoom: 1.0, pan: (0.0, 0.0), drag_last: None }
    }

    pub fn reload(&mut self) {
        self.dimensions = image_dimensions(&self.path).ok();
    }

    /// Zoom around the pointer so the inspected pixel stays under the cursor.
    pub fn zoom_by(&mut self, steps: f32, anchor: (f32, f32), area: (f32, f32, f32, f32)) -> bool {
        if steps.abs() < f32::EPSILON || self.dimensions.is_none() {
            return false;
        }
        let old_zoom = self.zoom;
        let next_zoom = (old_zoom * ZOOM_PER_STEP.powf(steps)).clamp(MIN_ZOOM, MAX_ZOOM);
        if (next_zoom - old_zoom).abs() < f32::EPSILON {
            return false;
        }

        let old = self.target_rect(area);
        let u = ((anchor.0 - old.0) / old.2.max(1.0)).clamp(0.0, 1.0);
        let v = ((anchor.1 - old.1) / old.3.max(1.0)).clamp(0.0, 1.0);
        self.zoom = next_zoom;

        let (draw_w, draw_h) = self.draw_size(area);
        let centered_x = area.0 + (area.2 - draw_w) * 0.5;
        let centered_y = area.1 + (area.3 - draw_h) * 0.5;
        self.pan.0 = anchor.0 - u * draw_w - centered_x;
        self.pan.1 = anchor.1 - v * draw_h - centered_y;
        self.clamp_pan(area);
        true
    }

    pub fn begin_drag(&mut self, point: (f32, f32), area: (f32, f32, f32, f32)) -> bool {
        if !contains(area, point) {
            return false;
        }
        self.drag_last = Some(point);
        true
    }

    pub fn drag_to(&mut self, point: (f32, f32), area: (f32, f32, f32, f32)) -> bool {
        let Some(previous) = self.drag_last.replace(point) else { return false };
        let before = self.pan;
        self.pan.0 += point.0 - previous.0;
        self.pan.1 += point.1 - previous.1;
        self.clamp_pan(area);
        self.pan != before
    }

    pub fn dragging(&self) -> bool {
        self.drag_last.is_some()
    }

    pub fn end_drag(&mut self) -> bool {
        self.drag_last.take().is_some()
    }

    /// 渲染矩形（zoom/pan/边缘钳制后的最终几何）。GPUI 壳按它摆放图片，
    /// 与 [`Self::draw`] 的旧壳渲染同一份数学。
    pub fn render_rect(&self, area: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
        self.target_rect(area)
    }

    /// 文件头读出的像素尺寸；None = 打开/解析失败。
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.dimensions
    }

    #[cfg(feature = "legacy-shell")]
    pub fn draw(
        &self,
        renderer: &mut Renderer,
        size: &SizeInfo,
        area: (f32, f32, f32, f32),
        scale: f32,
    ) {
        let target = self.target_rect(area);
        renderer.draw_background_image(
            size,
            &self.path,
            1.0,
            BackgroundImageFit::Fill,
            BackgroundImageAlignment::Center,
            target,
            area,
            8.0 * scale,
        );
    }

    /// Initial scale fills the viewport width; zoom and pan are relative to
    /// that stable baseline so resizing never accumulates rounding drift.
    fn draw_size(&self, area: (f32, f32, f32, f32)) -> (f32, f32) {
        let Some((width, height)) = self.dimensions else { return (area.2, area.3) };
        let base = area.2.max(1.0) / width.max(1) as f32;
        (width as f32 * base * self.zoom, height as f32 * base * self.zoom)
    }

    fn target_rect(&self, area: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
        let (draw_w, draw_h) = self.draw_size(area);
        let max_pan_x = ((draw_w - area.2) * 0.5).max(0.0);
        let max_pan_y = ((draw_h - area.3) * 0.5).max(0.0);
        let pan_x = self.pan.0.clamp(-max_pan_x, max_pan_x);
        let pan_y = self.pan.1.clamp(-max_pan_y, max_pan_y);
        (
            area.0 + (area.2 - draw_w) * 0.5 + pan_x,
            area.1 + (area.3 - draw_h) * 0.5 + pan_y,
            draw_w,
            draw_h,
        )
    }

    fn clamp_pan(&mut self, area: (f32, f32, f32, f32)) {
        let (draw_w, draw_h) = self.draw_size(area);
        let max_x = ((draw_w - area.2) * 0.5).max(0.0);
        let max_y = ((draw_h - area.3) * 0.5).max(0.0);
        self.pan.0 = self.pan.0.clamp(-max_x, max_x);
        self.pan.1 = self.pan.1.clamp(-max_y, max_y);
    }
}

#[cfg(not(target_os = "macos"))]
fn image_dimensions(path: &Path) -> Result<(u32, u32), String> {
    ::image::ImageReader::open(path)
        .map_err(|error| format!("open {path:?}: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("detect image format: {error}"))?
        .into_dimensions()
        .map_err(|error| format!("read image dimensions: {error}"))
}

#[cfg(target_os = "macos")]
fn image_dimensions(_path: &Path) -> Result<(u32, u32), String> {
    Err("Image preview is not enabled for this build".to_owned())
}

fn contains(area: (f32, f32, f32, f32), point: (f32, f32)) -> bool {
    point.0 >= area.0 && point.0 < area.0 + area.2 && point.1 >= area.1 && point.1 < area.1 + area.3
}

/// Draw an image path for standalone image tabs and Markdown image blocks.
#[cfg(feature = "legacy-shell")]
pub fn draw_path(
    renderer: &mut Renderer,
    size: &SizeInfo,
    path: &Path,
    area: (f32, f32, f32, f32),
    scale: f32,
) {
    let image_area = (
        area.0 + 12.0 * scale,
        area.1 + 12.0 * scale,
        (area.2 - 24.0 * scale).max(1.0),
        (area.3 - 24.0 * scale).max(1.0),
    );
    renderer.draw_background_image(
        size,
        path,
        1.0,
        BackgroundImageFit::Uniform,
        BackgroundImageAlignment::Center,
        image_area,
        image_area,
        8.0 * scale,
    );
}

#[cfg(test)]
mod tests {
    use super::ImageView;
    use std::path::PathBuf;

    fn view(dimensions: (u32, u32)) -> ImageView {
        ImageView {
            path: PathBuf::from("preview.png"),
            title: "preview.png".to_owned(),
            dimensions: Some(dimensions),
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag_last: None,
        }
    }

    #[test]
    fn initial_image_width_fills_the_viewport() {
        let image = view((1600, 900));
        let area = (20.0, 30.0, 800.0, 600.0);
        let target = image.target_rect(area);
        assert!((target.0 - area.0).abs() < 0.01);
        assert!((target.2 - area.2).abs() < 0.01);
    }

    #[test]
    fn zoom_keeps_the_pointer_anchor_stable() {
        // A square image overflows vertically when fitted to this landscape
        // viewport, so both anchor coordinates can move without edge clamps.
        let mut image = view((1600, 1600));
        let area = (20.0, 30.0, 800.0, 400.0);
        let anchor = (320.0, 180.0);
        let before = image.target_rect(area);
        let before_u = (anchor.0 - before.0) / before.2;
        let before_v = (anchor.1 - before.1) / before.3;

        assert!(image.zoom_by(2.0, anchor, area));
        let after = image.target_rect(area);
        let after_u = (anchor.0 - after.0) / after.2;
        let after_v = (anchor.1 - after.1) / after.3;
        assert!((before_u - after_u).abs() < 0.001);
        assert!((before_v - after_v).abs() < 0.001);
    }

    #[test]
    fn dragging_is_clamped_to_the_image_edges() {
        let mut image = view((1600, 900));
        let area = (0.0, 0.0, 800.0, 400.0);
        assert!(image.zoom_by(3.0, (400.0, 200.0), area));
        assert!(image.begin_drag((400.0, 200.0), area));
        image.drag_to((4000.0, 200.0), area);
        let target = image.target_rect(area);
        assert!(target.0 <= area.0 + 0.01);
        assert!(target.0 + target.2 >= area.0 + area.2 - 0.01);
        assert!(image.end_drag());
    }
}
