//! Read-only runtime projection and narrowly-scoped control actions.
//!
//! Keeping this adapter beside `WindowContext` lets the public API observe the
//! same tab/layout/pane authority as the GUI without exposing those mutable
//! implementation types to transport threads.

use nebula_terminal::event::Notify;

use crate::display::SplitDirection;
use crate::event::TabRequest;
use crate::runtime_api::{
    ApiError, RuntimeAgent, RuntimeAgentStateSource, RuntimeKey, RuntimeKeyModifiers,
    RuntimeLayout, RuntimePane, RuntimePaneProcesses, RuntimePaneRead, RuntimeSplitDirection,
    RuntimeTab, RuntimeTaskState, RuntimeWindow,
};

use super::{Layout, TabLaunch, WindowContext};

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
        direction: RuntimeSplitDirection,
    ) -> Result<u64, ApiError> {
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
        Ok(self.tabs[self.active_tab].active_pane)
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
        if submit {
            pane.nebula_state.last_committed.clone_from(&text);
        }
        pane.notifier.notify(text.into_bytes());
        if submit {
            pane.notifier.notify(vec![b'\r']);
        }
        // Direct API input has the same semantic effect as keyboard input:
        // stale completion/attention badges must not survive a new turn.
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        pane.nebula_state.needs_attention = false;
        pane.nebula_state.finished_unseen = false;
        pane.nebula_state.failed_unseen = false;
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
        let mode = *pane.terminal.lock().mode();
        let bytes =
            crate::input::terminal_input::build_runtime_sequence(key, modifiers, repeat, mode);
        if bytes.is_empty() {
            return Err(ApiError::new(
                "input_encoding_unavailable",
                "the requested key cannot be encoded for the pane's active terminal mode",
            ));
        }
        let bytes_sent = bytes.len();
        pane.notifier.notify(bytes);
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        self.dirty = true;
        self.display.window.request_redraw();
        Ok(bytes_sent)
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
        let run = crate::runtime_api::begin_runtime_run();
        let run_id = run.run_id;
        pane.nebula_state.active_run = Some(run);
        pane.nebula_state.last_run = None;
        pane.nebula_state.last_committed.clone_from(&command);
        pane.nebula_state.touched = true;
        pane.nebula_state.awaiting_input = false;
        pane.notifier.notify(command.into_bytes());
        pane.notifier.notify(vec![b'\r']);
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
