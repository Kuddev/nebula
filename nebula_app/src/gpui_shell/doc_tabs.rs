//! Image tab rendering, plus the shared editable Markdown tab entry point.
//! Image zoom/pan continues to use the existing tested ImageView geometry.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, ContentMask, Context, Corners, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, RenderImage,
    ScrollWheelEvent, Styled as _, Window, div, px,
};
use image::Frame;

use crate::display::image_viewer::ImageView;
use crate::gpui_shell::prelude::*;

/// 双击路由：应用内能读的开 tab（图片/文档/源码），其余交系统处理器。
/// 源码查看是 GPUI 壳新增能力，旧壳合同（`input/chrome.rs`）之上的超集。
pub fn openable_in_app(path: &Path) -> bool {
    crate::display::image_viewer::viewable_file(path)
        || crate::display::markdown_view::viewable_file(path)
        || crate::gpui_shell::code_tab::viewable_file(path)
}

pub struct ImageTabView {
    pub path: PathBuf,
    pub title: String,
    /// 共享几何状态机（zoom/pan/锚点/钳制）。
    geometry: ImageView,
    /// 后台解码的像素（BGRA 帧）；None = 解码中或失败。
    image: Option<Arc<RenderImage>>,
    error: Option<String>,
    /// 上一帧查看区矩形（窗口坐标）；事件换算用。绘制不依赖它——canvas
    /// paint 拿的是当帧 bounds。
    area: Rc<RefCell<Bounds<Pixels>>>,
}

impl ImageTabView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let geometry = ImageView::open(path.clone());
        let title = geometry.title.clone();
        let mut this = Self {
            path,
            title,
            geometry,
            image: None,
            error: None,
            area: Rc::new(RefCell::new(Bounds::default())),
        };
        this.spawn_decode(cx);
        this
    }

    /// 重新读盘（文件树再次双击同一路径时宿主调用，旧壳 reload 同义）。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.geometry.reload();
        self.image = None;
        self.error = None;
        self.spawn_decode(cx);
    }

    fn spawn_decode(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let task = cx.background_executor().spawn(async move { decode_bgra(&path) });
        cx.spawn(async move |this, cx| {
            let decoded = task.await;
            let _ = this.update(cx, |view, cx| {
                match decoded {
                    Ok(image) => view.image = Some(image),
                    Err(error) => view.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn area_tuple(&self) -> (f32, f32, f32, f32) {
        let bounds = *self.area.borrow();
        (
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        )
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let steps = event.delta.pixel_delta(px(40.0)).y.as_f32() / 40.0;
        let anchor = (f32::from(event.position.x), f32::from(event.position.y));
        if self.geometry.zoom_by(steps, anchor, self.area_tuple()) {
            cx.notify();
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        if self.geometry.begin_drag(point, self.area_tuple()) {
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.geometry.dragging() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            let _ = self.geometry.end_drag();
            return;
        }
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        if self.geometry.drag_to(point, self.area_tuple()) {
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.geometry.end_drag() {
            cx.notify();
        }
    }
}

/// 与壁纸解码同款：RGBA8 → BGRA（gpui 帧通道序）。跑在后台线程。
fn decode_bgra(path: &Path) -> Result<Arc<RenderImage>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("无法读取 {}: {error}", path.display()))?;
    let mut rgba = image::load_from_memory(&bytes)
        .map_err(|error| format!("无法解码 {}: {error}", path.display()))?
        .into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([Frame::new(rgba)])))
}

impl Render for ImageTabView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let store = self.area.clone();
        // paint 闭包吃当帧几何：矩形按当帧 bounds 现算，没有首帧空窗。
        let geometry_snapshot = self.geometry.clone();
        let image = self.image.clone();
        let painter = gpui::canvas(
            move |bounds, _, _| {
                *store.borrow_mut() = bounds;
                bounds
            },
            move |_, bounds: Bounds<Pixels>, window, _| {
                let Some(image) = image else { return };
                let area = (
                    f32::from(bounds.origin.x),
                    f32::from(bounds.origin.y),
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                );
                let target = geometry_snapshot.render_rect(area);
                let target_bounds = Bounds::new(
                    gpui::point(px(target.0), px(target.1)),
                    gpui::size(px(target.2.max(1.0)), px(target.3.max(1.0))),
                );
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let _ = window.paint_image(
                        target_bounds,
                        target_bounds,
                        Corners::all(px(0.0)),
                        image,
                        0,
                        false,
                    );
                });
            },
        )
        .absolute()
        .inset_0();

        let status: Option<String> = if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.image.is_none() {
            Some(String::from("正在加载图片…"))
        } else {
            None
        };

        div()
            .id("nebula-image-tab")
            .size_full()
            .relative()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(painter)
            .when_some(status, |root, text| {
                root.child(
                    div()
                        .absolute()
                        .top_2()
                        .left_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .text_color(muted)
                        .child(text),
                )
            })
    }
}

pub use super::file_editor::{TextFileEvent as DocTabViewEvent, TextFileView as DocTabView};
