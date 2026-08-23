//! GPUI 壳的提示三层（裁定见 `display/toast.rs` 模块文档）：
//!
//! - **toast**：已经结束的事实，没有待办动作（恢复成功、复制完成）。
//!   走组件库 `NotificationList`（`Root` 自带渲染层），5s 自动消失。
//! - **消息栏**：值得驻留、要用户看见的事（配置错误、断路器、AI 回合完成）。
//!   同一渲染层，停留 [`BANNER_TTL`] 后自动退场，带关闭按钮。**驻留不等于
//!   永不消失**——理由见那个常量的文档。
//! - **模态**：阻断性决策。调用点自建 `Dialog`，不经此模块。
//!
//! 判据是「有没有待办动作」而不是严重度。toast 允许自动消失的前提：每条
//! 同时落一行日志——toast 不承载唯一副本，错过那几秒的用户仍能在日志里
//! 查到。
//!
//! 与旧壳合同的已知差异（组件库行为）：停留 5s（旧壳 4.2s）、无同屏三条
//! 上限（5s 自动退场，溢出概率低）；重复冷却在本层补齐（600ms，同文本）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::notification::Notification;
use gpui_component::{Root, WindowExt as _};

use crate::gpui_shell::prelude::v_flex;

pub use crate::display::ToastKind;

/// 同一文本短时间连发只刷新语境，不叠一摞相同卡片（旧壳
/// `DUPLICATE_COOLDOWN` 同值同义；只挡逐字相同的文本）。
const DUPLICATE_COOLDOWN: Duration = Duration::from_millis(600);

/// 组件库 Notification 默认固定 `w_112`，短短一句也会铺成近半个窗口。
/// Nebula 的 toast 按内容收缩；过长消息到此上限后自然换行。
const TOAST_MIN_WIDTH: f32 = 240.0;
const TOAST_MAX_WIDTH: f32 = 440.0;

/// 驻留消息的兜底寿命。
///
/// 「不自动消失」本意是保证用户一定看见，实际效果是队列只增不减（用户
/// 08-23 报的「完成提示一直不消失」）：AI 回合完成、后台 tab 的 OSC 9 通知
/// 这类事件会持续产出，没人手动点掉就一直摞在右下角，而这些消息里根本没有
/// 待办动作可点。
///
/// 所以驻留改成「长而有限」：90s 远长于 toast 的 5s，够人从别的窗口回来看
/// 一眼；无人值守时也保证自己退场。手动点掉仍然立刻生效。
const BANNER_TTL: Duration = Duration::from_secs(90);

static LAST_TOAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn is_duplicate(text: &str) -> bool {
    let mut last = LAST_TOAST.lock().unwrap_or_else(|poison| poison.into_inner());
    let duplicate = last
        .as_ref()
        .is_some_and(|(prev, born)| prev == text && born.elapsed() < DUPLICATE_COOLDOWN);
    if !duplicate {
        *last = Some((text.to_owned(), Instant::now()));
    }
    duplicate
}

fn note(kind: ToastKind, text: String) -> Notification {
    let note = match kind {
        ToastKind::Info => Notification::info(text),
        ToastKind::Success => Notification::success(text),
        ToastKind::Warning => Notification::warning(text),
    };
    // `refine_style` 在组件自身的 `w_112` 之后应用，所以这里能可靠覆盖
    // 固定宽度；auto 让短消息收紧，max_w 给长路径/错误信息提供换行约束。
    note.w_auto().min_w(px(TOAST_MIN_WIDTH)).max_w(px(TOAST_MAX_WIDTH))
}

/// 窗口构造闭包里 `NebulaWorkspace::new` 早于外层 `Root::new` 返回；此时
/// 直接 push 会让组件库查找尚未安装的首层 Root 并 panic。启动期通知延后
/// 到当前 effect cycle 结束，正常运行期仍同步推送。
/// 允许少数业务通知复用 Nebula 的启动期 Root 守卫和右下角宿主；通知内容与
/// 动作仍由业务模块定义，避免把更新等领域状态塞进通用 toast 层。
pub(crate) fn push_notification(window: &mut Window, cx: &mut App, notification: Notification) {
    if window.root::<Root>().flatten().is_some() {
        window.push_notification(notification, cx);
    } else {
        window.defer(cx, move |window, cx| {
            if window.root::<Root>().flatten().is_some() {
                window.push_notification(notification, cx);
            } else {
                log::warn!("notification dropped because the window Root is unavailable");
            }
        });
    }
}

/// Nebula 的通知层定位：右下角固定 20px 安全边距，最新一条在最下，旧的
/// 向上堆叠。组件库默认 `NotificationList` 写死在右上角且拉满视口高度；
/// 这里继续复用它的 Notification 实体、生命周期、关闭和动画，只替换宿主
/// 的排列层，不复制组件内部状态机。
pub fn render_layer(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let root = window.root::<Root>()??;
    let list = root.read(cx).notification.clone();
    let mut items = list.read(cx).notifications();
    // 与旧壳同屏上限一致。保留队尾三条，顺序仍是旧→新，因此 v_flex
    // 底部锚定后最新项自然落在最下面。
    if items.len() > 3 {
        items.drain(..items.len() - 3);
    }
    if items.is_empty() {
        return None;
    }
    Some(
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_end()
            .justify_end()
            .p(px(20.0))
            // 用全视口透明宿主明确右下锚点；宿主不注册命中，只有实际
            // Notification 卡片会 occlude/接收鼠标，周围终端仍可正常点击。
            .child(v_flex().items_end().gap_2().children(items))
            .into_any_element(),
    )
}

/// 弹一条自动消失的提示（toast 层）。空文本与 600ms 内的重复文本静默丢弃。
pub fn toast(window: &mut Window, cx: &mut App, kind: ToastKind, text: impl Into<String>) {
    let text = text.into();
    if text.trim().is_empty() || is_duplicate(&text) {
        return;
    }
    log::info!("toast [{kind:?}]: {text}");
    push_notification(window, cx, note(kind, text));
}

/// 驻留一条消息（消息栏层）：停留远长于 toast，但**有上限**，见
/// [`BANNER_TTL`]。用于「值得驻留」的事——没什么值得驻留的请用 [`toast`]。
pub fn banner(window: &mut Window, cx: &mut App, kind: ToastKind, text: impl Into<String>) {
    let text = text.into();
    if text.trim().is_empty() {
        return;
    }
    log::warn!("banner [{kind:?}]: {text}");
    let note = note(kind, text).autohide(false);
    if window.root::<Root>().flatten().is_some() {
        window.push_notification(note, cx);
        schedule_banner_dismiss(window, cx);
    } else {
        window.defer(cx, move |window, cx| {
            if window.root::<Root>().flatten().is_some() {
                window.push_notification(note, cx);
                schedule_banner_dismiss(window, cx);
            } else {
                log::warn!("banner dropped because the window Root is unavailable");
            }
        });
    }
}

/// 给刚推入的驻留消息挂一个到点自尽的定时器。
///
/// 组件库把 autohide 做成 bool、5s 写死在 `NotificationList::push` 里，没有
/// 自定义时长的入口；为这个改 fork 要动 rev pin，不值得。换个办法：push 之后
/// 队尾那条就是我们刚推的，抓住它的**弱**引用，到点自己调 `dismiss`。用户
/// 提前点掉时弱引用已经失效，定时器什么也不做，不会去误关后来的消息。
fn schedule_banner_dismiss(window: &mut Window, cx: &mut App) {
    let Some(Some(root)) = window.root::<Root>() else { return };
    let list = root.read(cx).notification.clone();
    let Some(note) = list.read(cx).notifications().last().cloned() else { return };
    let note = note.downgrade();
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(BANNER_TTL).await;
            let _ = cx.update(|window, cx| {
                if let Some(note) = note.upgrade() {
                    note.update(cx, |note, cx| note.dismiss(window, cx));
                }
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_gate_blocks_only_rapid_identical_text() {
        // 进程级状态：用本测试独有的文本，别的测试并发跑也不会撞。
        assert!(!is_duplicate("toast-dup-gate-a"));
        assert!(is_duplicate("toast-dup-gate-a"));
        assert!(!is_duplicate("toast-dup-gate-b"), "不同文本不受冷却影响");
    }
}
