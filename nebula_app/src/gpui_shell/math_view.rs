//! GPUI 壳的公式渲染：旧壳数学管线接入组件库 TextView。
//!
//! 管线与旧壳三层同源：`compile_formula` 产出后端无关绘制指令 →
//! `MathGlyphRasterizer`（与旧壳 GL 图集同一支栅格化器）按物理像素把整条
//! 公式合成一张 BGRA 位图 → `window.paint_image` 上屏；数学字体缺字的
//! 字符（旧笔记里的中文等）用 gpui 文本系统按基线叠加补画，对应旧壳
//! `draw_math` 之后的 `draw_doc_text` 回补。
//!
//! 为什么走位图而不是 gpui 字形管线：数学排版要按字形 ID 绘制（拉伸变体、
//! 装配件都不经 cmap），而 gpui 的 `GlyphId` 构造器是 crate 私有的；位图
//! 路线反而让两条壳共享唯一一支栅格化器，公式像素级同源。
//!
//! 失败合同与旧壳一致：编译失败/超预算/缩到 [`MIN_READABLE_MATH_PX`] 之下
//! 的公式回退为源码文本（组件库侧代码样式），不撑破阅读列。

mod source_fallback;
use source_fallback::SourceFallback;

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::prelude::*;
use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, Corners, Element, ElementId, Entity,
    GlobalElementId, Image, ImageFormat, InspectorElementId, IntoElement, LayoutId, Pixels, Render,
    RenderImage, Rgba, SharedString, Size, Style, Subscription, Task, Window, div, point, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, button::ButtonVariants as _,
};
use image::Frame;
use image::ImageEncoder as _;

use super::copy_feedback::CopyFeedback;
use super::prelude::{Button, IconName, h_flex};
use super::scientific_render::{self, FormulaKey, ScientificRender};
use crate::display::ToastKind;
use crate::i18n::Message;
use crate::math::MIN_READABLE_MATH_PX;
use crate::math::layout::MathLayout;
use crate::math::rasterizer::MathGlyphRasterizer;

/// 探针编译字号：注册的渲染闭包用它判定"这条公式能否编译"，失败即让
/// 组件库走源码文本回退。解析/预算类失败与字号无关，任意正值等价。
const PROBE_PX: f32 = 16.0;

/// 与旧壳 `fit_math_run` 相同的收缩余量：布局对字号线性，留 2% 吸收
/// 取整误差，保证最右侧抗锯齿像素不越出阅读列。
const FIT_MARGIN: f32 = 0.98;

/// Clipboard export is a transient conversion from the existing cached BGRA
/// image to PNG. Keep that conversion bounded independently of the shared
/// scientific cache; a pathological formula should disable image copy rather
/// than allocate another 24 MiB buffer on the UI thread.
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// A copy needs one transformed raw frame and one encoded PNG at the same
/// time. Keep all simultaneous formula-copy work below a small, independent
/// transient budget; this is separate from (and does not raise) the shared
/// 48 MiB scientific cache.
const MAX_CLIPBOARD_WORK_BYTES: usize = MAX_CLIPBOARD_IMAGE_BYTES * 2;
static CLIPBOARD_WORK_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 注册 TextView 的公式渲染器；`gpui_shell::init` 调用一次。
pub fn register(cx: &mut App) {
    scientific_render::init(cx);
    cx.set_global(MathAssets::new(scientific_render::assets(cx)));
    gpui_component::text::set_math_renderer(cx, |spec, window, cx| {
        let assets = source_assets(cx);
        // 探针编译：失败的公式仍是文档文本，交回组件库按代码样式排版。
        assets.layout(&spec.source, spec.display, PROBE_PX, 1.0)?;
        let source = spec.source.clone();
        let display = spec.display;
        let key = ("markdown-math-actions", math_element_key(&source, display));
        let state = window
            .use_keyed_state(key, cx, |_, cx| MathFormulaView::new(source.clone(), display, cx));
        Some(state.into_any_element())
    });
}

// ---- Application-owned background resources ----

/// 位图内的定位信息（物理 px）：paint 时把位图基线吸附到元素基线。
#[derive(Clone, Copy, Debug)]
pub(super) struct ImageGeometry {
    /// 位图顶边到公式基线的距离。
    pub(super) baseline: u32,
    /// 位图左边相对公式盒左缘的外扩（出血）。
    pub(super) pad: u32,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(crate) struct MathAssets {
    verbatim_source: bool,
    engine: Arc<ScientificRender>,
}

impl gpui::Global for MathAssets {}
struct SourceMathAssets(MathAssets);
impl gpui::Global for SourceMathAssets {}

fn source_assets(cx: &mut App) -> &mut MathAssets {
    if cx.try_global::<SourceMathAssets>().is_none() {
        let mut assets = MathAssets::new(scientific_render::assets(cx));
        assets.verbatim_source = true;
        cx.set_global(SourceMathAssets(assets));
    }
    &mut cx.global_mut::<SourceMathAssets>().0
}

impl MathAssets {
    fn new(engine: Arc<ScientificRender>) -> Self {
        Self { verbatim_source: false, engine }
    }

    pub(crate) fn can_rasterize(&self) -> bool {
        true // Actual font/bitmap failures remain a source fallback in the worker cache.
    }

    pub(crate) fn can_compose(
        &mut self,
        source: &SharedString,
        display: bool,
        pixel_size: f32,
        pixels_per_point: f32,
        raster_scale: f32,
        color: Rgba,
    ) -> bool {
        self.image(source, display, pixel_size, pixels_per_point, raster_scale, color).is_some()
    }

    pub(crate) fn layout(
        &mut self,
        source: &SharedString,
        display: bool,
        pixel_size: f32,
        pixels_per_point: f32,
    ) -> Option<Arc<MathLayout>> {
        self.engine.layout(FormulaKey::new(
            source.clone(),
            display,
            self.verbatim_source,
            pixel_size,
            pixels_per_point,
        ))
    }

    /// 旧壳 `fit_math_run` 的等价物：超宽公式按线性比例缩字号，缩到
    /// [`MIN_READABLE_MATH_PX`] 之下就放弃（调用方回退源码文本）。
    fn fit(
        &mut self,
        source: &SharedString,
        display: bool,
        pixel_size: f32,
        pixels_per_point: f32,
        max_width: f32,
    ) -> Option<(Arc<MathLayout>, f32)> {
        let base = self.layout(source, display, pixel_size, pixels_per_point)?;
        if base.metrics.width <= max_width {
            return Some((base, pixel_size));
        }
        let fitted_size = pixel_size * (max_width / base.metrics.width) * FIT_MARGIN;
        if fitted_size < MIN_READABLE_MATH_PX {
            return None;
        }
        let fitted = self.layout(source, display, fitted_size, pixels_per_point)?;
        (fitted.metrics.width <= max_width).then_some((fitted, fitted_size))
    }

    /// 公式位图（含负缓存：超限公式不逐帧重试合成）。
    pub(crate) fn image(
        &mut self,
        source: &SharedString,
        display: bool,
        pixel_size: f32,
        pixels_per_point: f32,
        raster_scale: f32,
        color: Rgba,
    ) -> Option<(Arc<gpui::RenderImage>, ImageGeometry)> {
        self.engine.image(
            FormulaKey::new(
                source.clone(),
                display,
                self.verbatim_source,
                pixel_size,
                pixels_per_point,
            ),
            raster_scale,
            color,
        )
    }
}

/// Adapt the shared CPU bitmap to GPUI without copying its pixel buffer.
pub(super) fn compose_image(
    rasterizer: &MathGlyphRasterizer,
    layout: &MathLayout,
    raster_scale: f32,
    color: Rgba,
) -> Option<(Arc<gpui::RenderImage>, ImageGeometry)> {
    let color = [
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    ];
    let bitmap = crate::math::bitmap::compose(rasterizer, layout, raster_scale, color)?;
    let geometry = ImageGeometry {
        baseline: bitmap.baseline,
        pad: bitmap.pad,
        width: bitmap.width,
        height: bitmap.height,
    };
    let buffer = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.pixels)?;
    Some((Arc::new(gpui::RenderImage::new([Frame::new(buffer)])), geometry))
}

// ---- 公式元素 ----

fn math_element_key(source: &SharedString, display: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    display.hash(&mut hasher);
    hasher.finish()
}

fn latex_clipboard_text(source: &SharedString) -> String {
    source.to_string()
}

fn image_copy_is_complete(layout: &MathLayout) -> bool {
    layout.text.is_empty()
}

/// Return a ready-to-copy image from the same scientific cache used by the
/// visible formula. A formula with text fallback is intentionally rejected:
/// `MathLayout.text` is painted separately by [`MathView::paint_math`], so its
/// `RenderImage` alone would be an incomplete copy.
fn ready_formula_image(
    source: &SharedString,
    display: bool,
    pixel_size: f32,
    window: &Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let text_style = window.text_style();
    let pixels_per_point = crate::math::pixels_per_point(window.scale_factor());
    let color = Rgba::from(text_style.color);
    let raster_scale = window.scale_factor();
    let assets = source_assets(cx);
    let layout = assets.layout(source, display, pixel_size, pixels_per_point)?;
    if !image_copy_is_complete(&layout) {
        return None;
    }
    let (image, _) =
        assets.image(source, display, pixel_size, pixels_per_point, raster_scale, color)?;
    let size = image.size(0);
    let width = u32::from(size.width) as usize;
    let height = u32::from(size.height) as usize;
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    (expected > 0
        && expected <= MAX_CLIPBOARD_IMAGE_BYTES
        && image.as_bytes(0).is_some_and(|bytes| bytes.len() == expected))
    .then_some(image)
}

struct ClipboardWorkReservation {
    bytes: usize,
}

impl Drop for ClipboardWorkReservation {
    fn drop(&mut self) {
        CLIPBOARD_WORK_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn reserve_clipboard_work(input_bytes: usize) -> Option<ClipboardWorkReservation> {
    let bytes = input_bytes.checked_add(MAX_CLIPBOARD_IMAGE_BYTES)?;
    if bytes > MAX_CLIPBOARD_WORK_BYTES {
        return None;
    }
    let mut used = CLIPBOARD_WORK_BYTES.load(Ordering::Acquire);
    loop {
        let next = used.checked_add(bytes)?;
        if next > MAX_CLIPBOARD_WORK_BYTES {
            return None;
        }
        match CLIPBOARD_WORK_BYTES.compare_exchange_weak(
            used,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(ClipboardWorkReservation { bytes }),
            Err(actual) => used = actual,
        }
    }
}

/// A writer which fails before the output vector can grow beyond the
/// clipboard budget.  `PngEncoder` writes incrementally, so this bounds the
/// encoded allocation rather than encoding an oversized PNG and discarding it
/// afterwards.
struct BoundedPngWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedPngWriter {
    fn new(limit: usize, capacity: usize) -> Self {
        Self { bytes: Vec::with_capacity(capacity.min(limit)), limit }
    }
}

impl io::Write for BoundedPngWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if bytes.len() > remaining {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "PNG clipboard budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Convert GPUI's cached BGRA frame into a bounded RGBA PNG for the system
/// clipboard. The cached frame is straight-alpha (coverage in A, source color
/// in RGB), so only the channel order changes; no un-premultiplication is
/// required here. This function is called by a background task for UI copies.
fn formula_png(image: &RenderImage) -> Option<Vec<u8>> {
    let size = image.size(0);
    let width = u32::from(size.width);
    let height = u32::from(size.height);
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if expected == 0 || expected > MAX_CLIPBOARD_IMAGE_BYTES {
        return None;
    }
    let source = image.as_bytes(0)?;
    if source.len() != expected {
        return None;
    }
    let _reservation = reserve_clipboard_work(expected)?;
    formula_png_from_bgra(width, height, source)
}

fn formula_png_from_bgra(width: u32, height: u32, source: &[u8]) -> Option<Vec<u8>> {
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if expected == 0 || expected > MAX_CLIPBOARD_IMAGE_BYTES || source.len() != expected {
        return None;
    }
    let mut rgba = source.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let mut writer = BoundedPngWriter::new(MAX_CLIPBOARD_IMAGE_BYTES, expected);
    image::codecs::png::PngEncoder::new(&mut writer)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(writer.bytes)
}

/// Stateful wrapper around the layout element. The action controls are
/// absolutely positioned and only become visible while the formula group is
/// hovered, so inline baselines and display-math spacing remain unchanged.
struct MathFormulaView {
    source: SharedString,
    display: bool,
    visible_pixel_size: Rc<Cell<Option<f32>>>,
    latex_feedback: Entity<CopyFeedback>,
    image_feedback: Entity<CopyFeedback>,
    image_copy_task: Option<Task<()>>,
    _latex_feedback_subscription: Subscription,
    _image_feedback_subscription: Subscription,
}

impl MathFormulaView {
    fn new(source: SharedString, display: bool, cx: &mut Context<Self>) -> Self {
        let latex_feedback = cx.new(|_| CopyFeedback::new());
        let image_feedback = cx.new(|_| CopyFeedback::new());
        let latex_feedback_subscription = cx.observe(&latex_feedback, |_, _, cx| cx.notify());
        let image_feedback_subscription = cx.observe(&image_feedback, |_, _, cx| cx.notify());
        Self {
            source,
            display,
            visible_pixel_size: Rc::new(Cell::new(None)),
            latex_feedback,
            image_feedback,
            image_copy_task: None,
            _latex_feedback_subscription: latex_feedback_subscription,
            _image_feedback_subscription: image_feedback_subscription,
        }
    }

    fn copy_formula_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_copy_task.is_some() {
            return;
        }
        let language = super::config::ui_language(cx);
        let Some(pixel_size) = self.visible_pixel_size.get() else {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                ToastKind::Warning,
                language.text(Message::EditorMathImageUnavailable),
            );
            return;
        };
        let Some(image) = ready_formula_image(&self.source, self.display, pixel_size, window, cx)
        else {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                ToastKind::Warning,
                language.text(Message::EditorMathImageUnavailable),
            );
            return;
        };

        let executor = cx.background_executor().clone();
        let encode = executor.spawn(async move { formula_png(&image) });
        self.image_feedback.update(cx, |feedback, cx| feedback.mark_pending(cx));

        let window_handle = window.window_handle();
        self.image_copy_task = Some(cx.spawn(async move |this, cx| {
            let png = encode.await;
            let succeeded = png.is_some();
            let applied = this.update(cx, move |view, cx| {
                view.image_copy_task = None;
                match png {
                    Some(png) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
                            ImageFormat::Png,
                            png,
                        )));
                        view.image_feedback.update(cx, |feedback, cx| feedback.mark_copied(cx));
                    },
                    None => view.image_feedback.update(cx, |feedback, cx| feedback.clear(cx)),
                }
            });
            if applied.is_ok() {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let (kind, message) = if succeeded {
                        (ToastKind::Success, Message::EditorMathImageCopied)
                    } else {
                        (ToastKind::Warning, Message::EditorMathImageUnavailable)
                    };
                    crate::gpui_shell::toast::toast(window, cx, kind, language.text(message));
                });
            }
        }));
        cx.notify();
    }
}

impl Render for MathFormulaView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.source.clone();
        let source_for_latex = source.clone();
        let display = self.display;
        let key = math_element_key(&source, display);
        let group = format!("markdown-math-actions-{key:x}");
        let language = super::config::ui_language(cx);
        let latex_feedback = self.latex_feedback.clone();
        let image_feedback = self.image_feedback.clone();
        let latex_copied = latex_feedback.read(cx).is_copied();
        let image_copied = image_feedback.read(cx).is_copied();
        let image_pending = image_feedback.read(cx).is_pending();
        let visible_pixel_size = self.visible_pixel_size.get();
        let image_ready = !image_pending
            && visible_pixel_size.is_some_and(|pixel_size| {
                ready_formula_image(&source, display, pixel_size, window, cx).is_some()
            });
        let owner = cx.weak_entity();

        let latex_button = Button::new(("markdown-copy-math-latex", key))
            .custom(
                gpui_component::button::ButtonCustomVariant::new(cx)
                    .hover(cx.theme().list_hover)
                    .active(cx.theme().list_active),
            )
            .compact()
            .size(px(28.0))
            .icon(if latex_copied { IconName::Check } else { IconName::Copy })
            .tooltip(language.text(Message::EditorCopyMathLatex))
            .on_click(move |_, window, cx| {
                // The GPUI clipboard API is best-effort and returns `()`. The
                // payload is always valid here, so mark the visual action
                // complete immediately after submitting it.
                cx.write_to_clipboard(ClipboardItem::new_string(latex_clipboard_text(
                    &source_for_latex,
                )));
                latex_feedback.update(cx, |feedback, cx| feedback.mark_copied(cx));
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    ToastKind::Success,
                    language.text(Message::EditorMathLatexCopied),
                );
            });

        let owner_for_image = owner.clone();
        let image_button = Button::new(("markdown-copy-math-image", key))
            .custom(
                gpui_component::button::ButtonCustomVariant::new(cx)
                    .hover(cx.theme().list_hover)
                    .active(cx.theme().list_active),
            )
            .compact()
            .size(px(28.0))
            .icon(if image_pending {
                IconName::Loader
            } else if image_copied {
                IconName::Check
            } else {
                IconName::File
            })
            .loading(image_pending)
            .disabled(!image_ready || image_pending)
            .tooltip(if image_ready {
                language.text(Message::EditorCopyMathImage)
            } else {
                language.text(Message::EditorMathImageUnavailable)
            })
            .on_click(move |_, window, cx| {
                let _ = owner_for_image.update(cx, |view, cx| view.copy_formula_image(window, cx));
            });

        div()
            .relative()
            .flex_shrink_0()
            .group(group.clone())
            .child(MathView {
                source,
                display,
                visible_pixel_size: self.visible_pixel_size.clone(),
            })
            .child(
                h_flex()
                    .absolute()
                    .top(px(-4.0))
                    .right(px(0.0))
                    .invisible()
                    .when(latex_copied || image_copied || image_pending, |actions| {
                        actions.visible()
                    })
                    .group_hover(group, |actions| actions.visible())
                    .gap(px(2.0))
                    .p(px(2.0))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .shadow_sm()
                    .child(latex_button)
                    .child(image_button),
            )
    }
}

/// 一条公式的 GPUI 元素。行内公式由组件库放进换行 flex 行（底边对齐），
/// 自身用底部留白把公式基线补齐到文本基线；块级公式由组件库水平居中。
struct MathView {
    source: SharedString,
    display: bool,
    visible_pixel_size: Rc<Cell<Option<f32>>>,
}

/// measure 闭包与 paint 之间的当帧交接。taffy 可能以不同可用宽度多次
/// 调用 measure，槽里保存最后一次结果；paint 用实际 bounds 核对，不符
/// （罕见）就按 bounds 宽度重新 fit，宁可略小也不越界。
#[derive(Default)]
enum FitSlot {
    #[default]
    Pending,
    Math {
        layout: Arc<MathLayout>,
        pixel_size: f32,
        /// 元素底边到公式基线的距离（行内基线补偿；块级为 depth）。
        baseline_from_bottom: f32,
    },
    /// 排版等待或 fit 放弃时保留有界的多行源码预览。
    Text(SourceFallback),
}

impl IntoElement for MathView {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MathView {
    type RequestLayoutState = Rc<RefCell<FitSlot>>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let slot: Rc<RefCell<FitSlot>> = Rc::default();
        let text_style = window.text_style();
        let rem_size = window.rem_size();
        let font_size = text_style.font_size.to_pixels(rem_size);
        let line_height = f32::from(text_style.line_height_in_pixels(rem_size));
        let run = text_style.to_run(self.source.len());
        // 文本行盒里基线到底边的距离 = descent + 半行距；行内公式用它把
        // 自己的基线抬到与同行文本一致（旧壳 push_with_math 的基线合同）。
        let probe =
            window.text_system().shape_line("M".into(), font_size, &[text_style.to_run(1)], None);
        let text_baseline_from_bottom = f32::from(probe.descent)
            + (line_height - f32::from(probe.ascent) - f32::from(probe.descent)).max(0.0) / 2.0;

        let source = self.source.clone();
        let display = self.display;
        let nominal_px = f32::from(font_size);
        let pixels_per_point = crate::math::pixels_per_point(window.scale_factor());
        let raster_scale = window.scale_factor();
        let color = Rgba::from(text_style.color);
        let measure_slot = slot.clone();
        let style = Style { flex_shrink: 0.0, ..Style::default() };
        let visible_pixel_size = self.visible_pixel_size.clone();
        let layout_id = window.request_measured_layout(style, move |_, available, window, cx| {
            let max_width = match available.width {
                AvailableSpace::Definite(width) => f32::from(width).max(8.0),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => f32::INFINITY,
            };
            let assets = source_assets(cx);
            let fitted = assets
                .fit(&source, display, nominal_px, pixels_per_point, max_width)
                .filter(|(_, pixel_size)| {
                    assets.can_compose(
                        &source,
                        display,
                        *pixel_size,
                        pixels_per_point,
                        raster_scale,
                        color,
                    )
                });
            match fitted {
                Some((layout, pixel_size)) => {
                    visible_pixel_size.set(Some(pixel_size));
                    let bottom_pad = if display {
                        0.0
                    } else {
                        (text_baseline_from_bottom - layout.metrics.depth).max(0.0)
                    };
                    let width = layout.metrics.width;
                    let height = layout.metrics.height + layout.metrics.depth + bottom_pad;
                    let baseline_from_bottom = layout.metrics.depth + bottom_pad;
                    *measure_slot.borrow_mut() =
                        FitSlot::Math { layout, pixel_size, baseline_from_bottom };
                    size(px(width), px(height))
                },
                None => {
                    visible_pixel_size.set(None);
                    let fallback = SourceFallback::shape(
                        &source,
                        px(nominal_px),
                        run.clone(),
                        max_width,
                        px(line_height),
                        window,
                    );
                    let measured = fallback.size;
                    *measure_slot.borrow_mut() = FitSlot::Text(fallback);
                    measured
                },
            }
        });
        (layout_id, slot)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        slot: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text_style = window.text_style();
        let line_height = text_style.line_height_in_pixels(window.rem_size());
        match &*slot.borrow() {
            FitSlot::Pending => {},
            FitSlot::Text(text) => text.paint(bounds, line_height, window, cx),
            FitSlot::Math { layout, pixel_size, baseline_from_bottom } => {
                let bounds_width = f32::from(bounds.size.width);
                // taffy 最终宽度与最后一次 measure 不一致（罕见）：按实际
                // bounds 重新 fit，保证不越界绘制。
                if layout.metrics.width > bounds_width + 0.5 {
                    let nominal = f32::from(text_style.font_size.to_pixels(window.rem_size()));
                    let pixels_per_point = crate::math::pixels_per_point(window.scale_factor());
                    let assets = source_assets(cx);
                    let Some((layout, pixel_size)) = assets.fit(
                        &self.source,
                        self.display,
                        nominal,
                        pixels_per_point,
                        bounds_width,
                    ) else {
                        self.visible_pixel_size.set(None);
                        self.paint_source(bounds, window, cx);
                        return;
                    };
                    self.visible_pixel_size.set(Some(pixel_size));
                    let baseline = bounds.bottom() - px(layout.metrics.depth);
                    self.paint_math(layout.as_ref(), pixel_size, baseline, bounds, window, cx);
                    return;
                }
                let baseline = bounds.bottom() - px(*baseline_from_bottom);
                let layout = layout.clone();
                self.paint_math(layout.as_ref(), *pixel_size, baseline, bounds, window, cx);
            },
        }
    }
}

impl MathView {
    fn paint_source(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let style = window.text_style();
        let line_height = style.line_height_in_pixels(window.rem_size());
        let fallback = SourceFallback::shape(
            &self.source,
            style.font_size.to_pixels(window.rem_size()),
            style.to_run(self.source.len()),
            f32::from(bounds.size.width),
            line_height,
            window,
        );
        fallback.paint(bounds, line_height, window, cx);
    }

    /// 位图贴到基线上（物理像素吸附），数学字体缺字的字符用 gpui 文本
    /// 按各自字号补画在同一条基线上。
    fn paint_math(
        &self,
        layout: &MathLayout,
        pixel_size: f32,
        baseline: Pixels,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text_style = window.text_style();
        let color = Rgba::from(text_style.color);
        let raster_scale = window.scale_factor();
        let image = source_assets(cx).image(
            &self.source,
            self.display,
            pixel_size,
            crate::math::pixels_per_point(raster_scale),
            raster_scale,
            color,
        );
        if image.is_none() {
            self.paint_source(bounds, window, cx);
            return;
        }
        paint_cached_image(image, bounds.left(), baseline, window);

        for op in &layout.text {
            let mut buffer = [0u8; 4];
            let text: SharedString = op.character.encode_utf8(&mut buffer).to_string().into();
            let run = text_style.to_run(text.len());
            let line = window.text_system().shape_line(
                text,
                px(op.pixel_size),
                std::slice::from_ref(&run),
                None,
            );
            let origin =
                point(bounds.left() + px(op.x), baseline + px(op.baseline_y) - line.ascent);
            let _ = line.paint(
                origin,
                line.ascent + line.descent,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

/// 把一条公式的位图贴到指定基线（物理像素吸附），文档元素与终端覆盖层
/// 共用。位图缓存/合成走 [`MathAssets`]；数学字体缺字的 text ops 由调用
/// 方按各自的裁剪合同补画。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_formula_image(
    source: &SharedString,
    display: bool,
    pixel_size: f32,
    pixels_per_point: f32,
    left: Pixels,
    baseline: Pixels,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    let raster_scale = window.scale_factor();
    let assets = cx.global_mut::<MathAssets>();
    let image = assets.image(source, display, pixel_size, pixels_per_point, raster_scale, color);
    paint_cached_image(image, left, baseline, window);
}

fn paint_cached_image(
    image: Option<(Arc<gpui::RenderImage>, ImageGeometry)>,
    left: Pixels,
    baseline: Pixels,
    window: &mut Window,
) {
    let Some((render_image, geometry)) = image else {
        return;
    };
    let raster_scale = window.scale_factor();
    // 位图物理像素与屏幕 1:1：原点在物理网格上取整后再折回逻辑坐标。
    let left_physical = (f32::from(left) * raster_scale).round();
    let baseline_physical = (f32::from(baseline) * raster_scale).round();
    let origin = point(
        px((left_physical - geometry.pad as f32) / raster_scale),
        px((baseline_physical - geometry.baseline as f32) / raster_scale),
    );
    let image_size =
        size(px(geometry.width as f32 / raster_scale), px(geometry.height as f32 / raster_scale));
    let _ = window.paint_image(
        Bounds::new(origin, image_size),
        Bounds::new(origin, image_size),
        Corners::default(),
        render_image,
        0,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::layout::MathTextOp;
    use std::io::Write as _;

    #[test]
    fn formula_png_converts_gpui_bgra_to_rgba_without_changing_alpha() {
        let frame = image::RgbaImage::from_raw(
            2,
            1,
            vec![
                0x33, 0x22, 0x11, 0x80, // BGRA -> RGBA (11,22,33,80)
                0xCC, 0xBB, 0xAA, 0xFF,
            ],
        )
        .unwrap();
        let image = RenderImage::new([Frame::new(frame)]);
        let png = formula_png(&image).expect("small formula frame is exportable");
        let decoded = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(decoded.as_raw(), &[0x11, 0x22, 0x33, 0x80, 0xAA, 0xBB, 0xCC, 0xFF]);
    }

    #[test]
    fn formula_png_rejects_empty_frames() {
        let image = RenderImage::new([Frame::new(image::RgbaImage::new(0, 0))]);
        assert!(formula_png(&image).is_none());
    }

    #[test]
    fn formula_image_export_rejects_layouts_with_font_fallback_text() {
        let mut layout = MathLayout::default();
        assert!(image_copy_is_complete(&layout));
        layout
            .text
            .push(MathTextOp { character: '中', x: 0.0, baseline_y: 0.0, pixel_size: 16.0 });
        assert!(!image_copy_is_complete(&layout));
    }

    #[test]
    fn latex_copy_preserves_the_current_formula_source_verbatim() {
        let source: SharedString = "  x^2 + y^2  \r\n".into();
        assert_eq!(latex_clipboard_text(&source), "  x^2 + y^2  \r\n");
    }

    #[test]
    fn png_writer_rejects_output_past_the_clipboard_budget() {
        let mut writer = BoundedPngWriter::new(4, 0);
        assert!(writer.write_all(b"1234").is_ok());
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.bytes, b"1234");
    }
}
