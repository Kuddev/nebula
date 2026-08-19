//! GPUI 托盘与关窗驻留：旧壳 `tray::` / `keep_session` detach 的对应物。
//!
//! 旧壳关窗是 `detach_panes`（PTY 仍在进程里）再销毁 HWND；ATTACH 时再
//! `open_window` 把 pane 接回去。GPUI 用 **hide + mux ATTACH reveal** 对齐
//! 用户可见行为：关窗不杀 PTY，第二次启动回到同一套会话。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Window};
use nebula_split::{SplitDirection, SplitTree};
use serde_json::json;

use super::{NebulaWorkspace, WorkspaceTab};
use crate::gpui_shell::GpuiShellEvent;
use crate::runtime_api::{
    ApiError, RuntimeCommand, RuntimeDispatch, RuntimeLayout, RuntimePane, RuntimeSnapshot,
    RuntimeSplitDirection, RuntimeTab, RuntimeTaskState, RuntimeWindow,
};

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
                        workspace.publish_runtime_snapshot(window, cx);
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

    fn queue_or_answer_runtime(&mut self, dispatch: Arc<RuntimeDispatch>, cx: &mut Context<Self>) {
        match &dispatch.command {
            RuntimeCommand::NewWindow => {
                dispatch.respond(Err(ApiError::new(
                    "runtime_unavailable",
                    "the GPUI runtime currently owns one workspace window; window.create is unavailable",
                )));
            },
            RuntimeCommand::Focus { .. }
            | RuntimeCommand::NewTab { .. }
            | RuntimeCommand::Split { .. }
            | RuntimeCommand::Prompt { .. }
            | RuntimeCommand::SendKey { .. }
            | RuntimeCommand::Run { .. }
            | RuntimeCommand::AgentStart { .. }
            | RuntimeCommand::AgentPrompt { .. } => {
                self.reveal_session(cx);
                self.runtime_pending.push(dispatch);
            },
            RuntimeCommand::Snapshot
            | RuntimeCommand::ReadPane { .. }
            | RuntimeCommand::Procs { .. }
            | RuntimeCommand::AgentRead { .. } => {
                self.runtime_pending.push(dispatch);
            },
        }
    }

    fn drain_runtime_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.runtime_pending);
        for dispatch in pending {
            let response = self.execute_runtime_command(&dispatch.command, window, cx);
            dispatch.respond(response);
        }
    }

    fn execute_runtime_command(
        &mut self,
        command: &RuntimeCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, ApiError> {
        match command {
            RuntimeCommand::Snapshot => {
                serde_json::to_value(self.publish_runtime_snapshot(window, cx))
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::NewWindow => Err(ApiError::new(
                "runtime_unavailable",
                "window.create is not queued in the GPUI runtime",
            )),
            RuntimeCommand::Focus { window_id, pane_id } => {
                self.runtime_window_requested(*window_id)?;
                if let Some(pane_id) = pane_id {
                    let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                        return Err(ApiError::new(
                            "target_not_found",
                            format!("pane {pane_id} does not exist"),
                        ));
                    };
                    self.active = tab_ix;
                    if let Some(WorkspaceTab::Terminal { focused, zoomed, .. }) =
                        self.tabs.get_mut(tab_ix)
                    {
                        *focused = *pane_id;
                        *zoomed = false;
                    }
                }
                self.focus_active(window, cx);
                self.runtime_result(
                    json!({ "window_id": self.runtime_window_id, "pane_id": pane_id }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::NewTab { window_id, cwd } => {
                self.runtime_window_requested(*window_id)?;
                let pane_id = self.add_terminal_at(cwd.clone(), None, window, cx);
                self.runtime_result(
                    json!({ "window_id": self.runtime_window_id, "pane_id": pane_id }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::Split { window_id, direction } => {
                self.runtime_window_requested(*window_id)?;
                let direction = match direction {
                    RuntimeSplitDirection::LeftRight => SplitDirection::LeftRight,
                    RuntimeSplitDirection::TopBottom => SplitDirection::TopBottom,
                };
                let pane_id = self.split_focused(direction, window, cx)?;
                self.runtime_result(
                    json!({ "window_id": self.runtime_window_id, "pane_id": pane_id }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::Prompt { window_id, pane_id, text, submit } => {
                self.runtime_window_requested(*window_id)?;
                let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                    return Err(ApiError::new(
                        "target_not_found",
                        format!("pane {pane_id} does not exist"),
                    ));
                };
                let view = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => {
                        panes.iter().find(|pane| pane.id == *pane_id).map(|pane| pane.view.clone())
                    },
                    _ => None,
                }
                .expect("tab_of_pane resolved a terminal pane");
                view.update(cx, |view, cx| view.runtime_prompt(text.clone(), *submit, cx))?;
                self.runtime_result(
                    json!({
                        "window_id": self.runtime_window_id,
                        "pane_id": pane_id,
                        "submitted": submit
                    }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::ReadPane { window_id, pane_id, lines } => {
                self.runtime_window_requested(*window_id)?;
                let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                    return Err(ApiError::new(
                        "target_not_found",
                        format!("pane {pane_id} does not exist"),
                    ));
                };
                let read = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == *pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .read(cx)
                        .runtime_read(self.runtime_window_id, *lines),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                }?;
                serde_json::to_value(read)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::Procs { window_id, pane_id } => {
                self.runtime_window_requested(*window_id)?;
                let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                    return Err(ApiError::new(
                        "target_not_found",
                        format!("pane {pane_id} does not exist"),
                    ));
                };
                let processes = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == *pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .read(cx)
                        .runtime_procs(self.runtime_window_id),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                }?;
                serde_json::to_value(processes)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } => {
                self.runtime_window_requested(*window_id)?;
                let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                    return Err(ApiError::new(
                        "target_not_found",
                        format!("pane {pane_id} does not exist"),
                    ));
                };
                let view = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == *pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .clone(),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                };
                let bytes_sent = view
                    .update(cx, |view, cx| view.runtime_send_key(*key, *modifiers, *repeat, cx))?;
                self.runtime_result(
                    json!({
                        "window_id": self.runtime_window_id,
                        "pane_id": pane_id,
                        "key": key.as_str(),
                        "repeat": repeat,
                        "bytes_sent": bytes_sent
                    }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::Run { window_id, pane_id, command, .. } => {
                self.runtime_window_requested(*window_id)?;
                let Some(tab_ix) = self.tab_of_pane(*pane_id) else {
                    return Err(ApiError::new(
                        "target_not_found",
                        format!("pane {pane_id} does not exist"),
                    ));
                };
                let view = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == *pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .clone(),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                };
                let run_id = view.update(cx, |view, cx| view.runtime_run(command.clone(), cx))?;
                self.runtime_result(
                    json!({
                        "window_id": self.runtime_window_id,
                        "pane_id": pane_id,
                        "run_id": run_id
                    }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::AgentStart { window_id, name, kind, cwd, session_id, command } => {
                self.runtime_window_requested(*window_id)?;
                self.runtime_hub.ensure_agent_name_available(name)?;
                let pane_id = self.add_terminal_at(cwd.clone(), None, window, cx);
                let agent = self.runtime_hub.register_agent(
                    name.clone(),
                    *kind,
                    self.runtime_window_id,
                    pane_id,
                    session_id.clone(),
                )?;
                if let Some(meta) = self.tab_meta.get_mut(self.active) {
                    meta.custom_name = Some(name.clone());
                }
                let Some(tab_ix) = self.tab_of_pane(pane_id) else {
                    self.runtime_hub.close_agent(&agent.agent_id, "launch_failed");
                    return Err(ApiError::new(
                        "action_failed",
                        "the new agent pane was not registered in the workspace",
                    ));
                };
                let view = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .clone(),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                };
                if let Err(error) =
                    view.update(cx, |view, cx| view.runtime_prompt(command.clone(), true, cx))
                {
                    self.runtime_hub.close_agent(&agent.agent_id, "launch_failed");
                    return Err(error);
                }
                self.runtime_result(
                    json!({
                        "agent": agent,
                        "window_id": self.runtime_window_id,
                        "pane_id": pane_id
                    }),
                    window,
                    cx,
                )
            },
            RuntimeCommand::AgentPrompt { agent, generation, text, submit } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                self.runtime_window_requested(Some(managed.window_id))?;
                let Some(tab_ix) = self.tab_of_pane(managed.pane_id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live pane", managed.name),
                    ));
                };
                let view = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == managed.pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .clone(),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                };
                view.update(cx, |view, cx| view.runtime_prompt(text.clone(), *submit, cx))?;
                self.runtime_result(json!({ "agent": managed }), window, cx)
            },
            RuntimeCommand::AgentRead { agent, generation, lines } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                self.runtime_window_requested(Some(managed.window_id))?;
                let Some(tab_ix) = self.tab_of_pane(managed.pane_id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live pane", managed.name),
                    ));
                };
                let read = match self.tabs.get(tab_ix) {
                    Some(WorkspaceTab::Terminal { panes, .. }) => panes
                        .iter()
                        .find(|pane| pane.id == managed.pane_id)
                        .expect("tab_of_pane resolved a terminal pane")
                        .view
                        .read(cx)
                        .runtime_read(self.runtime_window_id, *lines),
                    _ => unreachable!("tab_of_pane only resolves terminal tabs"),
                }?;
                Ok(json!({ "agent": managed, "read": read }))
            },
        }
    }

    fn runtime_window_requested(&self, requested: Option<u64>) -> Result<(), ApiError> {
        if requested.is_none_or(|id| id == self.runtime_window_id) {
            Ok(())
        } else {
            Err(ApiError::new(
                "target_not_found",
                format!("window {} does not exist", requested.expect("checked Some")),
            ))
        }
    }

    fn runtime_result(
        &self,
        action: serde_json::Value,
        window: &Window,
        cx: &App,
    ) -> Result<serde_json::Value, ApiError> {
        let snapshot = self.publish_runtime_snapshot(window, cx);
        Ok(json!({ "action": action, "snapshot": snapshot }))
    }

    fn runtime_snapshot(&self, window: &Window, cx: &App) -> RuntimeSnapshot {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let (kind, focused_pane_id, layout, panes) = match tab {
                    WorkspaceTab::Terminal { panes, tree, focused, .. } => {
                        let runtime_panes = panes
                            .iter()
                            .map(|pane| {
                                let view = pane.view.read(cx);
                                RuntimePane {
                                    id: pane.id,
                                    active: pane.id == *focused,
                                    title: view.title.clone(),
                                    cwd: view.cwd.clone(),
                                    branch: view.branch.clone(),
                                    ssh_destination: view.ssh_destination.clone(),
                                    running_program: view.running_program.clone(),
                                    agent: view.runtime_agent(),
                                    task_state: view.runtime_task_state(),
                                    state_change_seq: 0,
                                    active_run: view.runtime_active_run(),
                                    last_run: view.runtime_last_run(),
                                }
                            })
                            .collect();
                        (
                            if panes.iter().any(|pane| pane.view.read(cx).ssh_destination.is_some())
                            {
                                "ssh"
                            } else {
                                "shell"
                            },
                            Some(*focused),
                            Some(runtime_layout(tree)),
                            runtime_panes,
                        )
                    },
                    WorkspaceTab::Settings { .. } => ("settings", None, None, Vec::new()),
                    WorkspaceTab::Image { .. } => ("image", None, None, Vec::new()),
                    WorkspaceTab::Document { .. } => ("document", None, None, Vec::new()),
                    WorkspaceTab::Code { .. } => ("code", None, None, Vec::new()),
                };
                RuntimeTab {
                    index,
                    active: index == self.active,
                    label: self.tab_title(index, cx).to_string(),
                    kind: kind.to_owned(),
                    bell: self.tab_meta.get(index).is_some_and(|meta| meta.has_bell),
                    focused_pane_id,
                    layout,
                    panes,
                }
            })
            .collect();
        let focused_pane_id = self.tabs.get(self.active).and_then(|tab| match tab {
            WorkspaceTab::Terminal { focused, .. } => Some(*focused),
            _ => None,
        });
        RuntimeSnapshot::new(
            usize::from(self.window_hidden),
            vec![RuntimeWindow {
                id: self.runtime_window_id,
                focused: !self.window_hidden && window.is_window_active(),
                session_exempt: false,
                active_tab: self.active,
                focused_pane_id,
                tabs,
            }],
        )
    }

    fn publish_runtime_snapshot(&self, window: &Window, cx: &App) -> RuntimeSnapshot {
        self.runtime_hub.publish(self.runtime_snapshot(window, cx))
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
                let Some(agent) = view.runtime_agent() else {
                    continue;
                };
                let place = Path::new(view.cwd.trim())
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let label = if place.is_empty() {
                    agent.display_name
                } else {
                    format!("{} · {place}", agent.display_name)
                };
                agents.push(crate::tray::TrayAgent {
                    window,
                    pane: pane.id,
                    label,
                    needs_attention: view.runtime_task_state() == RuntimeTaskState::Attention,
                });
            }
        }
        agents
    }
}

fn runtime_layout(tree: &SplitTree<u64>) -> RuntimeLayout {
    match tree {
        SplitTree::Leaf(pane_id) => RuntimeLayout::Pane { pane_id: *pane_id },
        SplitTree::Split { direction, ratio, first, second, .. } => RuntimeLayout::Split {
            direction: match direction {
                SplitDirection::LeftRight => RuntimeSplitDirection::LeftRight,
                SplitDirection::TopBottom => RuntimeSplitDirection::TopBottom,
            },
            ratio: *ratio,
            first: Box::new(runtime_layout(first)),
            second: Box::new(runtime_layout(second)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{ResidencyCloseAction, residency_close_action};

    #[test]
    fn keep_session_hides_even_when_tray_is_off() {
        assert_eq!(
            residency_close_action(true, true, false),
            ResidencyCloseAction::Hide,
            "tray=false must still hide, never minimize"
        );
        assert_eq!(residency_close_action(true, true, true), ResidencyCloseAction::Hide);
        assert_eq!(residency_close_action(false, true, false), ResidencyCloseAction::Close);
        assert_eq!(residency_close_action(true, false, true), ResidencyCloseAction::Close);
    }
}
