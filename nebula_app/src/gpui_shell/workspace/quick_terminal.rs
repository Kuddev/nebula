//! 快速终端全局热键。
//!
//! 与旧壳保持同一产品合同：进程内只有一扇独立窗口，位于最近活跃显示器
//! 顶部、全宽、屏高 40%，显示/隐藏时从屏幕上缘滑入/滑出。窗口只隐藏不
//! 销毁，因此 PTY 和终端状态跨切换保留。
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

/// 原生窗口位移动画的采样节奏。只在 90-120ms 的进出场期间运行，不常驻占帧。
pub(super) const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);

/// 旧壳 `Display::configure_quick_terminal` 的几何合同。
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct QuickTerminalGeometry {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
}

#[cfg(windows)]
impl QuickTerminalGeometry {
    fn animated_y(self, hidden_fraction: f32) -> i32 {
        self.y - (self.height as f32 * hidden_fraction.clamp(0.0, 1.0)).round() as i32
    }
}

/// 以普通工作区 HWND 所在显示器为目标；没有锚点时 Win32 回退主显示器。
#[cfg(windows)]
pub(super) fn native_geometry(anchor_hwnd: isize) -> Option<QuickTerminalGeometry> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
        MonitorFromWindow,
    };

    let anchor = anchor_hwnd as *mut core::ffi::c_void;
    let fallback =
        if anchor.is_null() { MONITOR_DEFAULTTOPRIMARY } else { MONITOR_DEFAULTTONEAREST };
    // SAFETY: anchor 来自当前进程已注册的 GPUI 窗口；失效或为空时 API 按
    // fallback 选择主/最近显示器。失败统一返回 None，不解引用 HWND。
    let monitor = unsafe { MonitorFromWindow(anchor, fallback) };
    if monitor.is_null() {
        return None;
    }
    let zero = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: zero,
        rcWork: zero,
        dwFlags: 0,
    };
    // 旧壳使用 monitor.size/position，即完整屏幕而非扣掉任务栏的 work area。
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let width = (info.rcMonitor.right - info.rcMonitor.left).max(1);
    let monitor_height = (info.rcMonitor.bottom - info.rcMonitor.top).max(1);
    let height = ((monitor_height as f64) * 0.4).round().max(1.0) as i32;
    Some(QuickTerminalGeometry { x: info.rcMonitor.left, y: info.rcMonitor.top, width, height })
}

/// 每帧只移动 Y；创建或跨屏重显时才重设宽高，避免动画连续触发 PTY resize。
#[cfg(windows)]
pub(super) fn position_native_window(
    hwnd: isize,
    geometry: QuickTerminalGeometry,
    hidden_fraction: f32,
    resize: bool,
    show: bool,
) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos,
    };

    if hwnd == 0 {
        return false;
    }
    let mut flags = SWP_NOACTIVATE;
    if !resize {
        flags |= SWP_NOSIZE;
    }
    if show {
        flags |= SWP_SHOWWINDOW;
    }
    // SAFETY: HWND 由 GPUI 窗口创建回调发布；SetWindowPos 对已失效窗口安全
    // 失败。SWP_NOACTIVATE 保证动画帧本身不反复抢前台，显式热键另行激活。
    unsafe {
        SetWindowPos(
            hwnd as *mut core::ffi::c_void,
            HWND_TOPMOST,
            geometry.x,
            geometry.animated_y(hidden_fraction),
            geometry.width,
            geometry.height,
            flags,
        ) != 0
    }
}

#[cfg(windows)]
pub(super) fn hide_native_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    if hwnd != 0 {
        // SAFETY: HWND 由 GPUI 创建；窗口已关闭时 ShowWindow 只会安全失败。
        unsafe { ShowWindow(hwnd as *mut core::ffi::c_void, SW_HIDE) };
    }
}

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

#[cfg(all(test, windows))]
mod tests {
    use super::QuickTerminalGeometry;

    #[test]
    fn slide_geometry_handles_negative_monitor_origins() {
        let geometry = QuickTerminalGeometry { x: -1920, y: -200, width: 1920, height: 480 };
        assert_eq!(geometry.animated_y(0.0), -200);
        assert_eq!(geometry.animated_y(0.5), -440);
        assert_eq!(geometry.animated_y(1.0), -680);
        assert_eq!(geometry.animated_y(2.0), -680);
    }
}
