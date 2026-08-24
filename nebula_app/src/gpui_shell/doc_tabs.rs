//! 图片 / 文档（Markdown）Tab —— GPUI 壳的只读查看器。
//!
//! 图片：缩放/平移/锚点数学**复用旧壳** `display::image_viewer::ImageView`
//! （单测锁定的几何状态机），渲染换成 GPUI `paint_image`（解码管线与壁纸
//! 同款：RGBA → BGRA、后台线程解码）。
//!
//! 文档：文件读入后交组件库 `TextView::markdown`（解析、代码高亮、选择、
//! 滚动都在组件内）。入口判定与旧壳同合同（`markdown_view::viewable_file`
//! ∪ `image_viewer::viewable_file`，其余交系统处理器）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, ContentMask, Context, Corners, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, RenderImage,
    ScrollWheelEvent, SharedString, Styled as _, Window, div, px,
};
use image::Frame;

use crate::display::image_viewer::ImageView;
use crate::gpui_shell::prelude::*;
use gpui_component::text::{TextView, TextViewStyle};

/// 双击路由：应用内能读的开 tab（图片/文档/源码），其余交系统处理器。
/// 源码查看是 GPUI 壳新增能力，旧壳合同（`input/chrome.rs`）之上的超集。
pub fn openable_in_app(path: &Path) -> bool {
    crate::display::image_viewer::viewable_file(path)
        || crate::display::markdown_view::viewable_file(path)
        || crate::gpui_shell::code_tab::viewable_file(path)
}

/// 文档一次性读入的上限。组件 TextView 是全量解析（没有旧壳 DocView 的
/// 行虚拟化），大日志全量喂进去会卡帧；超限截断并在文首说明。
const MAX_DOC_BYTES: usize = 1024 * 1024;

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

pub struct DocTabView {
    pub path: PathBuf,
    pub title: String,
    content: SharedString,
    notice: Option<String>,
    /// TextView 的 keyed state 标识；同路径 reload 时递增换新，让组件重新
    /// 解析而不是沿用旧缓存。
    revision: u64,
}

impl DocTabView {
    pub fn new(path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut this =
            Self { path, title, content: SharedString::default(), notice: None, revision: 0 };
        this.reload();
        this
    }

    pub fn reload(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let truncated = bytes.len() > MAX_DOC_BYTES;
                let slice = if truncated { &bytes[..MAX_DOC_BYTES] } else { &bytes[..] };
                let text = rewrite_doc_images(&String::from_utf8_lossy(slice), self.path.parent());
                self.notice = truncated
                    .then(|| format!("文件超过 {} KB，仅显示开头部分", MAX_DOC_BYTES / 1024));
                self.content = text.into();
            },
            Err(error) => {
                self.content = SharedString::default();
                self.notice = Some(format!("无法读取 {}: {error}", self.path.display()));
            },
        }
    }
}

impl Render for DocTabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let path_label: SharedString = self.path.display().to_string().into();
        let doc_id = SharedString::from(format!("doc-tab-{}-{}", self.revision, self.title));

        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .h(px(28.0))
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(path_label),
                    )
                    .child(
                        Button::new("doc-reload")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("重新读取文件")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reload();
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.notice.clone(), |root, notice| {
                root.child(div().text_xs().text_color(theme.warning).child(notice))
            })
            .child(
                div().flex_1().min_h_0().child(
                    TextView::markdown(doc_id, self.content.clone())
                        .selectable(true)
                        .scrollable(true)
                        // image_base：相对图片路径（README 的 logo/截图）按
                        // 文档所在目录解析；高亮主题跟当前壳主题走。
                        .style(TextViewStyle {
                            image_base: self.path.parent().map(Arc::from),
                            highlight_theme: cx.theme().highlight_theme.clone(),
                            is_dark: cx.theme().is_dark(),
                            ..TextViewStyle::default()
                        }),
                ),
            )
    }
}

/// 把文档里的本地 GIF / 动画 WebP 换成缓存里的单帧 PNG。
/// 网络图走 HTTP 客户端同一套压帧；本地图不经过 HTTP，不换的话 gpui
/// 仍会播 GIF，markdown 多图共用 element id 时越界 panic。
fn rewrite_doc_images(text: &str, base: Option<&Path>) -> String {
    use std::sync::LazyLock;
    static MARKDOWN_IMG: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(!\[[^\]]*\]\()([^)\s]+)"#).unwrap());
    // `regex` crate 不支持反向引用 `\2`，双引号/单引号拆开写。
    static HTML_IMG_DQ: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*")([^"]+)""#).unwrap());
    static HTML_IMG_SQ: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*')([^']+)'"#).unwrap());
    let rewritten = MARKDOWN_IMG.replace_all(text, |caps: &regex::Captures<'_>| {
        format!("{}{}", &caps[1], flatten_local_image_url(&caps[2], base))
    });
    let rewritten = HTML_IMG_DQ.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
        format!(r#"{}{}""#, &caps[1], flatten_local_image_url(&caps[2], base))
    });
    HTML_IMG_SQ
        .replace_all(&rewritten, |caps: &regex::Captures<'_>| {
            format!("{}{}'", &caps[1], flatten_local_image_url(&caps[2], base))
        })
        .into_owned()
}

fn flatten_local_image_url(url: &str, base: Option<&Path>) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:") {
        return url.to_owned();
    }
    let path = Path::new(url);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match base {
            Some(base) => base.join(path),
            None => return url.to_owned(),
        }
    };
    let Ok(bytes) = std::fs::read(&resolved) else {
        return url.to_owned();
    };
    let Some(png) = crate::gpui_shell::http::flatten_animated_to_png(&bytes) else {
        return url.to_owned();
    };
    let cache = std::env::temp_dir().join("nebula-md-img");
    if std::fs::create_dir_all(&cache).is_err() {
        return url.to_owned();
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&bytes, &mut hasher);
    let dest = cache.join(format!("{:016x}.png", std::hash::Hasher::finish(&hasher)));
    if !dest.exists() && std::fs::write(&dest, png).is_err() {
        return url.to_owned();
    }
    dest.to_string_lossy().replace('\\', "/")
}
