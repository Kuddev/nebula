//! Toast 浮层的视觉配方：一次性、自动消失的轻提示。
//!
//! 本模块只管**长什么样**和**在哪儿**——尺寸、语义色、入场位移、文字原点。
//! 生命周期（停留多久、什么时候淡出、怎么堆叠）属于状态，归
//! [`crate::display::toast`]。分家的理由和本层其余组件一致：几何有唯一来源，
//! 绘制与命中就不可能漂移；将来别处要弹提示，拿到的是同一份配方而不是又一
//! 套手写矩形。
//!
//! 位置定在右下角而不是底部居中：底部中线已经被 SSH 撤销条和 AI 建议条占着，
//! 那两条带按钮和键位、必须停在视觉中线上。toast 不接鼠标，让开就好——这样
//! 也省掉了一整套互相避让的逻辑。

use super::surface;
use super::theme::Skin;
use super::tokens::{Density, radius, space};
use crate::display::color::Rgb;
use crate::renderer::ui::{Rgba, UiQuad};

pub(crate) type Rect = (f32, f32, f32, f32);

/// 单条高度的下限（逻辑像素）。实际高度还要保证容得下一行文字。
const MIN_H: f32 = 48.0;
/// 距窗口右下角的留白。
const MARGIN: f32 = 20.0;
/// 单条最大宽度。再宽就不该用 toast 承载了。
const MAX_W: f32 = 560.0;
/// 左侧语义色细条的宽度。
const RAIL_W: f32 = 3.0;
/// 入场时先压低这么多再浮上来；淡出时反向沉下去。
const RISE: f32 = 10.0;

/// 提示的语气。语义色只花在左边那根细条上——强调色与渐变的预算不动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// 中性信息。
    Info,
    /// 已经办成的事：恢复成功、保存成功。
    Success,
    /// 值得知道，但不需要动手。真正要用户处理的仍然走消息栏。
    Warning,
}

impl ToastKind {
    fn rail(self, sk: &Skin) -> Rgba {
        match self {
            Self::Info => Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
            Self::Success => sk.ok,
            Self::Warning => sk.warn,
        }
    }
}

/// 单条的高度。
#[inline]
pub(crate) fn bar_height(cell_h: f32, scale: f32) -> f32 {
    (MIN_H * scale).max(cell_h + space::S * scale)
}

/// 文字能占的最大列数。布局与截断都用它，卡片因此永远不会被文字撑破。
pub(crate) fn text_capacity(viewport_w: f32, cell_w: f32, scale: f32) -> usize {
    let s = |value: f32| value * scale;
    let inner = max_width(viewport_w, scale) - s(space::M) * 2.0 - s(RAIL_W) - s(space::S);
    ((inner / cell_w.max(1.0)).floor() as usize).max(6)
}

fn max_width(viewport_w: f32, scale: f32) -> f32 {
    let s = |value: f32| value * scale;
    s(MAX_W).min(viewport_w - s(MARGIN) * 2.0).max(s(MIN_H) * 2.0)
}

/// 第 `index` 条的矩形（0 = 最下面那条，也就是最新的一条）。
///
/// `progress` 是 0..1 的出入场进度：不到 1 时整条压低 [`RISE`]，读作浮上来 /
/// 沉下去。坐标吸附到整像素——半像素的浮层边会被 GPU 采样糊成两条灰边。
pub(crate) fn toast_rect(
    viewport: (f32, f32),
    index: usize,
    text_cols: usize,
    cell_w: f32,
    cell_h: f32,
    scale: f32,
    progress: f32,
) -> Rect {
    let s = |value: f32| value * scale;
    let (view_w, view_h) = viewport;
    let bar_h = bar_height(cell_h, scale);
    let content = s(space::M) * 2.0 + s(RAIL_W) + s(space::S) + text_cols as f32 * cell_w;
    let bar_w = content.min(max_width(view_w, scale));
    let x = (view_w - s(MARGIN) - bar_w).max(0.0);
    let stack = index as f32 * (bar_h + s(space::XS));
    let y = view_h - s(MARGIN) - bar_h - stack + s(RISE) * (1.0 - progress.clamp(0.0, 1.0));
    (x.round(), y.round(), bar_w, bar_h)
}

/// 文字的左上角原点。与 [`toast_rect`] 同源，文字不会和卡片错位。
pub(crate) fn text_origin(rect: Rect, cell_h: f32, scale: f32) -> (f32, f32) {
    let s = |value: f32| value * scale;
    let (x, y, _, h) = rect;
    ((x + s(space::M) + s(RAIL_W) + s(space::S)).round(), (y + (h - cell_h) * 0.5).round())
}

/// 卡片本体：Menu 层的浮层配方 + 左侧语义色细条。
///
/// `progress` 同时驱动整条的透明度，所以出入场不需要调用方再去调色。
pub(crate) fn push_toast(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    viewport: (f32, f32),
    kind: ToastKind,
    scale: f32,
    sk: &Skin,
    density: Density,
    progress: f32,
) {
    let s = |value: f32| value * scale;
    let (x, y, _, h) = rect;
    surface::push_surface(quads, rect, viewport, scale, sk, density, surface::Elevation::Menu, progress);

    let rail_w = s(RAIL_W);
    let rail_h = (h - s(space::M)).max(s(space::S));
    quads.push(UiQuad::solid(
        x + s(space::M),
        (y + (h - rail_h) * 0.5).round(),
        rail_w,
        rail_h,
        rail_w * 0.5,
        surface::fade(kind.rail(sk), progress),
    ));
}

/// 文字的墨色。
///
/// chrome 的文字管线没有 alpha 通道，淡入淡出只能把墨混向卡片自己的底色——
/// 这也是卡片底必须不透明（[`surface::Elevation::Menu`]）的原因之一。
pub(crate) fn ink(sk: &Skin, progress: f32) -> Rgb {
    let progress = progress.clamp(0.0, 1.0);
    if progress >= 1.0 {
        return sk.ink;
    }
    let mix = |top: u8, bottom: u8| {
        (top as f32 * progress + bottom as f32 * (1.0 - progress)).round().clamp(0.0, 255.0) as u8
    };
    Rgb::new(mix(sk.ink.r, sk.panel.r), mix(sk.ink.g, sk.panel.g), mix(sk.ink.b, sk.panel.b))
}

/// 圆角查询：调用方需要在卡片上再叠东西时用，避免又写死一个数。
#[inline]
pub(crate) fn corner(scale: f32) -> f32 {
    radius::OVERLAY * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: (f32, f32) = (1000.0, 800.0);

    #[test]
    fn stack_grows_upward_from_the_bottom_right() {
        let first = toast_rect(VIEWPORT, 0, 20, 8.0, 18.0, 1.0, 1.0);
        let second = toast_rect(VIEWPORT, 1, 20, 8.0, 18.0, 1.0, 1.0);
        assert!(second.1 < first.1, "新的一条在下，旧的往上排");
        assert_eq!(first.0 + first.2, VIEWPORT.0 - MARGIN, "右边缘留白固定");
    }

    #[test]
    fn card_never_exceeds_the_viewport_budget() {
        let wide = toast_rect(VIEWPORT, 0, 400, 8.0, 18.0, 1.0, 1.0);
        assert!(wide.2 <= MAX_W, "超长文字不能把卡片撑破");
        assert!(wide.0 >= 0.0);
    }

    #[test]
    fn entry_progress_only_pushes_the_card_down() {
        let settled = toast_rect(VIEWPORT, 0, 20, 8.0, 18.0, 1.0, 1.0);
        let entering = toast_rect(VIEWPORT, 0, 20, 8.0, 18.0, 1.0, 0.0);
        assert_eq!(entering.1 - settled.1, RISE);
        assert_eq!(entering.0, settled.0, "入场只在纵向位移，横向不动");
    }

    #[test]
    fn text_origin_stays_inside_the_card() {
        let rect = toast_rect(VIEWPORT, 0, 20, 8.0, 18.0, 1.0, 1.0);
        let (tx, ty) = text_origin(rect, 18.0, 1.0);
        assert!(tx > rect.0 && tx < rect.0 + rect.2);
        assert!(ty > rect.1 && ty + 18.0 <= rect.1 + rect.3);
    }
}
