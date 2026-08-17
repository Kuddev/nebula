//! 旧壳设置控件的 GPUI 形态。
//!
//! - [`NebulaSwitch`]：液态胶囊开关。几何、配色与动画通道逐项对照旧壳
//!   `display/ui/widgets.rs::push_toggle`（HTML 参考的
//!   `transition: all 0.3s ease`）。组件库 `Switch` 是 0.15s 线性、蓝底白点
//!   的另一套语言，与旧壳中性胶囊在同一页面上会打架。
//! - [`NebulaButton`]：设置行动作按钮。几何对照 `row_action_rect` / HTML
//!   `.bt`，hover 色过渡对照 `.bt { transition: all .13s }`。组件库 `Button`
//!   的 `.hover()` / `.active()` 是瞬时换色，没有这段动效。

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, App, ClickEvent, ElementId, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderImage, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, ease_in_out, px,
};
use image::Frame;

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
const DURATION: Duration = Duration::from_millis(300);

/// 旧壳设置行的动画开关（液态胶囊）。
///
/// - 胶囊 48×26（圆角 13），knob 20×20、inset 2，行程 24px；
/// - 按住时 knob 拉伸 +8px（开态从右缘回弹，旧壳「liquid recoil」）；
/// - 状态切换 = 0.3s ease 同步过渡位置与颜色（border/track/knob 三通道）。
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
    Rgba::new(channel(overlay.r, base.r), channel(overlay.g, base.g), channel(overlay.b, base.b), base.a)
}

impl RenderOnce for NebulaSwitch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sk = crate::gpui_shell::theme::chrome_theme_resolved(cx).skin();
        let checked = self.checked;
        let disabled = self.disabled;

        // 动画驱动（组件库 Switch 同款结构）：keyed state 记录「上一次画的
        // 状态」，与本次 checked 不同才播 0.3s 过渡，结束后把状态落下来。
        let settled_id = ElementId::Name(format!("nebula-switch-settled-{}", self.key).into());
        let settled = window.use_keyed_state(settled_id, cx, |_, _| checked);
        let animate = !disabled && *settled.read(cx) != checked;
        if animate {
            let settled = settled.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(DURATION).await;
                let _ = settled.update(cx, |state, _| *state = checked);
            })
            .detach();
        }

        // 按住拉伸：knob +8px；开态按住时从右缘回弹（x 左移补偿）。
        let pressed_id = ElementId::Name(format!("nebula-switch-pressed-{}", self.key).into());
        let pressed = window.use_keyed_state(pressed_id, cx, |_, _| false);
        let is_pressed = !disabled && *pressed.read(cx);

        let hover_ink = Rgba::new(sk.icon_hover.r, sk.icon_hover.g, sk.icon_hover.b, 12);
        let track_of = move |t: f32| mix(sk.toggle_track_off, sk.toggle_track_on, t);
        let border_of = move |t: f32| mix(sk.toggle_border_off, sk.toggle_border_on, t);
        let knob_color_of = move |t: f32| mix(sk.knob_off, sk.knob_on, t);
        let knob_w = if is_pressed { KNOB + STRETCH } else { KNOB };
        let knob_x_of = move |t: f32| {
            let x = INSET + TRAVEL * t;
            // 开态按住：宽度向左生长，右缘保持贴住（旧壳 recoil 语义）。
            if is_pressed && t > 0.5 { x - STRETCH } else { x }
        };

        let knob = div()
            .absolute()
            .top(px((TRACK_H - 2.0 * INSET - KNOB) / 2.0))
            .w(px(knob_w))
            .h(px(KNOB))
            .rounded(px(KNOB / 2.0));
        let knob = if animate {
            knob.with_animation(
                ElementId::NamedInteger(
                    format!("nebula-switch-knob-{}", self.key).into(),
                    checked as u64,
                ),
                Animation::new(DURATION).with_easing(ease_in_out),
                move |knob, delta| {
                    let t = if checked { delta } else { 1.0 - delta };
                    knob.left(px(knob_x_of(t))).bg(knob_color_of(t))
                },
            )
            .into_any_element()
        } else {
            let t = if checked { 1.0 } else { 0.0 };
            knob.left(px(knob_x_of(t))).bg(knob_color_of(t)).into_any_element()
        };

        // 胶囊本体：动画帧由 with_animation 重染 bg/border；静止帧直接给
        // settled 色。命中与手势挂在外层 stateful div 上，动画期间照常可点。
        let track = div()
            .relative()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .rounded(px(TRACK_H / 2.0))
            .border_1()
            .child(knob);
        let track = if animate {
            track
                .with_animation(
                    ElementId::NamedInteger(
                        format!("nebula-switch-track-{}", self.key).into(),
                        checked as u64,
                    ),
                    Animation::new(DURATION).with_easing(ease_in_out),
                    move |track, delta| {
                        let t = if checked { delta } else { 1.0 - delta };
                        track.bg(track_of(t)).border_color(border_of(t))
                    },
                )
                .into_any_element()
        } else {
            let t = if checked { 1.0 } else { 0.0 };
            track
                .bg(track_of(t))
                .border_color(border_of(t))
                // hover 水洗（旧壳 border 提亮 + track 12/255 水洗的简化）。
                .hover(move |style| style.bg(over(hover_ink, track_of(t))))
                .into_any_element()
        };

        div()
            .id(ElementId::Name(format!("nebula-switch-{}", self.key).into()))
            .flex_shrink_0()
            .rounded(px(TRACK_H / 2.0))
            .cursor_pointer()
            .child(track)
            .when(!disabled, |wrapper| {
                let pressed_down = pressed.clone();
                let pressed_up = pressed;
                wrapper
                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = pressed_down.update(cx, |state, _| *state = true);
                    })
                    .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
                        let _ = pressed_up.update(cx, |state, _| *state = false);
                    })
                    .when_some(self.on_click, |wrapper, on_click| {
                        wrapper.on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            on_click(&!checked, window, cx);
                        })
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
        }
        NebulaButtonKind::Danger => {
            let wash = Rgba::new(sk.danger.r, sk.danger.g, sk.danger.b, 28);
            let line = Rgba::new(sk.danger.r, sk.danger.g, sk.danger.b, 90);
            (transparent, wash, transparent, line, sk.danger, sk.danger)
        }
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
            face.bg(bg_of(t))
                .border_color(border_of(t))
                .text_color(fg_of(t))
                .into_any_element()
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
