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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AvailableSpace, Bounds, Corners, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Rgba, ShapedLine, SharedString, Size, Style, Window, point, px,
    size,
};
use image::Frame;

use crate::math::layout::MathLayout;
use crate::math::rasterizer::MathGlyphRasterizer;
use crate::math::{DEFAULT_LIMITS, MIN_READABLE_MATH_PX, compile_formula};

/// 探针编译字号：注册的渲染闭包用它判定"这条公式能否编译"，失败即让
/// 组件库走源码文本回退。解析/预算类失败与字号无关，任意正值等价。
const PROBE_PX: f32 = 16.0;

/// 布局缓存条数上限。键含字号与 DPI，同一公式 fit 前后各占一条；超限
/// 整体清空，单条重建是亚毫秒级 compile，不值得上 LRU。
const MAX_LAYOUTS: usize = 1024;

/// 公式位图缓存的字节预算；超限整体清空，可见公式下一帧按需重建。
const IMAGE_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// 单条公式位图的物理边长/字节上限；超限回退源码文本，与旧壳"fit 不进
/// 列宽即回退"同一精神，防止病态长公式吃掉显存。
const MAX_IMAGE_EDGE_PX: u32 = 8192;
const MAX_IMAGE_BYTES: usize = 24 * 1024 * 1024;

/// 位图四周的出血余量（物理 px）：字形位图的 bearing 可以越出公式度量
/// 包围盒 1–2px（`GLYPH_PADDING` + 抗锯齿），出血兜住后仍越界的按行裁剪。
const CANVAS_PAD: u32 = 2;

/// 与旧壳 `fit_math_run` 相同的收缩余量：布局对字号线性，留 2% 吸收
/// 取整误差，保证最右侧抗锯齿像素不越出阅读列。
const FIT_MARGIN: f32 = 0.98;

/// 注册 TextView 的公式渲染器；`gpui_shell::init` 调用一次。
pub fn register(cx: &mut App) {
    cx.set_global(MathAssets::new());
    gpui_component::text::set_math_renderer(cx, |spec, _, cx| {
        let assets = cx.global_mut::<MathAssets>();
        if assets.rasterizer.is_none() {
            return None;
        }
        // 探针编译：失败的公式仍是文档文本，交回组件库按代码样式排版。
        assets.layout(&spec.source, spec.display, PROBE_PX, 1.0)?;
        Some(MathView { source: spec.source.clone(), display: spec.display }.into_any_element())
    });
}

// ---- 全局缓存 ----

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LayoutKey {
    source: SharedString,
    display: bool,
    pixel_size_bits: u32,
    pixels_per_point_bits: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ImageKey {
    source: SharedString,
    display: bool,
    pixel_size_bits: u32,
    raster_scale_bits: u32,
    color: u32,
}

/// 位图内的定位信息（物理 px）：paint 时把位图基线吸附到元素基线。
#[derive(Clone, Copy, Debug)]
struct ImageGeometry {
    /// 位图顶边到公式基线的距离。
    baseline: u32,
    /// 位图左边相对公式盒左缘的外扩（出血）。
    pad: u32,
    width: u32,
    height: u32,
}

pub(crate) struct MathAssets {
    /// 字体解析失败时为 None：探针直接失败，所有公式回退源码文本。
    rasterizer: Option<MathGlyphRasterizer>,
    layouts: HashMap<LayoutKey, Option<Arc<MathLayout>>>,
    images: HashMap<ImageKey, Option<(Arc<gpui::RenderImage>, ImageGeometry)>>,
    image_bytes: usize,
}

impl gpui::Global for MathAssets {}

impl MathAssets {
    fn new() -> Self {
        Self {
            rasterizer: MathGlyphRasterizer::new().ok(),
            layouts: HashMap::new(),
            images: HashMap::new(),
            image_bytes: 0,
        }
    }

    /// 数学字体加载失败时终端覆盖层必须保留源码，不能先跳格再画空位图。
    pub(crate) fn can_rasterize(&self) -> bool {
        self.rasterizer.is_some()
    }

    /// 编排（含负缓存：失败公式不逐帧重试）。
    pub(crate) fn layout(
        &mut self,
        source: &SharedString,
        display: bool,
        pixel_size: f32,
        pixels_per_point: f32,
    ) -> Option<Arc<MathLayout>> {
        let key = LayoutKey {
            source: source.clone(),
            display,
            pixel_size_bits: pixel_size.to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
        };
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() >= MAX_LAYOUTS {
                self.layouts.clear();
            }
            let compiled = compile_formula(
                source.as_ref(),
                display,
                pixel_size,
                pixels_per_point,
                DEFAULT_LIMITS,
            )
            .ok()
            .map(Arc::new);
            self.layouts.insert(key.clone(), compiled);
        }
        self.layouts.get(&key).and_then(Clone::clone)
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
        let color_bits = ((color.r * 255.0) as u32) << 16
            | ((color.g * 255.0) as u32) << 8
            | (color.b * 255.0) as u32;
        let key = ImageKey {
            source: source.clone(),
            display,
            pixel_size_bits: pixel_size.to_bits(),
            raster_scale_bits: raster_scale.to_bits(),
            color: color_bits,
        };
        if let Some(cached) = self.images.get(&key) {
            return cached.clone();
        }

        let layout = self.layout(source, display, pixel_size, pixels_per_point)?;
        let composed = self
            .rasterizer
            .as_ref()
            .and_then(|rasterizer| compose_image(rasterizer, &layout, raster_scale, color));
        if let Some((_, geometry)) = &composed {
            let bytes = geometry.width as usize * geometry.height as usize * 4;
            if self.image_bytes.saturating_add(bytes) > IMAGE_BUDGET_BYTES {
                self.images.clear();
                self.image_bytes = 0;
            }
            self.image_bytes += bytes;
        }
        self.images.insert(key, composed.clone());
        composed
    }
}

/// 把整条公式（字形 + 分数线等矩形）按物理像素合成为一张直通 alpha 的
/// BGRA 位图。字形位图原点吸附整数物理像素（旧壳同款），advance 保持
/// 全精度。
fn compose_image(
    rasterizer: &MathGlyphRasterizer,
    layout: &MathLayout,
    raster_scale: f32,
    color: Rgba,
) -> Option<(Arc<gpui::RenderImage>, ImageGeometry)> {
    let content_width = (layout.metrics.width * raster_scale).ceil().max(1.0) as u32;
    let ascent = (layout.metrics.height * raster_scale).ceil().max(0.0) as u32;
    let descent = (layout.metrics.depth * raster_scale).ceil().max(0.0) as u32;
    let width = content_width.checked_add(CANVAS_PAD * 2)?;
    let height = ascent.checked_add(descent)?.checked_add(CANVAS_PAD * 2)?.max(1);
    if width > MAX_IMAGE_EDGE_PX || height > MAX_IMAGE_EDGE_PX {
        return None;
    }
    let bytes = width as usize * height as usize * 4;
    if bytes > MAX_IMAGE_BYTES {
        return None;
    }
    let baseline = (ascent + CANVAS_PAD) as i64;

    // 先在 alpha 平面上合成（Porter-Duff over），最后一次性染色。
    let mut coverage = vec![0u8; width as usize * height as usize];
    for op in &layout.glyphs {
        let glyph = rasterizer.rasterize(op.glyph_id, op.pixel_size * raster_scale).ok()?;
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let origin_x = (op.x * raster_scale).round() as i64 + glyph.left as i64 + CANVAS_PAD as i64;
        let origin_y = baseline + (op.baseline_y * raster_scale).round() as i64 - glyph.top as i64;
        blit_over(
            &mut coverage,
            width,
            height,
            origin_x,
            origin_y,
            glyph.width as u32,
            glyph.height as u32,
            |x, y| glyph.rgba[(y * glyph.width as usize + x) * 4 + 3],
        );
    }
    for rule in &layout.rules {
        let x0 = (rule.x * raster_scale).round() as i64 + CANVAS_PAD as i64;
        let y0 = baseline + (rule.y * raster_scale).round() as i64;
        let w = (rule.width * raster_scale).round().max(1.0) as u32;
        let h = (rule.height * raster_scale).round().max(1.0) as u32;
        blit_over(&mut coverage, width, height, x0, y0, w, h, |_, _| u8::MAX);
    }

    // gpui 图像管线吃直通 alpha 的 BGRA（与 DirectWrite 字形同一约定）。
    let mut bgra = vec![0u8; bytes];
    let (r, g, b) = (
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    );
    for (pixel, alpha) in bgra.chunks_exact_mut(4).zip(&coverage) {
        pixel[0] = b;
        pixel[1] = g;
        pixel[2] = r;
        pixel[3] = *alpha;
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    let geometry = ImageGeometry { baseline: baseline as u32, pad: CANVAS_PAD, width, height };
    Some((Arc::new(gpui::RenderImage::new([Frame::new(buffer)])), geometry))
}

/// alpha 平面上的 Porter-Duff over 合成；目标越界的像素按行列裁剪。
#[allow(clippy::too_many_arguments)]
fn blit_over(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    origin_x: i64,
    origin_y: i64,
    width: u32,
    height: u32,
    sample: impl Fn(usize, usize) -> u8,
) {
    for row in 0..height as i64 {
        let y = origin_y + row;
        if y < 0 || y >= canvas_height as i64 {
            continue;
        }
        for column in 0..width as i64 {
            let x = origin_x + column;
            if x < 0 || x >= canvas_width as i64 {
                continue;
            }
            let source = sample(column as usize, row as usize) as u32;
            if source == 0 {
                continue;
            }
            let target = &mut canvas[y as usize * canvas_width as usize + x as usize];
            let existing = *target as u32;
            *target = (existing + source * (255 - existing) / 255).min(255) as u8;
        }
    }
}

// ---- 公式元素 ----

/// 一条公式的 GPUI 元素。行内公式由组件库放进换行 flex 行（底边对齐），
/// 自身用底部留白把公式基线补齐到文本基线；块级公式由组件库水平居中。
struct MathView {
    source: SharedString,
    display: bool,
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
    /// fit 放弃后的退化形态：单行源码文本（探针已保证可编译，只有
    /// 极窄列会走到这里）。
    Text(ShapedLine),
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
        let measure_slot = slot.clone();
        let style = Style { flex_shrink: 0.0, ..Style::default() };
        let layout_id = window.request_measured_layout(style, move |_, available, window, cx| {
            let max_width = match available.width {
                AvailableSpace::Definite(width) => f32::from(width).max(8.0),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => f32::INFINITY,
            };
            let assets = cx.global_mut::<MathAssets>();
            match assets.fit(&source, display, nominal_px, pixels_per_point, max_width) {
                Some((layout, pixel_size)) => {
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
                    let line = window.text_system().shape_line(
                        source.clone(),
                        px(nominal_px),
                        std::slice::from_ref(&run),
                        None,
                    );
                    let width = f32::from(line.width).min(max_width.max(8.0));
                    *measure_slot.borrow_mut() = FitSlot::Text(line);
                    size(px(width), px(line_height))
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
            FitSlot::Text(line) => {
                let _ = line.paint(bounds.origin, line_height, window, cx);
            },
            FitSlot::Math { layout, pixel_size, baseline_from_bottom } => {
                let bounds_width = f32::from(bounds.size.width);
                // taffy 最终宽度与最后一次 measure 不一致（罕见）：按实际
                // bounds 重新 fit，保证不越界绘制。
                if layout.metrics.width > bounds_width + 0.5 {
                    let nominal = f32::from(text_style.font_size.to_pixels(window.rem_size()));
                    let pixels_per_point = crate::math::pixels_per_point(window.scale_factor());
                    let assets = cx.global_mut::<MathAssets>();
                    let Some((layout, pixel_size)) = assets.fit(
                        &self.source,
                        self.display,
                        nominal,
                        pixels_per_point,
                        bounds_width,
                    ) else {
                        return;
                    };
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
        paint_formula_image(
            &self.source,
            self.display,
            pixel_size,
            crate::math::pixels_per_point(window.scale_factor()),
            bounds.left(),
            baseline,
            color,
            window,
            cx,
        );

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
            let _ = line.paint(origin, line.ascent + line.descent, window, cx);
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
    let Some((render_image, geometry)) =
        assets.image(source, display, pixel_size, pixels_per_point, raster_scale, color)
    else {
        return;
    };
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
        Corners::default(),
        render_image,
        0,
        false,
    );
}
