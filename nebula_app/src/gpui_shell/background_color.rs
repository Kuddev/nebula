//! 外观页「背景色」：闭态 combobox + 开态 SV/色相/色板/hex 浮层。
//!
//! 几何与命中对照旧壳 `combobox_rect` / `push_combobox` /
//! `background_color_popup`；不走组件库 `ColorPicker`。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, canvas, deferred, div, fill,
    point, px, size,
};

use crate::display::color::Rgb;
use crate::display::ui::tokens::radius;
use crate::display::{BACKGROUND_SWATCHES, BgPickerPart, hsv_to_rgb, rgb_to_hsv};
use crate::gpui_shell::prelude::*;
use gpui_component::input::InputEvent;
use nebula_settings::{format_hex_rgb, parse_hex_rgb};

use super::SettingsPane;

const COMBOBOX_W: f32 = 220.0;
const COMBOBOX_H: f32 = 32.0;
const CHIP: f32 = 16.0;
const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const SV_H: f32 = 128.0;
const HUE_H: f32 = 14.0;
const CELL: f32 = 30.0;
const HEX_H: f32 = 34.0;
const COLS: usize = 6;
const SV_COLS: usize = 24;
const SV_ROWS: usize = 16;
const HUE_STEPS: usize = 36;

fn popup_size() -> (f32, f32) {
    let grid_w = COLS as f32 * CELL + (COLS - 1) as f32 * GAP;
    let grid_h = 2.0 * CELL + GAP;
    let w = (grid_w + 2.0 * PAD).max(COMBOBOX_W);
    let h = PAD + SV_H + GAP + HUE_H + GAP + grid_h + GAP + HEX_H + PAD;
    (w, h)
}

impl SettingsPane {
    pub(super) fn effective_background_rgb(&self, cx: &gpui::App) -> [u8; 3] {
        if let Some(rgb) = self.runtime.background {
            return rgb;
        }
        let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
        [term.r, term.g, term.b]
    }

    fn set_hsv_from_rgb(&mut self, rgb: [u8; 3]) {
        let (h, s, v) = rgb_to_hsv(Rgb::new(rgb[0], rgb[1], rgb[2]));
        if s > f32::EPSILON && v > f32::EPSILON {
            self.bg_picker_hsv = (h, s, v);
        } else {
            self.bg_picker_hsv = (self.bg_picker_hsv.0, s, v);
        }
    }

    fn write_hex_field(&mut self, hex: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.bg_hex_syncing = true;
        self.bg_hex_input.update(cx, |input, cx| input.set_value(hex.to_owned(), window, cx));
        self.bg_hex_syncing = false;
    }

    fn persist_background_rgb(
        &mut self,
        rgb: [u8; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hex = format_hex_rgb(rgb);
        self.write_hex_field(&hex, window, cx);
        self.persist(&[("background", hex)], cx);
    }

    fn commit_background_rgb(&mut self, rgb: [u8; 3], window: &mut Window, cx: &mut Context<Self>) {
        self.set_hsv_from_rgb(rgb);
        self.persist_background_rgb(rgb, window, cx);
    }

    fn commit_hsv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (h, s, v) = self.bg_picker_hsv;
        let color = hsv_to_rgb(h, s, v);
        self.persist_background_rgb([color.r, color.g, color.b], window, cx);
    }

    pub(super) fn sync_background_color_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rgb = self.effective_background_rgb(cx);
        self.set_hsv_from_rgb(rgb);
        if !self.bg_hex_focused {
            self.write_hex_field(&format_hex_rgb(rgb), window, cx);
        }
    }

    pub(super) fn close_background_picker(&mut self, cx: &mut Context<Self>) {
        if !self.bg_picker_open {
            return;
        }
        self.bg_picker_open = false;
        self.bg_picker_drag = None;
        cx.notify();
    }

    fn toggle_background_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bg_picker_open {
            self.close_background_picker(cx);
            return;
        }
        let rgb = self.effective_background_rgb(cx);
        self.set_hsv_from_rgb(rgb);
        self.write_hex_field(&format_hex_rgb(rgb), window, cx);
        self.bg_picker_open = true;
        cx.notify();
    }

    pub(super) fn on_bg_hex_event(
        &mut self,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Focus => {
                self.bg_hex_focused = true;
                cx.notify();
            },
            InputEvent::Blur => {
                self.bg_hex_focused = false;
                cx.notify();
            },
            InputEvent::Change => {
                if self.bg_hex_syncing {
                    return;
                }
                let value = self.bg_hex_input.read(cx).value().to_string();
                if let Some(rgb) = parse_hex_rgb(&value) {
                    self.set_hsv_from_rgb(rgb);
                    self.persist(&[("background", format_hex_rgb(rgb))], cx);
                }
            },
            _ => {},
        }
    }

    pub(super) fn apply_bg_pointer(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(part) = self.bg_picker_drag else { return };
        match part {
            BgPickerPart::Sv => {
                let Some(bounds) = self.bg_sv_bounds else { return };
                let w = f32::from(bounds.size.width).max(1.0);
                let h = f32::from(bounds.size.height).max(1.0);
                let x = f32::from(position.x - bounds.origin.x);
                let y = f32::from(position.y - bounds.origin.y);
                self.bg_picker_hsv.1 = (x / w).clamp(0.0, 1.0);
                self.bg_picker_hsv.2 = (1.0 - y / h).clamp(0.0, 1.0);
            },
            BgPickerPart::Hue => {
                let Some(bounds) = self.bg_hue_bounds else { return };
                let w = f32::from(bounds.size.width).max(1.0);
                let x = f32::from(position.x - bounds.origin.x);
                self.bg_picker_hsv.0 = (x / w).clamp(0.0, 1.0) * 360.0;
            },
        }
        self.commit_hsv(window, cx);
    }

    pub(super) fn begin_bg_picker_drag(
        &mut self,
        part: BgPickerPart,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bg_picker_drag = Some(part);
        self.apply_bg_pointer(event.position, window, cx);
    }

    pub(super) fn on_bg_picker_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.bg_picker_drag.is_some() {
            self.apply_bg_pointer(event.position, window, cx);
        }
    }

    pub(super) fn finish_bg_picker_drag(&mut self, cx: &mut Context<Self>) {
        if self.bg_picker_drag.take().is_some() {
            cx.notify();
        }
    }

    /// 闭态 220×32 combobox + 开态右缘对齐浮层（行下 +6px）。
    pub(super) fn background_color_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let custom = self.runtime.background.is_some();
        let rgb = self.effective_background_rgb(cx);
        let hex = format_hex_rgb(rgb);
        let label: SharedString =
            if custom { hex.clone().into() } else { format!("主题默认  {hex}").into() };
        let open = self.bg_picker_open;
        let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let accent = super::rgb_hsla(sk.accent.r, sk.accent.g, sk.accent.b);
        let ink_dim = super::rgb_hsla(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b);
        let hairline = cx.theme().border;
        let surface = super::rgb_hsla(sk.panel.r, sk.panel.g, sk.panel.b);
        let hover = gpui::Rgba {
            r: f32::from(sk.hover.r) / 255.0,
            g: f32::from(sk.hover.g) / 255.0,
            b: f32::from(sk.hover.b) / 255.0,
            a: (f32::from(sk.hover.a) / 255.0).max(0.35),
        };
        let chip = super::rgb_hsla(rgb[0], rgb[1], rgb[2]);
        let picker = cx.entity().downgrade();
        let control = div()
            .id("background-color-combo")
            .relative()
            .w(px(COMBOBOX_W))
            .h(px(COMBOBOX_H))
            .flex_shrink_0()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(hairline)
            .bg(surface)
            .when(open, |el| el.bg(hover))
            .hover(|el| el.bg(hover))
            .cursor_pointer()
            .overflow_hidden()
            .child(
                div()
                    .h_full()
                    .px(px(12.0))
                    .pr(px(28.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .size(px(CHIP))
                            .flex_shrink_0()
                            .rounded(px(radius::CHIP))
                            .border_1()
                            .border_color(hairline)
                            .bg(chip),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(accent).child(label)),
            )
            .child(
                div().absolute().right(px(8.0)).top_0().bottom_0().flex().items_center().child(
                    Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                        .xsmall()
                        .text_color(ink_dim),
                ),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |pane, _| {
                            pane.bg_picker_trigger_bounds = Some(bounds);
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_background_picker(window, cx);
            }));

        let panel = if open { Some(self.background_picker_panel(cx)) } else { None };
        let trigger_bounds = self.bg_picker_trigger_bounds;
        let row = self.row_with_reset(
            "背景色",
            custom,
            |this, window, cx| {
                this.persist(&[("background", String::new())], cx);
                this.sync_background_color_picker(window, cx);
            },
            control,
            cx,
        );
        div().relative().w_full().flex_shrink_0().child(row).when_some(
            panel.zip(trigger_bounds),
            |anchor, (panel, trigger_bounds)| {
                anchor.child(
                    deferred(
                        anchored()
                            .anchor(gpui::Corner::TopRight)
                            .position(trigger_bounds.bottom_right())
                            .offset(gpui::point(px(0.0), px(6.0)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel),
                    )
                    .with_priority(2),
                )
            },
        )
    }

    fn background_picker_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let (h0, s0, v0) = self.bg_picker_hsv;
        let (pw, _) = popup_size();
        let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let hairline = cx.theme().border;
        let accent = super::rgb_hsla(sk.accent.r, sk.accent.g, sk.accent.b);
        let ink_dim = super::rgb_hsla(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b);
        let focused = self.bg_hex_focused;
        let hex_border = if focused {
            accent
        } else {
            gpui::Rgba {
                r: f32::from(sk.ink_dim.r) / 255.0,
                g: f32::from(sk.ink_dim.g) / 255.0,
                b: f32::from(sk.ink_dim.b) / 255.0,
                a: 120.0 / 255.0,
            }
            .into()
        };

        let picker = cx.entity().downgrade();
        let sv = div()
            .w_full()
            .h(px(SV_H))
            .rounded(px(radius::CHIP))
            .overflow_hidden()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_bg_picker_drag(BgPickerPart::Sv, event, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |pane, _| pane.bg_sv_bounds = Some(bounds));
                    },
                    move |bounds, _, window, _| {
                        paint_sv(window, bounds, h0, s0, v0);
                    },
                )
                .size_full(),
            );

        let picker = cx.entity().downgrade();
        let hue = div()
            .w_full()
            .h(px(HUE_H))
            .rounded(px(radius::CHIP))
            .overflow_hidden()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_bg_picker_drag(BgPickerPart::Hue, event, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |pane, _| pane.bg_hue_bounds = Some(bounds));
                    },
                    move |bounds, _, window, _| {
                        paint_hue(window, bounds, h0);
                    },
                )
                .size_full(),
            );

        let swatches = (0..BACKGROUND_SWATCHES.len()).fold(
            div()
                .flex()
                .flex_wrap()
                .gap(px(GAP))
                .w(px(COLS as f32 * CELL + (COLS - 1) as f32 * GAP)),
            |grid, index| {
                let color = BACKGROUND_SWATCHES[index];
                let rgb = [color.r, color.g, color.b];
                let selected = self.runtime.background == Some(rgb);
                grid.child(
                    div()
                        .id(SharedString::from(format!("bg-swatch-{index}")))
                        .size(px(CELL))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(if selected { accent } else { hairline })
                        .when(selected, |cell| {
                            cell.shadow(vec![gpui::BoxShadow {
                                color: accent,
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(0.0),
                                spread_radius: px(2.0),
                            }])
                        })
                        .bg(super::rgb_hsla(color.r, color.g, color.b))
                        .cursor_pointer()
                        .hover(|cell| cell.border_color(ink_dim))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.commit_background_rgb(rgb, window, cx);
                        })),
                )
            },
        );

        v_flex()
            .w(px(pw))
            .p(px(PAD))
            .gap(px(GAP))
            .popover_style(cx)
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.on_bg_picker_move(event, window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_bg_picker_drag(cx);
                }),
            )
            .child(sv)
            .child(hue)
            .child(swatches)
            .child(
                div()
                    .h(px(HEX_H))
                    .w_full()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(hex_border)
                    .overflow_hidden()
                    .child(Input::new(&self.bg_hex_input)),
            )
    }
}

fn paint_quad(
    window: &mut Window,
    origin: Point<Pixels>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    color: gpui::Hsla,
) {
    window.paint_quad(
        fill(
            Bounds::new(
                point(origin.x + px(x), origin.y + px(y)),
                size(px(w.max(0.0)), px(h.max(0.0))),
            ),
            color,
        )
        .corner_radii(px(r.max(0.0))),
    );
}

fn rgb_paint(color: Rgb) -> gpui::Hsla {
    gpui::Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: 1.0,
    }
    .into()
}

fn paint_sv(window: &mut Window, bounds: Bounds<Pixels>, h0: f32, s0: f32, v0: f32) {
    let origin = bounds.origin;
    let svw = f32::from(bounds.size.width);
    let svh = f32::from(bounds.size.height);
    let cell_w = svw / SV_COLS as f32;
    let cell_h = svh / SV_ROWS as f32;
    for row in 0..SV_ROWS {
        for col in 0..SV_COLS {
            let sat = (col as f32 + 0.5) / SV_COLS as f32;
            let val = 1.0 - (row as f32 + 0.5) / SV_ROWS as f32;
            let c = hsv_to_rgb(h0, sat, val);
            paint_quad(
                window,
                origin,
                col as f32 * cell_w,
                row as f32 * cell_h,
                cell_w + 0.5,
                cell_h + 0.5,
                0.0,
                rgb_paint(c),
            );
        }
    }
    let dot_x = s0.clamp(0.0, 1.0) * svw;
    let dot_y = (1.0 - v0.clamp(0.0, 1.0)) * svh;
    let dot_r = 6.0;
    paint_quad(
        window,
        origin,
        dot_x - dot_r,
        dot_y - dot_r,
        dot_r * 2.0,
        dot_r * 2.0,
        dot_r,
        gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }.into(),
    );
    paint_quad(
        window,
        origin,
        dot_x - dot_r + 1.5,
        dot_y - dot_r + 1.5,
        (dot_r - 1.5) * 2.0,
        (dot_r - 1.5) * 2.0,
        dot_r - 1.5,
        gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 200.0 / 255.0 }.into(),
    );
    let picked = hsv_to_rgb(h0, s0, v0);
    paint_quad(
        window,
        origin,
        dot_x - dot_r + 3.0,
        dot_y - dot_r + 3.0,
        (dot_r - 3.0) * 2.0,
        (dot_r - 3.0) * 2.0,
        dot_r - 3.0,
        rgb_paint(picked),
    );
}

fn paint_hue(window: &mut Window, bounds: Bounds<Pixels>, h0: f32) {
    let origin = bounds.origin;
    let huw = f32::from(bounds.size.width);
    let huh = f32::from(bounds.size.height);
    let hue_w = huw / HUE_STEPS as f32;
    for step in 0..HUE_STEPS {
        let hue = (step as f32 + 0.5) / HUE_STEPS as f32 * 360.0;
        let c = hsv_to_rgb(hue, 1.0, 1.0);
        paint_quad(window, origin, step as f32 * hue_w, 0.0, hue_w + 0.5, huh, 0.0, rgb_paint(c));
    }
    let cursor_x = (h0.rem_euclid(360.0) / 360.0) * huw;
    let cursor_outer = 4.0;
    let cursor_inner = 2.0;
    paint_quad(
        window,
        origin,
        cursor_x - cursor_outer * 0.5,
        -cursor_inner,
        cursor_outer,
        huh + cursor_inner * 2.0,
        cursor_outer * 0.5,
        gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }.into(),
    );
    paint_quad(
        window,
        origin,
        cursor_x - cursor_inner * 0.5,
        -cursor_inner * 0.5,
        cursor_inner,
        huh + cursor_inner,
        cursor_inner * 0.5,
        gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 180.0 / 255.0 }.into(),
    );
}
