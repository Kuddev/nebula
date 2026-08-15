use gpui::{App, Hsla, Pixels, Rgba as GpuiRgba, hsla, px};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode};

use crate::display::color::Rgb;
use crate::display::ui::theme::NebulaTheme;
use crate::renderer::ui::Rgba;
use nebula_settings::ThemeName;

/// settings 的 [`ThemeName`] → 旧壳 chrome 主题。变体一一同名；chrome
/// 色表的权威在 `display::ui::theme`，GPUI 壳与旧壳取同一份数据。
fn chrome_theme(name: ThemeName) -> NebulaTheme {
    match name {
        ThemeName::Nebula => NebulaTheme::Nebula,
        ThemeName::SilverLight => NebulaTheme::SilverLight,
        ThemeName::SteelDark => NebulaTheme::SteelDark,
        ThemeName::LimestoneLight => NebulaTheme::LimestoneLight,
        ThemeName::CoalDark => NebulaTheme::CoalDark,
        ThemeName::LinenLight => NebulaTheme::LinenLight,
        ThemeName::MossDark => NebulaTheme::MossDark,
    }
}

/// [`chrome_theme`] 的逆映射。两张表相邻放置：加主题时一起改。
fn settings_theme_name(theme: NebulaTheme) -> ThemeName {
    match theme {
        NebulaTheme::Nebula => ThemeName::Nebula,
        NebulaTheme::SilverLight => ThemeName::SilverLight,
        NebulaTheme::SteelDark => ThemeName::SteelDark,
        NebulaTheme::LimestoneLight => ThemeName::LimestoneLight,
        NebulaTheme::CoalDark => ThemeName::CoalDark,
        NebulaTheme::LinenLight => ThemeName::LinenLight,
        NebulaTheme::MossDark => ThemeName::MossDark,
    }
}

/// 生效主题：`follow_system_theme` 开启时按系统外观折算到用户主题家族的
/// 亮/暗成员（规则与旧壳 `NebulaTheme::for_system_appearance` 同一来源）。
/// chrome 令牌与终端 palette 都必须走这里，两层才不会分家。
pub fn effective_theme_name(cx: &App) -> ThemeName {
    let rt = nebula_settings::RuntimeSettings::load();
    if !rt.follow_system_theme {
        return rt.theme;
    }
    let is_light = matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
    );
    settings_theme_name(chrome_theme(rt.theme).for_system_appearance(is_light))
}

fn to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    GpuiRgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }
        .into()
}

/// 不透明 ink（旧壳 `Rgb` 令牌）。
fn ink(c: Rgb) -> Hsla {
    to_hsla(c.r, c.g, c.b)
}

/// 保留 alpha 的水洗层（hover/surface/hairline 这类叠加色）。
fn wash(c: Rgba) -> Hsla {
    GpuiRgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: f32::from(c.a) / 255.0,
    }
    .into()
}

/// 当不透明用的 `Rgba` 令牌（panel/danger/toggle 一族 alpha 本就 255）。
fn solid(c: Rgba) -> Hsla {
    to_hsla(c.r, c.g, c.b)
}

fn luma(r: u8, g: u8, b: u8) -> f32 {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
}

/// hover/active 派生：深色往白提、浅色往黑压，幅度 `k`。旧壳没有为
/// 实色按钮单列 hover 令牌（quad 直接换色），这里按明度方向补出两档。
fn shift3(r: u8, g: u8, b: u8, k: f32) -> Hsla {
    let target = if luma(r, g, b) < 140.0 { 255.0 } else { 0.0 };
    let mix = |v: u8| (f32::from(v) + (target - f32::from(v)) * k).round().clamp(0.0, 255.0) as u8;
    to_hsla(mix(r), mix(g), mix(b))
}

/// 压在语义色块上的文字：深块配近白、浅块配近黑（slate 两端）。
fn on_solid(c: Rgba) -> Hsla {
    if luma(c.r, c.g, c.b) < 150.0 { to_hsla(248, 250, 252) } else { to_hsla(15, 23, 42) }
}

/// 同色加浓（滚动条拖拽这类只调 alpha 的反馈；旧壳的 thumb 也是
/// "alpha applied at the call site"）。
fn wash_scaled(c: Rgba, f: f32) -> Hsla {
    GpuiRgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: (f32::from(c.a) / 255.0 * f).min(1.0),
    }
    .into()
}

/// 旧壳 `shell_frame_color` 的不透明形态：panel 按自身 alpha 预合成到
/// shell_bg 上。整窗清到这个色；终端以 term_bg 圆角卡浮于其上，顶栏与
/// 侧栏融进壳色（一体化外壳）。GPUI 壳暂不接透明度滑块，alpha 取 1。
fn shell_color(theme: NebulaTheme) -> Hsla {
    let p = theme.palette();
    let pa = f32::from(p.panel.a) / 255.0;
    let comp = |pv: u8, bv: u8| (f32::from(pv) * pa + f32::from(bv) * (1.0 - pa)).round() as u8;
    to_hsla(
        comp(p.panel.r, p.shell_bg.r),
        comp(p.panel.g, p.shell_bg.g),
        comp(p.panel.b, p.shell_bg.b),
    )
}

/// 终端卡圆角。权威值在旧壳：`draw_window_backdrop` 画卡用的是
/// UI_SHELL_RADIUS_LOGICAL(=14)，不是控件那档 8——两壳必须同径。
pub fn card_radius() -> Pixels {
    px(crate::display::UI_SHELL_RADIUS_LOGICAL)
}

/// 卡内容层底色 = 当前生效的终端背景。终端 tab 之外的卡内容（设置页）
/// 也用它，让每个 tab 都读作同一张浮在壳上的圆角卡。带窗口透明度
/// （与终端元素的默认背景同 alpha，透明窗口下整卡一致）。
pub fn card_content_bg(cx: &App) -> Hsla {
    let mut bg: Hsla = cx
        .try_global::<crate::gpui_shell::config::Settings>()
        .map(|s| s.palette.background.into())
        .unwrap_or_else(|| cx.theme().background);
    bg.a *= crate::gpui_shell::wallpaper::window_opacity(cx);
    bg
}

/// 旧壳设置页使用不透明 `Skin.panel`，不让终端壁纸穿透设置内容。
pub fn settings_panel_bg(cx: &App) -> Hsla {
    solid(chrome_theme(effective_theme_name(cx)).skin().panel)
}

/// 终端内补全浮层（ghost/弹窗）的配色切片，与旧壳 `draw_completion_popup`
/// 同源同义：ghost/tag 用最淡墨，行底=panel 预合成到终端底色（浮层底基准
/// 从卡底提亮，不透出壁纸），选中行=accent + 其上墨色。
pub(crate) struct CompletionColors {
    pub ghost: Hsla,
    pub row_bg: Hsla,
    pub row_fg: Hsla,
    pub tag_fg: Hsla,
    pub selected_bg: Hsla,
    pub selected_fg: Hsla,
}

pub(crate) fn completion_colors(cx: &App, term_bg: GpuiRgba) -> CompletionColors {
    let sk = chrome_theme(effective_theme_name(cx)).skin();
    let a = f32::from(sk.panel.a) / 255.0;
    let mix = |p: u8, b: f32| (f32::from(p) / 255.0) * a + b * (1.0 - a);
    let row_bg: Hsla = GpuiRgba {
        r: mix(sk.panel.r, term_bg.r),
        g: mix(sk.panel.g, term_bg.g),
        b: mix(sk.panel.b, term_bg.b),
        a: 1.0,
    }
    .into();
    CompletionColors {
        ghost: ink(sk.ink_faint),
        row_bg,
        row_fg: ink(sk.ink),
        tag_fg: ink(sk.ink_faint),
        selected_bg: ink(sk.accent),
        selected_fg: ink(sk.ink_on_accent),
    }
}

/// 设置页悬停/选中水洗与旧壳 `settings_skin` 同源：使用当前主题 accent，
/// 深浅主题分别控制透明度。通用 chrome hover 仍保持中性，两种语义不混用。
pub fn settings_hover_bg(cx: &App, strong: bool) -> Hsla {
    let sk = chrome_theme(effective_theme_name(cx)).skin();
    let (hover_alpha, strong_alpha) = if sk.is_light { (22, 34) } else { (30, 46) };
    let alpha = if strong { strong_alpha } else { hover_alpha };
    wash(Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, alpha))
}

/// 按运行时主题重建窗口 chrome：先切组件库深浅模式垫底（未映射的长尾
/// token 落在正确的底色系上），再用旧壳 [`Skin`] 覆写全部关键 token——
/// 七个主题（含浅色）共用同一条派生路径。启动、设置变更、系统外观变化
/// 都走这里；主题名先经 [`effective_theme_name`] 折算 follow_system。
pub fn apply_chrome_theme(cx: &mut App) {
    // 视效（模糊/透明度/壁纸）与主题同一时机刷新：设置热应用、系统外观
    // 变化都会走到这里，窗口级效果与 token 保持同帧一致。
    crate::gpui_shell::wallpaper::refresh(cx);

    let chrome = chrome_theme(effective_theme_name(cx));
    let mode = if chrome.skin().is_light { ThemeMode::Light } else { ThemeMode::Dark };
    Theme::change(mode, None, cx);
    apply_skin_tokens(chrome, cx);

    // 一体化外壳（对齐旧壳 draw_chrome）：窗口背景、侧栏、顶栏是同一块
    // 壳色，各自的分隔线取同色隐形；唯一的结构分界是内容区那张圆角卡。
    // 壳色带用户透明度（文字 token 不带——对比度不塌，旧壳裁定）。
    let opacity = crate::gpui_shell::wallpaper::window_opacity(cx);
    let mut shell = shell_color(chrome);
    shell.a *= opacity;
    let theme = Theme::global_mut(cx);
    theme.background = shell;
    theme.sidebar = shell;
    theme.sidebar_border = shell;
    theme.title_bar = shell;
    theme.title_bar_border = shell;
}

/// 旧壳 [`Skin`] → gpui-component 全局 token。改全局而不是逐组件覆样式：
/// 后续直接引入的组件自然进入 Nebula 视觉系统，上游升级时差异集中在一处。
///
/// 语义对照：
/// - ink 三档 → foreground / muted_foreground / `*_active_foreground`
/// - hover / hover_strong → 普通行悬停与选中水洗；侧栏 Tab 选中态单独使用
///   accent_soft，与旧壳 `draw_chrome` 一致
/// - surface / card / panel → secondary、group_box、popover 三层浮面
/// - accent → primary/ring/caret/link；浅色主题下 Skin 已把它折成中性
///   深灰（2026-07-31 裁定），无需此处分支
/// - danger/ok/warn → 语义三色，不随主题 accent 变
fn apply_skin_tokens(chrome: NebulaTheme, cx: &mut App) {
    let sk = chrome.skin();
    let transparent = hsla(0.0, 0.0, 0.0, 0.0);

    let theme = Theme::global_mut(cx);

    // 文字。
    theme.foreground = ink(sk.ink);
    theme.muted_foreground = ink(sk.ink_dim);

    // 面与线。
    theme.border = wash(sk.hairline);
    theme.input = wash(sk.hairline);
    theme.muted = wash(sk.surface);
    theme.group_box = wash(sk.card);
    theme.group_box_foreground = ink(sk.ink);
    theme.popover = solid(sk.panel);
    theme.popover_foreground = ink(sk.ink);
    theme.overlay = wash(sk.veil);

    // 悬停 / 选中水洗。
    theme.accent = wash(sk.hover);
    theme.accent_foreground = ink(sk.ink_strong);
    theme.accordion_hover = wash(sk.hover);
    theme.list_hover = wash(sk.hover);
    theme.list_active = wash(sk.hover_strong);
    theme.list_active_border = transparent;
    theme.sidebar_foreground = ink(sk.ink);
    theme.sidebar_accent = wash(sk.accent_soft);
    theme.sidebar_accent_foreground = ink(sk.ink_strong);
    theme.tab_foreground = ink(sk.ink_dim);
    theme.tab_active = wash(sk.accent_soft);
    theme.tab_active_foreground = ink(sk.ink_strong);

    // 按钮。
    theme.primary = ink(sk.accent);
    theme.primary_hover = shift3(sk.accent.r, sk.accent.g, sk.accent.b, 0.10);
    theme.primary_active = shift3(sk.accent.r, sk.accent.g, sk.accent.b, 0.18);
    theme.primary_foreground = ink(sk.ink_on_accent);
    theme.secondary = wash(sk.surface);
    theme.secondary_hover = wash(sk.hover);
    theme.secondary_active = wash(sk.hover_strong);
    theme.secondary_foreground = ink(sk.ink);

    // 语义三色。
    theme.danger = solid(sk.danger);
    theme.danger_hover = shift3(sk.danger.r, sk.danger.g, sk.danger.b, 0.10);
    theme.danger_active = shift3(sk.danger.r, sk.danger.g, sk.danger.b, 0.18);
    theme.danger_foreground = on_solid(sk.danger);
    theme.success = solid(sk.ok);
    theme.success_hover = shift3(sk.ok.r, sk.ok.g, sk.ok.b, 0.10);
    theme.success_active = shift3(sk.ok.r, sk.ok.g, sk.ok.b, 0.18);
    theme.success_foreground = on_solid(sk.ok);
    theme.warning = solid(sk.warn);
    theme.warning_hover = shift3(sk.warn.r, sk.warn.g, sk.warn.b, 0.10);
    theme.warning_active = shift3(sk.warn.r, sk.warn.g, sk.warn.b, 0.18);
    theme.warning_foreground = on_solid(sk.warn);

    // 焦点 / 选择 / 链接 / 拖拽。
    theme.ring = ink(sk.accent);
    theme.caret = ink(sk.accent);
    theme.selection = wash(sk.accent_soft);
    theme.link = ink(sk.accent);
    theme.link_hover = shift3(sk.accent.r, sk.accent.g, sk.accent.b, 0.10);
    theme.link_active = shift3(sk.accent.r, sk.accent.g, sk.accent.b, 0.18);
    theme.drag_border = ink(sk.accent);
    theme.drop_target = wash(sk.accent_soft);

    // 开关 / 滑条 / 滚动条。Switch 开态吃 primary 是上游硬编码；旧壳裁定
    // 的开态专色（#1e222b 一族）要在 fork 加 token 才能接上，记为后续。
    theme.switch = solid(sk.toggle_track_off);
    theme.switch_thumb = solid(sk.knob_on);
    theme.slider_bar = ink(sk.accent);
    theme.slider_thumb = solid(sk.knob_on);
    theme.scrollbar = transparent;
    theme.scrollbar_thumb = wash(sk.track_off);
    theme.scrollbar_thumb_hover = wash_scaled(sk.track_off, 1.6);

    // 字号与圆角：控件 pill 档 = 旧壳 UI_CORNER_RADIUS_LOGICAL(8)；浮层
    // 12，低于终端卡的 14——三档呼应旧壳的圆角层级。
    theme.font_size = px(14.0);
    theme.mono_font_size = px(13.0);
    theme.radius = px(crate::display::UI_CORNER_RADIUS_LOGICAL);
    theme.radius_lg = px(12.0);
}
