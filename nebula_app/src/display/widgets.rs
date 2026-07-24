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
use super::theme::Skin;
use crate::renderer::ui::{Rgba, UiQuad};

pub(crate) type Rect = (f32, f32, f32, f32);

/// Row-height of one dropdown option in logical px.
pub(crate) const POPUP_ROW_H: f32 = 36.0;
/// Closed combobox / spinner control height in logical px.
const CONTROL_H: f32 = 32.0;
/// Right inset shared by every row-trailing control.
const ROW_INSET: f32 = 16.0;

#[inline]
fn accent(sk: &Skin) -> Rgba {
    Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255)
}

#[inline]
fn opaque_panel(sk: &Skin) -> Rgba {
    Rgba::new(sk.panel.r, sk.panel.g, sk.panel.b, 255)
}

// ---- slider ----

/// Windows Terminal-style slider inside the row's wide hit rect: a thin
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
    quads.push(UiQuad::solid(cx - s(1.0), cy - s(1.0), cw + s(2.0), ch + s(2.0), s(7.0), sk.hairline));
    quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), opaque_panel(sk)));
    quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.surface));
    if hot || open {
        quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.hover));
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
    (popup.0 + s(4.0), popup.1 + s(4.0) + index as f32 * s(POPUP_ROW_H), popup.2 - s(8.0), s(POPUP_ROW_H))
}

pub(crate) fn popup_row_at(popup: Rect, count: usize, scale: f32, x: f32, y: f32) -> Option<usize> {
    (0..count).find(|&index| super::contains_rect(popup_row_rect(popup, index, scale), x, y))
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
    let (px, py, pw, ph) = popup;
    quads.push(UiQuad::glow(px - s(14.0), py - s(10.0), pw + s(28.0), ph + s(26.0), Rgba::new(0, 0, 0, 70)));
    quads.push(UiQuad::solid(px - s(1.0), py - s(1.0), pw + s(2.0), ph + s(2.0), s(9.0), sk.hairline));
    quads.push(UiQuad::solid(px, py, pw, ph, s(8.0), opaque_panel(sk)));
    quads.push(UiQuad::solid(px, py, pw, ph, s(8.0), sk.surface));
    for index in 0..count {
        let (rx, ry, rw, rh) = popup_row_rect(popup, index, scale);
        if selected == Some(index) {
            quads.push(UiQuad::solid(rx, ry + s(2.0), rw, rh - s(4.0), s(6.0), sk.accent_soft));
        } else if hover == Some(index) {
            quads.push(UiQuad::solid(rx, ry + s(2.0), rw, rh - s(4.0), s(6.0), sk.hover));
        }
    }
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
    let s = |v: f32| v * scale;
    let (value, up, down) = spinner_rects(row, scale);
    for (rect, hot, chevron_up) in
        [(value, false, None), (up, hot_up, Some(true)), (down, hot_down, Some(false))]
    {
        let (cx, cy, cw, ch) = rect;
        quads.push(UiQuad::solid(cx - s(1.0), cy - s(1.0), cw + s(2.0), ch + s(2.0), s(7.0), sk.hairline));
        quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), opaque_panel(sk)));
        quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.surface));
        if hot {
            quads.push(UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.hover));
        }
        if let Some(up_arrow) = chevron_up {
            let ink = Rgba::new(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b, 235);
            icons::push_chevron(quads, cx + cw * 0.5, cy + ch * 0.5, scale, ink, up_arrow);
        }
    }
}
