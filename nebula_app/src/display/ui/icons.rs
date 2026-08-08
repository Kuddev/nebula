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
pub(crate) fn push_more(
    quads: &mut Vec<UiQuad>,
    rect: (f32, f32, f32, f32),
    scale: f32,
    ink: Rgba,
) {
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
    let (from_y, mid_y) =
        if up { (center_y + drop, center_y - drop) } else { (center_y - drop, center_y + drop) };
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

/// Settings sidebar marks. These are intentionally small, font-independent
/// vector shapes so the navigation keeps the same silhouette at every DPI
/// and does not depend on a private-use glyph in the user's UI font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsNavIcon {
    Appearance,
    Profiles,
    Ssh,
    Interaction,
    Keymap,
    Advanced,
    Backup,
}

fn push_rect_outline(
    quads: &mut Vec<UiQuad>,
    (x, y, width, height): (f32, f32, f32, f32),
    stroke: f32,
    ink: Rgba,
) {
    push_segment(quads, (x, y), (x + width, y), stroke, ink);
    push_segment(quads, (x + width, y), (x + width, y + height), stroke, ink);
    push_segment(quads, (x + width, y + height), (x, y + height), stroke, ink);
    push_segment(quads, (x, y + height), (x, y), stroke, ink);
}

pub(crate) fn push_settings_nav_icon(
    quads: &mut Vec<UiQuad>,
    icon: SettingsNavIcon,
    rect: (f32, f32, f32, f32),
    scale: f32,
    ink: Rgba,
) {
    let (x, y, width, height) = rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let cx = x + width * 0.5;
    let cy = y + height * 0.5;
    let stroke = (1.25 * scale).max(1.0);
    let arm = |v: f32| v * scale;
    match icon {
        SettingsNavIcon::Appearance => {
            // A small sun/spark reads as appearance without competing with the
            // section label at the compact 13px icon size.
            quads.push(UiQuad::solid(
                cx - arm(3.0),
                cy - arm(3.0),
                arm(6.0),
                arm(6.0),
                arm(3.0),
                ink,
            ));
            for (from, to) in [
                ((cx, cy - arm(7.0)), (cx, cy - arm(5.0))),
                ((cx, cy + arm(5.0)), (cx, cy + arm(7.0))),
                ((cx - arm(7.0), cy), (cx - arm(5.0), cy)),
                ((cx + arm(5.0), cy), (cx + arm(7.0), cy)),
            ] {
                push_segment(quads, from, to, stroke, ink);
            }
        },
        SettingsNavIcon::Profiles => {
            push_rect_outline(
                quads,
                (cx - arm(6.0), cy - arm(5.0), arm(11.0), arm(10.0)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - arm(3.0), cy - arm(1.0)),
                (cx + arm(3.0), cy - arm(1.0)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - arm(3.0), cy + arm(2.0)),
                (cx + arm(2.0), cy + arm(2.0)),
                stroke,
                ink,
            );
        },
        SettingsNavIcon::Ssh => {
            push_rect_outline(
                quads,
                (cx - arm(7.0), cy - arm(5.0), arm(14.0), arm(10.0)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - arm(4.5), cy - arm(1.0)),
                (cx - arm(1.5), cy + arm(1.5)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - arm(1.5), cy + arm(1.5)),
                (cx - arm(4.5), cy + arm(4.0)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx + arm(1.5), cy + arm(3.5)),
                (cx + arm(4.5), cy + arm(3.5)),
                stroke,
                ink,
            );
        },
        SettingsNavIcon::Interaction => {
            push_segment(
                quads,
                (cx - arm(5.0), cy - arm(6.0)),
                (cx - arm(2.0), cy + arm(6.0)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - arm(5.0), cy - arm(6.0)),
                (cx + arm(5.5), cy - arm(1.5)),
                stroke,
                ink,
            );
            push_segment(quads, (cx + arm(5.5), cy - arm(1.5)), (cx + arm(1.0), cy), stroke, ink);
            push_segment(quads, (cx + arm(1.0), cy), (cx + arm(5.0), cy + arm(5.5)), stroke, ink);
        },
        SettingsNavIcon::Keymap => {
            push_rect_outline(
                quads,
                (cx - arm(7.0), cy - arm(5.0), arm(14.0), arm(10.0)),
                stroke,
                ink,
            );
            for dx in [-4.0, 0.0, 4.0] {
                quads.push(UiQuad::solid(
                    cx + arm(dx) - stroke * 0.5,
                    cy - arm(2.5),
                    stroke,
                    stroke,
                    stroke * 0.5,
                    ink,
                ));
            }
            push_segment(
                quads,
                (cx - arm(4.5), cy + arm(3.0)),
                (cx + arm(4.5), cy + arm(3.0)),
                stroke,
                ink,
            );
        },
        SettingsNavIcon::Advanced => {
            push_rect_outline(
                quads,
                (cx - arm(4.0), cy - arm(4.0), arm(8.0), arm(8.0)),
                stroke,
                ink,
            );
            for (from, to) in [
                ((cx, cy - arm(7.0)), (cx, cy - arm(4.0))),
                ((cx, cy + arm(4.0)), (cx, cy + arm(7.0))),
                ((cx - arm(7.0), cy), (cx - arm(4.0), cy)),
                ((cx + arm(4.0), cy), (cx + arm(7.0), cy)),
            ] {
                push_segment(quads, from, to, stroke, ink);
            }
        },
        SettingsNavIcon::Backup => {
            push_segment(quads, (cx, cy - arm(7.0)), (cx, cy + arm(2.0)), stroke, ink);
            push_segment(quads, (cx - arm(3.0), cy - arm(1.0)), (cx, cy + arm(2.0)), stroke, ink);
            push_segment(quads, (cx + arm(3.0), cy - arm(1.0)), (cx, cy + arm(2.0)), stroke, ink);
            push_rect_outline(
                quads,
                (cx - arm(6.0), cy + arm(2.5), arm(12.0), arm(4.0)),
                stroke,
                ink,
            );
        },
    }
}

/// 举起的手掌：「停下来等你」。标签徽章用它区别于「回合完成」的圆点。
///
/// # 为什么是实心而不是描边
///
/// 参照的原型画的是橙色描边、内部挖空的手掌轮廓。我们做不到：
/// 描边在这里只能靠 [`push_sidebar_toggle`] 那种「外层实心 + 内层填背景色」
/// 的挖空手法，而手指只有 2px 宽，两侧各让出 1px 描边就什么都不剩了。
/// 那条路要么把图标撑到 20px 以上，要么手指糊成一片——所以这里走实心，
/// 靠**形状简化**保清晰度。
///
/// # 指缝是这个图标的成败点
///
/// 几何按 13×13 的逻辑网格设计，中指最长、小指最短——手的辨识度几乎全在
/// 那条参差的指尖轮廓上，四根等长会读成一把梳子。
///
/// 但真正决定它是不是"一只手"的是**指缝**：第一版手指宽 1.9、间距 2.3，
/// 缝只剩 0.4px，100%–200% DPI 下四指全糊成一个团块。现在宽 1.6、间距
/// 2.8（缝 1.2），并且每个 quad 都走 [`UiQuad::pixel_snapped`]——1.2px 的
/// 缝若跨在像素边界上就是两条半透明的灰边，等于没有；吸附到整像素后
/// 100% DPI 也能看清四指。这条和 draw_ui_text 的整像素锚点是同一条铁律。
///
/// # 现状：未接线
///
/// 用户 08-02 裁定这一版形不满意（小尺寸下指缝仍读不成手），标签徽章的
/// 「等你批准」暂时退回 warn 色圆点，见 `chrome.rs` 的 `attention` 分支。
/// 几何与踩过的坑都留在这儿，等重画时接着用。
#[allow(dead_code)]
pub(crate) fn push_hand(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
) {
    let s = |v: f32| v * scale;
    let finger_w = s(2.2).max(1.4);

    // 四指：粗、短、紧——缝只留一条细线（0.7）。第一版指宽 1.6、缝 1.2，
    // 画出来是一把耙子：手的胖瘦不在指长，在**指占比**。指尖高低差保持
    // 拱形（中指最高、小指最矮），这是"手"而不是"梳子"的全部线索。
    for (dx, tip, len) in
        [(-4.35, -4.2, 5.4), (-1.45, -5.2, 6.4), (1.45, -4.7, 5.9), (4.35, -3.3, 4.5)]
    {
        quads.push(
            UiQuad::solid(
                center_x + s(dx) - finger_w * 0.5,
                center_y + s(tip),
                finger_w,
                s(len),
                finger_w * 0.5,
                ink,
            )
            .pixel_snapped(),
        );
    }

    // 掌与拇指的圆角从**自身高度**派生，不走 tokens::radius 阶梯：那道阶梯
    // 描述的是容器（浮层/控件/chip）的层级语言，而这里的圆头是有机形状的
    // 一部分——指腹和掌根本来就该随手指粗细走，跟着卡片圆角一起调反而错。
    //
    // 掌接住四指的整块，够高才圆得像掌根；边指与掌缘平齐，读作连体的手
    // 而不是"块上插齿"。
    let palm_h = s(6.0);
    quads.push(
        UiQuad::solid(center_x + s(-5.45), center_y, s(10.9), palm_h, palm_h * 0.47, ink)
            .pixel_snapped(),
    );

    // 拇指：向左伸出的粗短一截，圆头即半高。没有它，剩下的部分读作一把刷子。
    let thumb_h = s(2.8);
    quads.push(
        UiQuad::solid(center_x + s(-8.2), center_y + s(1.0), s(4.4), thumb_h, thumb_h * 0.5, ink)
            .pixel_snapped(),
    );
}

/// 忙碌指示器：一整圈暗色轨道 + 一段绕行的亮弧。
///
/// # 怎么在没有描边和旋转的管线里画出一个**真空心**的环
///
/// 原型是 `border-top-color` 的旋转圆环。`UiQuad` 既没有描边也不能旋转。
///
/// 中间那版用挖空法：实心圆叠一个"底色"的小圆。它的错在于**底色是猜的**
/// ——spinner 画在标签行上，而行底可能是面板色、悬浮色或选中药丸，挖空色
/// 一旦猜错，中间就不是空的，而是一块颜色不对的实心圆盖在上面。屏幕上看到
/// 的就是一颗深色圆点。
///
/// 现在整圈都由点铺成，**只画环本身，中间根本没画东西**——所以它在任何底
/// 上都是真的空心。两个教训叠在这同一个函数里：
///
/// 1. **间距要远小于点径**（这里取半个点径，50% 重叠），点与点才连成弧；
///    第一版间距 2.6px 而点径只有 2.1px，缝比点还宽，看到的是一圈珠子。
/// 2. **点必须是不透明色**。半透明的点一重叠，重叠区 alpha 翻倍、颜色加
///    深——环上亮暗相间，又是一圈珠子，这次的珠子是叠加缝画出来的。所以
///    轨道与亮弧先各自和 `base`（环所在处的底色，须不透明）合成成不透明
///    色再画；中间依然什么都没画，空心不靠猜底色。
///
/// `phase` 取 `0..1` 的一圈。挂在挂钟上而不是累加帧增量：掉帧只让它顿一下，
/// 不会让它变慢——"转得比别处慢"恰恰最容易被读成已经卡死。
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_spinner(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    radius: f32,
    phase: f32,
    track: Rgba,
    head: Rgba,
    base: Rgba,
) {
    let track = super::surface::over(track, base);
    let head = super::surface::over(head, base);
    let stroke = (radius * 0.30).max(1.0);
    // 点铺在轨道中线上：半径减半个笔画，环的外缘才正好落在 `radius` 上。
    let mid = radius - stroke * 0.5;
    let steps = ((mid * std::f32::consts::TAU / (stroke * 0.5)).ceil() as usize).clamp(24, 96);
    // 亮弧占整圈的三分之一：短了读不出方向，长了整圈都在发亮就不像在转。
    const ARC: f32 = 0.34;
    for step in 0..steps {
        let at = step as f32 / steps as f32;
        let angle = at * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        // 离头部多远（沿转动方向往回数），0 = 正是头部。
        let behind = (phase - at).rem_euclid(1.0);
        let t = (1.0 - behind / ARC).clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        quads.push(UiQuad::solid(
            center_x + mid * angle.cos() - stroke * 0.5,
            center_y + mid * angle.sin() - stroke * 0.5,
            stroke,
            stroke,
            stroke * 0.5,
            Rgba::new(
                mix(track.r, head.r),
                mix(track.g, head.g),
                mix(track.b, head.b),
                mix(track.a, head.a),
            ),
        ));
    }
}

/// 警示三角：命令以非零码退出。
///
/// UiQuad 没有多边形填充，所以三角靠**逐行收窄的横条**堆出来——行高取 1 物理
/// 像素，斜边就是一串整像素台阶，和字形栅格化的处理同源。行数按尺寸算，不写
/// 死：低 DPI 下八行足够，高 DPI 下再多几行斜边才不显毛糙。
///
/// 中间那道竖杠是感叹号，用底色挖空（[`blend_over`] 的用途），比另画一层墨迹
/// 更稳——三角内部是实心的，任何叠加色都会透出下面的红。
pub(crate) fn push_alert(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let s = |v: f32| v * scale;
    let half_w = s(6.2);
    let height = s(10.4);
    let top = center_y - height * 0.5;
    let step = (scale.round()).max(1.0);
    let rows = (height / step).ceil().max(6.0) as usize;

    for row in 0..rows {
        let t = row as f32 / rows as f32;
        // 顶端留一点宽度而不是收成尖，尖角在小尺寸下只会变成一个孤立的
        // 半透明点。
        let w = half_w * (0.18 + 0.82 * t);
        quads.push(
            UiQuad::solid(center_x - w, top + row as f32 * step, w * 2.0, step + 0.5, 0.0, ink)
                .pixel_snapped(),
        );
    }

    // 感叹号：一竖 + 一点，都用底色挖空。
    let bar_w = (s(1.5)).max(1.0);
    quads.push(
        UiQuad::solid(center_x - bar_w * 0.5, center_y - s(1.6), bar_w, s(3.4), 0.0, cutout)
            .pixel_snapped(),
    );
    let dot = (s(1.5)).max(1.0);
    quads.push(
        UiQuad::solid(center_x - dot * 0.5, center_y + s(2.8), dot, dot, dot * 0.5, cutout)
            .pixel_snapped(),
    );
}

/// 实心圆底 + 挖空的对勾：命令成功收尾的那一下。
///
/// 和 [`push_check`] 的区别是它自带底盘——徽章位上一个裸勾太轻，压不住
/// 旁边那颗同尺寸的圆点；有了底盘，它读起来才是"圆点完成态"的同一族。
pub(crate) fn push_check_badge(
    quads: &mut Vec<UiQuad>,
    center_x: f32,
    center_y: f32,
    scale: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let s = |v: f32| v * scale;
    let d = s(11.0);
    quads.push(UiQuad::solid(center_x - d * 0.5, center_y - d * 0.5, d, d, d * 0.5, ink));
    let stroke = (s(1.6)).max(1.0);
    push_segment(
        quads,
        (center_x - s(2.6), center_y + s(0.1)),
        (center_x - s(0.7), center_y + s(2.1)),
        stroke,
        cutout,
    );
    push_segment(
        quads,
        (center_x - s(0.7), center_y + s(2.1)),
        (center_x + s(2.9), center_y - s(2.2)),
        stroke,
        cutout,
    );
}
