//! 快速终端全局热键。
//!
//! 旧壳把它做成一扇独立的、从屏幕上缘滑入的窗口（`event.rs::toggle_quick_terminal`）。
//! GPUI 壳这一版只做「叫出来 / 收起去」的核心语义，作用于最近活跃的工作区窗口：
//! 独立滑入窗口要连带处理几何、多屏、DPI 和动画调度，规模远超「让热键能用」这件事，
//! 那属于另一个任务。
//!
//! 缺的一直是**系统注册**这一层：`settings_pane/keymap.rs` 能捕获并持久化组合键，
//! 但 GPUI 壳从未调用 `GlobalHotKeyManager::register`，所以用户设了键、按下去没反应。
//!
//! 为什么用轮询而不是阻塞接收：`global_hotkey` 的事件走进程级 channel，而 GPUI 主线程
//! 不能阻塞。这里沿用仓库既有的 `start_ai_hook_pump` / `start_agent_screen_watchdog`
//! 同一套「timer 让出 + `try_recv` 排空」节奏，不引入第三种调度风格。

use std::time::Duration;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::{App, Context, Global};

use super::NebulaWorkspace;

/// 热键事件的排空节奏。80ms 是「按下到窗口出现」的可接受上限，且与 ai-hook 泵同量级。
const POLL_INTERVAL: Duration = Duration::from_millis(80);

/// 每多少次轮询回读一次设置里的组合键（约 2 秒）。
///
/// 设置页改键后要立刻生效，但它的持久化路径（`settings_pane/keymap.rs::persist`）没有
/// 面向进程级单例的通知通道，而 GPUI 的内存全局 `config::Settings` 里也不存这个键。
/// 与其为一个低频动作新加一条事件链，这里在后台按固定节拍回读——它不在渲染路径上，
/// 每次只读一个不到 1KB 的设置文件。
const RESYNC_EVERY: u32 = 25;

/// 当前注册状态。`GlobalHotKeyManager` 必须活到进程结束：它的 `Drop` 会向系统注销热键。
struct QuickTerminalHotkey {
    manager: GlobalHotKeyManager,
    /// `None` = 组合键合法但系统拒绝注册（通常是被别的应用占用）。
    hotkey: Option<HotKey>,
    /// 已注册的原始字符串，用来判断设置是否变过。
    combo: String,
}

impl Global for QuickTerminalHotkey {}

impl NebulaWorkspace {
    /// 注册快速终端热键并启动事件泵。只应由初始窗口调用一次。
    pub(super) fn start_quick_terminal_hotkey(cx: &mut Context<Self>) {
        if cx.has_global::<QuickTerminalHotkey>() {
            return;
        }
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                // 非致命：热键不可用时终端其余功能完好，不值得弹提示打扰用户。
                log::warn!("quick terminal disabled: global hotkey init failed: {err}");
                return;
            },
        };
        let combo = current_combo();
        let hotkey = register(&manager, &combo);
        cx.set_global(QuickTerminalHotkey { manager, hotkey, combo });

        let executor = cx.background_executor().clone();
        cx.spawn(async move |_this, cx| {
            let mut ticks: u32 = 0;
            loop {
                executor.timer(POLL_INTERVAL).await;
                ticks = ticks.wrapping_add(1);
                let resync = ticks % RESYNC_EVERY == 0;
                let pressed = cx.update(|cx| {
                    if resync {
                        resync_combo(cx);
                    }
                    drain_pressed(cx)
                });
                if pressed {
                    cx.update(super::windowing::toggle_quick_terminal_window);
                }
            }
        })
        .detach();
    }
}

/// 设置里持久化的组合键；缺失或非法时回落到与旧壳同一个默认值。
fn current_combo() -> String {
    let stored = nebula_settings::RuntimeSettings::load().quick_terminal_hotkey;
    if stored.trim().is_empty() {
        crate::display::keymap::DEFAULT_QUICK_TERMINAL_HOTKEY.to_owned()
    } else {
        stored
    }
}

/// 注册一个组合键。返回 `None` 表示解析或注册失败——两者都不影响其余功能。
fn register(manager: &GlobalHotKeyManager, combo: &str) -> Option<HotKey> {
    let hotkey = match combo.parse::<HotKey>() {
        Ok(hotkey) => hotkey,
        Err(err) => {
            log::warn!("quick terminal hotkey {combo:?} is not a valid combo: {err}");
            return None;
        },
    };
    match manager.register(hotkey) {
        Ok(()) => Some(hotkey),
        Err(err) => {
            // 开发期常见：上一个实例被硬杀、Drop 没跑，键还被系统占着。
            log::debug!("quick terminal hotkey {combo:?} not registered: {err}");
            None
        },
    }
}

/// 设置改过就换键。先注册新键再注销旧键：系统拒绝新键时旧键仍然可用，
/// 不会出现「改了一个冲突的键，结果连原来那个也没了」。
fn resync_combo(cx: &mut App) {
    let latest = current_combo();
    let state = cx.global::<QuickTerminalHotkey>();
    if state.combo == latest && state.hotkey.is_some() {
        return;
    }
    let state = cx.global_mut::<QuickTerminalHotkey>();
    let replacement = register(&state.manager, &latest);
    if replacement.is_some() {
        if let Some(previous) = state.hotkey.take() {
            let _ = state.manager.unregister(previous);
        }
        state.hotkey = replacement;
    }
    state.combo = latest;
}

/// 排空事件队列，返回本轮是否收到过按下。
///
/// 必须整队排空而不是只看第一条：连按会攒下多条，留在队列里会在后续轮次里
/// 反复触发切换，看起来像窗口自己闪。同一轮的多次按下合并成一次切换。
fn drain_pressed(cx: &mut App) -> bool {
    let Some(id) = cx.global::<QuickTerminalHotkey>().hotkey.map(|hotkey| hotkey.id()) else {
        return false;
    };
    let mut pressed = false;
    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
        if event.id == id && event.state == HotKeyState::Pressed {
            pressed = true;
        }
    }
    pressed
}
