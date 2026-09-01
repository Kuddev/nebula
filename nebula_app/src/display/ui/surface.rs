//! 浮层配方：面板、卡片、描边、遮罩。本层的核心。
//!
//! # 为什么需要它
//!
//! 2026-07-29 侦察：`(x-1, y-1, w+2, h+2)` + hairline + 填充这套描边配方在
//! `display/` 下手写了 **95 处**，八个浮层用了**六种**圆角。结果是任何一条视觉
//! 规则（明度阶梯、阴影、遮罩、圆角）都无法一次改全——这正是"配色改不干净"的
//! 结构性原因。
//!
//! 这里的函数是那些配方的唯一来源。新的浮层不应该再手写 quad 序列。
//!
//! # 遮罩的二分（重要）
//!
//! [`Elevation`] 把「是否画遮罩」编码进类型，而不是留给调用方决定。理由是遮罩
//! 传达的是一份**模态承诺**——「我打断了你，请先处理我」。随手开关的浮层给出这
//! 份承诺，会凭空抬高用户的心理成本，还会遮住它自己经常需要被参考的上下文。
//!
//! 参照产品的命令面板给过像素证据：面板打开时背景终端文字和侧栏亮度完全
//! 未变，浮起靠的是「明度对比 + 大扩散阴影」，不是压暗背景。
//! 判据见 [`Elevation`] 各变体的文档：可 Esc、无后果、随手开关的是
//! popover，要求决策、有后果的是 modal。

use super::theme::Skin;
use super::tokens::{Density, control, elevation, radius};
use crate::renderer::ui::{Rgba, UiQuad};

pub use super::color_math::{fade, over};

/// `(x, y, width, height)`，逻辑像素。
pub type Rect = (f32, f32, f32, f32);

/// 浮层的高度层级。决定阴影扩散、圆角，以及**是否画遮罩**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// 小浮层：右键菜单、下拉 popup、tooltip。不画遮罩。
    Menu,
    /// 大浮层：命令面板、shell 选择器。可 Esc、无后果、随手开关——
    /// **不画遮罩**，靠明度对比与大扩散阴影浮起。
    Popover,
    /// 模态：确认框、设置页、SSH 编辑器。要求决策、有后果，
    /// 画遮罩（冷调压暗，不是白雾）。
    Modal,
}

impl Elevation {
    /// `(blur, offset_y)`，逻辑像素。阴影扩散必须与被托举的面积成比例：
    /// 一组适合小菜单的参数放在整块命令面板下面就托不住。
    #[inline]
    fn shadow(self) -> (f32, f32) {
        match self {
            Self::Menu => (elevation::MENU_BLUR, elevation::MENU_OFFSET_Y),
            Self::Popover => (elevation::POPOVER_BLUR, elevation::POPOVER_OFFSET_Y),
            Self::Modal => (elevation::MODAL_BLUR, elevation::MODAL_OFFSET_Y),
        }
    }

    /// 只有模态阻断交互，所以只有模态画遮罩。
    #[inline]
    pub fn dims_background(self) -> bool {
        matches!(self, Self::Modal)
    }

    /// 面板底是否必须完全不透明。
    ///
    /// 菜单和下拉紧贴着它们遮住的那一行内容，`Skin::panel` 那 4% 的透明度
    /// 会让底下的文字隐隐透上来——在选项文字背后叠一层错位的鬼影，正是
    /// 界面读起来"脏"的那种脏。命令面板和模态离内容远、且四周有阴影过渡，
    /// 留一点透视反而自然。
    #[inline]
    fn needs_opaque_fill(self) -> bool {
        matches!(self, Self::Menu)
    }
}

/// 浮层内容块的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    Default,
    Hover,
    /// 当前选中项。强调色预算一屏只花一次，就花在这里——
    /// 不要同时给推荐卡、导航 pill 也染色。
    Selected,
}

/// 发丝描边环。替代散落各处的手写 `(x-1, y-1, w+2, h+2)`。
///
/// 外圆角自动取 `radius + hairline`，保证内外弧**同心**——手写处常常内外用同一
/// 个半径，那样描边在圆角处会变粗，是"边缘发毛"的来源之一。
///
/// `radius` 传入的是**内**圆角（已乘过 scale）。
pub fn push_stroke(quads: &mut Vec<UiQuad>, rect: Rect, radius: f32, scale: f32, color: Rgba) {
    let hairline = (control::HAIRLINE * scale).max(1.0);
    let (x, y, w, h) = rect;
    quads.push(UiQuad::solid(
        x - hairline,
        y - hairline,
        w + hairline * 2.0,
        h + hairline * 2.0,
        radius + hairline,
        color,
    ));
}

/// 浮层的底：遮罩（仅 [`Elevation::Modal`]）→ 外阴影 → 发丝环 → 面板填充。
///
/// `viewport` 是整窗尺寸 `(width, height)`，只有模态用得上。
/// `progress` 是入场动画进度，所有颜色按它衰减。
#[allow(clippy::too_many_arguments)]
pub fn push_surface(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    viewport: (f32, f32),
    scale: f32,
    sk: &Skin,
    density: Density,
    level: Elevation,
    progress: f32,
) {
    push_surface_with_radius(
        quads,
        rect,
        (0.0, 0.0, viewport.0, viewport.1),
        0.0,
        scale,
        sk,
        level,
        progress,
        radius::overlay(density),
    );
}

/// Same elevated surface recipe as [`push_surface`], with a caller-selected
/// logical corner radius for references whose outer silhouette is intentionally
/// softer than the shared Fluent overlay token.
#[allow(clippy::too_many_arguments)]
pub fn push_surface_with_radius(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    veil_rect: Rect,
    veil_radius: f32,
    scale: f32,
    sk: &Skin,
    level: Elevation,
    progress: f32,
    radius_logical: f32,
) {
    push_surface_in_with_radius(
        quads,
        rect,
        veil_rect,
        veil_radius,
        scale,
        sk,
        level,
        progress,
        radius_logical * scale,
    );
}

/// 同 [`push_surface`]，但遮罩只铺满 `veil_rect` 而不是整窗，并带 `veil_radius`
/// 的圆角。
///
/// 用于那些语义上属于某块区域的模态：SSH 主机编辑器是"对终端做的事"，遮罩
/// 盖住终端卡就够了。连侧栏和标题栏一起罩黑会把它读成"整个应用被阻断"，而
/// 那时侧栏其实仍然是可见的上下文。
///
/// 圆角是必须的：终端是一张圆角卡，直角遮罩会在四角各露出一块，读起来像糊
/// 歪了一层贴纸。
#[allow(clippy::too_many_arguments)]
pub fn push_surface_in(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    veil_rect: Rect,
    veil_radius: f32,
    scale: f32,
    sk: &Skin,
    density: Density,
    level: Elevation,
    progress: f32,
) {
    push_surface_in_with_radius(
        quads,
        rect,
        veil_rect,
        veil_radius,
        scale,
        sk,
        level,
        progress,
        // 密度作为显式参数进来，而不是塞进 `Skin`——主题与密度是正交轴，皮肤
        // 的每个字段都是颜色。层契约「输入是矩形和 Skin，不读配置」仍然成立。
        radius::overlay(density) * scale,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_surface_in_with_radius(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    veil_rect: Rect,
    veil_radius: f32,
    scale: f32,
    sk: &Skin,
    level: Elevation,
    progress: f32,
    corner: f32,
) {
    let (x, y, w, h) = rect;

    if level.dims_background() {
        quads.push(UiQuad::solid(
            veil_rect.0,
            veil_rect.1,
            veil_rect.2,
            veil_rect.3,
            veil_radius,
            fade(sk.veil, progress),
        ));
    }

    // 外阴影承担 Z 轴层级；发丝环只负责在浅色背景上定义边界。两者分工不同，
    // 缺了阴影就只剩一条线，浮层会"贴"在背景上而不是浮起来。
    let (blur, offset_y) = level.shadow();
    let shadow_alpha =
        if sk.is_light { elevation::SHADOW_ALPHA_LIGHT } else { elevation::SHADOW_ALPHA_DARK };
    quads.push(UiQuad::shadow(
        x,
        y,
        w,
        h,
        corner,
        blur * scale,
        offset_y * scale,
        fade(Rgba::new(0, 0, 0, shadow_alpha), progress),
    ));

    push_stroke(quads, rect, corner, scale, fade(sk.hairline, progress));
    let fill = if level.needs_opaque_fill() {
        Rgba::new(sk.panel.r, sk.panel.g, sk.panel.b, 255)
    } else {
        sk.panel
    };
    quads.push(UiQuad::solid(x, y, w, h, corner, fade(fill, progress)));
}

/// 浮层内部的内容块。
pub fn push_card(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    density: Density,
    state: CardState,
) {
    let (x, y, w, h) = rect;
    let corner = radius::overlay(density) * scale;
    let fill = match state {
        CardState::Default => sk.card,
        CardState::Hover => sk.hover,
        CardState::Selected => sk.accent_soft,
    };
    quads.push(UiQuad::solid(x, y, w, h, corner, fill));
}

/// 带发丝描边的分组卡片：把同组字段收成一块可扫读的区域。
///
/// **不要写成 `push_stroke` + `push_card`。** [`push_stroke`] 画的是一个比
/// 目标大 1px 的**实心**圆角矩形，靠上层填充盖住中心才形成"环"。`panel` 和
/// `input` 都不透明，盖得住；而 `card` 只有 4.5% 不透明度，盖不住——描边色
/// 有 95% 透过整张卡片，把它压深整整一档。
///
/// 2026-07-31 量到的实迹（Nebula 深色）：panel (29,31,40) 经描边泄漏变成
/// (50,52,62)，再叠 card 得 (62,65,74)，而原型是 (56,61,73)。所以这里把
/// `card` 与 `panel` 预先合成成一个**不透明**色，一个 quad 画完，描边环也
/// 就只剩它该有的那 1px。
pub fn push_group(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    density: Density,
    progress: f32,
) {
    let (x, y, w, h) = rect;
    let corner = radius::overlay(density) * scale;
    push_stroke(quads, rect, corner, scale, fade(sk.hairline, progress));
    quads.push(UiQuad::solid(x, y, w, h, corner, fade(over(sk.card, sk.panel), progress)));
}

/// 下沉表面（文本输入框）。聚焦描边先与面板预合成为单一不透明颜色：这样
/// 不会因 hairline、面板和输入底三层 alpha 在圆角边缘叠出一圈杂色。
pub fn push_input(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    density: Density,
    focused: bool,
) {
    let (x, y, w, h) = rect;
    let corner = radius::control(density) * scale;
    let stroke = if focused {
        over(Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 118), sk.panel)
    } else {
        over(sk.hairline, sk.panel)
    };
    push_stroke(quads, rect, corner, scale, stroke);
    quads.push(UiQuad::solid(x, y, w, h, corner, sk.input));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::ui::theme::NebulaTheme;

    fn light() -> Skin {
        NebulaTheme::SilverLight.skin()
    }

    #[test]
    fn a_group_card_seals_the_stroke_ring_with_an_opaque_fill() {
        // 这条锁的是 2026-07-31 那个 bug：`push_stroke` 画的是实心矩形，
        // 靠上层填充盖住中心才成环。card 只有 4.5% 不透明度，直接叠上去
        // 会让描边色渗满整张卡片，把它压深一整档。所以内芯必须不透明。
        for skin in [light(), NebulaTheme::Nebula.skin()] {
            let mut quads = Vec::new();
            push_group(&mut quads, (40.0, 40.0, 300.0, 120.0), 1.0, &skin, Density::Standard, 1.0);

            assert_eq!(quads.len(), 2, "只该有描边环和内芯两个 quad");
            assert_eq!(quads[1].color0.a, 255, "内芯必须不透明，否则描边渗出来");
        }
    }

    #[test]
    fn a_group_card_reads_one_step_above_its_panel() {
        // card 是"比底板亮一档"的叠加色。合成之后这个方向必须还在——
        // 深色主题叠白、浅色主题叠 slate，两边的符号是相反的。
        let dark = NebulaTheme::Nebula.skin();
        let fill = over(dark.card, dark.panel);
        assert!(fill.r > dark.panel.r, "深色下卡片要比面板亮");

        let sun = light();
        let fill = over(sun.card, sun.panel);
        assert!(fill.r < sun.panel.r, "浅色下卡片要比面板暗");
    }

    #[test]
    fn stroke_ring_stays_concentric_with_its_fill() {
        // 内外弧同心的判据：外半径 - 内半径 == 描边宽度。不满足时描边在圆角
        // 处会变粗，边缘发毛。
        let mut quads = Vec::new();
        let rect = (100.0, 50.0, 200.0, 80.0);
        push_stroke(&mut quads, rect, 8.0, 1.0, Rgba::new(0, 0, 0, 40));

        let ring = &quads[0];
        assert_eq!(ring.radius - 8.0, 1.0, "外圆角必须等于内圆角 + 描边宽");
        assert_eq!((ring.x, ring.y), (99.0, 49.0));
        assert_eq!((ring.width, ring.height), (202.0, 82.0));
    }

    #[test]
    fn stroke_hairline_never_falls_below_one_physical_pixel() {
        // 缩放小于 1 时 hairline 会算出亚像素宽度，在整数像素栅格上会消失。
        let mut quads = Vec::new();
        push_stroke(&mut quads, (0.0, 0.0, 10.0, 10.0), 4.0, 0.5, Rgba::new(0, 0, 0, 40));
        assert_eq!(quads[0].width, 12.0, "描边宽应被钳到 1px，而不是 0.5px");
    }

    #[test]
    fn focused_input_uses_one_precomposed_soft_stroke_color() {
        let skin = NebulaTheme::Nebula.skin();
        let mut quads = Vec::new();
        push_input(&mut quads, (20.0, 20.0, 240.0, 34.0), 1.0, &skin, Density::Standard, true);

        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0].color0.a, skin.panel.a, "焦点线必须先合成为单色");
        assert_ne!(quads[0].color0, Rgba::opaque(skin.accent), "焦点线不应是刺眼的纯强调色");
    }

    #[test]
    fn only_modal_dims_the_background() {
        // 裁定三的回归防线：popover 加遮罩会凭空给出「我打断了你」的承诺，
        // 并遮住它自己经常需要被参考的上下文。
        assert!(!Elevation::Menu.dims_background());
        assert!(!Elevation::Popover.dims_background());
        assert!(Elevation::Modal.dims_background());

        let sk = light();
        for level in [Elevation::Menu, Elevation::Popover] {
            let mut quads = Vec::new();
            push_surface(
                &mut quads,
                (10.0, 10.0, 100.0, 60.0),
                (800.0, 600.0),
                1.0,
                &sk,
                Density::Standard,
                level,
                1.0,
            );
            assert!(
                !quads.iter().any(|q| q.width >= 800.0 && q.height >= 600.0),
                "{level:?} 不该产生任何整窗遮罩 quad",
            );
            assert_eq!(quads.len(), 3, "{level:?} 应只有阴影 + 发丝环 + 填充");
        }

        let mut quads = Vec::new();
        push_surface(
            &mut quads,
            (10.0, 10.0, 100.0, 60.0),
            (800.0, 600.0),
            1.0,
            &sk,
            Density::Standard,
            Elevation::Modal,
            1.0,
        );
        let veil = &quads[0];
        assert_eq!((veil.x, veil.y, veil.width, veil.height), (0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn compact_surfaces_reuse_the_control_radius_instead_of_a_new_value() {
        // 紧凑不是「另一套圆角」，而是同一阶梯降一档。这里从配方产出的
        // quad 上验证：紧凑浮层的圆角恰好等于标准档控件圆角。
        let sk = light();
        let corner_of = |density| {
            let mut quads = Vec::new();
            push_surface(
                &mut quads,
                (10.0, 10.0, 100.0, 60.0),
                (800.0, 600.0),
                1.0,
                &sk,
                density,
                Elevation::Menu,
                1.0,
            );
            quads.last().expect("面板填充是最后一个 quad").radius
        };
        assert_eq!(corner_of(Density::Standard), radius::OVERLAY);
        assert_eq!(corner_of(Density::Compact), radius::CONTROL);
    }

    #[test]
    fn shadow_spread_scales_with_surface_size() {
        // 小菜单的阴影参数放在整块面板下面托不住，所以按层级分档。
        let (menu_blur, _) = Elevation::Menu.shadow();
        let (popover_blur, _) = Elevation::Popover.shadow();
        let (modal_blur, _) = Elevation::Modal.shadow();
        assert!(menu_blur < popover_blur && popover_blur < modal_blur);
    }

    #[test]
    fn fade_is_identity_at_full_progress() {
        let c = Rgba::new(10, 20, 30, 200);
        assert_eq!(fade(c, 1.0).a, 200);
        assert_eq!(fade(c, 0.5).a, 100);
        assert_eq!(fade(c, 0.0).a, 0);
    }
}
