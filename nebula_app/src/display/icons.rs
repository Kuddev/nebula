//! Vector icon geometry for the chrome and settings surfaces.
//!
//! Every function here is a pure geometry producer: rectangle/center in,
//! [`UiQuad`]s out. No `Display` state, no hover logic, no theme lookups —
//! the caller decides colors (including hover ink) and passes them in. This
//! keeps drawing, hit-testing and theming in separate layers, so an icon can
//! never drift from its hit rect or secretly depend on renderer state.
//!
//! Font glyphs are deliberately NOT used for these marks: private-use glyph
//! outlines differ per font and drift at fractional DPI, which is what made
//! the caption buttons look unrelated before the vector rewrite.

use crate::renderer::ui::{Gradient, Rgba, UiQuad};

/// Blend `top` over `base` with straight alpha and return the resulting
/// opaque color. Used to compute the *effective* surface color under an icon
/// so cutout-style marks (rounded outlines) can "erase" their interior even
/// though the quad pipeline has no stencil or stroke primitive.
pub(crate) fn blend_over(base: Rgba, top: Rgba) -> Rgba {
    let alpha = top.a as f32 / 255.0;
    let mix = |b: u8, t: u8| (b as f32 * (1.0 - alpha) + t as f32 * alpha).round() as u8;
    Rgba::new(mix(base.r, top.r), mix(base.g, top.g), mix(base.b, top.b), base.a.max(top.a))
}

/// One straight stroke between two points, rendered as a thin quad. The
/// building block for X marks, chevrons and check marks.
pub(crate) fn push_segment(
    quads: &mut Vec<UiQuad>,
    from: (f32, f32),
    to: (f32, f32),
    thickness: f32,
    color: Rgba,
) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let len = dx.hypot(dy);
    if len <= f32::EPSILON {
        return;
    }
    let px = -dy / len * thickness * 0.5;
    let py = dx / len * thickness * 0.5;
    quads.push(UiQuad::poly(
        [
            [from.0 - px, from.1 - py],
            [from.0 + px, from.1 + py],
            [to.0 - px, to.1 - py],
            [to.0 + px, to.1 + py],
        ],
        color,
        color,
        Gradient::None,
    ));
}

/// Draw the "+" (new tab) mark centered in `rect`.
///
/// 图标墨迹不能跟随命中区缩放：侧栏收起时按钮是 28px，展开时是 20px，若从
/// rect 推导尺寸，同一个"新建 Tab"图标会在两种状态间忽大忽小。固定视觉
/// 尺寸，同时保留调用方的舒适命中区域。
pub(crate) fn push_add(quads: &mut Vec<UiQuad>, rect: (f32, f32, f32, f32), scale: f32, ink: Rgba) {
    let (x, y, width, height) = rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let center_x = x + width * 0.5;
    let center_y = y + height * 0.5;
    let stroke = (1.5 * scale).max(1.0);
    let arm = 7.0 * scale;
    quads.push(UiQuad::solid(
        center_x - arm,
        center_y - stroke * 0.5,
        arm * 2.0,
        stroke,
        stroke * 0.5,
        ink,
    ));
    quads.push(UiQuad::solid(
        center_x - stroke * 0.5,
        center_y - arm,
        stroke,
        arm * 2.0,
        stroke * 0.5,
        ink,
    ));
}

/// Vertical three-dot "more" mark centered in `rect`. Shares the "+" mark's
/// visual weight so the pair reads as one control family.
pub(crate) fn push_more(quads: &mut Vec<UiQuad>, rect: (f32, f32, f32, f32), scale: f32, ink: Rgba) {
    let (x, y, width, height) = rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let diameter = (2.8 * scale).max(2.0);
    let gap = 4.2 * scale;
    let center_x = x + width * 0.5;
    let center_y = y + height * 0.5;
    for offset in [-gap, 0.0, gap] {
        quads.push(UiQuad::solid(
            center_x - diameter * 0.5,
            center_y + offset - diameter * 0.5,
            diameter,
            diameter,
            diameter * 0.5,
            ink,
        ));
    }
}

/// A rounded-rectangle outline built as an ink plate with a `cutout`-colored
/// interior. The quad pipeline has no stroke primitive, so the caller must
/// pass the effective surface color behind the icon (see [`blend_over`]).
fn push_rounded_outline(
    quads: &mut Vec<UiQuad>,
    (x, y, size): (f32, f32, f32),
    radius: f32,
    stroke: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    quads.push(UiQuad::solid(x, y, size, size, radius, ink));
    quads.push(UiQuad::solid(
        x + stroke,
        y + stroke,
        size - 2.0 * stroke,
        size - 2.0 * stroke,
        (radius - stroke).max(0.0),
        cutout,
    ));
}

/// Which caption mark to draw. `Maximize { restore: true }` is the two
/// offset squares shown while the window is maximized or fullscreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowControlIcon {
    Minimize,
    Maximize { restore: bool },
    Close,
}

/// Windows 11-style caption marks: gray strokes, and the maximize/restore
/// squares carry soft rounded corners (the reference caption buttons), not
/// hard 90° outlines. `cutout` must be the button's effective background so
/// the square interiors stay "empty" on hover fills too.
pub(crate) fn push_window_control(
    quads: &mut Vec<UiQuad>,
    icon: WindowControlIcon,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let half = 5.0 * scale;
    let stroke = (1.25 * scale).max(1.0);
    match icon {
        WindowControlIcon::Minimize => {
            // 与最大化/关闭图标同一水平中线（2026-07-23 用户裁定：三大键
            // 水平对齐；不再模仿 Windows 的下沉减号）。
            let y = center_y;
            quads.push(UiQuad::solid(
                center_x - half,
                y - stroke * 0.5,
                half * 2.0,
                stroke,
                stroke * 0.5,
                ink,
            ));
        },
        WindowControlIcon::Maximize { restore: false } => {
            push_rounded_outline(
                quads,
                (center_x - half, center_y - half, half * 2.0),
                2.0 * scale,
                stroke,
                ink,
                cutout,
            );
        },
        WindowControlIcon::Maximize { restore: true } => {
            // Chrome/Windows restore mark: the back square is shifted up and
            // right, the front square down and left. Painting back-then-front
            // lets the front square's cutout erase the overlap, which is
            // exactly the native two-pane silhouette.
            let size = 8.0 * scale;
            let radius = 1.6 * scale;
            push_rounded_outline(
                quads,
                (center_x - 2.0 * scale, center_y - 5.0 * scale, size),
                radius,
                stroke,
                ink,
                cutout,
            );
            push_rounded_outline(
                quads,
                (center_x - 5.0 * scale, center_y - 2.0 * scale, size),
                radius,
                stroke,
                ink,
                cutout,
            );
        },
        WindowControlIcon::Close => {
            push_segment(
                quads,
                (center_x - half, center_y - half),
                (center_x + half, center_y + half),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (center_x + half, center_y - half),
                (center_x - half, center_y + half),
                stroke,
                ink,
            );
        },
    }
}

/// Sidebar fold toggle: a rounded window outline with a vertical divider.
/// `cutout` hollows the frame (same technique as the caption squares).
pub(crate) fn push_sidebar_toggle(
    quads: &mut Vec<UiQuad>,
    rect: (f32, f32, f32, f32),
    scale: f32,
    line: Rgba,
    cutout: Rgba,
) {
    let (x, y, width, height) = rect;
    let iw = 15.0 * scale;
    let ih = 15.0 * scale;
    let ix = x + (width - iw) * 0.5;
    let iy = y + (height - ih) * 0.5;
    let stroke = (1.35 * scale).max(1.0);
    quads.push(UiQuad::solid(ix, iy, iw, ih, 3.2 * scale, line));
    quads.push(UiQuad::solid(
        ix + stroke,
        iy + stroke,
        iw - 2.0 * stroke,
        ih - 2.0 * stroke,
        2.2 * scale,
        cutout,
    ));
    quads.push(UiQuad::solid(
        ix + 5.3 * scale,
        iy + stroke,
        stroke,
        ih - 2.0 * stroke,
        stroke * 0.5,
        line,
    ));
}

/// Dropdown chevron pointing down (`up = false`) or up, centered on
/// (`center_x`, `center_y`). Used by comboboxes and the font-size spinner.
pub(crate) fn push_chevron(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
    up: bool,
) {
    let arm = 3.9 * scale;
    let drop = 2.1 * scale;
    let stroke = (1.4 * scale).max(1.0);
    let (from_y, mid_y) = if up { (center_y + drop, center_y - drop) } else { (center_y - drop, center_y + drop) };
    push_segment(quads, (center_x - arm, from_y), (center_x, mid_y), stroke, ink);
    push_segment(quads, (center_x, mid_y), (center_x + arm, from_y), stroke, ink);
}

/// Check mark for selected dropdown rows, centered on (`center_x`, `center_y`).
pub(crate) fn push_check(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
) {
    let stroke = (1.5 * scale).max(1.0);
    push_segment(
        quads,
        (center_x - 4.2 * scale, center_y + 0.2 * scale),
        (center_x - 1.2 * scale, center_y + 3.2 * scale),
        stroke,
        ink,
    );
    push_segment(
        quads,
        (center_x - 1.2 * scale, center_y + 3.2 * scale),
        (center_x + 4.6 * scale, center_y - 3.4 * scale),
        stroke,
        ink,
    );
}
