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
    let stroke = (1.1 * scale).max(1.0);
    let arm = 6.0 * scale;
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

/// [`push_rounded_outline`] 的任意宽高版本，设置导航图标的统一骨架。
/// 外沿先吸附整像素（清晰度铁律），radius 允许到半高/半宽——胶囊与正圆
/// 都从这一个原语出（鼠标、地球、滑块环）。
fn push_rounded_frame(
    quads: &mut Vec<UiQuad>,
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    radius: f32,
    stroke: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let x0 = x0.round();
    let y0 = y0.round();
    let x1 = x1.round();
    let y1 = y1.round();
    let width = x1 - x0;
    let height = y1 - y0;
    if width < stroke * 2.0 || height < stroke * 2.0 {
        return;
    }
    let radius = radius.min(width * 0.5).min(height * 0.5);
    quads.push(UiQuad::solid(x0, y0, width, height, radius, ink));
    quads.push(UiQuad::solid(
        x0 + stroke,
        y0 + stroke,
        width - 2.0 * stroke,
        height - 2.0 * stroke,
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

/// 主机行右缘的动作图标：hover 才显形。主动作「连接」是文字按钮（原型里
/// 唯一带底的那个），这里只画次级动作——一行全是等价图标会逼人逐个悬停去
/// 猜哪个是"进去"。
///
/// 与设置导航图标同一套配方：线性圆润、真墨迹居中、外沿整像素吸附，尺寸统
/// 一按 `rect` 推导，因此几枚在任何 DPI 下都等重。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowActionIcon {
    /// 编辑：斜置笔杆 + 笔尖。
    Edit,
    /// 隐藏：眼睛加一道斜杠。
    Hide,
}

pub(crate) fn push_row_action_icon(
    quads: &mut Vec<UiQuad>,
    icon: RowActionIcon,
    rect: (f32, f32, f32, f32),
    scale: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let (x, y, width, height) = rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let cx = (x + width * 0.5).round();
    let cy = (y + height * 0.5).round();
    let stroke = (1.25 * scale).max(1.0);
    match icon {
        RowActionIcon::Edit => {
            // 笔杆：左下到右上一道斜线；笔尖再补一小段加粗，让它读成"笔"
            // 而不是"斜杠"（斜杠已经被 Hide 占用，两枚不能撞形）。
            let tip = (cx - 4.4 * scale, cy + 4.4 * scale);
            let tail = (cx + 4.4 * scale, cy - 4.4 * scale);
            push_segment(quads, tip, tail, stroke, ink);
            push_segment(
                quads,
                tip,
                (tip.0 + 1.8 * scale, tip.1 - 1.8 * scale),
                stroke * 2.1,
                ink,
            );
        },
        RowActionIcon::Hide => {
            // 眼睛：圆角胶囊挖空当外圈轮廓 + 中心瞳孔，再压一道斜杠。
            let w = 11.5 * scale;
            let h = 7.0 * scale;
            let ex = (cx - w * 0.5).round();
            let ey = (cy - h * 0.5).round();
            quads.push(UiQuad::solid(ex, ey, w, h, h * 0.5, ink));
            quads.push(UiQuad::solid(
                ex + stroke,
                ey + stroke,
                w - stroke * 2.0,
                h - stroke * 2.0,
                (h * 0.5 - stroke).max(0.0),
                cutout,
            ));
            let pupil = 2.8 * scale;
            quads.push(UiQuad::solid(
                cx - pupil * 0.5,
                cy - pupil * 0.5,
                pupil,
                pupil,
                pupil * 0.5,
                ink,
            ));
            // 斜杠先垫一条底色的更宽段，避免与眼睛墨迹糊成一团黑。
            let a = (cx - 5.6 * scale, cy + 5.6 * scale);
            let b = (cx + 5.6 * scale, cy - 5.6 * scale);
            push_segment(quads, a, b, stroke * 2.8, cutout);
            push_segment(quads, a, b, stroke, ink);
        },
    }
}

/// Settings sidebar marks. These are intentionally small, font-independent
/// vector shapes so the navigation keeps the same silhouette at every DPI
/// and does not depend on a private-use glyph in the user's UI font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsNavIcon {
    Appearance,
    Profiles,
    Providers,
    Ssh,
    Proxy,
    Interaction,
    Keymap,
    Advanced,
    Backup,
}

/// Whole-pixel horizontal bar (radius 0, snapped).
fn push_hbar(quads: &mut Vec<UiQuad>, x0: f32, x1: f32, y_center: f32, stroke: f32, ink: Rgba) {
    let y = (y_center - stroke * 0.5).round();
    let x0 = x0.round();
    quads.push(UiQuad::solid(x0, y, (x1.round() - x0).max(1.0), stroke, 0.0, ink));
}

/// Whole-pixel vertical bar (radius 0, snapped).
fn push_vbar(quads: &mut Vec<UiQuad>, x_center: f32, y0: f32, y1: f32, stroke: f32, ink: Rgba) {
    let x = (x_center - stroke * 0.5).round();
    let y0 = y0.round();
    quads.push(UiQuad::solid(x, y0, stroke, (y1.round() - y0).max(1.0), 0.0, ink));
}

/// Round dot with a whole-pixel bounding box.
fn push_dot(quads: &mut Vec<UiQuad>, center: (f32, f32), diameter: f32, ink: Rgba) {
    let d = diameter.round().max(2.0);
    let x = (center.0 - d * 0.5).round();
    let y = (center.1 - d * 0.5).round();
    quads.push(UiQuad::solid(x, y, d, d, d * 0.5, ink));
}

/// 设置导航图标。`cutout` 是图标落点的**有效底色**（面板色与选中/悬浮
/// 药丸合成后的结果）——整套图标全靠挖空手法做空心轮廓，传错底色环心
/// 就会变成一块色斑，见 [`blend_over`]。
///
/// 设计语言（2026-08-09 用户裁定：线性、干净、圆润，不要方方正正）：
/// - 一律细线轮廓，骨架统一走 [`push_rounded_frame`]——盒形图标全带圆角，
///   胶囊 / 正圆 / 跑道也是同一原语（radius 顶到半宽即是）；
/// - 不用大面积实心块，空心靠 cutout 挖，环内是真的透出底色；
/// - 清晰度铁律不动摇（与 `draw_ui_text` 的整像素锚点同源）：中心点先
///   吸附整数物理像素，横竖笔画从整数派生、宽取整数（`round().max(1)`），
///   斜线仍走 [`push_segment`] 的 poly 抗锯齿——斜边柔边是形状的一部分。
pub(crate) fn push_settings_nav_icon(
    quads: &mut Vec<UiQuad>,
    icon: SettingsNavIcon,
    rect: (f32, f32, f32, f32),
    scale: f32,
    ink: Rgba,
    cutout: Rgba,
) {
    let (x, y, width, height) = rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let cx = (x + width * 0.5).round();
    let cy = (y + height * 0.5).round();
    let stroke = (1.3 * scale).round().max(1.0);
    let u = |v: f32| v * scale;
    match icon {
        SettingsNavIcon::Appearance => {
            // Sun with a hollow core + 8 rays. The solid core read as a blob
            // next to the new outline family; a ring keeps the page linear.
            let core = u(6.2).round().max(4.0);
            push_rounded_frame(
                quads,
                (cx - core * 0.5, cy - core * 0.5, cx + core * 0.5, cy + core * 0.5),
                core * 0.5,
                stroke,
                ink,
                cutout,
            );
            let inner = u(5.0);
            let outer = u(7.4);
            push_vbar(quads, cx, cy - outer, cy - inner, stroke, ink);
            push_vbar(quads, cx, cy + inner, cy + outer, stroke, ink);
            push_hbar(quads, cx - outer, cx - inner, cy, stroke, ink);
            push_hbar(quads, cx + inner, cx + outer, cy, stroke, ink);
            let diag_inner = inner * std::f32::consts::FRAC_1_SQRT_2;
            let diag_outer = outer * std::f32::consts::FRAC_1_SQRT_2;
            for (sx, sy) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
                push_segment(
                    quads,
                    (cx + sx * diag_inner, cy + sy * diag_inner),
                    (cx + sx * diag_outer, cy + sy * diag_outer),
                    stroke,
                    ink,
                );
            }
        },
        SettingsNavIcon::Profiles => {
            // Rounded terminal window with a prompt: the shell-profile mark.
            push_rounded_frame(
                quads,
                (cx - u(7.2), cy - u(5.6), cx + u(7.2), cy + u(5.6)),
                u(2.6),
                stroke,
                ink,
                cutout,
            );
            push_segment(
                quads,
                (cx - u(4.4), cy - u(2.2)),
                (cx - u(2.0), cy + u(0.2)),
                stroke,
                ink,
            );
            push_segment(
                quads,
                (cx - u(2.0), cy + u(0.2)),
                (cx - u(4.4), cy + u(2.6)),
                stroke,
                ink,
            );
            push_hbar(quads, cx + u(0.8), cx + u(4.6), cy + u(2.3), stroke, ink);
        },
        SettingsNavIcon::Providers => {
            // A small bot face: the provider rail entry should read as AI,
            // while retaining the same hollow, DPI-stable outline language.
            push_rounded_frame(
                quads,
                (cx - u(6.4), cy - u(5.4), cx + u(6.4), cy + u(5.4)),
                u(2.8),
                stroke,
                ink,
                cutout,
            );
            push_dot(quads, (cx - u(2.5), cy - u(1.0)), u(1.8), ink);
            push_dot(quads, (cx + u(2.5), cy - u(1.0)), u(1.8), ink);
            push_hbar(quads, cx - u(2.8), cx + u(2.8), cy + u(3.0), stroke, ink);
            push_vbar(quads, cx, cy - u(8.0), cy - u(5.4), stroke, ink);
        },
        SettingsNavIcon::Ssh => {
            // Server stack: two rounded chassis, each with a status LED and a
            // vent slot. Same silhouette as before, corners softened.
            push_rounded_frame(
                quads,
                (cx - u(7.0), cy - u(6.4), cx + u(7.0), cy - u(1.0)),
                u(1.9),
                stroke,
                ink,
                cutout,
            );
            push_rounded_frame(
                quads,
                (cx - u(7.0), cy + u(1.0), cx + u(7.0), cy + u(6.4)),
                u(1.9),
                stroke,
                ink,
                cutout,
            );
            for mid in [cy - u(3.7), cy + u(3.7)] {
                push_dot(quads, (cx - u(4.3), mid), u(1.9), ink);
                push_hbar(quads, cx + u(0.8), cx + u(4.6), mid, stroke, ink);
            }
        },
        SettingsNavIcon::Proxy => {
            // Globe（用户 2026-08-09 点名：网络图标要地球）：外环 + 中央
            // 经线 + 赤道。经线是 radius=半宽的竖跑道，13px 下与椭圆无从
            // 分辨；顶底端正好落在外环内壁（同为整数坐标，零缝隙）。绘制
            // 顺序即层义：外环 → 经线 → 赤道最后横贯，网格交叉才成立。
            let d = u(15.0).round().max(9.0);
            let gx = (cx - d * 0.5).round();
            let gy = (cy - d * 0.5).round();
            push_rounded_frame(quads, (gx, gy, gx + d, gy + d), d * 0.5, stroke, ink, cutout);
            let mw = u(7.0).round().max(3.0);
            push_rounded_frame(
                quads,
                (cx - mw * 0.5, gy + stroke, cx + mw * 0.5, gy + d - stroke),
                mw * 0.5,
                stroke,
                ink,
                cutout,
            );
            push_hbar(quads, gx + stroke, gx + d - stroke, cy, stroke, ink);
        },
        SettingsNavIcon::Interaction => {
            // Mouse: a capsule outline with a scroll-wheel tick. The old
            // solid pointer-with-ripples was the loudest mark on the rail;
            // the page is about mouse behavior, so draw the mouse itself.
            let mw = u(9.2).round().max(6.0);
            let mh = u(15.0).round();
            let mx = (cx - mw * 0.5).round();
            let my = (cy - mh * 0.5).round();
            push_rounded_frame(quads, (mx, my, mx + mw, my + mh), mw * 0.5, stroke, ink, cutout);
            push_vbar(quads, cx, my + u(3.4), my + u(6.2), stroke, ink);
        },
        SettingsNavIcon::Keymap => {
            // 用户给定的键盘轮廓：横向圆角外框 + 两排小键 + 底部空格键。
            // 小尺寸不画字母，点阵比缩成噪声的字符更稳定，也与截图一致。
            let kw = u(16.0).round().max(10.0);
            let kh = u(12.0).round().max(8.0);
            let kx = (cx - kw * 0.5).round();
            let ky = (cy - kh * 0.5).round();
            push_rounded_frame(quads, (kx, ky, kx + kw, ky + kh), u(2.4), stroke, ink, cutout);
            let key = u(1.35).round().max(1.0);
            for row_y in [cy - u(2.2), cy + u(0.3)] {
                for offset in [-4.5, -1.5, 1.5, 4.5] {
                    quads.push(UiQuad::solid(
                        (cx + u(offset) - key * 0.5).round(),
                        (row_y - key * 0.5).round(),
                        key,
                        key,
                        key * 0.3,
                        ink,
                    ));
                }
            }
            push_hbar(quads, cx - u(3.2), cx + u(3.2), cy + u(3.3), key, ink);
        },
        SettingsNavIcon::Advanced => {
            // Three slider tracks with hollow knobs. The ring's cutout eats
            // the track inside it, so the knob visibly sits *on* the rail.
            for (offset, knob) in [(-4.4, -2.6), (0.0, 2.4), (4.4, -0.8)] {
                let track_y = cy + u(offset);
                push_hbar(quads, cx - u(7.0), cx + u(7.0), track_y, stroke, ink);
                let kd = u(4.8).round().max(3.0);
                let knob_x = (cx + u(knob) - kd * 0.5).round();
                let knob_y = ((track_y - stroke * 0.5).round() + stroke * 0.5 - kd * 0.5).round();
                push_rounded_frame(
                    quads,
                    (knob_x, knob_y, knob_x + kd, knob_y + kd),
                    kd * 0.5,
                    stroke,
                    ink,
                    cutout,
                );
            }
        },
        SettingsNavIcon::Backup => {
            // Down-arrow into a rounded tray. The tray is a full rounded
            // frame whose top edge is erased between the corner arcs——the
            // leftover arc stubs become the tray's rounded lips.
            push_vbar(quads, cx, cy - u(6.8), cy + u(0.8), stroke, ink);
            push_segment(quads, (cx - u(2.9), cy - u(1.2)), (cx, cy + u(1.8)), stroke, ink);
            push_segment(quads, (cx + u(2.9), cy - u(1.2)), (cx, cy + u(1.8)), stroke, ink);
            let tw = u(13.6).round();
            let th = u(5.0).round();
            let tx = (cx - tw * 0.5).round();
            let ty = (cy + u(2.2)).round();
            let radius = u(2.0).round();
            push_rounded_frame(quads, (tx, ty, tx + tw, ty + th), radius, stroke, ink, cutout);
            quads.push(UiQuad::solid(tx + radius, ty, tw - 2.0 * radius, stroke, 0.0, cutout));
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
