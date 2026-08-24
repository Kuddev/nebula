use gpui::{App, Bounds, Hsla, Pixels, Rgba as GpuiRgba, Window, fill, hsla, point, px, size};
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

/// 当前生效的旧壳 chrome 主题（SSH 连接卡片等复用旧 Skin/palette 的
/// 视图从这里取色，避免第二套品牌色定义）。
pub(crate) fn chrome_theme_resolved(cx: &App) -> NebulaTheme {
    chrome_theme(effective_theme_name(cx))
}

/// 当前 OS 外观是否为浅色。跟随系统时与旧壳 `WinitTheme::Light` 同一判定。
pub(crate) fn system_is_light(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
    )
}

/// 把用户点选的主题家族折算成此刻该显示的成员。
/// `follow_system` 关闭时原样返回 preference；开启时走
/// [`NebulaTheme::for_system_appearance`]。
pub(crate) fn resolve_theme_name(
    preference: ThemeName,
    follow_system: bool,
    system_is_light: bool,
) -> ThemeName {
    if !follow_system {
        return preference;
    }
    settings_theme_name(chrome_theme(preference).for_system_appearance(system_is_light))
}

/// 生效主题：`follow_system_theme` 开启时按系统外观折算到用户主题家族的
/// 亮/暗成员（规则与旧壳 `NebulaTheme::for_system_appearance` 同一来源）。
/// chrome 令牌与终端 palette 都必须走这里，两层才不会分家。
pub fn effective_theme_name(cx: &App) -> ThemeName {
    let rt = nebula_settings::RuntimeSettings::load();
    resolve_theme_name(rt.theme, rt.follow_system_theme, system_is_light(cx))
}

/// 点选一张主题卡时要写盘的键：对齐旧壳 `select_nebula_theme`
/// （关掉跟随系统）+ `apply_nebula_theme`（底色换成该主题 `term_bg`）。
pub(crate) fn theme_card_persist_updates(name: ThemeName) -> [(&'static str, String); 3] {
    [
        ("theme", name.prompt_name().to_owned()),
        ("follow_system_theme", "0".to_owned()),
        ("background", nebula_settings::format_hex_rgb(name.term_theme().background)),
    ]
}

/// 当前生效主题的终端底色（给「跟随主题」取色器同步用，不是壳色）。
pub(crate) fn theme_term_background(cx: &App) -> Hsla {
    let c = chrome_theme(effective_theme_name(cx)).palette().term_bg;
    to_hsla(c.r, c.g, c.b)
}

fn to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    GpuiRgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }
        .into()
}

/// 不透明 ink（旧壳 `Rgb` 令牌）。
fn ink(c: Rgb) -> Hsla {
    to_hsla(c.r, c.g, c.b)
}

/// 最淡一档墨色（旧壳 `Skin::ink_faint`）：比 `muted_foreground`(ink_dim)
/// 再暗一档，旧壳只用于「确实在场但不参与层级竞争」的元素——侧栏数量
/// chip 的数字、未绑定键帽等。GPUI 全局 token 没有这一档，按需来取。
pub(crate) fn faint_ink(cx: &App) -> Hsla {
    ink(chrome_theme(effective_theme_name(cx)).skin().ink_faint)
}

/// 侧栏运行 spinner 的两端颜色，逐字复用旧壳 `draw_chrome` 的底色裁定：
/// hairline / ink_dim 先与该 Tab 行的真实不透明底色合成。环由大量相交小圆
/// 铺成；若直接交给 GPUI 用半透明 hairline 叠画，交叠处会变深，看起来像
/// 一圈模糊珠子而不是连续圆环。
pub(crate) fn sidebar_spinner_colors(cx: &App, active: bool) -> (GpuiRgba, GpuiRgba) {
    let chrome = chrome_theme(effective_theme_name(cx));
    let palette = chrome.palette();
    let sk = chrome.skin();
    let shell = Rgba::new(palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b, 255);
    let base =
        if active { crate::display::ui::surface::over(sk.accent_soft, shell) } else { shell };
    let track = crate::display::ui::surface::over(sk.hairline, base);
    let head = crate::display::ui::surface::over(
        Rgba::new(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b, 255),
        base,
    );
    let gpui = |color: Rgba| GpuiRgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: 1.0,
    };
    (gpui(track), gpui(head))
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
    bg.a *= crate::gpui_shell::wallpaper::chrome_surface_opacity(cx);
    bg
}

/// 旧壳的透明清屏模型：只在圆角卡**外部**画一层壳色，卡内部保持透明，
/// 随后由卡自己的 `term_bg × opacity` 覆盖。若直接给 workspace 根节点上底色，
/// 卡区域会先吃一次 shell alpha、再吃一次 card alpha，实际不透明度从 `o`
/// 变成 `1-(1-o)^2`，DWM Acrylic 看起来就会发糊、发实。
///
/// GPUI 没有凹圆角 primitive，因此四角按物理像素扫描圆外区域；卡本身的
/// 抗锯齿圆角仍是唯一可见边，壳色不会侵入卡内形成第二次 alpha 叠加。
pub fn paint_shell_around_card(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    let color = cx.theme().background;
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let inset = 8.0_f32.min(width * 0.5).min(height * 0.5);
    if width <= 0.0 || height <= 0.0 || inset <= 0.0 {
        return;
    }

    let x = f32::from(bounds.origin.x);
    let y = f32::from(bounds.origin.y);
    let card_w = (width - inset * 2.0).max(0.0);
    let card_h = (height - inset * 2.0).max(0.0);
    let paint = |window: &mut Window, x: f32, y: f32, w: f32, h: f32| {
        if w > 0.0 && h > 0.0 {
            window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(w), px(h))), color));
        }
    };

    // 四条带互不重叠，每个像素只承受一次壳色 alpha。
    paint(window, x, y, width, inset);
    paint(window, x, y + height - inset, width, inset);
    paint(window, x, y + inset, inset, card_h);
    paint(window, x + width - inset, y + inset, inset, card_h);

    let radius = crate::display::UI_SHELL_RADIUS_LOGICAL.min(card_w * 0.5).min(card_h * 0.5);
    if radius <= 0.0 {
        return;
    }
    let step = 1.0 / window.scale_factor().max(1.0);
    let rows = (radius / step).ceil() as usize;
    let card_x = x + inset;
    let card_y = y + inset;
    let card_right = card_x + card_w;
    let card_bottom = card_y + card_h;
    for row in 0..rows {
        let row_top = row as f32 * step;
        let row_h = step.min(radius - row_top).max(0.0);
        let sample_y = (row_top + row_h * 0.5).min(radius);
        let dy = radius - sample_y;
        let outside = (radius - (radius * radius - dy * dy).max(0.0).sqrt()).clamp(0.0, radius);
        paint(window, card_x, card_y + row_top, outside, row_h);
        paint(window, card_right - outside, card_y + row_top, outside, row_h);
        paint(window, card_x, card_bottom - row_top - row_h, outside, row_h);
        paint(window, card_right - outside, card_bottom - row_top - row_h, outside, row_h);
    }
}

/// 旧壳设置页使用不透明 `Skin.panel`，不让终端壁纸穿透设置内容。
pub fn settings_panel_bg(cx: &App) -> Hsla {
    solid(chrome_theme(effective_theme_name(cx)).skin().panel)
}

/// 终端内补全浮层（ghost/弹窗）的配色切片，与旧壳 `draw_completion_popup`
/// 同源同义：ghost/tag 用最淡墨，行底=panel 预合成到终端底色（浮层底基准
/// 从卡底提亮，不透出壁纸）。弹窗本身使用低对比边框与 accent 水洗选中态，
/// 避免把整块候选列表染成高饱和按钮。
pub(crate) struct CompletionColors {
    pub ghost: Hsla,
    pub panel_bg: Hsla,
    pub panel_border: Hsla,
    pub panel_shadow: Hsla,
    pub row_bg: Hsla,
    pub row_fg: Hsla,
    pub tag_fg: Hsla,
    pub selected_bg: Hsla,
    pub selected_fg: Hsla,
    pub scroll_track: Hsla,
    pub scroll_thumb: Hsla,
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
    let accent: Hsla = ink(sk.accent);
    let selected_bg = accent.opacity(if sk.is_light { 0.16 } else { 0.24 });
    CompletionColors {
        ghost: ink(sk.ink_faint),
        panel_bg: row_bg,
        panel_border: accent.opacity(if sk.is_light { 0.28 } else { 0.34 }),
        panel_shadow: hsla(0.0, 0.0, 0.0, if sk.is_light { 0.18 } else { 0.42 }),
        row_bg,
        row_fg: ink(sk.ink),
        tag_fg: ink(sk.ink_faint),
        selected_bg,
        selected_fg: ink(sk.ink_strong),
        scroll_track: ink(sk.ink_faint).opacity(if sk.is_light { 0.24 } else { 0.32 }),
        scroll_thumb: ink(sk.ink_dim).opacity(if sk.is_light { 0.62 } else { 0.72 }),
    }
}

/// 设置页悬停/选中水洗与旧壳 `settings_skin` 同源：使用当前主题 accent，
/// 深浅主题分别控制透明度。通用 chrome hover 仍保持中性，两种语义不混用。
/// 设置页"这一项我改过"的标记色。
///
/// 不复用 accent：那是给按钮和选中底用的，饱和度是按"要被点"调的。这里只
/// 需要在一条 2px 的细线上被扫到，高饱和的紫在深底上会发光振动，细元素尤其
/// 明显——那就是"刺眼"的来源。所以**降饱和、保亮度**：可辨来自亮度差，刺眼
/// 来自饱和度，两者可以拆开。
pub fn settings_mark(cx: &App) -> Hsla {
    if chrome_theme(effective_theme_name(cx)).skin().is_light {
        // 浅色底上要更暗才看得见，同样压饱和。
        hsla(250.0 / 360.0, 0.40, 0.47, 1.0)
    } else {
        hsla(250.0 / 360.0, 0.36, 0.71, 1.0)
    }
}

/// 设置页的结构分割线（导航↔内容、页头↔正文）。
///
/// 比行左侧轨道更淡：轨道在说"这几行是一组"，是内容的一部分；这条线只是
/// 在说"这是两个区"，属于容器。两者同色同粗的话，画面上就出现两条同等分量
/// 的线在争同一件事的解释权。
pub fn settings_hairline(cx: &App) -> Hsla {
    let sk = chrome_theme(effective_theme_name(cx)).skin();
    // 浅色底上黑线比深色底上白线更"重"（同 alpha 视觉对比更高），所以浅色
    // 取更低的 alpha。
    let alpha = if sk.is_light { 20 } else { 18 };
    wash(Rgba::new(sk.ink.r, sk.ink.g, sk.ink.b, alpha))
}

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
    // 托盘与 chrome 同一热应用节拍：启动 / 设置变更 / 系统外观。
    crate::gpui_shell::apply_tray_setting();

    let chrome = chrome_theme(effective_theme_name(cx));
    let mode = if chrome.skin().is_light { ThemeMode::Light } else { ThemeMode::Dark };
    Theme::change(mode, None, cx);
    apply_skin_tokens(chrome, cx);
    apply_shell_opacity(chrome, cx);
}

/// 只按当前不透明度重算壳色，不做别的。
///
/// 拖不透明度滑块的快路径：[`apply_chrome_theme`] 那一整套（读设置文件、
/// 重建壁纸纹理、`Theme::change`、四十来个 token 重写、窗口级模糊）对"只改了
/// alpha"这件事是纯浪费，而滑块一次拖拽会发几十上百个事件。2026-08-21 定案：
/// 拖拽走这里，落盘与整套热应用等停手之后。
///
/// 主题名由调用方传入：[`effective_theme_name`] 内部会 `RuntimeSettings::load()`
/// 读盘一次，那正是这条路径要避开的东西。设置页自己持有 `runtime` 镜像。
pub fn reapply_shell_opacity(name: ThemeName, follow_system: bool, cx: &mut App) {
    let chrome = chrome_theme(resolve_theme_name(name, follow_system, system_is_light(cx)));
    apply_shell_opacity(chrome, cx);
}

/// 一体化外壳（对齐旧壳 draw_chrome）：窗口背景、侧栏、顶栏是同一块
/// 壳色，各自的分隔线取同色隐形；唯一的结构分界是内容区那张圆角卡。
/// 壳色带用户透明度（文字 token 不带——对比度不塌，旧壳裁定）。
fn apply_shell_opacity(chrome: NebulaTheme, cx: &mut App) {
    let opacity = crate::gpui_shell::wallpaper::chrome_surface_opacity(cx);
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
/// 面积色柔和化：把通道往自身亮度收，色相与明度都不动，只降饱和。
///
/// `keep` 是保留的饱和比例（1.0 原样）。用在开关轨道、主按钮、滑条填充这类
/// 成片的品牌色上；`link` / `caret` / `ring` 那些发丝级用途继续用原值，它们
/// 需要的是辨识度，而辨识度来自亮度差、不来自饱和度。
fn soften(c: crate::display::color::Rgb, keep: f32) -> crate::display::color::Rgb {
    let luma = 0.299 * f32::from(c.r) + 0.587 * f32::from(c.g) + 0.114 * f32::from(c.b);
    let pull =
        |channel: u8| (luma + (f32::from(channel) - luma) * keep).round().clamp(0.0, 255.0) as u8;
    crate::display::color::Rgb::new(pull(c.r), pull(c.g), pull(c.b))
}

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
    // 面积色降饱和，细元素不降。同一个 accent 用在两种尺度上：开关轨道、主
    // 按钮是成片的色块，link / caret / 焦点环是发丝级的细节。饱和度决定「刺
    // 不刺眼」，亮度差决定「看不看得见」——所以色块压饱和（暗底上 #52a8ff
    // 这种亮蓝铺开会发光，一整页只剩几个块在跳），细元素保持原值换辨识度。
    let soft_accent = soften(sk.accent, 0.62);
    theme.primary = ink(soft_accent);
    theme.primary_hover = shift3(soft_accent.r, soft_accent.g, soft_accent.b, 0.10);
    theme.primary_active = shift3(soft_accent.r, soft_accent.g, soft_accent.b, 0.18);
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
    theme.slider_bar = ink(soft_accent);
    theme.slider_thumb = solid(sk.knob_on);
    theme.scrollbar = transparent;
    theme.scrollbar_thumb = wash(sk.track_off);
    theme.scrollbar_thumb_hover = wash_scaled(sk.track_off, 1.6);

    // 字号与圆角：控件 pill 档 = 旧壳 UI_CORNER_RADIUS_LOGICAL(8)；浮层
    // 12，低于终端卡的 14——三档呼应旧壳的圆角层级。
    theme.font_size = px(14.0);
    theme.mono_font_size = px(13.0);

    // 整壳的兜底字体（fork `root.rs` 用 `theme.font_family` 给根容器）。上游
    // 默认 `.SystemUIFont` 在 Windows 上没落到 UI 字体，中文最终回落进终端等
    // 宽族，于是整页中文字距被拉开、拉丁与数字的节奏更明显。旧壳只有一套
    // glyph cache 时这是架构限制，GPUI 壳没有这个限制，不该继承那个观感。
    //
    // 我们是终端，所以等宽在这里是**语义标记**而不是全局字体：路径、键帽、
    // 命令、数值这类"机器读、要逐字符对齐、要能整段复制"的东西显式走 mono；
    // 标题和说明是给人读的，走 sans。
    #[cfg(target_os = "windows")]
    {
        theme.font_family = "Microsoft YaHei UI".into();
        // UI 中的等宽语义也必须稳定。终端字体由 TerminalView 单独读取；
        // 若把用户字体组写进全局 theme，tab、标题和代码字面量的字宽都会
        // 随终端主字体变化，进而破坏 chrome 的既定间距。
        theme.mono_font_family = crate::font_install::REQUIRED_FONT_FAMILY.into();
    }
    theme.radius = px(crate::display::UI_CORNER_RADIUS_LOGICAL);
    theme.radius_lg = px(12.0);
}

#[cfg(test)]
mod tests {
    use super::{resolve_theme_name, theme_card_persist_updates};
    use nebula_settings::ThemeName;

    #[test]
    fn follow_system_remaps_theme_family_and_manual_mode_keeps_preference() {
        assert_eq!(resolve_theme_name(ThemeName::SilverLight, true, false), ThemeName::SteelDark);
        assert_eq!(resolve_theme_name(ThemeName::Nebula, true, true), ThemeName::SilverLight);
        assert_eq!(
            resolve_theme_name(ThemeName::SilverLight, false, false),
            ThemeName::SilverLight
        );
    }

    #[test]
    fn picking_a_theme_card_disables_follow_system_and_writes_that_theme_background() {
        let updates = theme_card_persist_updates(ThemeName::LinenLight);
        assert_eq!(updates[0], ("theme", "LinenLight".to_owned()));
        assert_eq!(updates[1], ("follow_system_theme", "0".to_owned()));
        assert_eq!(
            updates[2],
            (
                "background",
                nebula_settings::format_hex_rgb(ThemeName::LinenLight.term_theme().background)
            )
        );
    }
}
