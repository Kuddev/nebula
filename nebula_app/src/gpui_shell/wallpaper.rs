//! GPUI 壳的窗口视效：背景模糊 / 窗口透明度 / 壁纸。
//!
//! 语义全部对齐旧壳，但**模糊的实现手段不能照抄旧壳**（见
//! [`background_appearance`]）：一律走 GPUI 自己的
//! [`WindowBackgroundAppearance`]，由平台层落到各自的原生 API。
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
    App, Bounds, ContentMask, Corners, Hsla, Pixels, RenderImage, Window,
    WindowBackgroundAppearance, fill, point, px, size,
};
use image::{Frame, RgbaImage, imageops};
use nebula_settings::BlurModeName;

use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};

/// 全局视效状态（App global）。
pub struct VisualEffects {
    pub opacity: f32,
    pub blur: BlurModeName,
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
    /// 铺满整窗时的窗口级合成图。绝不能每帧把原图像素丢给 GPU。
    window_cache: Mutex<Option<CardWallpaper>>,
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
                    window_cache: Mutex::new(None),
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
            window_cache: Mutex::new(None),
        })
    });

    cx.set_global(VisualEffects { opacity: rt.opacity, blur: rt.blur, wallpaper });
    apply_window_effects(cx);
}

/// 当前窗口透明度（无全局时视为不透明）。
#[allow(dead_code)]
pub fn window_opacity(cx: &App) -> f32 {
    cx.try_global::<VisualEffects>().map(|v| v.opacity).unwrap_or(1.0)
}

/// 拖不透明度滑块的快路径：只把新值写进视效全局。
///
/// 不读设置文件、不重建壁纸纹理、不碰窗口级模糊——透明度只影响我们自己绘制的
/// 像素 alpha 与壳色 token，那些都是纯浪费。调用方负责紧接着调
/// [`crate::gpui_shell::theme::reapply_shell_opacity`] 与 `cx.notify()`。
pub fn set_opacity_live(opacity: f32, cx: &mut App) {
    if cx.has_global::<VisualEffects>() {
        cx.global_mut::<VisualEffects>().opacity = opacity.clamp(0.0, 1.0);
    }
}

/// 壳/卡透明度严格跟随用户滑块。模糊是独立的窗口背景外观属性，不能反向
/// 篡改透明度；否则打开模糊会把用户设置的 100% 偷改成 88%，与旧壳语义
/// 不一致。铺满 chrome 的壁纸仍沿用自己的可见性上限。
///
/// 推论：不透明度 100% 时开模糊在画面上看不出变化——模糊的是窗口**后方**
/// 的内容，被完全不透明的像素挡住了。这是两个开关正交的必然结果，不是 bug。
pub fn chrome_surface_opacity(cx: &App) -> f32 {
    let Some(effects) = cx.try_global::<VisualEffects>() else {
        return 1.0;
    };
    let mut alpha = effects.opacity.clamp(0.0, 1.0);
    if effects.wallpaper.as_ref().is_some_and(|wp| wp.cover_chrome) {
        alpha = alpha.min(0.78);
    }
    alpha
}

/// 开窗参数用。GPUI 通用层在窗口创建时就会把这个值下发到平台层
/// （`gpui::Window::new` → `platform_window.set_background_appearance`），
/// 所以启动即模糊不需要等 [`refresh`] 补第二次。
pub fn initial_background_appearance() -> WindowBackgroundAppearance {
    background_appearance(nebula_settings::RuntimeSettings::load().blur)
}

/// 模糊开关 → 窗口背景外观。**唯一落笔点**，启动与热应用共用。
///
/// # 为什么不能照抄旧壳的 DWM system backdrop
///
/// 旧壳（winit）用 `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE,
/// DWMSBT_TRANSIENTWINDOW)` 三行就拿到 Acrylic。同一段代码搬到 GPUI 壳上
/// **比无效更糟**——2026-08-21 同窗对照实测（`opacity=0`、后方放高频视频画面）：
/// 写 `DWMSBT_TRANSIENTWINDOW` 后 `HRESULT=0`、属性能读回 3，但画面变成一块
/// **不透明浅灰色板**，后方内容整块消失（既不透明也不模糊）；改回
/// `DWMSBT_NONE` 并只写 AccentPolicy Acrylic，同一窗口立刻恢复真模糊。
///
/// 根因是合成结构不同：GPUI 只要没禁用 DirectComposition 就给窗口加
/// `WS_EX_NOREDIRECTIONBITMAP`（`gpui/src/platform/windows/window.rs`），DWM
/// 不为它创建重定向表面，system backdrop 没有正确的挂载点。旧壳是普通重定向
/// 窗口，才没这个问题。
///
/// 两条已被实测否决、不要再试的"修法"：
/// 1. 运行时 `SetWindowLongPtrW` 清掉 `WS_EX_NOREDIRECTIONBITMAP` 再配
///    `SWP_FRAMECHANGED`——该样式在 `CreateWindowEx` 时就被 DWM 消费，事后改
///    不回来（实测调用后读回样式位仍然置位），窗口内容也仍走 DComp visual。
/// 2. 用 system backdrop "替代" AccentPolicy——见上，结果是不透明色板。
///
/// 上游 Zed 走过同一条弯路：PR #41842 第一版把 `Blurred` 改成
/// `DWMSBT_TRANSIENTWINDOW`，第二版又改回 `SetWindowCompositionAttribute`，
/// 只把 Mica / MicaAlt 留给 DWM backdrop。所以这里直接用 GPUI 的
/// [`WindowBackgroundAppearance::Blurred`]——Windows 平台层落到
/// `ACCENT_ENABLE_ACRYLICBLURBEHIND`（且已处理 tint alpha 为 0 时 DWM 跳过
/// 模糊的坑），macOS / Linux 各自落到原生毛玻璃。一处开关，三平台生效，
/// 不必碰 fork 也不必自写 FFI。
///
/// # 不透明度 100% 时看不到模糊是正交结果，不是失效
///
/// Acrylic 层在窗口内容**下方**。`opacity=1.00` 下我们画的像素完全不透明，
/// 模糊层被整块盖住——此时开关在画面上零变化是必然的。验收模糊必须先把
/// 不透明度调到 100% 以下，否则任何实现都会被判成"没修复"。

///
/// # 关闭时为什么是 `Opaque` 而不是 `Transparent`
///
/// Windows 上窗口透明度完全由我们绘制的像素 alpha 决定（DirectComposition
/// swapchain 固定预乘 alpha），`ACCENT_DISABLED` 下 0% 不透明度实测就能透
/// 出后方内容。`Transparent` 会额外挂 `ACCENT_ENABLE_TRANSPARENTGRADIENT`
/// 的纯色渐变层，对我们没有收益。其他平台维持原有 `Transparent` 语义。
///
/// # Mica 为什么也要 `Blurred`
///
/// Windows 侧这个返回值只决定 GPUI 平台层预写哪套 AccentPolicy，随后
/// [`apply_windows_accent_policy`] 会按档位覆写。Mica 需要的是「窗口保持可
/// 透」这件事本身——材质在窗口内容**下方**，内容不透就看不见。两档模糊在这
/// 一点上诉求相同，所以共用 `Blurred`，差异全部落在覆写那一步。
fn background_appearance(blur: BlurModeName) -> WindowBackgroundAppearance {
    if blur.enabled() {
        return WindowBackgroundAppearance::Blurred;
    }
    #[cfg(windows)]
    {
        WindowBackgroundAppearance::Opaque
    }
    #[cfg(not(windows))]
    {
        WindowBackgroundAppearance::Transparent
    }
}

/// 已经真正落到窗口上的模糊档位。拖不透明度滑块会每帧走一遍 [`refresh`]，而
/// 窗口级材质是**跨进程**调用（`SetWindowCompositionAttribute` 两次 +
/// `DwmSetWindowAttribute`）。不做门控就等于每帧和 DWM 往返三次，滑块直接
/// 拖成幻灯片——2026-08-21 实测。档位没变时一次都不碰。
struct AppliedBlur(BlurModeName);

impl gpui::Global for AppliedBlur {}

/// 把窗口层效果应用到所有窗口。透明度完全由绘制像素 alpha 控制，因此模糊
/// 开关与 0%..100% 透明度互不绑死、无需跨帧时序补丁。
///
/// # 必须 `defer`：否则热切换整条链路静默失效
///
/// 设置页开关是在**某个窗口自己的 update 回调里**点的（点击 → `toggle` →
/// `persist` → `emit(Changed)` → `on_settings_event` → `apply_runtime_settings`
/// → `apply_chrome_theme` → [`refresh`] → 这里），此时该窗口已经被
/// `App::update_window` 从 slot 里 take 出来（`gpui/src/app.rs`：
/// `cx.windows.get_mut(id)?.take()?`），对同一 handle 再 update 只会拿到
/// `Err("window not found")`。
///
/// 2026-08-21 定案：这里原先写的是 `let _ = handle.update(..)`，把那个 Err 连同
/// 整个 `set_background_appearance` 一起吞掉了——**启动时模糊有效（走
/// `WindowOptions` 的 [`initial_background_appearance`]，不经过 update），运行中
/// 点开关却完全没反应，且已经开着的 Acrylic 也关不掉**。这正是"关了还带模糊"
/// 和"不切换实时生效"的同一个根因；旧壳 winit 直接对 HWND 落 API，没有这层
/// 借用模型，所以一直是丝滑的。
///
/// [`App::defer`] 把应用推到本轮 effect cycle 末尾，那时窗口已归还 slot。
/// update 失败不再静默：留 warn，避免同一个坑第三次被当成"DWM 不生效"。
///
/// # 模糊态没变就直接返回
///
/// 不透明度/壁纸改动也会走到这里，但它们只影响我们自己绘制的像素，窗口级
/// 模糊属性一个字节都不用改。透明度是滑块，一次拖拽几十上百个事件，所以这条
/// 短路是拖拽手感的必要条件，不是可选优化。重绘由调用链上的 `cx.notify()`
/// 与主题重建负责。
fn apply_window_effects(cx: &mut App) {
    // 全局缺失时按"关"处理而不是缺省档：这条路径只在极早期或异常态走到，
    // 宁可少一层材质，也不要凭空给窗口开上模糊再被 refresh 纠正一次。
    let blur = cx.try_global::<VisualEffects>().map(|v| v.blur).unwrap_or(BlurModeName::None);
    if cx.try_global::<AppliedBlur>().map(|applied| applied.0) == Some(blur) {
        return;
    }
    cx.set_global(AppliedBlur(blur));
    let appearance = background_appearance(blur);
    cx.defer(move |cx| {
        for handle in cx.windows() {
            if let Err(err) = handle.update(cx, |_, window, _| {
                window.set_background_appearance(appearance);
                #[cfg(windows)]
                apply_windows_accent_policy(window, blur);
                window.refresh();
            }) {
                log::warn!("failed to apply window visual effects: {err}");
            }
        }
    });
}

/// 显式落下 Windows 材质属性：AccentPolicy 与 system backdrop 二选一。
///
/// # 三档各自写什么
///
/// | 档位 | AccentPolicy | SYSTEMBACKDROP | DWM 每帧成本 |
/// |---|---|---|---|
/// | `None` | 全零 | `DWMSBT_NONE` | 无 |
/// | `Mica` | 全零 | `DWMSBT_MAINWINDOW` | 无（只在窗口移动时重采样壁纸） |
/// | `Acrylic` | state 4 + 非零 alpha | `DWMSBT_NONE` | 整窗实时高斯模糊 |
///
/// 两条通道**必须互斥**：同时开 Acrylic 与 system backdrop 时 DWM 的行为未
/// 定义（实测表现为 backdrop 赢，Acrylic 被吞）。所以每档都要把另一条显式
/// 写回中性值，不能只写自己那条。
///
/// # `DWMSBT_MAINWINDOW`(Mica) 能用，`DWMSBT_TRANSIENTWINDOW`(Acrylic 材质) 不能
///
/// 2026-08-21 记录过 `DWMSBT_TRANSIENTWINDOW` 在本壳上变成不透明浅灰色板，
/// 当时归因于 GPUI 的 `WS_EX_NOREDIRECTIONBITMAP`（DComp 窗口没有重定向表面
/// 给 system backdrop 挂载）。**2026-08-22 用户实测推翻了这个外推**：同一族的
/// `DWMSBT_MAINWINDOW` 在本壳上工作正常，能正确显示模糊的桌面壁纸色调。
///
/// 也就是说那次失败是 `TRANSIENTWINDOW` 这一个值的问题，不是整个 system
/// backdrop 家族的问题。**不要**因为看到旧注释就把 Mica 一起判死；反过来也
/// **不要**因为 Mica 能用就再去试 `TRANSIENTWINDOW`——那一条已经实测两次。
///
/// # Mica 天生没有玻璃感，靠壳侧自绘补
///
/// Acrylic 的观感由四层构成：实时背景采样 + 高斯模糊 + 噪点纹理 + 饱和度
/// 提升。Mica 只有前两层的廉价版（采样桌面壁纸 + 强模糊 + 色调映射），刻意
/// 不做噪点与饱和度——它的设计目标是"低调的材质底色"而非"毛玻璃"。
///
/// 所以 Mica 档的玻璃感由 [`paint_glass_overlay`] 在壳侧自绘补齐：噪点纹理与
/// tint 恰好是 Acrylic 里最便宜的两层，自己画的成本接近零，还换来完全可调。
///
/// `SetWindowCompositionAttribute` 未进入公开 SDK，所以和 GPUI 上游一样动态取
/// 函数地址；backdrop 与 `DwmFlush` 则用公开 DWM API。任一步失败都留日志，
/// 避免把 API 失败再次误判成"设置没有热应用"。
#[cfg(windows)]
fn apply_windows_accent_policy(window: &Window, blur: BlurModeName) {
    use windows_sys::Win32::Foundation::{BOOL, HWND};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: u32,
        data: *mut core::ffi::c_void,
        size: usize,
    }

    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;
    let backdrop: i32 = match blur {
        BlurModeName::Mica => DWMSBT_MAINWINDOW,
        BlurModeName::None | BlurModeName::Acrylic => DWMSBT_NONE,
    };
    let mut accent = match blur {
        BlurModeName::Acrylic => AccentPolicy {
            state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
            flags: 0,
            // alpha=0 会让部分 DWM 版本直接跳过 Acrylic。
            gradient_color: 0x0100_0000,
            animation_id: 0,
        },
        // Mica 走 backdrop 通道，AccentPolicy 必须让位——否则 Acrylic 残留会
        // 盖住 Mica，切档时看起来像"没生效"。
        BlurModeName::None | BlurModeName::Mica => {
            AccentPolicy { state: 0, flags: 0, gradient_color: 0, animation_id: 0 }
        },
    };
    let mut data = WindowCompositionAttributeData {
        attribute: 19, // WCA_ACCENT_POLICY
        data: &mut accent as *mut _ as *mut core::ffi::c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };

    // SAFETY: hwnd 来自当前存活的 GPUI 窗口；DWM 属性值、AccentPolicy 与
    // WCA 数据布局均与 Windows ABI/GPUI 后端一致。无效句柄由返回值安全报告。
    //
    // 这里**不要**再调 `DwmFlush`：它会阻塞等待下一次 DWM 合成（一个 vsync，
    // 约 8~16ms）。AccentPolicy 与 backdrop 的改动本来就在下一帧生效，配合
    // 调用方的 `window.refresh()` 已经即时；而 2026-08-21 实测，一旦这条路径
    // 被每帧走到（拖不透明度滑块），那次 flush 就把滑块拖成了幻灯片。
    let (backdrop_result, accent_result) = unsafe {
        let backdrop_result = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            &backdrop as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );

        let user32 = GetModuleHandleA(c"user32.dll".as_ptr() as *const u8);
        let accent_result = if user32.is_null() {
            None
        } else {
            GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr() as *const u8).map(
                |procedure| {
                    let set_attribute: SetWindowCompositionAttribute =
                        std::mem::transmute(procedure);
                    set_attribute(hwnd, &mut data)
                },
            )
        };
        (backdrop_result, accent_result)
    };
    if backdrop_result < 0 {
        log::warn!(
            "DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE={backdrop}) failed: HRESULT=0x{:08X}",
            backdrop_result as u32
        );
    }
    match accent_result {
        Some(0) => log::warn!("SetWindowCompositionAttribute(WCA_ACCENT_POLICY) failed"),
        None => log::warn!("SetWindowCompositionAttribute is unavailable in user32.dll"),
        Some(_) => {},
    }
}

/// 解码壁纸：RGBA8 → 把图片透明度烘进 alpha → BGRA（gpui 图像帧的
/// 通道序，zed 的图片资产装载器同款处理）。
fn decode(
    path: &std::path::Path,
    opacity: f32,
) -> Option<(Arc<RenderImage>, Arc<RgbaImage>, u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    let mut rgba = image::load_from_memory(&bytes).ok()?.into_rgba8();
    // 4K+ 原图若每帧按窗口重采样会卡死设置页开关。先压到长边 2560。
    const MAX_EDGE: u32 = 2560;
    let (src_w, src_h) = rgba.dimensions();
    if src_w.max(src_h) > MAX_EDGE {
        let scale = MAX_EDGE as f32 / src_w.max(src_h) as f32;
        let w = (src_w as f32 * scale).round().max(1.0) as u32;
        let h = (src_h as f32 * scale).round().max(1.0) as u32;
        rgba = imageops::resize(&rgba, w, h, imageops::FilterType::Triangle);
    }
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

/// 噪点 tile 边长（物理像素）。越大平铺次数越少、内存越高：512 时 3K 屏约
/// 28 次 `paint_image`，1MB 纹理——两头都便宜。
const NOISE_TILE_PX: u32 = 512;
/// 噪点强度。Acrylic 自带的颗粒非常细微（目测 3~5%），高于 ~8% 会从"玻璃"
/// 变成"脏"。
const NOISE_ALPHA: u8 = 14;
/// 白色 tint 浓度。暗色主题要更厚——Mica 的壁纸色调在暗色下偏沉，正是它
/// "不够透亮"的主因；亮色主题本就够亮，加太多会过曝。
const TINT_ALPHA_DARK: f32 = 0.08;
const TINT_ALPHA_LIGHT: f32 = 0.05;

thread_local! {
    /// 噪点 tile 只依赖上面几个常量，进程内生成一次即可。用 `thread_local`
    /// 而不是 `OnceLock`：`RenderImage` 只在渲染线程用，不必为跨线程共享去
    /// 背 `Send + Sync` 的约束。
    static NOISE_TILE: std::cell::RefCell<Option<Arc<RenderImage>>> =
        const { std::cell::RefCell::new(None) };
}

/// 生成（并缓存）噪点 tile。
fn noise_tile() -> Arc<RenderImage> {
    NOISE_TILE.with(|slot| {
        if let Some(tile) = slot.borrow().as_ref() {
            return tile.clone();
        }
        let mut buffer = RgbaImage::new(NOISE_TILE_PX, NOISE_TILE_PX);
        // LCG（数值出自 Numerical Recipes）：确定性、零依赖。噪点只要"看起来
        // 随机"，不需要统计学质量；确定性还让同一台机器每次启动的颗粒一致。
        let mut state: u32 = 0x9E37_79B9;
        for pixel in buffer.chunks_exact_mut(4) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // 取高位：LCG 的低位周期极短，直接用会出现肉眼可见的条带。
            let luma = (state >> 24) as u8;
            // GPUI 的图像帧是预乘 alpha，颜色必须先乘进去，否则半透明噪点
            // 整体偏亮。三通道同值，所以不必像 `decode` 那样 swap 成 BGRA。
            let premultiplied = ((u16::from(luma) * u16::from(NOISE_ALPHA)) / 255) as u8;
            pixel[0] = premultiplied;
            pixel[1] = premultiplied;
            pixel[2] = premultiplied;
            pixel[3] = NOISE_ALPHA;
        }
        let tile = Arc::new(RenderImage::new([Frame::new(buffer)]));
        *slot.borrow_mut() = Some(tile.clone());
        tile
    })
}

/// Mica 档的玻璃增强层：白 tint + 噪点颗粒，把 DWM 那层寡淡的壁纸色调补成
/// 接近 Acrylic 的毛玻璃观感。整窗一层，画在 chrome 与壁纸**之下**。
///
/// # 为什么这层值得自己画
///
/// Acrylic 的观感由四层构成：实时背景采样 + 高斯模糊 + 噪点纹理 + 亮度/
/// 饱和度提升。前两层是 DWM 每帧的重活，也正是 2026-08-22 实测里 `dwm.exe`
/// 均值 28% 的来源；后两层则是**纯静态叠加**——一张平铺小图加一个半透明
/// 矩形，在我们自己的绘制层里成本接近零。
///
/// Mica 恰好只给前两层的廉价版（采样桌面壁纸 + 强模糊，且只在窗口移动时
/// 更新），刻意省掉后两层。于是"拿 Mica 的成本 + 补 Acrylic 的观感"成立，
/// 这就是 2026-08-22 用户裁定的甜品区。
///
/// 调玻璃感只动本文件顶部那四个常量，不要改绘制顺序：tint 必须在噪点之下，
/// 否则颗粒会被 tint 冲淡到看不见。
///
/// Acrylic 档不叠这层——它自带噪点与亮度提升，再叠一遍就糊了。
pub fn paint_glass_overlay(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    let Some(effects) = cx.try_global::<VisualEffects>() else { return };
    if effects.blur != BlurModeName::Mica {
        return;
    }

    let is_light = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin().is_light;
    let tint = if is_light { TINT_ALPHA_LIGHT } else { TINT_ALPHA_DARK };
    window.paint_quad(fill(bounds, Hsla { h: 0.0, s: 0.0, l: 1.0, a: tint }));

    // 平铺而不是"按窗口尺寸生成一张大图"：后者在 3K 屏上是 25MB 纹理，且每次
    // resize 都要重新填充六百万像素——resize 是交互路径，不能挂这种活。
    //
    // `paint_image` 内部会 `bounds.scale(scale_factor)`，即入参是**逻辑**像素。
    // tile 是按物理像素生成的，所以这里必须先除以 scale_factor 才能得到 1:1
    // 的落点——否则在 3K/200% 屏上每个噪点会被 GPU 放大成 2×2 像素块，颗粒
    // 糊成噪斑（旧壳"禁 GPU 拉伸"那条清晰度铁律同源）。
    let tile = noise_tile();
    let scale = window.scale_factor().max(0.5);
    let step = px(NOISE_TILE_PX as f32 / scale);
    let right = bounds.origin.x + bounds.size.width;
    let bottom = bounds.origin.y + bounds.size.height;
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let mut y = bounds.origin.y;
        while y < bottom {
            let mut x = bounds.origin.x;
            while x < right {
                let _ = window.paint_image(
                    Bounds::new(point(x, y), size(step, step)),
                    Corners::default(),
                    tile.clone(),
                    0,
                    false,
                );
                x += step;
            }
            y += step;
        }
    });
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
    let Some(image) = window_wallpaper(bounds, window, wp) else { return };
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let _ = window.paint_image(bounds, Corners::all(px(0.0)), image, 0, false);
    });
}

/// 整窗壁纸：按视口物理像素合成一次并缓存。打开 cover 时绝不能把
/// 原图丢给 GPU 每帧缩放——那是设置开关「一点就卡死」的根因。
fn window_wallpaper(
    bounds: Bounds<Pixels>,
    window: &Window,
    wp: &Wallpaper,
) -> Option<Arc<RenderImage>> {
    let scale = window.scale_factor().max(0.5);
    let width = (f32::from(bounds.size.width) * scale).ceil().max(1.0) as u32;
    let height = (f32::from(bounds.size.height) * scale).ceil().max(1.0) as u32;
    let key = CardWallpaperKey {
        card_width: width,
        card_height: height,
        anchor_width: width,
        anchor_height: height,
        crop_x: 0,
        crop_y: 0,
    };
    if let Some(image) = wp.window_cache.lock().ok().and_then(|cache| {
        cache.as_ref().filter(|cached| cached.key == key).map(|c| c.image.clone())
    }) {
        return Some(image);
    }
    let composed = compose_anchor(wp, width, height);
    let image = Arc::new(RenderImage::new([Frame::new(composed)]));
    if let Ok(mut cache) = wp.window_cache.lock() {
        *cache = Some(CardWallpaper { key, image: image.clone() });
    }
    Some(image)
}
