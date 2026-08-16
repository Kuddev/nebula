//! GPUI 形态的 SSH 连接卡片：状态机、阶段文案、几何常量全部直接复用
//! `display::ssh_connect`（两壳同一份语义），只有绘制层换成 GPUI 的
//! canvas/div。视觉合同（卡片结构、轨道三态节点、粒子流、Logs 折叠、
//! 失败即展开日志）逐条对齐旧壳 push_quads/draw_text。
//!
//! 与旧壳的已知等价替换：
//! - `UiQuad::gradient`（进度条渐变）→ 24 段实心（2px 高的轨道上不可辨）
//! - `UiQuad::glow`（粒子/节点外溢）→ 同心低 alpha 圆
//! - 等宽 cell 度量（`SizeInfo`）→ UI 基准字号（同一设置源）

use gpui::{
    AnyElement, Bounds, Context, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, canvas, div, fill, hsla, point,
    px, size,
};

use crate::display::ssh_connect::{self, SshConnectState, stage_labels, stage_message};
use crate::gpui_shell::prelude::*;

/// UiLanguage 由设置折算（两壳同源）。
pub(super) fn language() -> crate::display::UiLanguage {
    let runtime = nebula_settings::RuntimeSettings::load();
    crate::display::LanguagePreference::parse(runtime.language.settings_value())
        .unwrap_or_default()
        .resolved()
}

fn hsla_from_rgba(r: u8, g: u8, b: u8) -> Hsla {
    let (rf, gf, bf) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return hsla(0.0, 0.0, l, 1.0);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - rf).abs() < f32::EPSILON {
        (gf - bf) / d + (if gf < bf { 6.0 } else { 0.0 })
    } else if (max - gf).abs() < f32::EPSILON {
        (bf - rf) / d + 2.0
    } else {
        (rf - gf) / d + 4.0
    };
    hsla(h / 6.0, s, l, 1.0)
}

fn lerp_hsla(a: Hsla, b: Hsla, k: f32) -> Hsla {
    let k = k.clamp(0.0, 1.0);
    hsla(
        a.h + (b.h - a.h) * k,
        a.s + (b.s - a.s) * k,
        a.l + (b.l - a.l) * k,
        1.0,
    )
}

/// 品牌紫→青（旧壳 palette.edge_l/edge_r；浅色主题压暗 0.28）。
fn brand_pair(cx: &gpui::App) -> (Hsla, Hsla) {
    let chrome = crate::gpui_shell::theme::chrome_theme_resolved(cx);
    let sk = chrome.skin();
    let palette = chrome.palette();
    let darken = |v: crate::renderer::ui::Rgba| -> Hsla {
        let mut h = hsla_from_rgba(v.r, v.g, v.b);
        if sk.is_light {
            h.l *= 0.72;
            h.s *= 0.9;
        }
        h
    };
    (darken(palette.edge_l), darken(palette.edge_r))
}

/// 轨道（底轨 + 渐变填充 + 粒子 + 节点）的 canvas 绘制。坐标全部是
/// canvas 局部逻辑 px：rail 两端各让半个节点（旧 `rail_x0/x1` 相对卡片
/// 内容缘的同一条公式，pad 已由外层卡片 padding 吃掉）。
pub(super) fn rail_canvas(
    state: &SshConnectState,
    cx: &gpui::App,
    theme: &gpui_component::Theme,
) -> AnyElement {
    let (brand_l, brand_r) = brand_pair(cx);
    let light = !theme.is_dark();
    let danger = theme.danger;
    let accent = theme.link;
    let hairline = theme.border;
    let panel = theme.popover;
    let state = state.clone();

    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            let w = f32::from(bounds.size.width);
            let node_d = 10.0_f32;
            let x0 = node_d * 0.5;
            let x1 = (w - node_d * 0.5).max(x0 + 1.0);
            let ox = f32::from(bounds.origin.x);
            let oy = f32::from(bounds.origin.y);
            let cy = oy + f32::from(bounds.size.height) * 0.5;

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

            // 底轨
            let rail_h = 2.0;
            quad(x0, cy - rail_h * 0.5, x1 - x0, rail_h, rail_h * 0.5, hairline);

            let node_x = |i: usize| x0 + (x1 - x0) * i as f32 / 3.0;

            // 已完成段：品牌渐变拆 24 段实心。
            let fill_to = node_x(0) + (x1 - x0) * (state.fill_now() / 3.0);
            let fill_w = (fill_to - x0).max(0.0);
            if fill_w > 0.5 {
                if state.failed() {
                    quad(x0, cy - rail_h * 0.5, fill_w, rail_h, rail_h * 0.5, danger);
                } else {
                    let steps = 24;
                    let seg_w = fill_w / steps as f32;
                    for s in 0..steps {
                        let t1 = (s + 1) as f32 / steps as f32;
                        let color = lerp_hsla(brand_l, brand_r, t1);
                        quad(
                            x0 + fill_w * (s as f32 / steps as f32),
                            cy - rail_h * 0.5,
                            seg_w + 0.5,
                            rail_h,
                            rail_h * 0.5,
                            color,
                        );
                    }
                }
            }

            // 粒子（旧 PARTICLE_* 常量与 ease 曲线照搬）
            if !state.failed() {
                let seg = state.stage_index().min(2);
                let from = node_x(seg);
                let to = node_x(seg + 1);
                let span = (x1 - x0).max(1.0);
                let ease = |s: f32| 0.35 * s + 0.65 * (s * s * (3.0 - 2.0 * s));
                for i in 0..5usize {
                    let head =
                        (state.phase_now() + [0.0f32, 0.17, 0.31, 0.52, 0.63][i]).fract();
                    for k in (0..=5usize).rev() {
                        let t = head - k as f32 * 0.032;
                        if !(0.0..=1.0).contains(&t) {
                            continue;
                        }
                        let x = from + (to - from) * ease(t);
                        let fade = 1.0 - k as f32 / (5.0 + 1.2);
                        let edge = (t / 0.14).min((1.0 - t) / 0.14).min(1.0);
                        let alpha = fade * fade * edge * if light { 0.55 } else { 0.9 };
                        if alpha < 0.012 {
                            continue;
                        }
                        let color =
                            lerp_hsla(brand_l, brand_r, ((x - x0) / span).clamp(0.0, 1.0));
                        let cr = if k == 0 { 1.55 } else { 1.3 * fade + 0.2 };
                        let mut col = color;
                        col.a = alpha * if k == 0 { 1.0 } else { 0.78 };
                        if !light {
                            let rad = if k == 0 { 5.4 } else { 4.4 * fade + 1.2 };
                            let mut g = color;
                            g.a = alpha * 0.22;
                            quad(x - rad, cy - rad, rad * 2.0, rad * 2.0, rad, g);
                        }
                        quad(x - cr, cy - cr, cr * 2.0, cr * 2.0, cr, col);
                    }
                }
            }

            // 节点三态
            let done_upto = state.fill_now().floor() as usize;
            let active = state.stage_index();
            for i in 0..4usize {
                let d = node_d;
                let x = node_x(i) - d * 0.5;
                let y = cy - d * 0.5;
                if state.failed() && i == active {
                    quad(x, y, d, d, d * 0.5, danger);
                } else if i <= done_upto && i < active {
                    quad(x, y, d, d, d * 0.5, brand_r);
                } else if i == active {
                    if !light {
                        let mut g = accent;
                        g.a = 0.45;
                        quad(x - 3.0, y - 3.0, d + 6.0, d + 6.0, (d + 6.0) * 0.5, g);
                    }
                    quad(x, y, d, d, d * 0.5, accent);
                } else {
                    quad(x, y, d, d, d * 0.5, hairline);
                    let inset = 1.5;
                    quad(
                        x + inset,
                        y + inset,
                        d - inset * 2.0,
                        d - inset * 2.0,
                        (d - inset * 2.0) * 0.5,
                        panel,
                    );
                }
            }
        },
    )
    .h(px(10.0))
    .w_full()
    .into_any_element()
}

/// 整张卡片的 GPUI 组装。文字与按钮是真元素（命中/hover 由 GPUI 负责），
/// 轨道与粒子走 [`rail_canvas`]。垂直堆叠逐项对应旧 `layout()` 的内容
/// 度量（icon 40 → rail → caption → status → detail → logs → buttons）。
pub(super) fn overlay(
    state: &SshConnectState,
    cx: &Context<super::view::TerminalView>,
) -> AnyElement {
    use gpui::prelude::FluentBuilder as _;

    let theme = cx.theme();
    let lang = language();
    let ui_px = cx
        .try_global::<crate::gpui_shell::config::Settings>()
        .map(|settings| settings.font_size_px)
        .unwrap_or(15.0);
    let family: SharedString = cx
        .try_global::<crate::gpui_shell::config::Settings>()
        .map(|settings| settings.font_family.clone())
        .unwrap_or_else(|| "Cascadia Mono".to_owned())
        .into();
    let ink_strong = theme.sidebar_accent_foreground;
    let ink = theme.foreground;
    let ink_dim = theme.muted_foreground;
    let ink_faint = crate::gpui_shell::theme::faint_ink(cx);
    let danger = theme.danger;
    let panel = theme.popover;
    let card_bg = theme.group_box;
    let hairline = theme.border;
    let accent = theme.link;

    // ── 身份行：机架图标 + 两行文字 + Logs 按钮 ──
    let rack_ink = ink_dim;
    let rack_panel = panel;
    let rack = canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            // 服务器机架：两层机箱 + 指示灯（旧 push_quads 的矢量复刻）。
            let cxp = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
            let cyp = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
            let mut quad = |x: f32, y: f32, w: f32, h: f32, r: f32, bg: Hsla| {
                window.paint_quad(
                    fill(
                        Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0)))),
                        bg,
                    )
                    .corner_radii(px(r.max(0.0))),
                );
            };
            let (bw, bh, gap) = (18.0f32, 7.5f32, 3.0f32);
            let (stroke, r) = (1.4f32, 2.0f32);
            for row in [-1.0f32, 1.0] {
                let by = cyp + row * (bh + gap) * 0.5 - bh * 0.5;
                let bx = cxp - bw * 0.5;
                quad(bx, by, bw, bh, r, rack_ink);
                quad(
                    bx + stroke,
                    by + stroke,
                    bw - stroke * 2.0,
                    bh - stroke * 2.0,
                    (r - stroke).max(0.0),
                    rack_panel,
                );
                let d = 2.4;
                quad(
                    bx + stroke + 2.2,
                    by + bh * 0.5 - d * 0.5,
                    d,
                    d,
                    d * 0.5,
                    rack_ink,
                );
            }
        },
    )
    .size(px(24.0))
    .into_any_element();

    let name: SharedString = ssh_connect::short_name(state.destination()).into();
    let meta: SharedString = format!("SSH · {}", state.destination()).into();
    let logs_target = cx.entity().downgrade();
    let logs_label: SharedString =
        format!("Logs  {}", if state.is_logs_open() { "⌃" } else { "⌄" }).into();
    let identity = h_flex()
        .gap(px(16.0))
        .items_center()
        .child(
            div()
                .size(px(40.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(hairline)
                .bg(card_bg)
                .flex()
                .items_center()
                .justify_center()
                .child(rack),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .font_family(family.clone())
                        .text_size(px(ui_px))
                        .text_color(ink_strong)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .font_family(family.clone())
                        .text_size(px(ui_px * 0.80))
                        .text_color(ink_dim)
                        .truncate()
                        .child(meta),
                ),
        )
        .child(
            // 可交互按钮统一走 gpui-component：命中、Hover、键盘焦点与
            // 主题 token 都由组件库负责，不在业务卡片里维护第二套按钮。
            Button::new("ssh-connect-logs")
                .label(logs_label)
                .outline()
                .small()
                .on_click(move |_, _, cx| {
                    if let Some(view) = logs_target.upgrade() {
                        view.update(cx, |this, cx| {
                            if let Some(state) = &mut this.ssh_connect {
                                state.toggle_logs();
                                cx.notify();
                            }
                        });
                    }
                }),
        );

    // ── 轨道 + 阶段标签（首尾贴齐轨道两端，中间以节点为中心）──
    let labels = stage_labels(lang);
    let active = state.stage_index();
    let caption_h = ui_px * 0.75;
    let rail_block = v_flex()
        .w_full()
        .gap(px(8.0))
        .mt(px(24.0))
        .child(rail_canvas(state, cx, &theme))
        .child(
            div().relative().h(px(caption_h)).children(
                labels
                    .iter()
                    .enumerate()
                    .map(|(i, label)| {
                        let ink_i = if state.failed() && i == active {
                            danger
                        } else if i == active {
                            ink
                        } else if i < active {
                            ink_dim
                        } else {
                            ink_faint
                        };
                        let text = div()
                            .font_family(family.clone())
                            .text_size(px(caption_h))
                            .text_color(ink_i)
                            .child(label.clone());
                        match i {
                            0 => div().absolute().left(px(0.0)).child(text).into_any_element(),
                            3 => div().absolute().right(px(0.0)).child(text).into_any_element(),
                            // 中间：零宽锚点 + 140px 居中槽（节点近似中心）
                            _ => div()
                                .absolute()
                                .left(gpui::relative(if i == 1 {
                                    0.328
                                } else {
                                    0.659
                                }))
                                .w(px(0.0))
                                .child(
                                    div()
                                        .w(px(140.0))
                                        .ml(px(-70.0))
                                        .flex()
                                        .justify_center()
                                        .child(text),
                                )
                                .into_any_element(),
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        );

    // ── 状态行 ──
    let (msg, msg_ink) = if state.failed() {
        (ssh_connect::failure_headline(lang), danger)
    } else {
        (stage_message(&state.stage(), lang), ink)
    };
    let status_row = h_flex()
        .w_full()
        .mt(px(24.0))
        .justify_between()
        .child(
            div()
                .font_family(family.clone())
                .text_size(px(ui_px))
                .text_color(msg_ink)
                .child(msg),
        )
        .when(!state.failed(), |row| {
            row.child(
                div()
                    .font_family(family.clone())
                    .text_size(px(ui_px))
                    .text_color(ink_faint)
                    .child(state.elapsed_text()),
            )
        });

    // ── 失败详情（两行，超出收省略号）──
    let per_line = 44usize;
    let detail = state.failure().map(|reason| {
        v_flex()
            .mt(px(8.0))
            .gap(px(ui_px * 0.28))
            .children(
                ssh_connect::wrap(reason, per_line)
                    .into_iter()
                    .take(2)
                    .map(|line| {
                        div()
                            .font_family(family.clone())
                            .text_size(px(ui_px * 0.80))
                            .text_color(ink_dim)
                            .child(ssh_connect::truncate_cols(&line, per_line))
                    })
                    .collect::<Vec<_>>(),
            )
    });

    // ── Logs 区（末尾 6 行，最新在最下）──
    let logs_area = state.is_logs_open().then(|| {
        let line_h = ui_px * 0.80;
        let logs = state.logs();
        let start = logs.len().saturating_sub(6);
        v_flex()
            .mt(px(24.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(hairline)
            .bg(card_bg)
            .p(px(12.0))
            .gap(px(2.0))
            .children(
                logs[start..]
                    .iter()
                    .map(|line| {
                        let color = if line.contains("error") { danger } else { ink_dim };
                        div()
                            .font_family(family.clone())
                            .text_size(px(line_h))
                            .text_color(color)
                            .truncate()
                            .child(line.clone())
                    })
                    .collect::<Vec<_>>(),
            )
    });

    // ── 底部按钮（旧裁定：没有重试；连接中=取消，失败=关闭=主按钮）──
    let action_label: SharedString = if state.failed() {
        lang.pick("关闭", "Close").into()
    } else {
        lang.pick("取消", "Cancel").into()
    };
    let failed = state.failed();
    let action_target = cx.entity().downgrade();
    let buttons = h_flex()
        .w_full()
        .mt(px(24.0))
        .justify_end()
        .child(
            Button::new("ssh-connect-action")
                .label(action_label)
                .small()
                .map(|button| if failed { button.primary() } else { button.outline() })
                .on_click(move |_, _, cx| {
                    if let Some(view) = action_target.upgrade() {
                        view.update(cx, |this, cx| {
                            // 取消与关闭是同一个动作（旧裁定）：这个 pane
                            // 除了这条连接没有别的内容。
                            this.ssh_connect = None;
                            cx.emit(super::view::TerminalViewEvent::RequestClose);
                            cx.notify();
                        });
                    }
                }),
        );

    // ── 组装 ──
    div()
        .id("ssh-connect-overlay")
        .absolute()
        .inset_0()
        .occlude()
        // 模态：卡片在场时吞掉一切点击（旧 covers 合同）
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        // 遮罩：pane 底色整块盖住（连接期间 grid 是空的，跟着重绘会闪）
        .child(div().absolute().inset_0().bg(panel))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .child(
                    v_flex()
                        .w(px(600.0))
                        .max_w(gpui::relative(1.0))
                        .p(px(24.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(hairline)
                        .bg(panel)
                        .shadow_md()
                        .child(identity)
                        .child(rail_block)
                        .child(status_row)
                        .when_some(detail, |card, d| card.child(d))
                        .when_some(logs_area, |card, d| card.child(d))
                        .child(buttons),
                ),
        )
        .into_any_element()
}
