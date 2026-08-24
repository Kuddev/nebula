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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[cfg(windows)]
use std::sync::OnceLock;

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
/// （`gpui::Window::new` → `platform_window.set_background_appearance`）。
/// Mica / Mica Alt 因此从首帧就走平台原生 backdrop，不再先挂一层普通透明背景。
pub fn initial_background_appearance() -> WindowBackgroundAppearance {
    background_appearance(nebula_settings::RuntimeSettings::load().blur)
}

/// 模糊开关 → 窗口背景外观。**唯一落笔点**，启动与热应用共用。
///
/// # 两条 Windows 原生通道为什么要分开
///
/// Mica / Mica Alt 分别使用 GPUI 的 `MicaBackdrop` / `MicaAltBackdrop`，由平台层
/// 映射到 `DWMSBT_MAINWINDOW` / `DWMSBT_TABBEDWINDOW`。Nebula 不读取壁纸文件，
/// 也不自行猜测多显示器排布。
///
/// Aero/Acrylic 继续使用 GPUI 已验证的 AccentPolicy 通道。Aero 额外使用
/// `DwmEnableBlurBehindWindow` + 半透明深色玻璃配方；GPUI 的
/// DirectComposition 窗口上，`DWMSBT_TRANSIENTWINDOW` 会形成不透明灰板，不能
/// 因为 Mica 与它同属 system backdrop 就混用；切换档位时会显式清理另一条通道。
///
/// # 不透明度 100% 时看不到模糊是正交结果，不是失效
///
/// Acrylic 层在窗口内容**下方**。`opacity=1.00` 下我们画的像素完全不透明，
/// 模糊层被整块盖住——此时开关在画面上零变化是必然的。验收模糊必须先把
/// 不透明度调到 100% 以下，否则任何实现都会被判成"没修复"。

///
/// # 关闭材质时为什么仍是 `Transparent`
///
/// GPUI 的 Windows renderer 会按这个枚举选择清屏 alpha：`Opaque` 固定以
/// alpha=1 清空交换链，场景中后续绘制的透明像素无法把它重新变透明。因此
/// `None` 也必须保留透明交换链；紧随其后的 Windows 原生清理会关闭 WCA 与
/// DWMSBT，最终语义是“窗口可透明，但没有任何模糊材质”。
///
/// # 哪些档位要窗口保持可透
///
/// `Aero` / `Acrylic` 的材质由 DWM 画在窗口内容**下方**，内容不透就看不见，所以
/// 要 `Blurred`——这个返回值决定 GPUI 平台层预写哪套 AccentPolicy（启动即生效，
/// 不必等 [`refresh`] 补第二次），随后 [`apply_windows_accent_policy`] 覆写成
/// state 3 / state 4。
///
/// `Mica` / `Mica Alt` 在 Windows 11 22H2 起直接使用 GPUI 原生枚举；较旧系统
/// 依次回退为经典模糊或普通透明。不能回退到 `Opaque`，否则客户区像素会遮住
/// DWM 在窗口下方合成的材质。
fn background_appearance(blur: BlurModeName) -> WindowBackgroundAppearance {
    #[cfg(windows)]
    {
        match blur {
            BlurModeName::Aero | BlurModeName::Acrylic => WindowBackgroundAppearance::Blurred,
            BlurModeName::Mica if windows_build_number() >= 22_621 => {
                WindowBackgroundAppearance::MicaBackdrop
            },
            BlurModeName::MicaAlt if windows_build_number() >= 22_621 => {
                WindowBackgroundAppearance::MicaAltBackdrop
            },
            BlurModeName::Mica | BlurModeName::MicaAlt if windows_build_number() >= 17_763 => {
                WindowBackgroundAppearance::Blurred
            },
            BlurModeName::Mica | BlurModeName::MicaAlt => WindowBackgroundAppearance::Transparent,
            BlurModeName::None => WindowBackgroundAppearance::Transparent,
        }
    }
    #[cfg(not(windows))]
    {
        if blur.enabled() {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Transparent
        }
    }
}

/// `GetVersionEx` 会受应用兼容清单影响；RtlGetVersion 才能可靠决定公开的
/// `DWMWA_SYSTEMBACKDROP_TYPE` 是否存在。缓存结果，避免热应用时重复进内核。
#[cfg(windows)]
fn windows_build_number() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
        use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = std::mem::size_of_val(&info) as u32;
        let status = unsafe { RtlGetVersion(&mut info) };
        if status == 0 { info.dwBuildNumber } else { 0 }
    })
}

/// 已经真正落到窗口上的模糊档位。拖不透明度滑块会每帧走一遍 [`refresh`]，而
/// 窗口级材质是**跨进程**调用（`SetWindowCompositionAttribute` 两次 +
/// `DwmSetWindowAttribute`）。不做门控就等于每帧和 DWM 往返三次，滑块直接
/// 拖成幻灯片——2026-08-21 实测。档位没变时一次都不碰。
struct AppliedBlur {
    blur: BlurModeName,
    windows: HashSet<gpui::WindowId>,
}

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
/// # 模糊态没变时只处理新窗口
///
/// 不透明度/壁纸改动也会走到这里，但它们只影响我们自己绘制的像素，窗口级
/// 模糊属性一个字节都不用改。透明度是滑块，一次拖拽几十上百个事件，所以这条
/// 短路是拖拽手感的必要条件，不是可选优化。多窗口下则按 `WindowId` 补应用新窗，
/// 避免全局档位相同就让第二个窗口漏掉原生 backdrop。
fn apply_window_effects(cx: &mut App) {
    // 全局缺失时按"关"处理而不是缺省档：这条路径只在极早期或异常态走到，
    // 宁可少一层材质，也不要凭空给窗口开上模糊再被 refresh 纠正一次。
    let blur = cx.try_global::<VisualEffects>().map(|v| v.blur).unwrap_or(BlurModeName::None);
    let appearance = background_appearance(blur);
    cx.defer(move |cx| {
        // 必须在 defer 后枚举：触发设置变更的窗口此时才重新放回 App 窗口表。
        let handles = cx.windows();
        let window_ids = handles.iter().map(|handle| handle.window_id()).collect::<HashSet<_>>();
        let already_applied = cx
            .try_global::<AppliedBlur>()
            .filter(|applied| applied.blur == blur)
            .map(|applied| applied.windows.clone())
            .unwrap_or_default();
        let pending = handles
            .into_iter()
            .filter(|handle| !already_applied.contains(&handle.window_id()))
            .collect::<Vec<_>>();
        cx.set_global(AppliedBlur { blur, windows: window_ids });
        for handle in pending {
            if let Err(err) = handle.update(cx, |_, window, _| {
                window.set_background_appearance(appearance);
                #[cfg(windows)]
                apply_windows_accent_policy(window, blur, appearance);
                window.refresh();
            }) {
                log::warn!("failed to apply window visual effects: {err}");
            }
        }
    });
}

/// 显式落下 Windows 材质属性。
///
/// # 五档各自写什么
///
/// | 档位 | AccentPolicy | SYSTEMBACKDROP | DWM 每帧成本 |
/// |---|---|---|---|
/// | `None` | 全零 | `DWMSBT_NONE` | 无 |
/// | `Aero` | state 3 + 玻璃色调 | `DWMSBT_NONE` | 整窗实时玻璃模糊 |
/// | `Mica` | 全零 | `DWMSBT_MAINWINDOW` | 系统壁纸 backdrop |
/// | `Mica Alt` | 全零 | `DWMSBT_TABBEDWINDOW` | 强色调系统壁纸 backdrop |
/// | `Acrylic` | state 4 + 非零 alpha | `DWMSBT_NONE` | 实时模糊 + tint/噪点/饱和 |
///
/// `Mica` / `Mica Alt` 使用公开 DWM 属性。Nebula 不读取
/// `SPI_GETDESKWALLPAPER` 或 `TranscodedWallpaper`；显示器选择、壁纸排布、模糊和
/// 色调全部交给系统合成器。Windows 不支持该属性时退回 Acrylic，避免透明空洞。
///
/// 两条通道**必须互斥**：同时开 Acrylic 与 system backdrop 时 DWM 的行为未
/// 定义（实测表现为 backdrop 赢，Acrylic 被吞）。所以每档都要把另一条显式
/// 写回中性值，不能只写自己那条。
///
/// `SetWindowCompositionAttribute` 未进入公开 SDK，所以和 GPUI 上游一样动态取
/// 函数地址；backdrop 则用公开 DWM API。任一步失败都留日志，避免把 API 失败
/// 再次误判成"设置没有热应用"。
#[cfg(windows)]
fn apply_windows_accent_policy(
    window: &Window,
    blur: BlurModeName,
    appearance: WindowBackgroundAppearance,
) {
    use windows_sys::Win32::Foundation::{BOOL, HWND};
    use windows_sys::Win32::Graphics::Dwm::{
        DWM_BB_ENABLE, DWM_BLURBEHIND, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW,
        DWMWA_SYSTEMBACKDROP_TYPE, DwmEnableBlurBehindWindow, DwmSetWindowAttribute,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    #[derive(Clone, Copy)]
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
    let set_attribute: Option<SetWindowCompositionAttribute> = unsafe {
        let user32 = GetModuleHandleA(c"user32.dll".as_ptr() as *const u8);
        if user32.is_null() {
            None
        } else {
            GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr() as *const u8)
                .map(|procedure| std::mem::transmute(procedure))
        }
    };
    if set_attribute.is_none() {
        log::warn!("SetWindowCompositionAttribute is unavailable in user32.dll");
    }

    let apply_accent = |mut accent: AccentPolicy, phase: &str| {
        let Some(set_attribute) = set_attribute else { return };
        let mut data = WindowCompositionAttributeData {
            attribute: 19, // WCA_ACCENT_POLICY
            data: &mut accent as *mut _ as *mut core::ffi::c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        // SAFETY: hwnd 来自当前存活的 GPUI 窗口，数据在调用期间保持有效。
        if unsafe { set_attribute(hwnd, &mut data) } == 0 {
            log::warn!("SetWindowCompositionAttribute({phase}) failed");
        }
    };

    let disabled_accent = AccentPolicy {
        state: 0,
        // 与 GPUI 非 Acrylic 路径一致，清理旧材质时保留标准边框绘制语义。
        flags: 2,
        gradient_color: 0,
        animation_id: 0,
    };
    let system_material_requested = matches!(
        appearance,
        WindowBackgroundAppearance::MicaBackdrop | WindowBackgroundAppearance::MicaAltBackdrop
    );

    // 必须先移除旧 WCA 层。反过来先写 DWMSBT 时，Aero/Acrylic 的
    // AccentPolicy 会阻止 DWM 接纳新材质，事后再清也不会自动重算 frame。
    if system_material_requested {
        apply_accent(disabled_accent, "clear-before-system-backdrop");
    }

    let blur_behind = DWM_BLURBEHIND {
        dwFlags: DWM_BB_ENABLE,
        fEnable: i32::from(blur == BlurModeName::Aero),
        hRgnBlur: std::ptr::null_mut(),
        fTransitionOnMaximized: 0,
    };
    // 对所有档位都显式 enable/disable，避免从 Aero 热切换后遗留玻璃层。
    let blur_behind_result = unsafe { DwmEnableBlurBehindWindow(hwnd, &blur_behind) };
    let backdrop: i32 = match appearance {
        WindowBackgroundAppearance::MicaBackdrop => DWMSBT_MAINWINDOW,
        WindowBackgroundAppearance::MicaAltBackdrop => DWMSBT_TABBEDWINDOW,
        _ => DWMSBT_NONE,
    };
    // 公开 system-backdrop 属性仅存在于 22621+。旧系统的回退只走 WCA，
    // 不应把预期的 E_INVALIDARG 记录成运行时故障。
    let backdrop_result = (windows_build_number() >= 22_621).then(|| unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            &backdrop as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    });
    let system_material_available =
        system_material_requested && backdrop_result.is_some_and(|result| result >= 0);

    let accent = match blur {
        BlurModeName::Acrylic => AccentPolicy {
            state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
            flags: 0,
            // alpha=0 会让部分 DWM 版本直接跳过 Acrylic。
            gradient_color: 0x0100_0000,
            animation_id: 0,
        },
        // Aero 使用 Win32 公开接口组合：实时 BlurBehind + 约 60% 深色玻璃色调。
        BlurModeName::Aero => AccentPolicy {
            state: 3, // ACCENT_ENABLE_BLURBEHIND
            flags: 0,
            gradient_color: 0x982B_2B2B,
            animation_id: 0,
        },
        // 1809..22H2 回退到经典模糊；新系统若原生 backdrop 调用失败，
        // 同样保留 Acrylic 兜底。成功的系统材质不能再叠第二层 AccentPolicy。
        BlurModeName::Mica | BlurModeName::MicaAlt
            if matches!(appearance, WindowBackgroundAppearance::Blurred)
                || (system_material_requested && !system_material_available) =>
        {
            AccentPolicy { state: 4, flags: 0, gradient_color: 0x0100_0000, animation_id: 0 }
        },
        BlurModeName::Mica | BlurModeName::MicaAlt | BlurModeName::None => disabled_accent,
    };

    if !(system_material_requested && system_material_available) {
        apply_accent(accent, "final");
    }

    // 重绘 GPUI 内容不足以让 DWM 重新读取 DWMSBT。材质切换是低频操作，
    // 在 AppliedBlur 门控后刷新一次非客户区 frame，不影响透明度滑块性能。
    if backdrop_result.is_some() {
        let frame_result = unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        };
        if frame_result == 0 {
            log::warn!("failed to refresh the window frame after changing system backdrop");
        }
    }

    if let Some(backdrop_result) = backdrop_result.filter(|result| *result < 0) {
        if matches!(blur, BlurModeName::Mica | BlurModeName::MicaAlt) {
            log::warn!(
                "system {blur:?} is unavailable (HRESULT=0x{:08X}); falling back to Acrylic",
                backdrop_result as u32
            );
        } else {
            log::warn!(
                "DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE={backdrop}) failed: HRESULT=0x{:08X}",
                backdrop_result as u32
            );
        }
    }
    if blur_behind_result < 0 {
        log::warn!(
            "DwmEnableBlurBehindWindow(enable={}) failed: HRESULT=0x{:08X}",
            blur == BlurModeName::Aero,
            blur_behind_result as u32
        );
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

// ---- 以下一组只服务已停用的 [`paint_glass_overlay`]（见其文档）。按用户要求
// ---- 保留实现，因此统一标 `dead_code`，不要因为"没人用"就删掉。

/// 噪点 tile 边长（物理像素）。越大平铺次数越少、内存越高：512 时 3K 屏约
/// 28 次 `paint_image`，1MB 纹理——两头都便宜。
#[allow(dead_code)]
const NOISE_TILE_PX: u32 = 512;
/// 噪点强度。Acrylic 自带的颗粒非常细微（目测 3~5%），高于 ~8% 会从"玻璃"
/// 变成"脏"。
#[allow(dead_code)]
const NOISE_ALPHA: u8 = 14;
/// 白色 tint 浓度。暗色主题要更厚——Mica 的壁纸色调在暗色下偏沉，正是它
/// "不够透亮"的主因；亮色主题本就够亮，加太多会过曝。
#[allow(dead_code)]
const TINT_ALPHA_DARK: f32 = 0.08;
#[allow(dead_code)]
const TINT_ALPHA_LIGHT: f32 = 0.05;

thread_local! {
    /// 噪点 tile 只依赖上面几个常量，进程内生成一次即可。用 `thread_local`
    /// 而不是 `OnceLock`：`RenderImage` 只在渲染线程用，不必为跨线程共享去
    /// 背 `Send + Sync` 的约束。
    #[allow(dead_code)]
    static NOISE_TILE: std::cell::RefCell<Option<Arc<RenderImage>>> =
        const { std::cell::RefCell::new(None) };
}

/// 生成（并缓存）噪点 tile。
#[allow(dead_code)]
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

/// 【已停用，保留作参考】Mica 档的玻璃增强层：白 tint + 噪点颗粒。
///
/// # 为什么停用
///
/// 这层的前提是"系统 Mica 已经提供了壁纸模糊，我们只补噪点与 tint"。2026-08-22
/// 实测证明前提不成立——系统 Mica 在本壳上从未生效，底下是一块纯色兜底，于是
/// 这层只是在纯色上再刷一层雾，观感上离 Mica 更远（用户判定"明显不是 Mica"）。
///
/// Mica 现在由 DWM 的系统 backdrop 完整合成，噪点与 tint 不应在客户区重复叠加，
/// 所以这层不再有调用点。代码按用户要求保留，供其他材质实验复用。
///
/// 调玻璃感只动本文件顶部那四个常量，不要改绘制顺序：tint 必须在噪点之下，
/// 否则颗粒会被 tint 冲淡到看不见。
#[allow(dead_code)]
pub fn paint_glass_overlay(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    let Some(effects) = cx.try_global::<VisualEffects>() else { return };
    if !matches!(effects.blur, BlurModeName::Mica | BlurModeName::MicaAlt) {
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
        let _ = window.paint_image(bounds, bounds, Corners::all(px(0.0)), image, 0, false);
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
