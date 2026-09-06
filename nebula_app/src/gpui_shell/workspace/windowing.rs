//! GPUI 进程级多窗口注册、启动路由与活体标签迁移。
//!
//! `RuntimeServer`、托盘和 AI hook 都是进程级资源；窗口只持有自己的
//! `NebulaWorkspace`。所有外部启动和 runtime 命令先在这里选择窗口，再把
//! 变更投递到对应 workspace，避免多个 receiver 竞争消费同一事件流。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Global, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, WeakEntity, Window,
    WindowBounds, WindowOptions, div, point, px, size,
};
use gpui_component::{ActiveTheme as _, Root, TitleBar};
use nebula_split::{SplitNav, SplitTree};
use serde_json::json;

use super::session_persistence::{SaveReason, SessionPersistence, combine_sessions};
use super::{NebulaWorkspace, TabMeta, WorkspaceTab, dock_tree};
use crate::gpui_shell::GpuiShellEvent;
#[cfg(windows)]
use crate::motion::{MotionClock, MotionPolicy, MotionRole, Tween};
use crate::runtime_api::{
    ApiError, RuntimeCommand, RuntimeDispatch, RuntimeSnapshot, RuntimeWindow,
};

/// 新窗口的首帧内容。只有进程的第一个窗口恢复全局 session；其它窗口必须
/// 明确创建一个新终端或暂时保持空白，不能把同一份 session 重放多次。
pub(crate) enum WorkspaceStartup {
    RestoreOrDefault,
    NewTerminal { cwd: Option<PathBuf> },
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowRole {
    Regular,
    #[cfg(windows)]
    QuickTerminal,
}

#[derive(Clone)]
struct WindowEntry {
    runtime_window_id: u64,
    handle: AnyWindowHandle,
    workspace: WeakEntity<NebulaWorkspace>,
    last_activated: u64,
    native_hwnd: isize,
    role: WindowRole,
}

#[cfg(windows)]
struct QuickTerminalWindow {
    runtime_window_id: u64,
    handle: AnyWindowHandle,
    native_hwnd: isize,
    geometry: super::quick_terminal::QuickTerminalGeometry,
    target_visible: bool,
    motion: Tween,
    motion_clock: MotionClock,
    animation_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeWindowPolicy {
    Preserve,
    CreateWithoutActivation,
    Focus,
}

/// Runtime 命令的窗口副作用合同；新增命令必须在这个唯一判据里明确归类。
///
/// | 命令 | show | activate |
/// | --- | --- | --- |
/// | `focus`（包括 orchestrate `Focus`） | 仅窗口已隐藏时恢复 | 是 |
/// | `new-window`、需要新窗的 `new-tab` | 创建可见窗口（`focus=false`） | 否 |
/// | `snapshot`、`read`、`procs`、`agent-read`、`exec` | 否 | 否 |
/// | `close-*`、`rename-tab`、`move-tab`、`split`、`zoom`、`resize` | 否 | 否 |
/// | `prompt`、`paste`、`send-key`、`run`、`agent-*`、现有窗 `new-tab` | 否 | 否 |
///
/// `describe`、`agents`、`wait`、`subscribe` 在 runtime server 内直接应答，
/// 不会进入此窗口分发器，因此天然不 show、不 activate。
fn runtime_window_policy(command: &RuntimeCommand) -> RuntimeWindowPolicy {
    match command {
        RuntimeCommand::Focus { .. } => RuntimeWindowPolicy::Focus,
        RuntimeCommand::NewWindow { .. } => RuntimeWindowPolicy::CreateWithoutActivation,
        RuntimeCommand::Snapshot
        | RuntimeCommand::CloseWindow { .. }
        | RuntimeCommand::NewTab { .. }
        | RuntimeCommand::CloseTab { .. }
        | RuntimeCommand::RenameTab { .. }
        | RuntimeCommand::MoveTab { .. }
        | RuntimeCommand::Split { .. }
        | RuntimeCommand::ClosePane { .. }
        | RuntimeCommand::ZoomPane { .. }
        | RuntimeCommand::ResizePane { .. }
        | RuntimeCommand::Prompt { .. }
        | RuntimeCommand::Paste { .. }
        | RuntimeCommand::ReadPane { .. }
        | RuntimeCommand::Procs { .. }
        | RuntimeCommand::SendKey { .. }
        | RuntimeCommand::Run { .. }
        | RuntimeCommand::Exec { .. }
        | RuntimeCommand::AgentStart { .. }
        | RuntimeCommand::AgentFork { .. }
        | RuntimeCommand::AgentPrompt { .. }
        | RuntimeCommand::AgentPaste { .. }
        | RuntimeCommand::AgentRead { .. } => RuntimeWindowPolicy::Preserve,
    }
}

pub(crate) struct WindowRegistry {
    next_window_id: u64,
    activation_sequence: u64,
    entries: Vec<WindowEntry>,
    runtime_hub: crate::runtime_api::RuntimeHub,
    session_persistence: SessionPersistence,
    #[cfg(windows)]
    quick_terminal: Option<QuickTerminalWindow>,
    _subscriptions: Vec<Subscription>,
}

impl Global for WindowRegistry {}

/// GPUI 原生 drag payload 由整个 App 共享。这里只携带稳定 pane id 和源实体，
/// 不把 `WorkspaceTab` 本体塞进 payload；真正的活体所有权只在 drop 事务中移动。
#[derive(Clone)]
pub(crate) struct CrossWindowTabDrag {
    source: WeakEntity<NebulaWorkspace>,
    source_window_id: u64,
    pane_id: u64,
    title: SharedString,
}

impl CrossWindowTabDrag {
    pub(crate) fn source_window_id(&self) -> u64 {
        self.source_window_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CrossWindowDropDestination {
    pub(crate) window_id: u64,
    pub(crate) dock: Option<SplitNav>,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeCursorTarget {
    root_hwnd: isize,
    client_x: i32,
    client_y: i32,
}

pub(crate) struct TabDragPreview {
    title: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(280.0))
            .px_3()
            .py_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_md()
            .child(self.title.clone())
    }
}

struct DetachedTerminalTab {
    tab: WorkspaceTab,
    meta: TabMeta,
    pane_ids: Vec<u64>,
    source_window_id: u64,
    source_handle: AnyWindowHandle,
    source_became_empty: bool,
}

pub(crate) fn initialize(cx: &mut App, runtime_hub: crate::runtime_api::RuntimeHub) {
    cx.set_global(WindowRegistry {
        next_window_id: 1,
        activation_sequence: 1,
        entries: Vec::new(),
        runtime_hub,
        session_persistence: SessionPersistence::default(),
        #[cfg(windows)]
        quick_terminal: None,
        _subscriptions: Vec::new(),
    });

    let quit_subscription = cx.on_app_quit(|cx| {
        save_combined_session(cx, true);
        async {}
    });
    let closed_subscription = cx.on_window_closed(|cx, _window_id| {
        prune_entries(cx);
        // 快速终端不能改变普通 session 的生命周期：最后一扇普通窗口关闭后，
        // 即使隐藏的 Quake 窗口仍存活，也不能再补写一份空的普通 session。
        if cx
            .global::<WindowRegistry>()
            .entries
            .iter()
            .any(|entry| entry.role == WindowRole::Regular)
        {
            save_combined_session(cx, false);
        }
    });
    cx.global_mut::<WindowRegistry>()
        ._subscriptions
        .extend([quit_subscription, closed_subscription]);

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            cx.update(autosave_tick);
        }
    })
    .detach();
}

pub(crate) fn open_initial_window(
    cx: &mut App,
    ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
    shell_events: std::sync::mpsc::Receiver<GpuiShellEvent>,
    initial_cwd: Option<PathBuf>,
) {
    let startup = match initial_cwd {
        Some(cwd) => WorkspaceStartup::NewTerminal { cwd: Some(cwd) },
        None => WorkspaceStartup::RestoreOrDefault,
    };
    open_workspace_window(
        cx,
        startup,
        Some(ai_events),
        Some(shell_events),
        true,
        WindowRole::Regular,
    )
    .expect("failed to open Nebula GPUI window");
}

fn workspace_window_options(cx: &mut App, focus: bool, role: WindowRole) -> WindowOptions {
    match role {
        WindowRole::Regular => {
            let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.0), px(540.0))),
                titlebar: Some(TitleBar::title_bar_options()),
                app_id: Some("nebula".to_owned()),
                window_background: crate::gpui_shell::wallpaper::initial_background_appearance(),
                focus,
                ..Default::default()
            }
        },
        #[cfg(windows)]
        WindowRole::QuickTerminal => {
            let (display_id, visible) = quick_terminal_anchor_display(cx)
                .map(|(id, bounds)| (Some(id), bounds))
                .or_else(|| {
                    cx.primary_display().map(|display| (Some(display.id()), display.bounds()))
                })
                .unwrap_or_else(|| (None, Bounds::centered(None, size(px(1080.0), px(720.0)), cx)));
            let height = px((f32::from(visible.size.height) * 0.4).round().max(1.0));
            let bounds = Bounds {
                origin: point(visible.origin.x, visible.origin.y - height),
                size: size(visible.size.width, height),
            };
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // 与旧壳一致：无系统标题栏，但保留 Nebula 自绘标题栏及三枚
                // 窗口按钮。TitleBar options 负责 Windows 客户区命中测试。
                titlebar: Some(TitleBar::title_bar_options()),
                app_id: Some("nebula-quick-terminal".to_owned()),
                window_background: crate::gpui_shell::wallpaper::initial_background_appearance(),
                focus: false,
                show: false,
                display_id,
                ..Default::default()
            }
        },
    }
}

fn allocate_window(cx: &mut App) -> (u64, crate::runtime_api::RuntimeHub) {
    let registry = cx.global_mut::<WindowRegistry>();
    let id = registry.next_window_id;
    registry.next_window_id = registry.next_window_id.saturating_add(1);
    (id, registry.runtime_hub.clone())
}

fn open_workspace_window(
    cx: &mut App,
    startup: WorkspaceStartup,
    ai_events: Option<std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>>,
    shell_events: Option<std::sync::mpsc::Receiver<GpuiShellEvent>>,
    focus: bool,
    role: WindowRole,
) -> gpui::Result<(u64, Entity<NebulaWorkspace>)> {
    let (runtime_window_id, runtime_hub) = allocate_window(cx);
    let options = workspace_window_options(cx, focus, role);
    let workspace_slot = Rc::new(RefCell::new(None));
    let hwnd_slot = Rc::new(RefCell::new(0isize));
    let workspace_out = workspace_slot.clone();
    let hwnd_out = hwnd_slot.clone();
    let handle = cx.open_window(options, move |window, cx| {
        window.set_window_title(crate::brand::NAME);
        *hwnd_out.borrow_mut() = native_hwnd(window).unwrap_or_default();
        #[cfg(windows)]
        crate::gpui_shell::set_native_window_icon(window);
        let workspace = cx.new(|cx| {
            NebulaWorkspace::new(
                window,
                ai_events,
                shell_events,
                runtime_window_id,
                runtime_hub,
                startup,
                role,
                cx,
            )
        });
        if runtime_window_id == 1
            && let Ok(path) = std::env::var("NEBULA_GPUI_OPEN_DOC")
            && !path.is_empty()
        {
            workspace.update(cx, |workspace, cx| {
                workspace.open_document_at_startup(path.into(), window, cx);
            });
        }
        *workspace_out.borrow_mut() = Some(workspace.clone());
        cx.new(|cx| Root::new(workspace, window, cx))
    })?;
    let workspace =
        workspace_slot.borrow_mut().take().expect("window builder must publish its workspace");
    let handle: AnyWindowHandle = handle.into();
    let native_hwnd = *hwnd_slot.borrow();
    let registry = cx.global_mut::<WindowRegistry>();
    registry.activation_sequence = registry.activation_sequence.saturating_add(1);
    let last_activated = registry.activation_sequence;
    registry.entries.push(WindowEntry {
        runtime_window_id,
        handle,
        workspace: workspace.downgrade(),
        last_activated,
        native_hwnd,
        role,
    });
    crate::gpui_shell::wallpaper::refresh(cx);
    Ok((runtime_window_id, workspace))
}

pub(crate) fn open_new_window(cx: &mut App, cwd: Option<PathBuf>) -> gpui::Result<(u64, u64)> {
    let (window_id, workspace) = open_workspace_window(
        cx,
        WorkspaceStartup::NewTerminal { cwd },
        None,
        None,
        true,
        WindowRole::Regular,
    )?;
    let pane_id = workspace.read(cx).active_terminal_pane_id().unwrap_or_default();
    Ok((window_id, pane_id))
}

fn open_runtime_window(cx: &mut App, cwd: Option<PathBuf>) -> gpui::Result<(u64, u64)> {
    let (window_id, workspace) = open_workspace_window(
        cx,
        WorkspaceStartup::NewTerminal { cwd },
        None,
        None,
        false,
        WindowRole::Regular,
    )?;
    let pane_id = workspace.read(cx).active_terminal_pane_id().unwrap_or_default();
    Ok((window_id, pane_id))
}

pub(crate) fn mark_active(runtime_window_id: u64, cx: &mut App) {
    let registry = cx.global_mut::<WindowRegistry>();
    if !registry.entries.iter().any(|entry| {
        entry.runtime_window_id == runtime_window_id && entry.role == WindowRole::Regular
    }) {
        return;
    }
    if registry
        .entries
        .iter()
        .filter(|entry| entry.role == WindowRole::Regular)
        .max_by_key(|entry| entry.last_activated)
        .is_some_and(|entry| entry.runtime_window_id == runtime_window_id)
    {
        return;
    }
    registry.activation_sequence = registry.activation_sequence.saturating_add(1);
    let sequence = registry.activation_sequence;
    if let Some(entry) =
        registry.entries.iter_mut().find(|entry| entry.runtime_window_id == runtime_window_id)
    {
        entry.last_activated = sequence;
    }
}

fn prune_entries(cx: &mut App) {
    let open = cx.windows();
    let registry = cx.global_mut::<WindowRegistry>();
    registry
        .entries
        .retain(|entry| open.contains(&entry.handle) && entry.workspace.upgrade().is_some());
    #[cfg(windows)]
    {
        let quick_is_open = registry.quick_terminal.as_ref().is_some_and(|quick| {
            open.contains(&quick.handle)
                && registry.entries.iter().any(|entry| {
                    entry.runtime_window_id == quick.runtime_window_id
                        && entry.role == WindowRole::QuickTerminal
                })
        });
        if !quick_is_open {
            registry.quick_terminal = None;
        }
    }
}

fn entries_by_mru(cx: &mut App) -> Vec<WindowEntry> {
    prune_entries(cx);
    let active = cx.active_window();
    let mut entries = cx
        .global::<WindowRegistry>()
        .entries
        .iter()
        .filter(|entry| entry.role == WindowRole::Regular)
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        (active.is_some_and(|handle| handle == entry.handle), entry.last_activated)
    });
    entries.reverse();
    entries
}

fn select_mru_window(
    behavior: nebula_settings::WindowingBehaviorName,
    cx: &mut App,
) -> Option<WindowEntry> {
    let entries = entries_by_mru(cx);
    if behavior != nebula_settings::WindowingBehaviorName::UseExisting {
        return entries.into_iter().next();
    }

    #[cfg(windows)]
    {
        let mut queried = false;
        for entry in &entries {
            match is_window_on_current_virtual_desktop(entry.native_hwnd) {
                Some(true) => return Some(entry.clone()),
                Some(false) => queried = true,
                None => {},
            }
        }
        // COM 不可用时保持可用性，回退全局 MRU；确认当前桌面没有窗口时
        // 返回 None，由调用方创建新窗口。
        if !queried {
            return entries.into_iter().next();
        }
        None
    }
    #[cfg(not(windows))]
    {
        entries.into_iter().next()
    }
}

fn entry_by_id(runtime_window_id: u64, cx: &mut App) -> Option<WindowEntry> {
    entries_by_mru(cx).into_iter().find(|entry| entry.runtime_window_id == runtime_window_id)
}

fn logical_client_position(client_x: i32, client_y: i32, scale_factor: f32) -> (f32, f32) {
    debug_assert!(scale_factor.is_finite() && scale_factor > 0.0);
    let scale_factor =
        if scale_factor.is_finite() && scale_factor > 0.0 { scale_factor } else { 1.0 };
    (client_x as f32 / scale_factor, client_y as f32 / scale_factor)
}

#[cfg(windows)]
fn native_cursor_target() -> Option<NativeCursorTarget> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetCursorPos, WindowFromPoint,
    };

    let mut screen = POINT { x: 0, y: 0 };
    // 这些调用都在 GPUI UI 线程的 DPI context 中完成。保留 POINT 的有符号
    // i32，左侧或上侧副屏的负坐标不能经过 u32/LOWORD 截断。
    if unsafe { GetCursorPos(&mut screen) } == 0 {
        return None;
    }
    let hit = unsafe { WindowFromPoint(screen) };
    if hit.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    if root.is_null() {
        return None;
    }
    let mut client = screen;
    if unsafe { ScreenToClient(root, &mut client) } == 0 {
        return None;
    }
    Some(NativeCursorTarget { root_hwnd: root as isize, client_x: client.x, client_y: client.y })
}

#[cfg(windows)]
fn cursor_drop_target(
    source_window_id: u64,
    cx: &mut App,
) -> Option<(WindowEntry, NativeCursorTarget)> {
    let cursor = native_cursor_target()?;
    let entry = entries_by_mru(cx).into_iter().find(|entry| {
        entry.runtime_window_id != source_window_id
            && entry.native_hwnd != 0
            && entry.native_hwnd == cursor.root_hwnd
    })?;
    Some((entry, cursor))
}

pub(crate) fn clear_cross_window_drag_target(target_window_id: Option<u64>, cx: &mut App) {
    let Some(target_window_id) = target_window_id else { return };
    let Some(entry) = entry_by_id(target_window_id, cx) else { return };
    let target = entry.workspace.clone();
    let _ = entry.handle.update(cx, move |_, _window, cx| {
        target.update(cx, |workspace, cx| {
            if workspace.cross_window_dock.take().is_some() {
                cx.notify();
            }
        })
    });
}

#[cfg(windows)]
pub(crate) fn update_cross_window_drag_target(
    payload: &CrossWindowTabDrag,
    previous_target: Option<u64>,
    cx: &mut App,
) -> Option<CrossWindowDropDestination> {
    let resolved = cursor_drop_target(payload.source_window_id, cx);
    let resolved_id = resolved.as_ref().map(|(entry, _)| entry.runtime_window_id);
    if previous_target != resolved_id {
        clear_cross_window_drag_target(previous_target, cx);
    }
    let Some((entry, cursor)) = resolved else { return None };
    let target_window_id = entry.runtime_window_id;
    let target = entry.workspace.clone();
    let result = entry.handle.update(cx, move |_, window, cx| {
        let (x, y) =
            logical_client_position(cursor.client_x, cursor.client_y, window.scale_factor());
        target.update(cx, |workspace, cx| {
            let dock = workspace.dock_nav_at(x, y);
            if workspace.cross_window_dock != dock {
                workspace.cross_window_dock = dock;
                cx.notify();
            }
            dock
        })
    });
    match result {
        Ok(Ok(dock)) => Some(CrossWindowDropDestination { window_id: target_window_id, dock }),
        _ => {
            clear_cross_window_drag_target(Some(target_window_id), cx);
            None
        },
    }
}

#[cfg(not(windows))]
pub(crate) fn update_cross_window_drag_target(
    _payload: &CrossWindowTabDrag,
    previous_target: Option<u64>,
    cx: &mut App,
) -> Option<CrossWindowDropDestination> {
    clear_cross_window_drag_target(previous_target, cx);
    None
}

fn entry_with_pane(pane_id: u64, cx: &mut App) -> Option<WindowEntry> {
    entries_by_mru(cx).into_iter().find(|entry| {
        entry
            .workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).tab_of_pane(pane_id).is_some())
    })
}

fn route_entry(command: &RuntimeCommand, cx: &mut App) -> Result<WindowEntry, ApiError> {
    let runtime_hub = cx.global::<WindowRegistry>().runtime_hub.clone();
    let (window_id, pane_id) = match command {
        RuntimeCommand::Focus { window_id, pane_id }
        | RuntimeCommand::Split { window_id, pane_id, .. }
        | RuntimeCommand::AgentStart { window_id, pane_id, .. } => (*window_id, *pane_id),
        RuntimeCommand::NewTab { window_id, .. }
        | RuntimeCommand::CloseWindow { window_id }
        | RuntimeCommand::CloseTab { window_id, .. }
        | RuntimeCommand::RenameTab { window_id, .. }
        | RuntimeCommand::MoveTab { window_id, .. } => (*window_id, None),
        RuntimeCommand::Prompt { window_id, pane_id, .. }
        | RuntimeCommand::Paste { window_id, pane_id, .. }
        | RuntimeCommand::ClosePane { window_id, pane_id }
        | RuntimeCommand::ZoomPane { window_id, pane_id, .. }
        | RuntimeCommand::ResizePane { window_id, pane_id, .. }
        | RuntimeCommand::ReadPane { window_id, pane_id, .. }
        | RuntimeCommand::Procs { window_id, pane_id }
        | RuntimeCommand::SendKey { window_id, pane_id, .. }
        | RuntimeCommand::Run { window_id, pane_id, .. }
        | RuntimeCommand::Exec { window_id, pane_id, .. } => (*window_id, Some(*pane_id)),
        RuntimeCommand::AgentFork { window_id, source_pane_id, .. } => {
            (*window_id, *source_pane_id)
        },
        RuntimeCommand::AgentPrompt { agent, generation, .. }
        | RuntimeCommand::AgentPaste { agent, generation, .. }
        | RuntimeCommand::AgentRead { agent, generation, .. } => {
            let managed = runtime_hub.active_agent(agent, *generation)?;
            (Some(managed.window_id), Some(managed.pane_id))
        },
        RuntimeCommand::Snapshot | RuntimeCommand::NewWindow { .. } => (None, None),
    };

    let entry = window_id
        .and_then(|id| entry_by_id(id, cx))
        .or_else(|| pane_id.and_then(|id| entry_with_pane(id, cx)))
        .or_else(|| {
            select_mru_window(nebula_settings::RuntimeSettings::load().windowing_behavior, cx)
        });
    entry.ok_or_else(|| ApiError::new("target_not_found", "no terminal window is available"))
}

pub(crate) fn dispatch_shell_events(events: Vec<GpuiShellEvent>, cx: &mut App) {
    for event in events {
        match event {
            GpuiShellEvent::NotificationFocus(pane_id) => focus_notification(pane_id, cx),
            GpuiShellEvent::TrayFocus(pane_id) => {
                let target =
                    pane_id.and_then(|pane_id| entry_with_pane(pane_id, cx)).or_else(|| {
                        select_mru_window(
                            nebula_settings::RuntimeSettings::load().windowing_behavior,
                            cx,
                        )
                    });
                if let Some(entry) = target {
                    focus_entry(&entry, pane_id, cx);
                }
            },
            GpuiShellEvent::TrayQuit => {
                quit_all(cx);
                return;
            },
            GpuiShellEvent::MuxAttach => {
                if let Some(entry) = entries_by_mru(cx).into_iter().next() {
                    focus_entry(&entry, None, cx);
                }
            },
            GpuiShellEvent::RuntimeControl(dispatch) => dispatch_runtime(dispatch, cx),
            GpuiShellEvent::UpdateAvailable(result) => {
                let Some(entry) = entries_by_mru(cx).into_iter().next() else { continue };
                let _ = entry.handle.update(cx, move |_, window, cx| {
                    super::show_update_notification(result, window, cx);
                });
            },
            GpuiShellEvent::SshPrompt(request) => {
                let Some(entry) = entries_by_mru(cx).into_iter().next() else {
                    request.respond(crate::ssh_prompt::PromptResponse::Cancel);
                    continue;
                };
                let pending = request.clone();
                if entry
                    .handle
                    .update(cx, move |_, window, cx| {
                        super::ssh_dialog::show(pending, window, cx);
                    })
                    .is_err()
                {
                    request.respond(crate::ssh_prompt::PromptResponse::Cancel);
                }
            },
        }
    }
    publish_runtime_snapshot(cx);
}

pub(crate) fn dispatch_ai_events(events: Vec<crate::ai_hook::AiHookEvent>, cx: &mut App) {
    let mut handled = Vec::new();
    for event in crate::ai_hook::reorder_batch(events) {
        // 明确 pane id 是严格路由合同：pane 已关闭时，迟到 Hook 必须丢弃；
        // 只有发送端确实没有 NEBULA_PANE_ID 时才允许落到 MRU 窗口。
        let target = match event.pane {
            Some(pane_id) => entry_with_pane(pane_id, cx),
            None => entries_by_mru(cx).into_iter().next(),
        };
        let Some(entry) = target else { continue };
        if let Some(workspace) = entry.workspace.upgrade() {
            let routed = event.clone();
            if workspace.update(cx, |workspace, cx| workspace.handle_ai_hook(event, cx)) {
                handled.push(routed);
            }
        }
    }
    if handled.is_empty() {
        return;
    }
    publish_runtime_snapshot(cx);
    let hub = cx.global::<WindowRegistry>().runtime_hub.clone();
    for event in &handled {
        hub.complete_delegations_for_turn(event);
    }
    dispatch_ready_delegation_callbacks(cx);
}

/// Try callbacks after every relevant state refresh. A completion can arrive
/// while the origin Agent is still finishing its own turn, so hook delivery
/// alone is not a sufficient retry clock.
pub(crate) fn dispatch_ready_delegation_callbacks(cx: &mut App) {
    let hub = cx.global::<WindowRegistry>().runtime_hub.clone();
    let callbacks = hub.take_ready_delegation_callbacks();
    let mut delivered = false;
    for callback in callbacks {
        let Some(entry) = entry_with_pane(callback.origin.pane_id, cx) else {
            log::warn!(
                "delegation callback dropped task_id={} origin_pane={} reason=origin_closed",
                callback.task_id,
                callback.origin.pane_id
            );
            continue;
        };
        let Some(workspace) = entry.workspace.upgrade() else { continue };
        match workspace
            .update(cx, |workspace, cx| workspace.deliver_delegation_callback(&callback, cx))
        {
            Ok(()) => {
                delivered = true;
                log::info!(
                    "delegation callback delivered task_id={} origin_pane={}",
                    callback.task_id,
                    callback.origin.pane_id
                );
            },
            Err(error)
                if matches!(error.code.as_str(), "input_in_progress" | "origin_not_ready") =>
            {
                log::debug!(
                    "delegation callback deferred task_id={} origin_pane={} reason={}",
                    callback.task_id,
                    callback.origin.pane_id,
                    error.code
                );
                hub.requeue_delegation_callback(callback);
            },
            Err(error) => {
                log::warn!(
                    "delegation callback dropped task_id={} origin_pane={} reason={}",
                    callback.task_id,
                    callback.origin.pane_id,
                    error.code
                );
            },
        }
    }
    if delivered {
        publish_runtime_snapshot(cx);
    }
}

fn dispatch_runtime(dispatch: Arc<RuntimeDispatch>, cx: &mut App) {
    let window_policy = runtime_window_policy(&dispatch.command);
    match &dispatch.command {
        RuntimeCommand::Snapshot => {
            let snapshot = publish_runtime_snapshot(cx);
            dispatch.respond(
                serde_json::to_value(snapshot)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string())),
            );
            return;
        },
        RuntimeCommand::NewWindow { cwd } => {
            let response = open_runtime_window(cx, cwd.clone())
                .map_err(|error| ApiError::new("window_create_failed", error.to_string()))
                .map(|(window_id, pane_id)| {
                    let snapshot = publish_runtime_snapshot(cx);
                    json!({
                        "action": { "window_id": window_id, "pane_id": pane_id },
                        "snapshot": snapshot
                    })
                });
            dispatch.respond(response);
            return;
        },
        RuntimeCommand::NewTab { window_id: None, cwd }
            if nebula_settings::RuntimeSettings::load().windowing_behavior
                == nebula_settings::WindowingBehaviorName::UseNew =>
        {
            let response = open_runtime_window(cx, cwd.clone())
                .map_err(|error| ApiError::new("window_create_failed", error.to_string()))
                .map(|(window_id, pane_id)| {
                    let snapshot = publish_runtime_snapshot(cx);
                    json!({
                        "action": { "window_id": window_id, "pane_id": pane_id },
                        "snapshot": snapshot
                    })
                });
            dispatch.respond(response);
            return;
        },
        _ => {},
    }

    if let RuntimeCommand::Exec { pane_id, argv, timeout_ms, max_output_bytes, .. } =
        &dispatch.command
    {
        let entry = match route_entry(&dispatch.command, cx) {
            Ok(entry) => entry,
            Err(error) => {
                dispatch.respond(Err(error));
                return;
            },
        };
        let Some(workspace) = entry.workspace.upgrade() else {
            dispatch.respond(Err(ApiError::new(
                "target_not_found",
                "the target workspace no longer exists",
            )));
            return;
        };
        match workspace.read(cx).runtime_exec_context(*pane_id, cx) {
            Ok((context, cwd)) => crate::runtime_exec::spawn(
                dispatch.clone(),
                context,
                cwd,
                argv.clone(),
                *timeout_ms,
                *max_output_bytes,
            ),
            Err(error) => dispatch.respond(Err(error)),
        }
        return;
    }

    let entry = match route_entry(&dispatch.command, cx) {
        Ok(entry) => entry,
        Err(error)
            if matches!(dispatch.command, RuntimeCommand::NewTab { window_id: None, .. }) =>
        {
            let RuntimeCommand::NewTab { cwd, .. } = &dispatch.command else { unreachable!() };
            let response = open_runtime_window(cx, cwd.clone())
                .map_err(|create| {
                    ApiError::new(
                        "window_create_failed",
                        format!(
                            "{}: {}; creating a fallback window also failed: {create}",
                            error.code, error.message
                        ),
                    )
                })
                .map(|(window_id, pane_id)| {
                    let snapshot = publish_runtime_snapshot(cx);
                    json!({
                        "action": { "window_id": window_id, "pane_id": pane_id },
                        "snapshot": snapshot
                    })
                });
            dispatch.respond(response);
            return;
        },
        Err(error) => {
            dispatch.respond(Err(error));
            return;
        },
    };
    let command = dispatch.command.clone();
    let workspace = entry.workspace.clone();
    let result = entry.handle.update(cx, move |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            apply_runtime_window_policy(window_policy, workspace, window);
            workspace.execute_runtime_command(&command, window, cx)
        })
    });
    dispatch.respond(match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(ApiError::new("target_not_found", error.to_string())),
        Err(error) => Err(ApiError::new("target_not_found", error.to_string())),
    });
}

fn apply_runtime_window_policy(
    policy: RuntimeWindowPolicy,
    workspace: &mut NebulaWorkspace,
    window: &mut Window,
) {
    match policy {
        RuntimeWindowPolicy::Preserve => {},
        RuntimeWindowPolicy::CreateWithoutActivation => {
            debug_assert!(false, "window.create is handled before existing-window dispatch");
        },
        RuntimeWindowPolicy::Focus => focus_workspace_window(workspace, window),
    }
}

fn focus_workspace_window(workspace: &mut NebulaWorkspace, window: &mut Window) {
    if workspace.window_hidden {
        crate::gpui_shell::reveal_native_window(window);
        workspace.window_hidden = false;
    }
    window.activate_window();
}

/// 快速终端是独立窗口，不占用普通工作区的 MRU、runtime 路由或 session 恢复槽。
/// 三态仍与旧壳一致：隐藏时显示并聚焦；可见但在后台时只聚焦；只有可见且
/// 已在前台时才向上收起。
#[cfg(windows)]
pub(crate) fn toggle_quick_terminal_window(cx: &mut App) {
    prune_entries(cx);
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
            if !super::quick_terminal::slide_native_window(hwnd, geometry, 1.0) {
                log::warn!("quick terminal could not prepare its hidden position");
                let _ = handle.update(cx, |_, window, _| window.remove_window());
                cx.global_mut::<WindowRegistry>().quick_terminal = None;
                return;
            }
            super::quick_terminal::show_native_window(hwnd);
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
fn quick_terminal_anchor_display(cx: &mut App) -> Option<(gpui::DisplayId, Bounds<gpui::Pixels>)> {
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
    let anchor_hwnd = quick_terminal_anchor_hwnd(cx);
    let Some(geometry) = super::quick_terminal::native_geometry(anchor_hwnd) else {
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
    if !super::quick_terminal::configure_native_window(entry.native_hwnd, geometry, 1.0, true) {
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
    if !super::quick_terminal::slide_native_window(hwnd, geometry, hidden) {
        log::warn!("quick terminal native positioning failed");
        window.remove_window();
        cx.global_mut::<WindowRegistry>().quick_terminal = None;
        return false;
    }
    if !active && !target_visible {
        super::quick_terminal::hide_native_window(hwnd);
    }
    active
}

#[cfg(not(windows))]
pub(crate) fn toggle_quick_terminal_window(cx: &mut App) {
    let target = entries_by_mru(cx).into_iter().next().map(|entry| (entry.handle, entry.workspace));
    let Some((handle, workspace)) = target else { return };
    let _ = handle.update(cx, move |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
            if workspace.window_hidden {
                focus_workspace_window(workspace, window);
            } else if window.is_window_active() {
                crate::gpui_shell::hide_native_window(window);
                workspace.window_hidden = true;
            } else {
                window.activate_window();
            }
            cx.notify();
        });
    });
}

pub(crate) fn is_quick_terminal_window(handle: AnyWindowHandle, cx: &App) -> bool {
    #[cfg(windows)]
    {
        return cx
            .global::<WindowRegistry>()
            .quick_terminal
            .as_ref()
            .is_some_and(|quick| quick.handle == handle);
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, cx);
        false
    }
}

fn notification_target<Entry>(
    pane_id: Option<u64>,
    candidates: impl IntoIterator<Item = (Entry, WindowRole, Vec<u64>)>,
) -> Option<Entry> {
    candidates.into_iter().find_map(|(entry, role, panes)| {
        let matches = match pane_id {
            Some(pane_id) => panes.contains(&pane_id),
            None => role == WindowRole::Regular,
        };
        matches.then_some(entry)
    })
}

pub(crate) fn notification_view(
    pane_id: u64,
    cx: &App,
) -> Option<Entity<crate::gpui_shell::terminal::view::TerminalView>> {
    cx.global::<WindowRegistry>().entries.iter().find_map(|entry| {
        let workspace = entry.workspace.upgrade()?;
        workspace.read(cx).tabs.iter().find_map(|tab| match tab {
            WorkspaceTab::Terminal { panes, .. } => {
                panes.iter().find(|pane| pane.id == pane_id).map(|pane| pane.view.clone())
            },
            _ => None,
        })
    })
}

pub(crate) fn focus_notification(pane_id: Option<u64>, cx: &mut App) {
    prune_entries(cx);
    let active_window = cx.active_window();
    let mut entries = cx.global::<WindowRegistry>().entries.clone();
    entries.sort_by_key(|entry| {
        std::cmp::Reverse((
            active_window.is_some_and(|handle| handle == entry.handle),
            entry.last_activated,
        ))
    });
    let target = notification_target(
        pane_id,
        entries.into_iter().filter_map(|entry| {
            let workspace = entry.workspace.upgrade()?;
            let panes = workspace
                .read(cx)
                .tabs
                .iter()
                .filter_map(|tab| match tab {
                    WorkspaceTab::Terminal { panes, .. } => Some(panes.iter().map(|pane| pane.id)),
                    _ => None,
                })
                .flatten()
                .collect();
            let role = entry.role;
            Some((entry, role, panes))
        }),
    );
    let Some(entry) = target else { return };
    #[cfg(windows)]
    if entry.role == WindowRole::QuickTerminal {
        let geometry = cx
            .global::<WindowRegistry>()
            .quick_terminal
            .as_ref()
            .filter(|quick| quick.runtime_window_id == entry.runtime_window_id)
            .map(|quick| quick.geometry);
        let Some(geometry) = geometry else { return };
        if !super::quick_terminal::slide_native_window(entry.native_hwnd, geometry, 0.0) {
            log::warn!("notification could not reveal quick terminal");
            return;
        }
        if let Some(quick) = cx.global_mut::<WindowRegistry>().quick_terminal.as_mut() {
            quick.target_visible = true;
            quick.motion.snap_to(0.0);
            quick.motion_clock.reset();
            quick.animation_generation = quick.animation_generation.wrapping_add(1);
        }
        super::quick_terminal::show_native_window(entry.native_hwnd);
    }
    let workspace = entry.workspace.clone();
    let _ = entry.handle.update(cx, move |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
            let tab_ix = match pane_id {
                Some(pane_id) => match workspace.tab_of_pane(pane_id) {
                    Some(tab_ix) => tab_ix,
                    None => return,
                },
                None => workspace.active,
            };
            #[cfg(windows)]
            if let Some(hwnd) = native_hwnd(window) {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    IsIconic, SW_RESTORE, ShowWindow,
                };
                unsafe {
                    let hwnd = hwnd as *mut std::ffi::c_void;
                    if IsIconic(hwnd) != 0 {
                        ShowWindow(hwnd, SW_RESTORE);
                    }
                }
            }
            focus_workspace_window(workspace, window);
            workspace.activate_tab(tab_ix, window, cx);
            if let Some(pane_id) = pane_id {
                if let Some(WorkspaceTab::Terminal { zoomed, .. }) = workspace.tabs.get_mut(tab_ix)
                {
                    *zoomed = false;
                }
                workspace.focus_pane(tab_ix, pane_id, window, cx);
            }
            if let Some(meta) = workspace.tab_meta.get_mut(tab_ix) {
                meta.has_bell = false;
            }
            workspace.reveal_active_tab();
            workspace.focus_active(window, cx);
            workspace.sync_side_panel_to_active(true, cx);
            cx.notify();
        });
    });
}

fn focus_entry(entry: &WindowEntry, pane_id: Option<u64>, cx: &mut App) {
    let workspace = entry.workspace.clone();
    let _ = entry.handle.update(cx, move |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
            focus_workspace_window(workspace, window);
            if let Some(pane_id) = pane_id
                && let Some(tab_ix) = workspace.tab_of_pane(pane_id)
            {
                workspace.active = tab_ix;
                if let Some(WorkspaceTab::Terminal { focused, zoomed, .. }) =
                    workspace.tabs.get_mut(tab_ix)
                {
                    *focused = pane_id;
                    *zoomed = false;
                }
            }
            workspace.focus_active(window, cx);
            cx.notify();
        });
    });
}

pub(crate) fn publish_runtime_snapshot(cx: &mut App) -> RuntimeSnapshot {
    let entries = entries_by_mru(cx);
    let mut hidden = 0usize;
    let mut windows = Vec::new();
    for entry in entries {
        let workspace = entry.workspace.clone();
        if let Ok(Some((is_hidden, runtime_window))) =
            entry.handle.update(cx, move |_, window, cx| {
                workspace
                    .upgrade()
                    .map(|workspace| workspace.read(cx).runtime_window_snapshot(window, cx))
            })
        {
            hidden += usize::from(is_hidden);
            windows.push(runtime_window);
        }
    }
    windows.sort_by_key(|window| window.id);
    let snapshot = RuntimeSnapshot::new(hidden, windows);
    cx.global::<WindowRegistry>().runtime_hub.publish(snapshot)
}

pub(crate) fn publish_runtime_snapshot_with_current(
    current_hidden: bool,
    current: RuntimeWindow,
    cx: &mut App,
) -> RuntimeSnapshot {
    let current_id = current.id;
    let entries = entries_by_mru(cx);
    let mut hidden = usize::from(current_hidden);
    let mut windows = vec![current];
    for entry in entries {
        if entry.runtime_window_id == current_id {
            continue;
        }
        let workspace = entry.workspace.clone();
        if let Ok(Some((is_hidden, runtime_window))) =
            entry.handle.update(cx, move |_, window, cx| {
                workspace
                    .upgrade()
                    .map(|workspace| workspace.read(cx).runtime_window_snapshot(window, cx))
            })
        {
            hidden += usize::from(is_hidden);
            windows.push(runtime_window);
        }
    }
    windows.sort_by_key(|window| window.id);
    let snapshot = RuntimeSnapshot::new(hidden, windows);
    cx.global::<WindowRegistry>().runtime_hub.publish(snapshot)
}

pub(crate) fn autosave_tick(cx: &mut App) {
    save_combined_session(cx, false);
}

fn combined_session(
    mut current: Option<(u64, crate::session::Session)>,
    cx: &App,
) -> Option<crate::session::Session> {
    let mut entries = cx
        .global::<WindowRegistry>()
        .entries
        .iter()
        .filter(|entry| entry.role == WindowRole::Regular)
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.runtime_window_id);
    let active_handle = cx
        .active_window()
        .filter(|handle| entries.iter().any(|entry| entry.handle == *handle))
        .or_else(|| {
            entries.iter().max_by_key(|entry| entry.last_activated).map(|entry| entry.handle)
        });
    let mut sessions = Vec::new();
    for entry in entries {
        let session = if current.as_ref().is_some_and(|(id, _)| *id == entry.runtime_window_id) {
            current.take().expect("current window snapshot exists").1
        } else {
            let Some(workspace) = entry.workspace.upgrade() else { continue };
            workspace.read(cx).snapshot_session(cx)
        };
        sessions.push((active_handle == Some(entry.handle), session));
    }
    combine_sessions(sessions)
}

fn save_combined_session(cx: &mut App, clean: bool) {
    let session = combined_session(None, cx);
    let reason = if clean { SaveReason::Quit } else { SaveReason::Checkpoint };
    cx.global_mut::<WindowRegistry>().session_persistence.save(session, reason);
}

pub(super) fn save_current_window_session(
    runtime_window_id: u64,
    session: crate::session::Session,
    reason: SaveReason,
    cx: &mut App,
) {
    if !cx.global::<WindowRegistry>().entries.iter().any(|entry| {
        entry.runtime_window_id == runtime_window_id && entry.role == WindowRole::Regular
    }) {
        return;
    }
    let session = combined_session(Some((runtime_window_id, session)), cx);
    cx.global_mut::<WindowRegistry>().session_persistence.save(session, reason);
}

pub(crate) fn quit_all(cx: &mut App) {
    save_combined_session(cx, true);
    prune_entries(cx);
    let entries = cx.global::<WindowRegistry>().entries.clone();
    for entry in entries {
        let workspace = entry.workspace.clone();
        let _ = entry.handle.update(cx, move |_, _window, cx| {
            let _ = workspace.update(cx, |workspace, cx| workspace.shutdown_terminal_panes(cx));
        });
    }
    crate::tray::shutdown();
    cx.quit();
}

pub(crate) fn move_tab_to_new_window(payload: CrossWindowTabDrag, cx: &mut App) {
    let opened =
        open_workspace_window(cx, WorkspaceStartup::Empty, None, None, true, WindowRole::Regular);
    let Ok((target_window_id, target_workspace)) = opened else {
        log::warn!("failed to open a window for moved tab");
        return;
    };
    let Some(entry) = entry_by_id(target_window_id, cx) else { return };
    let target = target_workspace.downgrade();
    let result = entry.handle.update(cx, move |_, window, cx| {
        target.update(cx, |workspace, cx| {
            workspace.accept_cross_window_tab(&payload, None, window, cx)
        })
    });
    if !matches!(result, Ok(Ok(true))) {
        unregister(target_window_id, cx);
        let _ = entry.handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// 把 mouse-up 时确认的跨窗目标作为一次延迟事务提交。目标窗口可能在释放
/// 与 defer 执行之间关闭；必须先重新取得它的 handle/workspace，成功进入
/// 目标窗口上下文后才允许 `accept_cross_window_tab` detach 源 tab。
pub(crate) fn drop_tab_to_existing_window(
    payload: CrossWindowTabDrag,
    destination: CrossWindowDropDestination,
    cx: &mut App,
) -> bool {
    let Some(entry) = entry_by_id(destination.window_id, cx) else {
        log::debug!(
            "cross-window tab drop cancelled: target window {} closed before commit",
            destination.window_id
        );
        return false;
    };
    let target = entry.workspace.clone();
    let result = entry.handle.update(cx, move |_, window, cx| {
        target.update(cx, |workspace, cx| {
            workspace.accept_cross_window_tab(&payload, destination.dock, window, cx)
        })
    });
    if matches!(result, Ok(Ok(true))) {
        true
    } else {
        log::debug!(
            "cross-window tab drop cancelled: target window {} became unavailable",
            destination.window_id
        );
        false
    }
}

fn unregister(runtime_window_id: u64, cx: &mut App) {
    let registry = cx.global_mut::<WindowRegistry>();
    registry.entries.retain(|entry| entry.runtime_window_id != runtime_window_id);
    #[cfg(windows)]
    if registry
        .quick_terminal
        .as_ref()
        .is_some_and(|quick| quick.runtime_window_id == runtime_window_id)
    {
        registry.quick_terminal = None;
    }
}

pub(super) fn close_saved_workspace_window(
    runtime_window_id: u64,
    session: crate::session::Session,
    window: &mut Window,
    cx: &mut App,
) {
    save_current_window_session(runtime_window_id, session, SaveReason::WindowClose, cx);
    unregister(runtime_window_id, cx);
    window.remove_window();
}

pub(crate) fn close_empty_workspace_window(
    runtime_window_id: u64,
    window: &mut Window,
    cx: &mut App,
) {
    let regular = cx.global::<WindowRegistry>().entries.iter().any(|entry| {
        entry.runtime_window_id == runtime_window_id && entry.role == WindowRole::Regular
    });
    unregister(runtime_window_id, cx);
    if regular {
        let session = combined_session(None, cx);
        let reason =
            if session.is_some() { SaveReason::TabsClosed } else { SaveReason::WindowClose };
        let session = session.unwrap_or_else(|| crate::session::Session::new(0, Vec::new()));
        cx.global_mut::<WindowRegistry>().session_persistence.save(Some(session), reason);
    }
    window.remove_window();
}

impl NebulaWorkspace {
    pub(crate) fn cross_window_drag_payload(
        &self,
        ix: usize,
        cx: &Context<Self>,
    ) -> Option<CrossWindowTabDrag> {
        let WorkspaceTab::Terminal { focused, .. } = self.tabs.get(ix)? else { return None };
        Some(CrossWindowTabDrag {
            source: cx.entity().downgrade(),
            source_window_id: self.runtime_window_id,
            pane_id: *focused,
            title: self.tab_title(ix, cx),
        })
    }

    pub(crate) fn cross_window_drag_preview(
        payload: &CrossWindowTabDrag,
        cx: &mut App,
    ) -> Entity<TabDragPreview> {
        cx.new(|_| TabDragPreview { title: payload.title.clone() })
    }

    pub(crate) fn schedule_move_tab_to_new_window(&self, ix: usize, cx: &mut Context<Self>) {
        let Some(payload) = self.cross_window_drag_payload(ix, cx) else { return };
        cx.defer(move |cx| move_tab_to_new_window(payload, cx));
    }

    pub(crate) fn accept_cross_window_tab(
        &mut self,
        payload: &CrossWindowTabDrag,
        dock: Option<SplitNav>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if payload.source_window_id == self.runtime_window_id {
            return false;
        }
        let detached = match payload
            .source
            .update(cx, |source, _| source.detach_terminal_tab(payload.pane_id))
        {
            Ok(Some(detached)) => detached,
            _ => return false,
        };

        let pane_ids = detached.pane_ids.clone();
        let source_window_id = detached.source_window_id;
        let source_handle = detached.source_handle;
        let source_became_empty = detached.source_became_empty;
        let mut tab = detached.tab;
        if let WorkspaceTab::Terminal { panes, .. } = &mut tab {
            for pane in panes {
                pane._subscription = cx.subscribe_in(&pane.view, window, Self::on_terminal_event);
            }
        }
        self.runtime_hub.move_panes_to_window(source_window_id, self.runtime_window_id, &pane_ids);
        self.attach_terminal_tab(tab, detached.meta, dock, window, cx);

        let source = payload.source.clone();
        if source_became_empty {
            unregister(source_window_id, cx);
            let _ = source_handle.update(cx, |_, source_window, _| source_window.remove_window());
        } else {
            let _ = source_handle.update(cx, move |_, source_window, cx| {
                let _ = source.update(cx, |source, cx| {
                    source.reveal_active_tab();
                    source.focus_active(source_window, cx);
                    source.sync_side_panel_to_active(true, cx);
                    cx.notify();
                });
            });
        }
        publish_runtime_snapshot_with_current(
            self.window_hidden,
            self.runtime_window_snapshot(window, cx).1,
            cx,
        );
        true
    }

    fn detach_terminal_tab(&mut self, pane_id: u64) -> Option<DetachedTerminalTab> {
        let ix = self.tab_of_pane(pane_id)?;
        let (tab, meta) = self.remove_tab_at(ix)?;
        let WorkspaceTab::Terminal { panes, .. } = &tab else {
            self.insert_tab_at(ix, tab, meta);
            return None;
        };
        let pane_ids = panes.iter().map(|pane| pane.id).collect::<Vec<_>>();
        for pane_id in &pane_ids {
            self.pane_bounds.borrow_mut().remove(pane_id);
        }
        self.split_bounds.borrow_mut().clear();
        self.tab_drag = None;
        self.tab_menu = None;
        self.tab_rename = None;
        if ix < self.active {
            self.active -= 1;
        }
        if !self.tabs.is_empty() {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        let source_became_empty = self.tabs.is_empty();
        Some(DetachedTerminalTab {
            tab,
            meta,
            pane_ids,
            source_window_id: self.runtime_window_id,
            source_handle: self.window_handle,
            source_became_empty,
        })
    }

    fn attach_terminal_tab(
        &mut self,
        tab: WorkspaceTab,
        meta: TabMeta,
        dock: Option<SplitNav>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let can_dock = dock.is_some()
            && matches!(self.tabs.get(self.active), Some(WorkspaceTab::Terminal { .. }));
        if can_dock {
            let Some(nav) = dock else { unreachable!("can_dock requires a direction") };
            let WorkspaceTab::Terminal {
                panes: source_panes,
                tree: source_tree,
                focused: source_focused,
                ..
            } = tab
            else {
                unreachable!("cross-window drag payloads only contain terminal tabs")
            };
            let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed, broadcast, .. }) =
                self.tabs.get_mut(self.active)
            else {
                unreachable!("can_dock checked the active terminal tab")
            };
            let target_tree = std::mem::replace(tree, SplitTree::leaf(source_focused));
            *tree = dock_tree(target_tree, source_tree, nav);
            panes.extend(source_panes);
            *focused = source_focused;
            *zoomed = false;
            // 跨窗 dock 改变了 tab 的接收集合，不能继承任一侧的广播开关。
            *broadcast = false;
        } else {
            let at = self.tabs.len();
            self.insert_tab_at(at, tab, meta);
            self.active = at;
        }
        self.cross_window_dock = None;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        self.sync_side_panel_to_active(true, cx);
        cx.notify();
    }

    pub(crate) fn active_terminal_pane_id(&self) -> Option<u64> {
        match self.tabs.get(self.active) {
            Some(WorkspaceTab::Terminal { focused, .. }) => Some(*focused),
            _ => None,
        }
    }

    fn shutdown_terminal_panes(&self, cx: &App) {
        for tab in &self.tabs {
            let WorkspaceTab::Terminal { panes, .. } = tab else { continue };
            for pane in panes {
                pane.view.read(cx).shutdown();
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn native_hwnd(window: &Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return None };
    Some(handle.hwnd.get())
}

#[cfg(not(windows))]
pub(super) fn native_hwnd(_window: &Window) -> Option<isize> {
    None
}

#[cfg(windows)]
fn is_window_on_current_virtual_desktop(hwnd: isize) -> Option<bool> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoUninitialize,
    };
    use windows_sys::core::{GUID, HRESULT};

    const CLSID_VIRTUAL_DESKTOP_MANAGER: GUID =
        GUID::from_u128(0xaa509086_5ca9_4c25_8f95_589d3c07b48a);
    const IID_VIRTUAL_DESKTOP_MANAGER: GUID =
        GUID::from_u128(0xa5cd92ff_29be_454c_8d04_d82879fb3f1b);

    #[repr(C)]
    struct Interface<T> {
        vtable: *const T,
    }
    #[repr(C)]
    struct IUnknownVTable {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    #[repr(C)]
    struct VirtualDesktopManagerVTable {
        base: IUnknownVTable,
        is_window_on_current_virtual_desktop:
            unsafe extern "system" fn(*mut c_void, HWND, *mut i32) -> HRESULT,
        _get_window_desktop_id: usize,
        _move_window_to_desktop: usize,
    }

    let initialized = unsafe {
        CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32)
    };
    let should_uninitialize = initialized >= 0;
    if initialized < 0 && initialized != RPC_E_CHANGED_MODE {
        return None;
    }
    let mut pointer = std::ptr::null_mut();
    let created = unsafe {
        CoCreateInstance(
            &CLSID_VIRTUAL_DESKTOP_MANAGER,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_VIRTUAL_DESKTOP_MANAGER,
            &mut pointer,
        )
    };
    if created < 0 || pointer.is_null() {
        if should_uninitialize {
            unsafe { CoUninitialize() };
        }
        return None;
    }
    let interface = unsafe { &*pointer.cast::<Interface<VirtualDesktopManagerVTable>>() };
    let vtable = unsafe { &*interface.vtable };
    let mut on_current = 0i32;
    let queried = unsafe {
        (vtable.is_window_on_current_virtual_desktop)(pointer, hwnd as *mut c_void, &mut on_current)
    };
    unsafe { (vtable.base.release)(pointer) };
    if should_uninitialize {
        unsafe { CoUninitialize() };
    }
    (queried >= 0).then_some(on_current != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_agents::AgentKind;
    use crate::runtime_api::{RuntimeKey, RuntimeKeyModifiers, RuntimeSplitDirection};

    #[test]
    fn notification_target_routes_by_live_pane_not_original_window() {
        let windows =
            vec![(10, WindowRole::Regular, vec![1, 2]), (20, WindowRole::Regular, vec![3])];
        assert_eq!(notification_target(Some(3), windows.clone()), Some(20));
        assert_eq!(notification_target(Some(99), windows.clone()), None);
        assert_eq!(notification_target(None, windows), Some(10));
        let moved =
            vec![(10, WindowRole::Regular, vec![1, 2, 3]), (20, WindowRole::Regular, Vec::new())];
        assert_eq!(notification_target(Some(3), moved), Some(10));
    }

    #[cfg(windows)]
    #[test]
    fn notification_target_includes_quick_terminal_only_for_exact_pane() {
        let windows =
            vec![(30, WindowRole::QuickTerminal, vec![7]), (10, WindowRole::Regular, vec![1])];
        assert_eq!(notification_target(Some(7), windows.clone()), Some(30));
        assert_eq!(notification_target(None, windows), Some(10));
        assert_eq!(notification_target(Some(9), [(30, WindowRole::QuickTerminal, vec![7])]), None);
    }

    #[test]
    fn cross_window_client_position_uses_target_scale_factor() {
        assert_eq!(logical_client_position(240, 180, 1.5), (160.0, 120.0));
        assert_eq!(logical_client_position(240, 180, 2.0), (120.0, 90.0));
    }

    #[test]
    fn cross_window_client_position_keeps_signed_coordinates() {
        assert_eq!(logical_client_position(-120, -60, 1.5), (-80.0, -40.0));
    }

    #[test]
    fn runtime_window_policy_only_explicit_focus_activates() {
        assert_eq!(
            runtime_window_policy(&RuntimeCommand::Focus { window_id: Some(1), pane_id: Some(2) }),
            RuntimeWindowPolicy::Focus
        );
        assert_eq!(
            runtime_window_policy(&RuntimeCommand::NewWindow { cwd: None }),
            RuntimeWindowPolicy::CreateWithoutActivation
        );

        let preserve = vec![
            RuntimeCommand::Snapshot,
            RuntimeCommand::CloseWindow { window_id: Some(1) },
            RuntimeCommand::NewTab { window_id: Some(1), cwd: None },
            RuntimeCommand::CloseTab { window_id: Some(1), tab_index: 0 },
            RuntimeCommand::RenameTab {
                window_id: Some(1),
                tab_index: 0,
                name: "renamed".to_owned(),
            },
            RuntimeCommand::MoveTab { window_id: Some(1), tab_index: 0, to_index: 1 },
            RuntimeCommand::Split {
                window_id: Some(1),
                pane_id: Some(2),
                direction: RuntimeSplitDirection::LeftRight,
            },
            RuntimeCommand::ClosePane { window_id: Some(1), pane_id: 2 },
            RuntimeCommand::ZoomPane { window_id: Some(1), pane_id: 2, zoomed: true },
            RuntimeCommand::ResizePane { window_id: Some(1), pane_id: 2, ratio: 0.5 },
            RuntimeCommand::Prompt {
                window_id: Some(1),
                pane_id: 2,
                text: "prompt".to_owned(),
                submit: true,
            },
            RuntimeCommand::Paste {
                window_id: Some(1),
                pane_id: 2,
                text: "paste".to_owned(),
                submit: false,
            },
            RuntimeCommand::ReadPane { window_id: Some(1), pane_id: 2, lines: 20 },
            RuntimeCommand::Procs { window_id: Some(1), pane_id: 2 },
            RuntimeCommand::SendKey {
                window_id: Some(1),
                pane_id: 2,
                key: RuntimeKey::Enter,
                modifiers: RuntimeKeyModifiers::default(),
                repeat: 1,
            },
            RuntimeCommand::Run {
                window_id: Some(1),
                pane_id: 2,
                command: "echo ok".to_owned(),
                wait: false,
                timeout_ms: 1_000,
            },
            RuntimeCommand::Exec {
                window_id: Some(1),
                pane_id: 2,
                argv: vec!["cmd".to_owned()],
                timeout_ms: 1_000,
                max_output_bytes: 1024,
            },
            RuntimeCommand::AgentStart {
                window_id: Some(1),
                pane_id: Some(2),
                name: "agent".to_owned(),
                kind: AgentKind::Codex,
                cwd: None,
                session_id: None,
                command: "codex".to_owned(),
                worktree: None,
            },
            RuntimeCommand::AgentFork {
                window_id: Some(1),
                source_pane_id: Some(2),
                source_cwd: None,
                name: "fork".to_owned(),
                kind: AgentKind::Codex,
                session_id: None,
                command: "codex".to_owned(),
                branch: None,
                base: None,
                path: None,
                allow_dirty_source: false,
            },
            RuntimeCommand::AgentPrompt {
                agent: "agent".to_owned(),
                generation: Some(1),
                text: "prompt".to_owned(),
                submit: true,
            },
            RuntimeCommand::AgentPaste {
                agent: "agent".to_owned(),
                generation: Some(1),
                text: "paste".to_owned(),
                submit: false,
            },
            RuntimeCommand::AgentRead { agent: "agent".to_owned(), generation: Some(1), lines: 20 },
        ];

        for command in preserve {
            assert_eq!(
                runtime_window_policy(&command),
                RuntimeWindowPolicy::Preserve,
                "{command:?} must preserve foreground and visibility"
            );
        }
    }
}
