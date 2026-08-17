//! 旧壳设置控件的 GPUI 形态。
//!
//! - [`NebulaSwitch`]：液态胶囊开关。几何对照 `push_toggle`；四通道动画
//!   对照 `SettingsToggleAnim`（position 400ms `LiquidToggle` 且不夹到
//!   0/1，stretch 250ms `CssStandard`，color/hover 300ms `CssEase`）。
//!   组件库 `Switch` 是 0.15s 线性、蓝底白点，禁止混用。
//! - [`NebulaButton`]：设置行动作按钮。几何对照 `row_action_rect` / HTML
//!   `.bt`，hover 色过渡对照 `.bt { transition: all .13s }`。组件库 `Button`
//!   的 `.hover()` / `.active()` 是瞬时换色，没有这段动效。

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, App, Bounds, ClickEvent, ElementId, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderImage, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, ease_in_out, fill, point, px,
    size,
};
use image::Frame;

use crate::display::ui::widgets::ToggleMotion;
use crate::motion::{Easing, MotionClock, MotionPolicy, Tween};
use crate::renderer::ui::Rgba;

/// Shell 的彩色品牌图标（`extra/shell-icons` 的 PNG）预缩放成与物理像素
/// 一一对应的纹理。设置页的默认 Shell 下拉与新建终端的 Shell 选择弹窗
/// 共用同一份资产和同一条缩放路径——把 128px 原图交给 GPUI 每帧缩小会糊
/// 边，两处各写一遍缩放又迟早缩出两种观感。
///
/// `logical_size` 是控件里图标的逻辑边长，`scale_factor` 是窗口 DPI 缩放。
/// 返回 `None` 表示该 id 没有品牌资产，调用方保留自己的字形回落。
pub fn shell_brand_image(
    id: &str,
    logical_size: f32,
    scale_factor: f32,
) -> Option<Arc<RenderImage>> {
    let bytes = crate::shell_detect::color_icon_png(id)?;
    let source = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = source.dimensions();
    let target_size = (logical_size * scale_factor).round().max(1.0) as u32;
    let (mut rgba, width, height) =
        crate::display::prepare_ai_logo_texture(source.as_raw(), width, height, target_size);
    // GPUI RenderImage 使用 BGRA；预缩放必须在 RGBA 上完成，换序放在最后，
    // 否则 Lanczos 会把红蓝通道按错误语义混合。
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let rgba = image::RgbaImage::from_raw(width, height, rgba)?;
    Some(Arc::new(RenderImage::new([Frame::new(rgba)])))
}

const TRACK_W: f32 = 48.0;
const TRACK_H: f32 = 26.0;
const KNOB: f32 = 20.0;
const INSET: f32 = 2.0;
const TRAVEL: f32 = 24.0;
const STRETCH: f32 = 8.0;
const POSITION_MS: Duration = Duration::from_millis(400);
const STRETCH_MS: Duration = Duration::from_millis(250);
const COLOR_MS: Duration = Duration::from_millis(300);

/// 与旧壳 `display/mod.rs::SettingsToggleAnim` 同一套四通道。
/// `position` 不夹到 0/1：`Easing::LiquidToggle` 的过冲就是灵动来源。
struct SwitchAnim {
    clock: MotionClock,
    position: Tween,
    stretch: Tween,
    color: Tween,
    hover: Tween,
}

impl SwitchAnim {
    fn new(on: bool) -> Self {
        let value = if on { 1.0 } else { 0.0 };
        Self {
            clock: MotionClock::default(),
            position: Tween::new(value),
            stretch: Tween::new(0.0),
            color: Tween::new(value),
            hover: Tween::new(0.0),
        }
    }

    fn step(&mut self, on: bool, pressed: bool, hovered: bool) -> ToggleMotion {
        let position = if on { if pressed { 16.0 / 24.0 } else { 1.0 } } else { 0.0 };
        let color = if on { 1.0 } else { 0.0 };
        let stretch = if pressed { 1.0 } else { 0.0 };
        let hover = if hovered { 1.0 } else { 0.0 };
        if (self.position.target() - position).abs() > f32::EPSILON {
            self.position.animate_to(
                position,
                POSITION_MS,
                Easing::LiquidToggle,
                MotionPolicy::Full,
            );
        }
        if (self.stretch.target() - stretch).abs() > f32::EPSILON {
            self.stretch.animate_to(stretch, STRETCH_MS, Easing::CssStandard, MotionPolicy::Full);
        }
        if (self.color.target() - color).abs() > f32::EPSILON {
            self.color.animate_to(color, COLOR_MS, Easing::CssEase, MotionPolicy::Full);
        }
        if (self.hover.target() - hover).abs() > f32::EPSILON {
            self.hover.animate_to(hover, COLOR_MS, Easing::CssEase, MotionPolicy::Full);
        }
        let frame = self.clock.tick();
        self.position.step(frame);
        self.stretch.step(frame);
        self.color.step(frame);
        self.hover.step(frame);
        self.value()
    }

    fn value(&self) -> ToggleMotion {
        ToggleMotion {
            position: self.position.value(),
            stretch: self.stretch.value().clamp(0.0, 1.0),
            color: self.color.value().clamp(0.0, 1.0),
            hover: self.hover.value().clamp(0.0, 1.0),
        }
    }

    fn animating(&self, on: bool, pressed: bool, hovered: bool) -> bool {
        let position = if on { if pressed { 16.0 / 24.0 } else { 1.0 } } else { 0.0 };
        let color = if on { 1.0 } else { 0.0 };
        let stretch = if pressed { 1.0 } else { 0.0 };
        let hover = if hovered { 1.0 } else { 0.0 };
        [
            (self.position, position),
            (self.stretch, stretch),
            (self.color, color),
            (self.hover, hover),
        ]
        .into_iter()
        .any(|(tween, target)| tween.is_active() || (tween.value() - target).abs() > 0.004)
    }
}

/// 旧壳设置行的液态胶囊开关。
///
/// 几何对照 `push_toggle`：胶囊 48×26 圆角 13，knob 20×20、inset 2，
/// `kx = inset + 24*position`，`knob_w = 20 + 8*stretch`。四通道走
/// [`crate::motion::Tween`] + `LiquidToggle` / `CssEase` / `CssStandard`，
/// 不是组件库 `Switch`，也不是单次 0.3s ease_in_out。
#[derive(IntoElement)]
pub struct NebulaSwitch {
    key: SharedString,
    checked: bool,
    disabled: bool,
    on_click: Option<Rc<dyn Fn(&bool, &mut Window, &mut App)>>,
}

impl NebulaSwitch {
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self { key: key.into(), checked: false, disabled: false, on_click: None }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    #[allow(dead_code)]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&bool, &mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// u8 通道混色（旧壳 `widgets::mix_color` 的语义），返回 gpui 颜色。
fn mix(start: Rgba, end: Rgba, t: f32) -> gpui::Rgba {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) / 255.0;
    gpui::Rgba {
        r: channel(start.r, end.r),
        g: channel(start.g, end.g),
        b: channel(start.b, end.b),
        a: channel(start.a, end.a),
    }
}

/// `overlay`（带 alpha）盖到 `base`（不透明）上（旧壳 `surface::over` 语义）。
fn over(overlay: Rgba, base: gpui::Rgba) -> gpui::Rgba {
    let alpha = f32::from(overlay.a) / 255.0;
    let channel = |o: u8, b: f32| b + (f32::from(o) / 255.0 - b) * alpha;
    gpui::Rgba {
        r: channel(overlay.r, base.r),
        g: channel(overlay.g, base.g),
        b: channel(overlay.b, base.b),
        a: base.a,
    }
}

fn over_rgba(overlay: Rgba, base: Rgba) -> Rgba {
    let alpha = f32::from(overlay.a) / 255.0;
    let channel = |o: u8, b: u8| {
        (f32::from(b) + (f32::from(o) - f32::from(b)) * alpha).round().clamp(0.0, 255.0) as u8
    };
    Rgba::new(
        channel(overlay.r, base.r),
        channel(overlay.g, base.g),
        channel(overlay.b, base.b),
        base.a,
    )
}

fn to_hsla(color: gpui::Rgba) -> Hsla {
    color.into()
}

impl RenderOnce for NebulaSwitch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let checked = self.checked;
        let disabled = self.disabled;

        let pressed_id = ElementId::Name(format!("nebula-switch-pressed-{}", self.key).into());
        let pressed = window.use_keyed_state(pressed_id, cx, |_, _| false);
        let hover_id = ElementId::Name(format!("nebula-switch-hover-{}", self.key).into());
        let hovered = window.use_keyed_state(hover_id, cx, |_, _| false);
        let anim_id = ElementId::Name(format!("nebula-switch-anim-{}", self.key).into());
        let anim = window.use_keyed_state(anim_id, cx, |_, _| SwitchAnim::new(checked));

        let is_pressed = !disabled && *pressed.read(cx);
        let is_hovered = !disabled && *hovered.read(cx);
        let motion = anim.update(cx, |anim, _| anim.step(checked, is_pressed, is_hovered));
        if !disabled && anim.read(cx).animating(checked, is_pressed, is_hovered) {
            window.request_animation_frame();
        }

        let hover_ink = Rgba::new(sk.icon_hover.r, sk.icon_hover.g, sk.icon_hover.b, 255);
        let border = mix(sk.toggle_border_off, sk.toggle_border_on, motion.color);
        let border = mix(
            Rgba::new(
                (border.r * 255.0).round() as u8,
                (border.g * 255.0).round() as u8,
                (border.b * 255.0).round() as u8,
                (border.a * 255.0).round() as u8,
            ),
            hover_ink,
            motion.hover * 0.14,
        );
        let track = mix(sk.toggle_track_off, sk.toggle_track_on, motion.color);
        let track = over(
            Rgba::new(sk.icon_hover.r, sk.icon_hover.g, sk.icon_hover.b, (motion.hover * 12.0) as u8),
            track,
        );
        let knob = mix(sk.knob_off, sk.knob_on, motion.color);
        let knob_w = KNOB + STRETCH * motion.stretch;
        let kx = INSET + TRAVEL * motion.position;
        let ky = (TRACK_H - KNOB) / 2.0;
        let border_hsla = to_hsla(border);
        let track_hsla = to_hsla(track);
        let knob_hsla = to_hsla(knob);

        let face = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let mut quad = |x: f32, y: f32, w: f32, h: f32, r: f32, bg: Hsla| {
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(px(ox + x), px(oy + y)),
                                size(px(w.max(0.0)), px(h.max(0.0))),
                            ),
                            bg,
                        )
                        .corner_radii(px(r.max(0.0))),
                    );
                };
                // 发丝描边：整胶囊 border，再 inset 1px 铺轨道（旧壳 push_stroke）。
                quad(0.0, 0.0, TRACK_W, TRACK_H, TRACK_H * 0.5, border_hsla);
                quad(1.0, 1.0, TRACK_W - 2.0, TRACK_H - 2.0, (TRACK_H - 2.0) * 0.5, track_hsla);
                quad(kx, ky, knob_w, KNOB, KNOB * 0.5, knob_hsla);
            },
        )
        .w(px(TRACK_W))
        .h(px(TRACK_H));

        div()
            .id(ElementId::Name(format!("nebula-switch-{}", self.key).into()))
            .flex_shrink_0()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .rounded(px(TRACK_H / 2.0))
            .when(disabled, |wrapper| wrapper.opacity(0.45).cursor_default())
            .when(!disabled, |wrapper| wrapper.cursor_pointer())
            .child(face)
            .when(!disabled, |wrapper| {
                let pressed_down = pressed.clone();
                let pressed_up = pressed;
                let hover_state = hovered;
                wrapper
                    .on_hover(move |hovered, _, cx| {
                        let _ = hover_state.update(cx, |state, _| *state = *hovered);
                    })
                    .on_mouse_down(gpui::MouseButton::Left, {
                        let on_click = self.on_click.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = pressed_down.update(cx, |state, _| *state = true);
                            if let Some(on_click) = on_click.as_ref() {
                                on_click(&!checked, window, cx);
                            }
                        }
                    })
                    .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
                        let _ = pressed_up.update(cx, |state, _| *state = false);
                    })
            })
    }
}

const BUTTON_H: f32 = 30.0;
const BUTTON_PX: f32 = 13.0;
const BUTTON_DURATION: Duration = Duration::from_millis(130);

/// 旧壳设置行的文字按钮。
///
/// 对照两处权威，不对照组件库 `Button`：
/// - 几何：旧壳 `row_action_rect`（高 30）+ HTML `.bt`（`padding: 0 13px`，
///   圆角走 chrome `UI_CORNER_RADIUS_LOGICAL`）；
/// - 动效：HTML `.bt { transition: all .13s }`，hover 只插值底/边/字色，
///   不改尺寸（旧壳 `push_button_frame`：「hover 只改亮度，不改尺寸」）；
/// - 配色：默认态 = `action_button` 的 `sk.surface` → `sk.hover`；描边态 =
///   `push_outline_button` 的发丝 + panel；主按钮 = accent 实心再按亮度抬。
#[derive(IntoElement)]
pub struct NebulaButton {
    key: SharedString,
    label: SharedString,
    kind: NebulaButtonKind,
    disabled: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NebulaButtonKind {
    Default,
    Primary,
    Outline,
    Danger,
}

impl NebulaButton {
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: SharedString::default(),
            kind: NebulaButtonKind::Default,
            disabled: false,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub fn primary(mut self) -> Self {
        self.kind = NebulaButtonKind::Primary;
        self
    }

    pub fn outline(mut self) -> Self {
        self.kind = NebulaButtonKind::Outline;
        self
    }

    pub fn danger(mut self) -> Self {
        self.kind = NebulaButtonKind::Danger;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

fn rgb_rgba(c: crate::display::color::Rgb) -> Rgba {
    Rgba::new(c.r, c.g, c.b, 255)
}

fn mix_rgba(start: Rgba, end: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Rgba::new(
        channel(start.r, end.r),
        channel(start.g, end.g),
        channel(start.b, end.b),
        channel(start.a, end.a),
    )
}

fn button_palette(
    kind: NebulaButtonKind,
    sk: &crate::display::ui::theme::Skin,
) -> (Rgba, Rgba, Rgba, Rgba, Rgba, Rgba) {
    // (bg0, bg1, border0, border1, fg0, fg1)
    let ink = rgb_rgba(sk.ink);
    let ink_dim = rgb_rgba(sk.ink_dim);
    let transparent = Rgba::new(0, 0, 0, 0);
    let hover_line = mix_rgba(sk.hairline, rgb_rgba(sk.icon_hover), 0.14);
    match kind {
        NebulaButtonKind::Default => (sk.surface, sk.hover, sk.hairline, hover_line, ink_dim, ink),
        NebulaButtonKind::Outline => (sk.panel, sk.hover, sk.hairline, hover_line, ink_dim, ink),
        NebulaButtonKind::Primary => {
            let rest = Rgba::opaque(sk.accent);
            let wash =
                if sk.is_light { Rgba::new(0, 0, 0, 20) } else { Rgba::new(255, 255, 255, 26) };
            let hover = over_rgba(wash, rest);
            let on_accent = rgb_rgba(sk.ink_on_accent);
            (rest, hover, transparent, transparent, on_accent, on_accent)
        },
        NebulaButtonKind::Danger => {
            let wash = Rgba::new(sk.danger.r, sk.danger.g, sk.danger.b, 28);
            let line = Rgba::new(sk.danger.r, sk.danger.g, sk.danger.b, 90);
            (transparent, wash, transparent, line, sk.danger, sk.danger)
        },
    }
}

impl RenderOnce for NebulaButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let disabled = self.disabled;
        let kind = self.kind;
        let (bg0, bg1, border0, border1, fg0, fg1) = button_palette(kind, &sk);
        let stroked = !matches!(kind, NebulaButtonKind::Primary);

        let hover_id = ElementId::Name(format!("nebula-btn-hover-{}", self.key).into());
        let hover = window.use_keyed_state(hover_id, cx, |_, _| false);
        let hovered = !disabled && *hover.read(cx);

        let settled_id = ElementId::Name(format!("nebula-btn-settled-{}", self.key).into());
        let settled = window.use_keyed_state(settled_id, cx, |_, _| false);
        let animate = !disabled && *settled.read(cx) != hovered;
        if animate {
            let settled = settled.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(BUTTON_DURATION).await;
                let _ = settled.update(cx, |state, _| *state = hovered);
            })
            .detach();
        }

        let bg_of = move |t: f32| mix(bg0, bg1, t);
        let border_of = move |t: f32| mix(border0, border1, t);
        let fg_of = move |t: f32| mix(fg0, fg1, t);

        let face = div()
            .h(px(BUTTON_H))
            .px(px(BUTTON_PX))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
            .when(stroked, |face| face.border_1())
            .child(self.label);
        let face = if animate {
            face.with_animation(
                ElementId::NamedInteger(
                    format!("nebula-btn-face-{}", self.key).into(),
                    hovered as u64,
                ),
                Animation::new(BUTTON_DURATION).with_easing(ease_in_out),
                move |face, delta| {
                    let t = if hovered { delta } else { 1.0 - delta };
                    face.bg(bg_of(t)).border_color(border_of(t)).text_color(fg_of(t))
                },
            )
            .into_any_element()
        } else {
            let t = if hovered { 1.0 } else { 0.0 };
            face.bg(bg_of(t)).border_color(border_of(t)).text_color(fg_of(t)).into_any_element()
        };

        div()
            .id(ElementId::Name(format!("nebula-btn-{}", self.key).into()))
            .flex_shrink_0()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
            .when(disabled, |wrapper| wrapper.opacity(0.45).cursor_default())
            .when(!disabled, |wrapper| wrapper.cursor_pointer())
            .child(face)
            .when(!disabled, |wrapper| {
                let hover_enter = hover.clone();
                wrapper
                    .on_hover(move |hovered, _, cx| {
                        let _ = hover_enter.update(cx, |state, _| *state = *hovered);
                    })
                    .when_some(self.on_click, |wrapper, on_click| {
                        wrapper.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            on_click(event, window, cx);
                        })
                    })
            })
    }
}
