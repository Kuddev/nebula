//! Reusable settings-control primitives: slider, toggle, combobox, spinner.
//!
//! Same layering contract as [`super::icons`]: pure geometry + quad output,
//! no `Display` state and no hover decisions. Layout, drawing and hit-testing
//! all call the SAME rect helpers here, so a control's clickable area can
//! never drift from its painted area — the drift class of bug that made the
//! hand-rolled per-page controls expensive to debug.
//!
//! Text (current values, option labels) is deliberately NOT drawn here: the
//! renderer draws chrome text in a separate pass, so widgets expose the
//! geometry (`*_rect`, [`combobox_text_x`]) and the text pass reuses it.

use super::icons;
use super::surface;
use super::theme::Skin;
use super::tokens::radius;
use crate::renderer::ui::{Rgba, UiQuad};

pub(crate) type Rect = (f32, f32, f32, f32);

/// Row-height of one dropdown option in logical px.
pub(crate) const POPUP_ROW_H: f32 = 36.0;
/// Closed combobox / spinner control height in logical px.
const CONTROL_H: f32 = 32.0;
/// Right inset shared by every row-trailing control.
const ROW_INSET: f32 = 16.0;

/// Visual state for a text chip. Quiet chips intentionally have no plate: the
/// selected state is the only decoration, which keeps a row of filters calm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipState {
    Quiet,
    Hover,
    Selected,
}

// ---- overlay scrollbar ----

/// Geometry for a trackless overlay scrollbar. `hit` is intentionally wider
/// than `thumb`: the control stays easy to grab without painting a dark rail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OverlayScrollbar {
    pub(crate) thumb: Rect,
    pub(crate) hit: Rect,
    track_y: f32,
    track_h: f32,
    max_scroll: f32,
}

impl OverlayScrollbar {
    pub(crate) fn hit_test(self, x: f32, y: f32) -> bool {
        crate::display::contains_rect(self.hit, x, y)
    }

    /// Map a pointer y coordinate to content scroll, preserving the point at
    /// which the thumb was grabbed. Track presses pass half the thumb height.
    pub(crate) fn target_scroll(self, y: f32, grab: f32) -> f32 {
        let travel = (self.track_h - self.thumb.3).max(1.0);
        let thumb_y = (y - grab - self.track_y).clamp(0.0, travel);
        self.max_scroll * thumb_y / travel
    }

    pub(crate) fn target_offset(self, y: f32, grab: f32, max_offset: usize) -> usize {
        if self.max_scroll <= 0.0 {
            return 0;
        }
        (self.target_scroll(y, grab) / self.max_scroll * max_offset as f32)
            .round()
            .clamp(0.0, max_offset as f32) as usize
    }
}

pub(crate) fn overlay_scrollbar(
    area: Rect,
    viewport: f32,
    content: f32,
    scroll: f32,
    scale: f32,
) -> Option<OverlayScrollbar> {
    let max_scroll = content - viewport;
    if viewport <= 0.0 || max_scroll <= 0.0 {
        return None;
    }
    let s = |v: f32| v * scale;
    let track_y = area.1 + s(6.0);
    let track_h = (area.3 - s(12.0)).max(s(26.0));
    let thumb_h = (track_h * viewport / content).max(s(26.0)).min(track_h);
    let thumb_y = track_y + (track_h - thumb_h) * (scroll / max_scroll).clamp(0.0, 1.0);
    let visual_w = s(5.0);
    let hit_w = s(14.0);
    let right = area.0 + area.2 - s(4.0);
    Some(OverlayScrollbar {
        thumb: (right - visual_w, thumb_y, visual_w, thumb_h),
        hit: (right - hit_w, track_y, hit_w, track_h),
        track_y,
        track_h,
        max_scroll,
    })
}

/// Paint only the thumb. The transparent hit region is geometry, never a rail.
pub(crate) fn push_overlay_scrollbar(
    quads: &mut Vec<UiQuad>,
    scrollbar: OverlayScrollbar,
    _scale: f32,
    sk: &Skin,
    hot: bool,
    dragging: bool,
) {
    let alpha = if dragging {
        0.78
    } else if hot {
        0.64
    } else if sk.is_light {
        0.50
    } else {
        0.46
    };
    let color = sk.scrollbar_thumb.with_alpha(alpha);
    quads.push(UiQuad::solid(
        scrollbar.thumb.0,
        scrollbar.thumb.1,
        scrollbar.thumb.2,
        scrollbar.thumb.3,
        scrollbar.thumb.2 * 0.5,
        color,
    ));
}

#[inline]
fn accent(sk: &Skin) -> Rgba {
    Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255)
}

#[inline]
fn opaque_panel(sk: &Skin) -> Rgba {
    Rgba::new(sk.panel.r, sk.panel.g, sk.panel.b, 255)
}

/// Paint the reusable pill background shared by launcher filters and future
/// compact segmented controls. Text stays in the caller's text pass so the
/// same geometry can be used for layout, hit testing, and drawing.
pub(crate) fn push_chip(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    state: ChipState,
) {
    let (x, y, w, h) = rect;
    let corner = h * 0.5;
    match state {
        ChipState::Quiet => {},
        ChipState::Hover => {
            quads.push(UiQuad::solid(x, y, w, h, corner, surface::over(sk.hover, sk.panel)));
        },
        ChipState::Selected => {
            let stroke = Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 112);
            surface::push_stroke(quads, rect, corner, scale, stroke);
            quads.push(UiQuad::solid(x, y, w, h, corner, surface::over(sk.accent_soft, sk.panel)));
        }
    }
}

// ---- slider ----

/// Slider inside the row's wide hit rect: a thin
/// track + accent fill + a ringed round thumb whose inner dot grows while
/// the pointer is on the control (`hot`). No plate behind the track — the
/// hit rect stays invisible.
pub(crate) fn push_slider(
    quads: &mut Vec<UiQuad>,
    hit: Rect,
    value: f32,
    scale: f32,
    sk: &Skin,
    hot: bool,
) {
    let s = |v: f32| v * scale;
    let (hit_x, hit_y, hit_w, hit_h) = hit;
    let frac = value.clamp(0.0, 1.0);
    let center_y = hit_y + hit_h * 0.5;
    // Keep the full thumb inside the hit rect at 0% and 100%.
    let thumb_r = s(9.0);
    let track_x = hit_x + thumb_r;
    let track_w = (hit_w - 2.0 * thumb_r).max(s(24.0));
    let track_h = s(4.0);
    let track_y = center_y - track_h * 0.5;
    let thumb_x = track_x + track_w * frac;

    quads.push(UiQuad::solid(track_x, track_y, track_w, track_h, track_h * 0.5, sk.track_off));
    quads.push(UiQuad::solid(
        track_x,
        track_y,
        (track_w * frac).max(track_h),
        track_h,
        track_h * 0.5,
        accent(sk),
    ));

    // Thumb: hairline ring → light plate → accent dot (grows when hot).
    quads.push(UiQuad::solid(
        thumb_x - thumb_r - s(1.0),
        center_y - thumb_r - s(1.0),
        (thumb_r + s(1.0)) * 2.0,
        (thumb_r + s(1.0)) * 2.0,
        thumb_r + s(1.0),
        sk.hairline,
    ));
    quads.push(UiQuad::solid(
        thumb_x - thumb_r,
        center_y - thumb_r,
        thumb_r * 2.0,
        thumb_r * 2.0,
        thumb_r,
        sk.knob_off,
    ));
    let dot_r = if hot { s(5.0) } else { s(3.5) };
    quads.push(UiQuad::solid(
        thumb_x - dot_r,
        center_y - dot_r,
        dot_r * 2.0,
        dot_r * 2.0,
        dot_r,
        accent(sk),
    ));
}

// ---- toggle ----

/// A toggle switch at the right edge of `row`: pill track + round thumb.
/// `on` fills the track with the accent; off stays a muted gray.
pub(crate) fn push_toggle(quads: &mut Vec<UiQuad>, row: Rect, on: bool, scale: f32, sk: &Skin) {
    let s = |v: f32| v * scale;
    let (rx, ry, rw, rh) = row;
    let tw = s(38.0);
    let th = s(20.0);
    let tx = rx + rw - s(ROW_INSET) - tw;
    let ty = ry + (rh - th) / 2.0;
    quads.push(UiQuad::solid(tx, ty, tw, th, th / 2.0, if on { accent(sk) } else { sk.track_off }));
    let knob = th - s(6.0);
    let kx = if on { tx + tw - s(3.0) - knob } else { tx + s(3.0) };
    let kcol = if on { sk.knob_on } else { sk.knob_off };
    quads.push(UiQuad::solid(kx, ty + s(3.0), knob, knob, knob / 2.0, kcol));
}

// ---- combobox ----

/// The closed dropdown control docked at the right edge of its settings row.
pub(crate) fn combobox_rect(row: Rect, scale: f32) -> Rect {
    let s = |v: f32| v * scale;
    let (rx, ry, rw, rh) = row;
    let cw = s(220.0).min(rw * 0.46).max(s(132.0));
    let ch = s(CONTROL_H);
    (rx + rw - s(ROW_INSET) - cw, ry + (rh - ch) / 2.0, cw, ch)
}

/// Left edge for the closed control's value text.
pub(crate) fn combobox_text_x(rect: Rect, scale: f32) -> f32 {
    rect.0 + 12.0 * scale
}

/// Right edge available to the value text (the chevron well starts here).
pub(crate) fn combobox_text_right(rect: Rect, scale: f32) -> f32 {
    rect.0 + rect.2 - 28.0 * scale
}

/// Closed combobox: hairline frame, quiet surface, trailing chevron.
pub(crate) fn push_combobox(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    hot: bool,
    open: bool,
) {
    let s = |v: f32| v * scale;
    let (cx, cy, cw, ch) = rect;
    let corner = radius::CONTROL * scale;
    surface::push_stroke(quads, rect, corner, scale, sk.hairline);
    quads.push(UiQuad::solid(cx, cy, cw, ch, corner, opaque_panel(sk)));
    quads.push(UiQuad::solid(cx, cy, cw, ch, corner, sk.surface));
    if hot || open {
        quads.push(UiQuad::solid(cx, cy, cw, ch, corner, sk.hover));
    }
    let ink = Rgba::new(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b, 235);
    icons::push_chevron(quads, cx + cw - s(15.0), cy + ch * 0.5, scale, ink, open);
}

/// Popup rect for `count` options anchored under (or, when the viewport
/// bottom is too close, above) the closed control. The popup floats over
/// later rows instead of pushing them down.
pub(crate) fn combobox_popup_rect(
    anchor: Rect,
    count: usize,
    scale: f32,
    clip_top: f32,
    clip_bot: f32,
) -> Rect {
    let s = |v: f32| v * scale;
    let row_h = s(POPUP_ROW_H);
    let pad = s(4.0);
    let height = count as f32 * row_h + 2.0 * pad;
    let below = anchor.1 + anchor.3 + s(4.0);
    let y = if below + height <= clip_bot || anchor.1 - s(4.0) - height < clip_top {
        below
    } else {
        anchor.1 - s(4.0) - height
    };
    (anchor.0, y, anchor.2, height)
}

pub(crate) fn popup_row_rect(popup: Rect, index: usize, scale: f32) -> Rect {
    let s = |v: f32| v * scale;
    (
        popup.0 + s(4.0),
        popup.1 + s(4.0) + index as f32 * s(POPUP_ROW_H),
        popup.2 - s(8.0),
        s(POPUP_ROW_H),
    )
}

pub(crate) fn popup_row_at(popup: Rect, count: usize, scale: f32, x: f32, y: f32) -> Option<usize> {
    (0..count)
        .find(|&index| crate::display::contains_rect(popup_row_rect(popup, index, scale), x, y))
}

/// The floating option list: opaque plate + soft shadow so it reads as a
/// layer ABOVE the page, selected row in the accent wash, hover in the quiet
/// wash. Option text and check marks belong to the text pass.
pub(crate) fn push_combobox_popup(
    quads: &mut Vec<UiQuad>,
    popup: Rect,
    count: usize,
    selected: Option<usize>,
    hover: Option<usize>,
    scale: f32,
    sk: &Skin,
) {
    let s = |v: f32| v * scale;
    // 层级走 Menu：真外阴影 + 同心描边 + 不透明底。此前这里是
    // `UiQuad::glow` —— glow 向外扩散亮度，不建立高度关系，在浅色主题上
    // 只会让下拉四周发灰，看着像脏了一圈而不是浮起来。
    surface::push_surface(quads, popup, (0.0, 0.0), scale, sk, surface::Elevation::Menu, 1.0);
    quads.push(UiQuad::solid(
        popup.0,
        popup.1,
        popup.2,
        popup.3,
        radius::OVERLAY * scale,
        sk.surface,
    ));
    for index in 0..count {
        let (rx, ry, rw, rh) = popup_row_rect(popup, index, scale);
        let fill = if selected == Some(index) {
            sk.accent_soft
        } else if hover == Some(index) {
            sk.hover
        } else {
            continue;
        };
        quads.push(UiQuad::solid(rx, ry + s(2.0), rw, rh - s(4.0), radius::CONTROL * scale, fill));
    }
}

// ---- close button ----

/// 按钮的可视方块：在命中区里居中的正方形。绘制与 hover 反馈都用它，命中区
/// 本身可以比它宽（消息栏那种按网格列切出来的 3 列区域），指针容差因此不会
/// 被图标的视觉尺寸绑死。
pub(crate) fn close_button_plate_rect(hit: Rect) -> Rect {
    let (hx, hy, hw, hh) = hit;
    let side = hw.min(hh);
    ((hx + (hw - side) * 0.5).round(), (hy + (hh - side) * 0.5).round(), side, side)
}

/// 关闭按钮：一块底 + 一个 ✕ 墨迹。
///
/// `fill` 由调用方按状态给（常态一层淡底、hover 加深）——本层不做 hover 决策。
/// 常态就画底是刻意的：裸 ✕ 在横幅上读不出"这是个可点的东西"，用户反馈过
/// 「看不到关闭按钮 = 无法关闭」。
///
/// `ink` 同样由调用方给：消息栏是终端色系（黄/红底 + 背景色的字），不走 Skin。
pub(crate) fn push_close_button(
    quads: &mut Vec<UiQuad>,
    hit: Rect,
    scale: f32,
    ink: Rgba,
    fill: Rgba,
) {
    let plate = close_button_plate_rect(hit);
    let (px, py, side, _) = plate;
    quads.push(UiQuad::solid(px, py, side, side, radius::CHIP * scale, fill));

    let cx = px + side * 0.5;
    let cy = py + side * 0.5;
    let half = (side * 0.22).max(3.0 * scale);
    let stroke = (1.4 * scale).max(1.0);
    icons::push_segment(quads, (cx - half, cy - half), (cx + half, cy + half), stroke, ink);
    icons::push_segment(quads, (cx + half, cy - half), (cx - half, cy + half), stroke, ink);
}

// ---- spinner ----

/// Numeric stepper docked at the right edge of its row:
/// `[ value ] [∧] [∨]`, Windows 11 style. Returns (value box, up, down).
pub(crate) fn spinner_rects(row: Rect, scale: f32) -> (Rect, Rect, Rect) {
    let s = |v: f32| v * scale;
    let (rx, ry, rw, rh) = row;
    let ch = s(CONTROL_H);
    let cy = ry + (rh - ch) / 2.0;
    let button_w = s(32.0);
    let value_w = s(56.0);
    let gap = s(4.0);
    let down = (rx + rw - s(ROW_INSET) - button_w, cy, button_w, ch);
    let up = (down.0 - gap - button_w, cy, button_w, ch);
    let value = (up.0 - gap - value_w, cy, value_w, ch);
    (value, up, down)
}

pub(crate) fn push_spinner(
    quads: &mut Vec<UiQuad>,
    row: Rect,
    scale: f32,
    sk: &Skin,
    hot_up: bool,
    hot_down: bool,
) {
    let (value, up, down) = spinner_rects(row, scale);
    let corner = radius::CONTROL * scale;
    for (rect, hot, chevron_up) in
        [(value, false, None), (up, hot_up, Some(true)), (down, hot_down, Some(false))]
    {
        let (cx, cy, cw, ch) = rect;
        surface::push_stroke(quads, rect, corner, scale, sk.hairline);
        quads.push(UiQuad::solid(cx, cy, cw, ch, corner, opaque_panel(sk)));
        quads.push(UiQuad::solid(cx, cy, cw, ch, corner, sk.surface));
        if hot {
            quads.push(UiQuad::solid(cx, cy, cw, ch, corner, sk.hover));
        }
        if let Some(up_arrow) = chevron_up {
            let ink = Rgba::new(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b, 235);
            icons::push_chevron(quads, cx + cw * 0.5, cy + ch * 0.5, scale, ink, up_arrow);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::overlay_scrollbar;

    #[test]
    fn overlay_scrollbar_has_a_wide_invisible_hit_area() {
        let bar = overlay_scrollbar((0.0, 0.0, 400.0, 300.0), 300.0, 900.0, 300.0, 1.0)
            .expect("overflowing content needs a scrollbar");
        assert_eq!(bar.thumb.2, 5.0);
        assert_eq!(bar.hit.2, 14.0);
        assert!(bar.hit.0 < bar.thumb.0);
    }

    #[test]
    fn overlay_scrollbar_drag_maps_to_the_full_scroll_range() {
        let bar = overlay_scrollbar((0.0, 0.0, 400.0, 300.0), 300.0, 900.0, 0.0, 1.0)
            .expect("overflowing content needs a scrollbar");
        let grab = bar.thumb.3 * 0.5;
        assert_eq!(bar.target_offset(bar.hit.1 + grab, grab, 20), 0);
        assert_eq!(bar.target_offset(bar.hit.1 + bar.hit.3, grab, 20), 20);
    }

    #[test]
    fn overlay_scrollbar_is_absent_without_overflow() {
        assert!(overlay_scrollbar((0.0, 0.0, 400.0, 300.0), 300.0, 300.0, 0.0, 1.0).is_none());
    }
}
