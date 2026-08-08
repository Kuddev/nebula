//! Toast 的生命周期：谁在场、活多久、什么时候淡出。
//!
//! 视觉配方全在 [`ui::toast`](crate::display::ui::toast)，这里只推进状态。
//!
//! # 为什么这条提示不该用消息栏
//!
//! 消息栏那条横幅会一直占着窗口底部整整一行，直到用户找到并点掉它。这个代价
//! 只在「用户必须做点什么」时才划算：配置写错了、断路器拦下了会话恢复。而
//! 「已恢复 3 个标签」这类**已经结束的事实**没有任何待办动作，却同样要求用户
//! 去点一个他根本没注意到的关闭按钮，于是横幅就一直挂在那儿。这类信息归
//! toast：说完就走，不占布局，不接鼠标。
//!
//! # 允许它自动消失的前提
//!
//! 每条 toast 同时写一行 `log::info!`。toast 不承载唯一副本——错过那几秒的
//! 用户仍然能在日志里查到，否则「自动消失」就是在丢信息。

use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use crate::display::Display;
use crate::display::ui::toast as recipe;
use crate::motion::{Easing, MotionPolicy, Tween};

pub use recipe::ToastKind;

/// 完全可见的停留时长（不含出入场）。4.2s：够从容读完一句中文长句，又短到
/// 用户不会开始琢磨"它怎么还不走"。
const HOLD: Duration = Duration::from_millis(4200);
const FADE_IN: Duration = Duration::from_millis(140);
const FADE_OUT: Duration = Duration::from_millis(240);
/// Repeating the same explicit action in a short burst should refresh the
/// user's context, not create a stack of identical cards. This is deliberately
/// a duplicate-only gate: distinct copy results and unrelated notifications
/// remain visible independently.
const DUPLICATE_COOLDOWN: Duration = Duration::from_millis(600);
/// 同屏最多叠三条，再多就挤掉最旧的。toast 是提示不是日志。
const MAX_VISIBLE: usize = 3;

#[derive(Debug)]
pub(super) struct Toast {
    text: String,
    kind: ToastKind,
    born: Instant,
    progress: Tween,
    /// 已进入淡出段。用标志位而不是每帧比时间，是为了让 `animate_to` 只被调用
    /// 一次——重复下发会把补间起点一直重置在当前值上，结果永远淡不完。
    leaving: bool,
}

impl Toast {
    fn new(text: String, kind: ToastKind) -> Self {
        let mut progress = Tween::new(0.0);
        progress.animate_to(1.0, FADE_IN, Easing::SwiftOut, MotionPolicy::Full);
        Self { text, kind, born: Instant::now(), progress, leaving: false }
    }
}

impl Display {
    /// 弹一条自动消失的提示。
    ///
    /// 同时落一行日志：这是 toast 被允许自动消失的前提，见模块文档。
    ///
    /// 立即请求一次重绘——调用点通常在启动或某个操作的收尾处，那之后窗口可能
    /// 长时间没有任何重绘理由，提示就不会有第一帧。
    pub fn push_toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        if self
            .nebula_toasts
            .last()
            .is_some_and(|last| last.text == text && last.born.elapsed() < DUPLICATE_COOLDOWN)
        {
            return;
        }
        log::info!("toast [{kind:?}]: {text}");
        self.nebula_toasts.push(Toast::new(text, kind));
        while self.nebula_toasts.len() > MAX_VISIBLE {
            self.nebula_toasts.remove(0);
        }
        self.window.request_redraw();
    }
}

/// 推进并绘制整叠提示。最新的一条在最下，旧的往上排。
pub(super) fn draw(d: &mut Display) {
    if d.nebula_toasts.is_empty() {
        return;
    }

    let frame = d.nebula_ui_anims.frame();
    for toast in &mut d.nebula_toasts {
        toast.progress.step(frame);
        if !toast.leaving && toast.born.elapsed() >= HOLD {
            toast.progress.animate_to(0.0, FADE_OUT, Easing::EaseInQuad, MotionPolicy::Full);
            toast.leaving = true;
        }
    }
    d.nebula_toasts.retain(|toast| !toast.leaving || toast.progress.is_active());
    // 这一帧本身就是「已经没有提示」的那一帧（present 每帧全量重画），清空后
    // 直接收工，不必再约下一帧。
    if d.nebula_toasts.is_empty() {
        return;
    }

    let size = d.ui_size_info();
    let scale = d.window.scale_factor as f32;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let viewport = (size.width(), size.height());
    let sk = d.nebula_theme.skin();
    let capacity = recipe::text_capacity(viewport.0, cell_w, scale);

    // 先把这一帧要画的摘出来：绘制要同时可变借用 renderer 与 glyph_cache，
    // 不能再攥着 toast 列表。最多三条短字符串，无所谓。
    let items: Vec<(String, ToastKind, f32)> = d
        .nebula_toasts
        .iter()
        .rev()
        .map(|toast| {
            (fit_cols(&toast.text, capacity), toast.kind, toast.progress.value().clamp(0.0, 1.0))
        })
        .collect();

    for (index, (text, kind, progress)) in items.iter().enumerate() {
        if *progress <= 0.004 {
            continue;
        }
        let rect =
            recipe::toast_rect(viewport, index, text_cols(text), cell_w, cell_h, scale, *progress);

        let mut quads = Vec::new();
        recipe::push_toast(&mut quads, rect, viewport, *kind, scale, &sk, *progress);
        d.renderer.draw_ui(&size, &quads);

        let (text_x, text_y) = recipe::text_origin(rect, cell_h, scale);
        let ink = recipe::ink(&sk, *progress);
        d.renderer.draw_chrome_text(&size, text_x, text_y, ink, text, &mut d.glyph_cache);
    }

    // 保活：提示全靠自己的时钟退场，中间没有任何输入事件来推动帧循环。
    d.window.request_redraw();
}

fn text_cols(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(1).max(1)).sum()
}

/// 按列宽裁剪并补省略号。按显示宽度累加而不是数字符：一个汉字占两列，数字符
/// 会让中文提示超出卡片。
fn fit_cols(text: &str, cols: usize) -> String {
    if text_cols(text) <= cols {
        return text.to_owned();
    }
    let budget = cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(1).max(1);
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cols_counts_cjk_as_two_columns() {
        assert_eq!(fit_cols("已恢复", 6), "已恢复");
        // 6 列预算减去省略号那一列 = 5 列，只放得下两个汉字。
        assert_eq!(fit_cols("已恢复三个标签", 6), "已恢…");
    }

    #[test]
    fn fit_cols_keeps_short_text_untouched() {
        assert_eq!(fit_cols("ok", 10), "ok");
    }
}
