//! GPUI 托盘与关窗驻留：旧壳 `tray::` / `keep_session` detach 的对应物。
//!
//! 旧壳关窗是 `detach_panes`（PTY 仍在进程里）再销毁 HWND；ATTACH 时再
//! `open_window` 把 pane 接回去。GPUI 用 **hide + mux ATTACH reveal** 对齐
//! 用户可见行为：关窗不杀 PTY，第二次启动回到同一套会话。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Window};
use serde_json::json;

use super::{NebulaWorkspace, SidebarActivity, WorkspaceTab};
use crate::gpui_shell::GpuiShellEvent;
use crate::runtime_api::{ApiError, RuntimeCommand, RuntimeDispatch};

/// 关窗处置。`tray` 故意不参与：旧壳无托盘也会 detach，GPUI 一律 hide，
/// 禁止「没托盘就 minimize」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResidencyCloseAction {
    Hide,
    Close,
}

pub(super) fn residency_close_action(
    keep_session: bool,
    has_live_panes: bool,
    tray: bool,
) -> ResidencyCloseAction {
    let _ = tray;
    if keep_session && has_live_panes {
        ResidencyCloseAction::Hide
    } else {
        ResidencyCloseAction::Close
    }
}

impl NebulaWorkspace {
    pub(super) fn start_shell_event_pump(
        rx: std::sync::mpsc::Receiver<GpuiShellEvent>,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(120)).await;
                let mut events = Vec::new();
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                if events.is_empty() {
                    continue;
                }

                let handle = match this.update(cx, |workspace, cx| {
                    workspace.dispatch_shell_events(&events, cx);
                    workspace.window_handle
                }) {
                    Ok(handle) => handle,
                    Err(_) => return,
                };

                let _ = handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |workspace, cx| {
                        workspace.drain_runtime_commands(window, cx);
                    });
                });
            }
        })
        .detach();
    }

    fn dispatch_shell_events(&mut self, events: &[GpuiShellEvent], cx: &mut Context<Self>) {
        for event in events {
            match event {
                GpuiShellEvent::TrayFocus(pane) => self.handle_tray_focus(*pane, cx),
                GpuiShellEvent::TrayQuit => {
                    self.quit_from_tray(cx);
                    return;
                },
                GpuiShellEvent::MuxAttach => self.reveal_session(cx),
                GpuiShellEvent::RuntimeControl(dispatch) => {
                    self.queue_or_answer_runtime(dispatch.clone(), cx);
                },
            }
        }
    }

    fn queue_or_answer_runtime(
        &mut self,
        dispatch: Arc<RuntimeDispatch>,
        cx: &mut Context<Self>,
    ) {
        match &dispatch.command {
            RuntimeCommand::NewTab { .. } => {
                self.reveal_session(cx);
                self.runtime_pending.push(dispatch);
            },
            RuntimeCommand::Focus { pane_id, .. } => {
                self.handle_tray_focus(*pane_id, cx);
                dispatch.respond(Ok(json!({ "window_id": 1, "pane_id": pane_id })));
            },
            RuntimeCommand::NewWindow => {
                self.reveal_session(cx);
                dispatch.respond(Ok(json!({ "window_id": 1 })));
            },
            RuntimeCommand::Snapshot => {
                dispatch.respond(Ok(json!({
                    "revision": 0,
                    "process_id": std::process::id(),
                    "detached_windows": if self.window_hidden { 1 } else { 0 },
                    "windows": []
                })));
            },
            RuntimeCommand::Split { .. } | RuntimeCommand::Prompt { .. } => {
                dispatch.respond(Err(ApiError::new(
                    "method_not_found",
                    "GPUI shell does not implement this runtime method",
                )));
            },
        }
    }

    fn drain_runtime_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.runtime_pending);
        for dispatch in pending {
            let RuntimeCommand::NewTab { cwd, .. } = &dispatch.command else {
                dispatch.respond(Err(ApiError::new(
                    "runtime_unavailable",
                    "queued runtime command was not tab.new",
                )));
                continue;
            };
            self.add_terminal_at(cwd.clone(), None, window, cx);
            let pane_id = match self.tabs.get(self.active) {
                Some(WorkspaceTab::Terminal { focused, .. }) => *focused,
                _ => 0,
            };
            dispatch.respond(Ok(json!({ "window_id": 1, "pane_id": pane_id })));
        }
    }

    fn handle_tray_focus(&mut self, pane: Option<u64>, cx: &mut Context<Self>) {
        self.reveal_session(cx);
        if let Some(pane_id) = pane
            && let Some(tab_ix) = self.tab_of_pane(pane_id)
        {
            self.active = tab_ix;
            if let Some(WorkspaceTab::Terminal { focused, .. }) = self.tabs.get_mut(tab_ix) {
                *focused = pane_id;
            }
        }
        cx.notify();
    }

    fn reveal_session(&mut self, cx: &mut Context<Self>) {
        crate::gpui_shell::reveal_all_windows(cx);
        self.window_hidden = false;
    }

    fn quit_from_tray(&mut self, cx: &mut Context<Self>) {
        self.save_clean_window_session(cx);
        for tab in &self.tabs {
            let WorkspaceTab::Terminal { panes, .. } = tab else { continue };
            for pane in panes {
                pane.view.read(cx).shutdown();
            }
        }
        crate::tray::shutdown();
        cx.quit();
    }

    fn tab_of_pane(&self, pane_id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| match tab {
            WorkspaceTab::Terminal { panes, .. } => panes.iter().any(|pane| pane.id == pane_id),
            _ => false,
        })
    }

    pub(super) fn reveal_if_tray_disabled(&mut self, cx: &mut Context<Self>) {
        if self.window_hidden && !nebula_settings::RuntimeSettings::load().tray {
            self.reveal_session(cx);
        }
    }

    /// 旧壳 detach：关窗不杀 PTY、不弹忙进程确认。GPUI 用 hide 代替拆 pane。
    pub(super) fn keep_session_on_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let runtime = nebula_settings::RuntimeSettings::load();
        if residency_close_action(
            runtime.keep_session,
            self.has_live_terminal_panes(),
            runtime.tray,
        ) != ResidencyCloseAction::Hide
        {
            return false;
        }
        let snapshot = self.snapshot_session(cx);
        crate::session::save(&snapshot);
        self.last_saved_session = Some(snapshot);
        crate::gpui_shell::hide_native_window(window);
        self.window_hidden = true;
        true
    }

    fn has_live_terminal_panes(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| matches!(tab, WorkspaceTab::Terminal { panes, .. } if !panes.is_empty()))
    }

    pub(super) fn publish_tray_agents(&self, cx: &App) {
        crate::tray::update(self.tray_agents(cx));
    }

    fn tray_agents(&self, cx: &App) -> Vec<crate::tray::TrayAgent> {
        let window = winit::window::WindowId::dummy();
        let mut agents = Vec::new();
        for tab in &self.tabs {
            let WorkspaceTab::Terminal { panes, .. } = tab else { continue };
            for pane in panes {
                let view = pane.view.read(cx);
                let Some(program) = view
                    .running_program
                    .as_deref()
                    .filter(|program| crate::ai_agents::AgentKind::parse(program).is_some())
                else {
                    continue;
                };
                let place = Path::new(view.cwd.trim())
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let label = if place.is_empty() {
                    program.to_owned()
                } else {
                    format!("{program} · {place}")
                };
                agents.push(crate::tray::TrayAgent {
                    window,
                    pane: pane.id,
                    label,
                    needs_attention: view.sidebar_activity() == SidebarActivity::Attention,
                });
            }
        }
        agents
    }
}

#[cfg(test)]
mod tests {
    use super::{residency_close_action, ResidencyCloseAction};

    #[test]
    fn keep_session_hides_even_when_tray_is_off() {
        assert_eq!(
            residency_close_action(true, true, false),
            ResidencyCloseAction::Hide,
            "tray=false must still hide, never minimize"
        );
        assert_eq!(
            residency_close_action(true, true, true),
            ResidencyCloseAction::Hide
        );
        assert_eq!(
            residency_close_action(false, true, false),
            ResidencyCloseAction::Close
        );
        assert_eq!(
            residency_close_action(true, false, true),
            ResidencyCloseAction::Close
        );
    }
}
