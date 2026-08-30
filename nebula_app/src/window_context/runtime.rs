//! Read-only runtime projection and narrowly-scoped control actions.
//!
//! Keeping this adapter beside `WindowContext` lets the public API observe the
//! same tab/layout/pane authority as the GUI without exposing those mutable
//! implementation types to transport threads.

use nebula_terminal::event::Notify;
use nebula_terminal::grid::Dimensions;
use nebula_terminal::index::{Column, Line, Point};
use nebula_terminal::term::TermMode;
use serde_json::{Value, json};

use crate::display::SplitDirection;
use crate::event::TabRequest;
use crate::runtime_api::{
    ApiError, RuntimeAgent, RuntimeAgentStateSource, RuntimeKey, RuntimeKeyModifiers,
    RuntimeLayout, RuntimePane, RuntimePaneProcesses, RuntimePaneRead, RuntimeSplitDirection,
    RuntimeTab, RuntimeTaskState, RuntimeWindow,
};

use super::{Layout, TabLaunch, WindowContext};

fn runtime_key_sequence(
    pane: &super::Pane,
    key: RuntimeKey,
    modifiers: RuntimeKeyModifiers,
    repeat: u16,
) -> Result<Vec<u8>, ApiError> {
    let mode = *pane.terminal.lock().mode();
    let bytes = crate::input::terminal_input::build_runtime_sequence(key, modifiers, repeat, mode);
    if bytes.is_empty() {
        return Err(ApiError::new(
            "input_encoding_unavailable",
            "the requested key cannot be encoded for the pane's active terminal mode",
        ));
    }
    Ok(bytes)
}

fn runtime_screen_snapshot(pane: &super::Pane) -> Option<String> {
    let term = pane.terminal.lock();
    let lines = term.screen_lines();
    if lines == 0 || term.columns() == 0 {
        return None;
    }
    let start = Point::new(Line(0), Column(0));
    let end = Point::new(Line(lines as i32 - 1), Column(term.columns().saturating_sub(1)));
    Some(term.bounds_to_string(start, end))
}

fn runtime_close_confirmation(process: String, details: Value) -> ApiError {
    ApiError::new(
        "confirmation_required",
        format!("{process} is still running; refusing to close it without user confirmation"),
    )
    .details(details)
}

impl WindowContext {
    pub(crate) fn runtime_snapshot(&self) -> RuntimeWindow {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let special = tab.doc.is_some() || tab.image.is_some() || tab.settings;
                let mut pane_ids = Vec::new();
                if !special {
                    tab.layout.leaves(&mut pane_ids);
                }
                let panes = pane_ids
                    .iter()
                    .filter_map(|id| self.pane(*id))
                    .map(|pane| RuntimePane {
                        id: pane.id,
                        active: pane.id == tab.active_pane,
                        title: pane.title.clone(),
                        cwd: pane.nebula_state.cwd.clone(),
                        branch: pane.nebula_state.branch.clone(),
                        ssh_destination: pane.ssh_destination.clone(),
                        running_program: pane.nebula_state.running_program.clone(),
                        agent: runtime_agent(pane),
                        task_state: task_state(pane),
                        // Seeded to 0; the hub stamps the real transition
                        // counter at publish time, where the previous snapshot
                        // is available to compare against.
                        state_change_seq: 0,
                        active_run: pane.nebula_state.active_run,
                        last_run: pane.nebula_state.last_run.clone(),
                    })
                    .collect();
                let label = tab.custom_name.clone().unwrap_or_else(|| {
                    self.pane(tab.active_pane)
                        .map(Self::chrome_tab_label)
                        .unwrap_or_else(|| tab_kind(&tab.launch).to_owned())
                });
                RuntimeTab {
                    index,
                    active: index == self.active_tab,
                    label,
                    kind: tab_kind(&tab.launch).to_owned(),
                    bell: tab.has_bell,
                    focused_pane_id: (!special).then_some(tab.active_pane),
                    zoomed_pane_id: self.zoom.filter(|pane_id| pane_ids.contains(pane_id)),
                    layout: (!special).then(|| runtime_layout(&tab.layout)),
                    panes,
                }
            })
            .collect();

        RuntimeWindow {
            id: self.id().into(),
            focused: self.display.window.has_focus(),
            session_exempt: self.session_exempt,
            active_tab: self.active_tab,
            focused_pane_id: self
                .tabs
                .get(self.active_tab)
                .filter(|tab| tab.doc.is_none() && tab.image.is_none() && !tab.settings)
                .map(|tab| tab.active_pane),
            tabs,
        }
    }

    pub(crate) fn runtime_contains_pane(&self, pane_id: u64) -> bool {
        self.pane(pane_id).is_some()
    }

    pub(crate) fn runtime_focus(&mut self, pane_id: Option<u64>) -> Result<(), ApiError> {
        if let Some(pane_id) = pane_id {
            let tab_index = self.tabs.iter().position(|tab| {
                let mut ids = Vec::new();
                tab.layout.leaves(&mut ids);
                ids.contains(&pane_id)
            });
            let Some(tab_index) = tab_index else {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("pane {pane_id} does not belong to the target window"),
                ));
            };
            self.select_tab(tab_index);
            self.tabs[tab_index].active_pane = pane_id;
        }
        self.display.window.focus_window();
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_new_tab(
        &mut self,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<u64, ApiError> {
        let before = self.tabs.len();
        let request = match cwd {
            Some(dir) => TabRequest::NewAtDirectory(dir),
            None => TabRequest::New,
        };
        self.handle_tab_request(request);
        if self.tabs.len() == before {
            return Err(ApiError::new(
                "action_failed",
                "the default shell could not be started for a new tab",
            ));
        }
        Ok(self.tabs[self.active_tab].active_pane)
    }

    pub(crate) fn runtime_split(
        &mut self,
        pane_id: Option<u64>,
        direction: RuntimeSplitDirection,
    ) -> Result<(u64, u64), ApiError> {
        if let Some(pane_id) = pane_id {
            let tab_index = self.tabs.iter().position(|tab| {
                let mut ids = Vec::new();
                tab.layout.leaves(&mut ids);
                ids.contains(&pane_id)
            });
            let Some(tab_index) = tab_index else {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("pane {pane_id} does not belong to the target window"),
                ));
            };
            self.select_tab(tab_index);
            self.tabs[tab_index].active_pane = pane_id;
        }
        let special = self
            .tabs
            .get(self.active_tab)
            .is_none_or(|tab| tab.doc.is_some() || tab.image.is_some() || tab.settings);
        if special {
            return Err(ApiError::new(
                "invalid_state",
                "document, image, and settings tabs cannot be split",
            ));
        }
        let source_pane_id = self.tabs[self.active_tab].active_pane;
        let before = self.panes.len();
        let direction = match direction {
            RuntimeSplitDirection::LeftRight => SplitDirection::LeftRight,
            RuntimeSplitDirection::TopBottom => SplitDirection::TopBottom,
        };
        self.handle_tab_request(TabRequest::SplitToggle(direction));
        if self.panes.len() == before {
            return Err(ApiError::new(
                "action_failed",
                "the default shell could not be started for a new pane",
            ));
        }
        Ok((source_pane_id, self.tabs[self.active_tab].active_pane))
    }

    pub(crate) fn runtime_close_window(&mut self) -> Result<(), ApiError> {
        let pane_ids: Vec<_> = self.panes.iter().map(|pane| pane.id).collect();
        if let Some(process) = self.busy_process_in(&pane_ids) {
            return Err(runtime_close_confirmation(
                process,
                json!({ "target": "window", "window_id": u64::from(self.id()) }),
            ));
        }
        while !self.tabs.is_empty() {
            let last = self.tabs.len() - 1;
            self.close_tab(last);
        }
        self.display.window.hold = false;
        Ok(())
    }

    pub(crate) fn runtime_close_tab(&mut self, tab_index: usize) -> Result<bool, ApiError> {
        let Some(tab) = self.tabs.get(tab_index) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("tab {tab_index} does not exist in window {}", u64::from(self.id())),
            ));
        };
        let mut pane_ids = Vec::new();
        tab.layout.leaves(&mut pane_ids);
        if let Some(process) = self.busy_process_in(&pane_ids) {
            return Err(runtime_close_confirmation(
                process,
                json!({
                    "target": "tab",
                    "window_id": u64::from(self.id()),
                    "tab_index": tab_index
                }),
            ));
        }
        Ok(self.close_tab(tab_index))
    }

    pub(crate) fn runtime_rename_tab(
        &mut self,
        tab_index: usize,
        name: String,
    ) -> Result<(), ApiError> {
        let Some(tab) = self.tabs.get_mut(tab_index) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("tab {tab_index} does not exist in window {}", u64::from(self.id())),
            ));
        };
        let name = name.trim();
        tab.custom_name = (!name.is_empty()).then(|| name.to_owned());
        self.sync_chrome_tabs();
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_move_tab(
        &mut self,
        tab_index: usize,
        to_index: usize,
    ) -> Result<(), ApiError> {
        let len = self.tabs.len();
        if tab_index >= len || to_index >= len {
            return Err(ApiError::new(
                "target_not_found",
                format!(
                    "tab move {tab_index} -> {to_index} is outside window {} with {len} tabs",
                    u64::from(self.id())
                ),
            ));
        }
        self.move_tab(tab_index, to_index);
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_close_pane(&mut self, pane_id: u64) -> Result<bool, ApiError> {
        if self.pane(pane_id).is_none() {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        }
        if let Some(process) = self.busy_process_in(&[pane_id]) {
            return Err(runtime_close_confirmation(
                process,
                json!({
                    "target": "pane",
                    "window_id": u64::from(self.id()),
                    "pane_id": pane_id
                }),
            ));
        }
        Ok(self.close_pane(pane_id))
    }

    pub(crate) fn runtime_set_zoom(&mut self, pane_id: u64, zoomed: bool) -> Result<(), ApiError> {
        let Some(tab_index) = self.tabs.iter().position(|tab| {
            let mut panes = Vec::new();
            tab.layout.leaves(&mut panes);
            panes.contains(&pane_id)
        }) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        if zoomed && !matches!(&self.tabs[tab_index].layout, Layout::Split { .. }) {
            return Err(ApiError::new(
                "invalid_state",
                "a single-pane tab is already full size and cannot be zoomed",
            ));
        }

        self.select_tab(tab_index);
        self.tabs[tab_index].active_pane = pane_id;
        let next = zoomed.then_some(pane_id);
        if self.zoom != next {
            self.zoom = next;
            self.resize_active_layout();
        }
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_resize_pane(&mut self, pane_id: u64, ratio: f32) -> Result<(), ApiError> {
        let Some(tab_index) = self.tabs.iter().position(|tab| {
            let mut panes = Vec::new();
            tab.layout.leaves(&mut panes);
            panes.contains(&pane_id)
        }) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        if !self.tabs[tab_index].layout.set_leaf_parent_ratio(pane_id, ratio) {
            return Err(ApiError::new(
                "invalid_state",
                "pane.resize requires a pane with a direct parent split",
            ));
        }
        if tab_index == self.active_tab {
            self.resize_active_layout();
        }
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_prompt(
        &mut self,
        pane_id: u64,
        text: String,
        submit: bool,
    ) -> Result<(), ApiError> {
        crate::runtime_api::validate_prompt(&text)?;
        let Some(index) = self.pane_index(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        let pane = &mut self.panes[index];
        if submit && pane.nebula_state.runtime_submit_barrier.is_some() {
            return Err(ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        let recognized_agent = submit && runtime_agent(pane).is_some();
        let mode = *pane.terminal.lock().mode();
        let mut bytes = crate::input::terminal_input::build_runtime_text_sequence(&text, mode);
        if submit {
            let submit_bytes =
                runtime_key_sequence(pane, RuntimeKey::Enter, RuntimeKeyModifiers::default(), 1)?;
            if text.is_empty() {
                bytes.extend(submit_bytes);
            } else {
                pane.nebula_state.runtime_submit_barrier =
                    Some(crate::display::state::RuntimeSubmitBarrier {
                        baseline_screen: runtime_screen_snapshot(pane).unwrap_or_default(),
                        submit_bytes,
                    });
            }
        }
        if submit {
            pane.nebula_state.last_committed.clone_from(&text);
        }
        pane.notifier.notify(bytes);
        // Direct API input has the same semantic effect as keyboard input:
        // stale completion/attention badges must not survive a new turn.
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        pane.nebula_state.needs_attention = false;
        pane.nebula_state.finished_unseen = false;
        pane.nebula_state.failed_unseen = false;
        if recognized_agent {
            pane.nebula_state.agent_status = crate::ai_agents::AgentStatus::Working;
            pane.nebula_state.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
            pane.nebula_state.agent_status_rule = None;
            pane.nebula_state.agent_runtime_submit_pending = true;
            pane.nebula_state.idle_screen_streak = 0;
            pane.nebula_state.command_started.get_or_insert_with(std::time::Instant::now);
        }
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_paste(
        &mut self,
        pane_id: u64,
        text: String,
        submit: bool,
        require_agent: bool,
    ) -> Result<(), ApiError> {
        crate::runtime_api::validate_paste_text(&text)?;
        let Some(index) = self.pane_index(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        let pane = &mut self.panes[index];
        if let Some(destination) = pane.ssh_destination.as_deref() {
            return Err(ApiError::new(
                "remote_target_refused",
                format!(
                    "program-provided local content cannot be sent automatically to remote pane {destination:?}"
                ),
            ));
        }
        let recognized_agent = runtime_agent(pane).is_some();
        if require_agent && !recognized_agent {
            return Err(ApiError::new(
                "invalid_target",
                "multi-line Agent input requires a live Agent pane",
            ));
        }
        if pane.nebula_state.runtime_submit_barrier.is_some() {
            return Err(ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        let mode = *pane.terminal.lock().mode();
        if !mode.contains(TermMode::BRACKETED_PASTE) {
            return Err(ApiError::new(
                "unsafe_input_mode",
                "the target pane is not ready for safe multi-line input",
            ));
        }

        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(normalized.replace("\x1b[201~", "").as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        if submit {
            let submit_bytes =
                runtime_key_sequence(pane, RuntimeKey::Enter, RuntimeKeyModifiers::default(), 1)?;
            pane.nebula_state.runtime_submit_barrier =
                Some(crate::display::state::RuntimeSubmitBarrier {
                    baseline_screen: runtime_screen_snapshot(pane).unwrap_or_default(),
                    submit_bytes,
                });
        }
        pane.notifier.notify(bytes);
        pane.nebula_state.touched = true;
        if submit {
            pane.nebula_state.awaiting_input = false;
            pane.nebula_state.needs_attention = false;
            pane.nebula_state.finished_unseen = false;
            pane.nebula_state.failed_unseen = false;
            if recognized_agent {
                pane.nebula_state.agent_status = crate::ai_agents::AgentStatus::Working;
                pane.nebula_state.agent_status_source =
                    crate::ai_agents::AgentStatusSource::Process;
                pane.nebula_state.agent_status_rule = None;
                pane.nebula_state.agent_runtime_submit_pending = true;
                pane.nebula_state.idle_screen_streak = 0;
                pane.nebula_state.command_started.get_or_insert_with(std::time::Instant::now);
            }
        }
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(())
    }

    pub(crate) fn runtime_read(
        &self,
        pane_id: u64,
        lines: usize,
    ) -> Result<RuntimePaneRead, ApiError> {
        let Some(pane) = self.pane(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        let term = pane.terminal.lock();
        Ok(crate::runtime_api::capture_terminal_tail(
            &term,
            self.id().into(),
            pane_id,
            lines,
            task_state(pane),
            false,
            None,
        ))
    }

    pub(crate) fn runtime_procs(&self, pane_id: u64) -> Result<RuntimePaneProcesses, ApiError> {
        let Some(pane) = self.pane(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        if pane.ssh_destination.is_some() {
            return Err(ApiError::new(
                "remote_process_unavailable",
                "pane.procs cannot infer a remote process tree from the local SSH transport",
            ));
        }
        crate::runtime_api::capture_process_tree(self.id().into(), pane_id, pane.shell_pid)
    }

    pub(crate) fn runtime_send_key(
        &mut self,
        pane_id: u64,
        key: RuntimeKey,
        modifiers: RuntimeKeyModifiers,
        repeat: u16,
    ) -> Result<usize, ApiError> {
        let Some(index) = self.pane_index(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        let pane = &mut self.panes[index];
        let bytes = runtime_key_sequence(pane, key, modifiers, repeat)?;
        let bytes_sent = bytes.len();
        pane.notifier.notify(bytes);
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(bytes_sent)
    }

    pub(crate) fn runtime_exec_context(
        &self,
        pane_id: u64,
    ) -> Result<(crate::runtime_exec::PaneExecContext, String), ApiError> {
        let pane = self.pane(pane_id).ok_or_else(|| {
            ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            )
        })?;
        if pane.ssh_destination.is_some() {
            return Err(ApiError::new(
                "remote_exec_unsupported",
                "pane.exec is local-only and cannot execute on an SSH pane",
            )
            .details(json!({
                "pane_id": pane_id,
                "ssh_destination": &pane.ssh_destination
            })));
        }
        let context = pane.exec_context.clone().ok_or_else(|| {
            ApiError::new(
                "exec_context_unavailable",
                "the target pane has no local process execution context",
            )
        })?;
        Ok((context, pane.nebula_state.cwd.clone()))
    }

    pub(crate) fn runtime_run(&mut self, pane_id: u64, command: String) -> Result<u64, ApiError> {
        crate::runtime_api::validate_command_line(&command)?;
        let Some(index) = self.pane_index(pane_id) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to the target window"),
            ));
        };
        let pane = &mut self.panes[index];
        if pane.ssh_destination.is_some() {
            return Err(ApiError::new(
                "exit_code_unavailable",
                "pane.run is unavailable for native SSH panes because the remote integration does not report exit codes",
            ));
        }
        if pane.nebula_state.command_started.is_some() || pane.nebula_state.active_run.is_some() {
            return Err(ApiError::new("run_in_progress", "the pane is already running a command"));
        }
        if pane.nebula_state.runtime_submit_barrier.is_some() {
            return Err(ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        let mode = *pane.terminal.lock().mode();
        let bytes = crate::input::terminal_input::build_runtime_text_sequence(&command, mode);
        let submit_bytes =
            runtime_key_sequence(pane, RuntimeKey::Enter, RuntimeKeyModifiers::default(), 1)?;
        pane.nebula_state.runtime_submit_barrier =
            Some(crate::display::state::RuntimeSubmitBarrier {
                baseline_screen: runtime_screen_snapshot(pane).unwrap_or_default(),
                submit_bytes,
            });
        let run = crate::runtime_api::begin_runtime_run();
        let run_id = run.run_id;
        pane.nebula_state.active_run = Some(run);
        pane.nebula_state.last_run = None;
        pane.nebula_state.last_committed.clone_from(&command);
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        pane.notifier.notify(bytes);
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(run_id)
    }

    pub(crate) fn runtime_set_tab_name(
        &mut self,
        pane_id: u64,
        name: String,
    ) -> Result<(), ApiError> {
        let Some(tab) = self.tabs.iter_mut().find(|tab| {
            let mut ids = Vec::new();
            tab.layout.leaves(&mut ids);
            ids.contains(&pane_id)
        }) else {
            return Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not belong to a terminal tab"),
            ));
        };
        tab.custom_name = Some(name);
        Ok(())
    }

    pub(crate) fn runtime_discard_agent_tab(&mut self, pane_id: u64) {
        let Some(tab_index) = self.tabs.iter().position(|tab| {
            let mut panes = Vec::new();
            tab.layout.leaves(&mut panes);
            panes == [pane_id] && tab.doc.is_none() && tab.image.is_none() && !tab.settings
        }) else {
            return;
        };
        // 只回收本次启动产生的单 Pane 终端；若结构已变化，宁可保留也不
        // 能把用户并行操作创建或拆分出的 Tab 一并关闭。
        let _ = self.close_tab(tab_index);
    }

    pub(crate) fn runtime_flush_pending_submit(&mut self, pane_id: Option<u64>) {
        let pane_id = pane_id.unwrap_or_else(|| self.focused_pane_id());
        let Some(index) = self.pane_index(pane_id) else { return };
        let ready = {
            let pane = &self.panes[index];
            let Some(pending) = pane.nebula_state.runtime_submit_barrier.as_ref() else {
                return;
            };
            runtime_screen_snapshot(pane).is_some_and(|screen| screen != pending.baseline_screen)
        };
        if !ready {
            return;
        }
        let pane = &mut self.panes[index];
        let pending = pane.nebula_state.runtime_submit_barrier.take().expect("checked above");
        pane.notifier.notify(pending.submit_bytes);
    }
}

fn runtime_agent(pane: &super::Pane) -> Option<RuntimeAgent> {
    let state = &pane.nebula_state;
    let raw = state
        .ai_session
        .as_ref()
        .map(|identity| identity.source.as_str())
        .or(state.running_program.as_deref())?;
    let kind = crate::ai_agents::AgentKind::parse(raw)?;
    let state_source = match state.agent_status_source {
        crate::ai_agents::AgentStatusSource::Hook => RuntimeAgentStateSource::Hook,
        crate::ai_agents::AgentStatusSource::Screen => RuntimeAgentStateSource::Screen,
        crate::ai_agents::AgentStatusSource::Process
        | crate::ai_agents::AgentStatusSource::Unknown => RuntimeAgentStateSource::Process,
    };
    Some(RuntimeAgent {
        agent_id: None,
        generation: None,
        name: None,
        worktree: None,
        kind: kind.slug().to_owned(),
        display_name: kind.display_name().to_owned(),
        session_id: state.ai_session.as_ref().map(|identity| identity.session_id.clone()),
        state_source,
        state_rule: state.agent_status_rule.clone(),
        hook_seen: state.agent_hook_seen,
    })
}

fn task_state(pane: &super::Pane) -> RuntimeTaskState {
    let state = &pane.nebula_state;
    if state.needs_attention {
        RuntimeTaskState::Attention
    } else if state.awaiting_input {
        RuntimeTaskState::WaitingInput
    } else if state.command_started.is_some() {
        RuntimeTaskState::Running
    } else if state.failed_unseen {
        RuntimeTaskState::Failed
    } else if state.finished_unseen {
        RuntimeTaskState::Finished
    } else {
        RuntimeTaskState::Idle
    }
}

fn tab_kind(launch: &TabLaunch) -> &'static str {
    match launch {
        TabLaunch::Default => "shell",
        TabLaunch::Profile(_) => "profile",
        TabLaunch::Shell { .. } => "shell",
        TabLaunch::Ssh(_) => "ssh",
        TabLaunch::Document(_) => "document",
        TabLaunch::Image(_) => "image",
        TabLaunch::Settings => "settings",
    }
}

fn runtime_layout(layout: &Layout) -> RuntimeLayout {
    match layout {
        Layout::Leaf(pane_id) => RuntimeLayout::Pane { pane_id: *pane_id },
        Layout::Split { direction, ratio, first, second, .. } => RuntimeLayout::Split {
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
