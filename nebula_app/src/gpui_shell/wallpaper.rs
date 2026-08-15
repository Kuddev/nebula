//! GPUI 壳的窗口视效：背景模糊 / 窗口透明度 / 壁纸。
//!
//! 语义全部对齐旧壳：
//! - 模糊 = DWM 系统 backdrop（`DWMSBT_TRANSIENTWINDOW`，旧壳
//!   `display::window::apply_windows_backdrop` 同款调用；< 22621 的系统该
//!   调用无效，自动回落纯 alpha 透明）。
//! - 透明度 = 壳底色与终端默认背景的 alpha（文字与彩色单元背景保持不
//!   透明，对比度不塌——旧壳 `draw_window_backdrop` 裁定）。
//! - 壁纸 = 底色之上、单元格之下的一层图（旧壳 `renderer::image` 的
//!   fit/alignment/透明度语义；图自身透明度独立于窗口 opacity）。
//!
//! 设置来源是共享层 `nebula_settings`（新增壁纸五键），解码结果按
//! (路径, mtime, 透明度) 缓存；[`refresh`] 在启动与设置热应用时调用。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use gpui::{
    App, Bounds, ContentMask, Corners, Pixels, RenderImage, Window, WindowBackgroundAppearance,
    point, px, size,
};
use image::{Frame, RgbaImage, imageops};

use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};

/// 全局视效状态（App global）。
pub struct VisualEffects {
    pub opacity: f32,
    pub blur: bool,
    wallpaper: Option<Wallpaper>,
}

impl gpui::Global for VisualEffects {}

struct Wallpaper {
    image: Arc<RenderImage>,
    /// 已烘焙壁纸透明度、并转换成 GPUI 所需 BGRA 顺序的原始像素。
    /// 卡片模式要先把目标图裁成卡片大小，才能让 GPUI 对最终边界做圆角。
    source: Arc<RgbaImage>,
    /// 原图像素尺寸（fit 数学用）。
    width: u32,
    height: u32,
    fit: BackgroundImageFit,
    alignment: BackgroundImageAlignment,
    /// 铺满整窗（chrome 之下也画）而非仅终端卡。
    cover_chrome: bool,
    /// 解码缓存键。
    path: PathBuf,
    mtime: Option<SystemTime>,
    baked_opacity: f32,
    /// 当前窗口只显示一张正文卡；保留最近一次尺寸即可覆盖拖拽 resize，
    /// 又不会让连续缩放积累大量 GPU 纹理。
    card_cache: Mutex<Option<CardWallpaper>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CardWallpaperKey {
    card_width: u32,
    card_height: u32,
    anchor_width: u32,
    anchor_height: u32,
    crop_x: i32,
    crop_y: i32,
}

struct CardWallpaper {
    key: CardWallpaperKey,
    image: Arc<RenderImage>,
}

/// 读取设置并重建视效状态；壁纸命中缓存键则复用已解码纹理。
/// 随后把窗口层效果（背景外观 + DWM backdrop）应用到所有已开窗口。
pub fn refresh(cx: &mut App) {
    let rt = nebula_settings::RuntimeSettings::load();

    let wallpaper = rt.background_image.as_ref().and_then(|raw_path| {
        let path = PathBuf::from(raw_path);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let opacity = rt.background_image_opacity;
        let fit = rt
            .background_image_fit
            .as_deref()
            .and_then(BackgroundImageFit::parse)
            .unwrap_or_default();
        let alignment = rt
            .background_image_alignment
            .as_deref()
            .and_then(BackgroundImageAlignment::parse)
            .unwrap_or_default();

        let cover_chrome = rt.background_image_cover_chrome;

        // 缓存：路径/mtime/烘焙透明度都没变就不重解码。
        let cached = cx.try_global::<VisualEffects>().and_then(|v| v.wallpaper.as_ref());
        if let Some(prev) = cached {
            if prev.path == path && prev.mtime == mtime && prev.baked_opacity == opacity {
                return Some(Wallpaper {
                    image: prev.image.clone(),
                    source: prev.source.clone(),
                    width: prev.width,
                    height: prev.height,
                    fit,
                    alignment,
                    cover_chrome,
                    path,
                    mtime,
                    baked_opacity: opacity,
                    card_cache: Mutex::new(None),
                });
            }
        }

        let (image, source, width, height) = decode(&path, opacity)?;
        Some(Wallpaper {
            image,
            source,
            width,
            height,
            fit,
            alignment,
            cover_chrome,
            path,
            mtime,
            baked_opacity: opacity,
            card_cache: Mutex::new(None),
        })
    });

    cx.set_global(VisualEffects { opacity: rt.opacity, blur: rt.blur, wallpaper });
    apply_window_effects(cx);
}

/// 当前窗口透明度（无全局时视为不透明）。
pub fn window_opacity(cx: &App) -> f32 {
    cx.try_global::<VisualEffects>().map(|v| v.opacity).unwrap_or(1.0)
}

/// 开窗参数用：按设置决定窗口背景外观。透明/模糊窗口必须在创建时就
/// 声明成非 Opaque，否则合成器不给 alpha 通道。
pub fn initial_background_appearance() -> WindowBackgroundAppearance {
    let rt = nebula_settings::RuntimeSettings::load();
    if rt.blur || rt.opacity < 1.0 {
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// 把窗口层效果应用到所有窗口：gpui 背景外观（alpha 合成通道）+
/// DWM 系统 backdrop（真正的模糊；旧壳同款，关闭写 NONE 不写 AUTO）。
fn apply_window_effects(cx: &mut App) {
    let (blur, opacity) =
        cx.try_global::<VisualEffects>().map(|v| (v.blur, v.opacity)).unwrap_or((false, 1.0));
    let appearance = if blur || opacity < 1.0 {
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Opaque
    };
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| {
            window.set_background_appearance(appearance);
            apply_dwm_backdrop(window, blur);
        });
    }
}

/// 旧壳 `apply_windows_backdrop` 的 gpui 版：Win11 22621+ 的系统级
/// acrylic（TRANSIENT），低版本上调用失败自动退化为纯 alpha。
#[cfg(windows)]
fn apply_dwm_backdrop(window: &Window, enabled: bool) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // 显式走 trait 方法：`Window` 自带的固有方法 `window_handle()`
    // 返回的是 gpui 的 AnyWindowHandle，会遮蔽同名 trait 方法。
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;

    // 关掉时写 NONE 而不是 AUTO：AUTO 把决定权交还系统，"关闭"会没反应。
    let backdrop: i32 = if enabled { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_NONE };
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            &backdrop as *const _ as *const core::ffi::c_void,
            size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn apply_dwm_backdrop(_window: &Window, _enabled: bool) {}

/// 解码壁纸：RGBA8 → 把图片透明度烘进 alpha → BGRA（gpui 图像帧的
/// 通道序，zed 的图片资产装载器同款处理）。
fn decode(
    path: &std::path::Path,
    opacity: f32,
) -> Option<(Arc<RenderImage>, Arc<RgbaImage>, u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    let mut rgba = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (width, height) = rgba.dimensions();
    let factor = opacity.clamp(0.0, 1.0);
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = (f32::from(pixel[3]) * factor).round() as u8;
        pixel.swap(0, 2);
    }
    let source = Arc::new(rgba);
    let image = Arc::new(RenderImage::new([Frame::new((*source).clone())]));
    Some((image, source, width, height))
}

/// fit/alignment 布局数学（与旧壳 `renderer::image` 一致）：以 `anchor`
/// 为定位空间求壁纸目标矩形。native 模式按设备像素 1:1（逻辑尺寸 =
/// 像素 / 缩放），与旧壳"native pixel size"语义相同。
fn layout_target(anchor: Bounds<Pixels>, wp: &Wallpaper, scale: f32) -> Option<Bounds<Pixels>> {
    let bw = f32::from(anchor.size.width);
    let bh = f32::from(anchor.size.height);
    if bw <= 1.0 || bh <= 1.0 {
        return None;
    }
    let iw = wp.width.max(1) as f32;
    let ih = wp.height.max(1) as f32;

    let (tw, th) = match wp.fit {
        BackgroundImageFit::Fill => (bw, bh),
        BackgroundImageFit::Uniform => {
            let s = (bw / iw).min(bh / ih);
            (iw * s, ih * s)
        },
        BackgroundImageFit::UniformToFill => {
            let s = (bw / iw).max(bh / ih);
            (iw * s, ih * s)
        },
        BackgroundImageFit::None => (iw / scale, ih / scale),
    };
    let (fx, fy) = wp.alignment.factors();
    Some(Bounds::new(
        point(anchor.origin.x + px((bw - tw) * fx), anchor.origin.y + px((bh - th) * fy)),
        size(px(tw), px(th)),
    ))
}

/// 终端卡的壁纸层（卡底色之上、内容之下；由卡容器的 canvas 调用，
/// 覆盖整卡含内边距带）。
///
/// 卡模式：以卡为定位空间。铺满整窗模式：以整窗为定位空间、这里只
/// 画卡内那片切片——图在窗口坐标系里连续，与 chrome 底层无缝。
pub fn paint_wallpaper_card(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    let Some(effects) = cx.try_global::<VisualEffects>() else { return };
    let Some(wp) = &effects.wallpaper else { return };
    let Some(image) = card_wallpaper(bounds, window, wp) else { return };

    // GPUI 的 ContentMask 只有矩形，paint_image 的半径又作用于图片自身
    // bounds，而不是独立的裁剪矩形。cover/contain/native 的图片 bounds
    // 往往不等于卡片，直接画会把四角重新铺方。这里传入已裁成卡片尺寸的
    // 纹理，使图片 bounds 与旧壳 shader 的圆角 clip rect 完全重合。
    let _ = window.paint_image(
        bounds,
        Corners::all(crate::gpui_shell::theme::card_radius()),
        image,
        0,
        false,
    );
}

/// 生成卡片尺寸的壁纸纹理。卡片模式以卡片为布局锚点；铺满整窗模式先按
/// 整窗布局，再截取卡片所在切片，保证卡内外图案连续。
fn card_wallpaper(
    bounds: Bounds<Pixels>,
    window: &Window,
    wp: &Wallpaper,
) -> Option<Arc<RenderImage>> {
    let scale = window.scale_factor().max(0.5);
    let card_width = (f32::from(bounds.size.width) * scale).ceil().max(1.0) as u32;
    let card_height = (f32::from(bounds.size.height) * scale).ceil().max(1.0) as u32;

    let (anchor_width, anchor_height, crop_x, crop_y) = if wp.cover_chrome {
        let viewport = window.viewport_size();
        (
            (f32::from(viewport.width) * scale).ceil().max(1.0) as u32,
            (f32::from(viewport.height) * scale).ceil().max(1.0) as u32,
            (f32::from(bounds.origin.x) * scale).floor() as i32,
            (f32::from(bounds.origin.y) * scale).floor() as i32,
        )
    } else {
        (card_width, card_height, 0, 0)
    };

    let key =
        CardWallpaperKey { card_width, card_height, anchor_width, anchor_height, crop_x, crop_y };
    if let Some(image) = wp.card_cache.lock().ok().and_then(|cache| {
        cache.as_ref().filter(|cached| cached.key == key).map(|c| c.image.clone())
    }) {
        return Some(image);
    }

    let anchor = compose_anchor(wp, anchor_width, anchor_height);
    let card = if wp.cover_chrome { crop_card(&anchor, key) } else { anchor };
    let image = Arc::new(RenderImage::new([Frame::new(card)]));
    if let Ok(mut cache) = wp.card_cache.lock() {
        *cache = Some(CardWallpaper { key, image: image.clone() });
    }
    Some(image)
}

/// 按旧层 `wallpaper_rect` 的 fit/alignment 语义合成完整锚点图。对 cover
/// 先裁源图再缩放，避免极端长宽比图片产生远大于窗口的临时位图。
fn compose_anchor(wp: &Wallpaper, width: u32, height: u32) -> RgbaImage {
    use image::imageops::FilterType;

    let source = wp.source.as_ref();
    let source_width = source.width().max(1);
    let source_height = source.height().max(1);
    let (fx, fy) = wp.alignment.factors();

    match wp.fit {
        BackgroundImageFit::Fill => imageops::resize(source, width, height, FilterType::Triangle),
        BackgroundImageFit::UniformToFill => {
            let source_aspect = source_width as f32 / source_height as f32;
            let target_aspect = width as f32 / height as f32;
            let (crop_width, crop_height) = if source_aspect > target_aspect {
                (
                    ((source_height as f32 * target_aspect).round() as u32).clamp(1, source_width),
                    source_height,
                )
            } else {
                (
                    source_width,
                    ((source_width as f32 / target_aspect).round() as u32).clamp(1, source_height),
                )
            };
            let crop_x = ((source_width - crop_width) as f32 * fx).round() as u32;
            let crop_y = ((source_height - crop_height) as f32 * fy).round() as u32;
            let cropped = imageops::crop_imm(source, crop_x, crop_y, crop_width, crop_height);
            imageops::resize(&cropped.to_image(), width, height, FilterType::Triangle)
        },
        BackgroundImageFit::Uniform => {
            let scale =
                (width as f32 / source_width as f32).min(height as f32 / source_height as f32);
            let draw_width = (source_width as f32 * scale).round().max(1.0) as u32;
            let draw_height = (source_height as f32 * scale).round().max(1.0) as u32;
            let resized = imageops::resize(source, draw_width, draw_height, FilterType::Triangle);
            let mut output = RgbaImage::new(width, height);
            let x = ((width - draw_width) as f32 * fx).round() as i64;
            let y = ((height - draw_height) as f32 * fy).round() as i64;
            imageops::overlay(&mut output, &resized, x, y);
            output
        },
        BackgroundImageFit::None => {
            let mut output = RgbaImage::new(width, height);
            let x = ((width as i64 - source_width as i64) as f32 * fx).round() as i64;
            let y = ((height as i64 - source_height as i64) as f32 * fy).round() as i64;
            imageops::overlay(&mut output, source, x, y);
            output
        },
    }
}

/// 从整窗锚点图中取出卡片切片。常规布局完全位于视口内；边界保护用于
/// DPI/开窗过渡帧，避免 1px 的负坐标或越界导致 panic。
fn crop_card(anchor: &RgbaImage, key: CardWallpaperKey) -> RgbaImage {
    let mut card = RgbaImage::new(key.card_width, key.card_height);
    let source_x = key.crop_x.max(0) as u32;
    let source_y = key.crop_y.max(0) as u32;
    let target_x = key.crop_x.saturating_neg() as u32;
    let target_y = key.crop_y.saturating_neg() as u32;
    let copy_width =
        key.card_width.saturating_sub(target_x).min(anchor.width().saturating_sub(source_x));
    let copy_height =
        key.card_height.saturating_sub(target_y).min(anchor.height().saturating_sub(source_y));
    if copy_width == 0 || copy_height == 0 {
        return card;
    }

    let slice = imageops::crop_imm(anchor, source_x, source_y, copy_width, copy_height).to_image();
    imageops::replace(&mut card, &slice, i64::from(target_x), i64::from(target_y));
    card
}

/// 整窗壁纸底层（chrome 之下），仅铺满整窗模式绘制；workspace 根部的
/// canvas 调用。侧栏/标题栏以壳色 alpha 盖在其上（透明度低时透出），
/// 终端卡内的那片由卡容器在卡底色之上重画（旧壳同一层模型）。
pub fn paint_wallpaper_under_chrome(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    let Some(effects) = cx.try_global::<VisualEffects>() else { return };
    let Some(wp) = &effects.wallpaper else { return };
    if !wp.cover_chrome {
        return;
    }
    let scale = window.scale_factor().max(0.5);
    let Some(target) = layout_target(bounds, wp, scale) else { return };
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let _ = window.paint_image(target, Corners::all(px(0.0)), wp.image.clone(), 0, false);
    });
}
