//! Dedicated quick-window lifecycle and explicit recall of regular windows.

use super::*;

/// Animation coordinates must never be mistaken for a user's window placement.
pub(in crate::gpui_shell::workspace) fn quick_terminal_bounds_changed(
    id: u64,
    window: &mut Window,
    cx: &mut App,
) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic, IsZoomed};
    let Some(quick) = cx.global_mut::<WindowRegistry>().quick_terminal.as_mut() else { return };
    if quick.runtime_window_id != id || !quick.target_visible || quick.motion.is_active() {
        return;
    }
    let hwnd = quick.native_hwnd as *mut core::ffi::c_void;
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // Ignore minimization/maximization; only a normal user-resized window becomes the preference.
    if unsafe { IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 || GetWindowRect(hwnd, &mut rect) == 0 }
    {
        return;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let resized = quick.geometry.width != width || quick.geometry.height != height;
    quick.geometry = super::super::quick_terminal::QuickTerminalGeometry {
        x: rect.left,
        y: rect.top,
        width,
        height,
    };
    if resized {
        let scale = window.scale_factor().max(1.0);
        cx.global_mut::<WindowRegistry>().quick_size_dirty =
            nebula_settings::QuickTerminalSize::new(width as f32 / scale, height as f32 / scale);
    }
}

pub(super) fn persist_quick_size(cx: &mut App) {
    let Some(size) = cx.global::<WindowRegistry>().quick_size_dirty else { return };
    match nebula_settings::persist_keys(&[
        ("quick_terminal_width", size.width.to_string()),
        ("quick_terminal_height", size.height.to_string()),
    ]) {
        Ok(()) => cx.global_mut::<WindowRegistry>().quick_size_dirty = None,
        Err(error) => log::warn!("could not save quick terminal size: {error}"),
    }
}

/// 快速终端是独立窗口，不占用普通工作区的 MRU、runtime 路由或 session 恢复槽。
/// 三态仍与旧壳一致：隐藏时显示并聚焦；可见但在后台时只聚焦；只有可见且
/// 已在前台时才向上收起。
#[cfg(windows)]
pub(crate) fn toggle_quick_terminal_window(cx: &mut App) {
    prune_entries(cx);
    if nebula_settings::RuntimeSettings::load().quick_terminal_mode
        == nebula_settings::QuickTerminalMode::Existing
    {
        if let Some(entry) = entries_by_mru(cx).into_iter().next() {
            focus_entry(&entry, None, cx);
        } else {
            let _ = open_workspace_window(
                cx,
                WorkspaceStartup::NewTerminal { cwd: None },
                None,
                None,
                true,
                WindowRole::Regular,
            );
        }
        return;
    }
    let quick = cx.global::<WindowRegistry>().quick_terminal.as_ref().map(|quick| {
        (
            quick.handle,
            quick.native_hwnd,
            quick.geometry,
            quick.target_visible,
            quick.motion.is_active(),
        )
    });
    let Some((handle, hwnd, geometry, target_visible, motion_active)) = quick else {
        open_quick_terminal_window(cx);
        return;
    };

    let is_active = handle.update(cx, |_, window, _| window.is_window_active()).unwrap_or(false);
    if target_visible && !is_active {
        // 用户从其它应用按热键时是召回，不是把仍可见的快速终端反向隐藏。
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }

    if target_visible {
        let registry = cx.global_mut::<WindowRegistry>();
        let Some(quick) = registry.quick_terminal.as_mut() else { return };
        quick.motion_clock.reset();
        quick.target_visible = false;
        quick.motion.animate_role(1.0, MotionRole::Exit, MotionPolicy::Full);
    } else {
        // 对齐旧壳：完整隐藏后从屏幕外的 1.0 位置重新开始；若用户在退场
        // 中反向切换，则保留当前进度，不跳回起点。
        if !motion_active {
            if !super::super::quick_terminal::slide_native_window(hwnd, geometry, 1.0) {
                log::warn!("quick terminal could not prepare its hidden position");
                let _ = handle.update(cx, |_, window, _| window.remove_window());
                cx.global_mut::<WindowRegistry>().quick_terminal = None;
                return;
            }
            super::super::quick_terminal::show_native_window(hwnd);
        }
        // 显式热键显示与旧壳 `focus_window` 相同，激活发生在动画开始前。
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        let registry = cx.global_mut::<WindowRegistry>();
        let Some(quick) = registry.quick_terminal.as_mut() else { return };
        quick.motion_clock.reset();
        if !motion_active {
            quick.motion.snap_to(1.0);
        }
        quick.target_visible = true;
        quick.motion.animate_role(0.0, MotionRole::Enter, MotionPolicy::Full);
    }
    start_quick_terminal_animation(cx);
}

#[cfg(windows)]
fn quick_terminal_anchor_hwnd(cx: &mut App) -> isize {
    entries_by_mru(cx).into_iter().next().map_or(0, |entry| entry.native_hwnd)
}

#[cfg(windows)]
pub(super) fn quick_terminal_anchor_display(
    cx: &mut App,
) -> Option<(gpui::DisplayId, Bounds<gpui::Pixels>)> {
    let entry = entries_by_mru(cx).into_iter().next()?;
    entry
        .handle
        .update(cx, |_, window, cx| {
            window.display(cx).map(|display| (display.id(), display.bounds()))
        })
        .ok()
        .flatten()
}

#[cfg(windows)]
fn open_quick_terminal_window(cx: &mut App) {
    persist_quick_size(cx);
    let anchor_hwnd = quick_terminal_anchor_hwnd(cx);
    let Some(geometry) = super::super::quick_terminal::native_geometry(
        anchor_hwnd,
        nebula_settings::RuntimeSettings::load().quick_terminal_size,
    ) else {
        log::warn!("quick terminal could not resolve a target monitor");
        return;
    };
    let opened = open_workspace_window(
        cx,
        WorkspaceStartup::NewTerminal { cwd: None },
        None,
        None,
        false,
        WindowRole::QuickTerminal,
    );
    let Ok((runtime_window_id, _)) = opened else {
        log::warn!("quick terminal window creation failed: {}", opened.unwrap_err());
        return;
    };
    let Some(entry) = cx
        .global::<WindowRegistry>()
        .entries
        .iter()
        .find(|entry| entry.runtime_window_id == runtime_window_id)
        .cloned()
    else {
        log::warn!("quick terminal window was not registered after creation");
        return;
    };

    // 首次显示前在屏幕外完成唯一一次 native 尺寸同步。窗口此时已经使用
    // QuickTerminal 的 GPUI bounds 构造，且 workspace 不再排队普通网格
    // resize；WM_SIZE/ResizeBuffers 会在用户看见任何像素之前完成。
    if !super::super::quick_terminal::configure_native_window(
        entry.native_hwnd,
        geometry,
        1.0,
        true,
    ) {
        log::warn!("quick terminal initial native configuration failed");
        let _ = entry.handle.update(cx, |_, window, _| window.remove_window());
        return;
    }

    let mut motion = Tween::new(1.0);
    motion.animate_role(0.0, MotionRole::Enter, MotionPolicy::Full);
    cx.global_mut::<WindowRegistry>().quick_terminal = Some(QuickTerminalWindow {
        runtime_window_id,
        handle: entry.handle,
        native_hwnd: entry.native_hwnd,
        geometry,
        target_visible: true,
        motion,
        motion_clock: MotionClock::default(),
        animation_generation: 0,
    });
    let _ = entry.handle.update(cx, |_, window, _| window.activate_window());
    start_quick_terminal_animation(cx);
}

#[cfg(windows)]
fn start_quick_terminal_animation(cx: &mut App) {
    let (generation, handle) = {
        let Some(quick) = cx.global_mut::<WindowRegistry>().quick_terminal.as_mut() else {
            return;
        };
        quick.animation_generation = quick.animation_generation.wrapping_add(1);
        (quick.animation_generation, quick.handle)
    };
    let _ = handle.update(cx, move |_, window, _| {
        window.on_next_frame(move |window, cx| {
            quick_terminal_animation_frame(generation, window, cx);
        });
    });
}

#[cfg(windows)]
fn quick_terminal_animation_frame(generation: u64, window: &mut Window, cx: &mut App) {
    if quick_terminal_animation_tick(generation, window, cx) {
        window.on_next_frame(move |window, cx| {
            quick_terminal_animation_frame(generation, window, cx);
        });
    }
}

#[cfg(windows)]
fn quick_terminal_animation_tick(generation: u64, window: &mut Window, cx: &mut App) -> bool {
    prune_entries(cx);
    let frame = {
        let Some(quick) = cx.global_mut::<WindowRegistry>().quick_terminal.as_mut() else {
            return false;
        };
        if quick.animation_generation != generation {
            return false;
        }
        let active = quick.motion.step(quick.motion_clock.tick());
        let frame =
            (quick.native_hwnd, quick.geometry, quick.motion.value(), quick.target_visible, active);
        frame
    };
    let (hwnd, geometry, hidden, target_visible, active) = frame;
    if !super::super::quick_terminal::slide_native_window(hwnd, geometry, hidden) {
        log::warn!("quick terminal native positioning failed");
        window.remove_window();
        cx.global_mut::<WindowRegistry>().quick_terminal = None;
        return false;
    }
    if !active && !target_visible {
        super::super::quick_terminal::hide_native_window(hwnd);
    }
    active
}
