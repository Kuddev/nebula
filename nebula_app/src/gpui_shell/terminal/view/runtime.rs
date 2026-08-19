//! GPUI 终端对 Runtime API 暴露的读取、输入与任务状态边界。

use gpui::{Context, EventEmitter as _};

use super::{SidebarActivity, TerminalView, TerminalViewEvent};

impl TerminalView {
    pub fn runtime_task_state(&self) -> crate::runtime_api::RuntimeTaskState {
        use crate::ai_agents::AgentStatus;
        use crate::runtime_api::RuntimeTaskState;
        if self.error.is_some()
            || self.exited.is_some()
            || matches!(self.ssh_stage.as_ref(), Some(crate::ssh_session::SshStage::Failed(_)))
        {
            return RuntimeTaskState::Failed;
        }
        if self.agent_status == AgentStatus::Blocked {
            return RuntimeTaskState::Attention;
        }
        if self.awaiting_input {
            return RuntimeTaskState::WaitingInput;
        }
        match self.agent_status {
            AgentStatus::Working => return RuntimeTaskState::Running,
            AgentStatus::Done => return RuntimeTaskState::Finished,
            AgentStatus::Idle => return RuntimeTaskState::Idle,
            AgentStatus::Blocked => unreachable!("handled above"),
            AgentStatus::Unknown => {},
        }
        if self.command_running
            || self.running_program.is_some()
            || self
                .ssh_stage
                .as_ref()
                .is_some_and(|stage| !matches!(stage, crate::ssh_session::SshStage::Ready))
        {
            RuntimeTaskState::Running
        } else {
            RuntimeTaskState::Idle
        }
    }

    pub fn runtime_agent(&self) -> Option<crate::runtime_api::RuntimeAgent> {
        let raw = self
            .ai_session
            .as_ref()
            .map(|identity| identity.source.as_str())
            .or(self.running_program.as_deref())?;
        let kind = crate::ai_agents::AgentKind::parse(raw)?;
        let state_source = match self.agent_status_source {
            crate::ai_agents::AgentStatusSource::Hook => {
                crate::runtime_api::RuntimeAgentStateSource::Hook
            },
            crate::ai_agents::AgentStatusSource::Screen => {
                crate::runtime_api::RuntimeAgentStateSource::Screen
            },
            crate::ai_agents::AgentStatusSource::Process
            | crate::ai_agents::AgentStatusSource::Unknown => {
                crate::runtime_api::RuntimeAgentStateSource::Process
            },
        };
        Some(crate::runtime_api::RuntimeAgent {
            agent_id: None,
            generation: None,
            name: None,
            worktree: None,
            kind: kind.slug().to_owned(),
            display_name: kind.display_name().to_owned(),
            session_id: self.ai_session.as_ref().map(|identity| identity.session_id.clone()),
            state_source,
            state_rule: self.agent_status_rule.clone(),
            hook_seen: self.agent_hook_seen,
        })
    }

    pub fn sidebar_activity(&self) -> SidebarActivity {
        match self.runtime_task_state() {
            crate::runtime_api::RuntimeTaskState::Running => SidebarActivity::Running,
            crate::runtime_api::RuntimeTaskState::Attention => SidebarActivity::Attention,
            crate::runtime_api::RuntimeTaskState::Finished => SidebarActivity::Done,
            crate::runtime_api::RuntimeTaskState::Failed => SidebarActivity::Failed,
            crate::runtime_api::RuntimeTaskState::Idle
            | crate::runtime_api::RuntimeTaskState::WaitingInput => SidebarActivity::Idle,
        }
    }

    fn ensure_runtime_readable(&self) -> Result<(), crate::runtime_api::ApiError> {
        if self.ssh_destination.is_none() {
            return Ok(());
        }
        match self.ssh_stage.as_ref() {
            Some(crate::ssh_session::SshStage::Ready) => Ok(()),
            Some(crate::ssh_session::SshStage::Failed(reason)) => Err(
                crate::runtime_api::ApiError::new("ssh_not_ready", "SSH pane is in a failed state")
                    .details(serde_json::json!({ "reason": reason })),
            ),
            stage => Err(crate::runtime_api::ApiError::new(
                "ssh_not_ready",
                format!("SSH pane is not ready for terminal reads: {stage:?}"),
            )),
        }
    }

    fn runtime_key_sequence(
        &self,
        key: crate::runtime_api::RuntimeKey,
        modifiers: crate::runtime_api::RuntimeKeyModifiers,
        repeat: u16,
    ) -> Result<Vec<u8>, crate::runtime_api::ApiError> {
        let bytes = crate::input::terminal_input::build_runtime_sequence(
            key,
            modifiers,
            repeat,
            self.term_mode(),
        );
        if bytes.is_empty() {
            return Err(crate::runtime_api::ApiError::new(
                "input_encoding_unavailable",
                "the requested key cannot be encoded for the pane's active terminal mode",
            ));
        }
        Ok(bytes)
    }

    pub fn runtime_read(
        &self,
        window_id: u64,
        lines: usize,
    ) -> Result<crate::runtime_api::RuntimePaneRead, crate::runtime_api::ApiError> {
        self.ensure_runtime_readable()?;
        let Some(session) = &self.session else {
            return Err(crate::runtime_api::ApiError::new(
                "runtime_unavailable",
                "terminal session is unavailable for this pane",
            ));
        };
        let term = session.term.lock();
        Ok(crate::runtime_api::capture_terminal_tail(
            &term,
            window_id,
            self.pane_id,
            lines,
            self.runtime_task_state(),
            self.exited.is_some(),
            self.exited.clone(),
        ))
    }

    pub fn runtime_procs(
        &self,
        window_id: u64,
    ) -> Result<crate::runtime_api::RuntimePaneProcesses, crate::runtime_api::ApiError> {
        if self.ssh_destination.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "remote_process_unavailable",
                "pane.procs cannot infer a remote process tree from the local SSH transport",
            ));
        }
        let Some(session) = &self.session else {
            return Err(crate::runtime_api::ApiError::new(
                "runtime_unavailable",
                "terminal session is unavailable for this pane",
            ));
        };
        crate::runtime_api::capture_process_tree(window_id, self.pane_id, session.shell_pid)
    }

    pub fn runtime_send_key(
        &mut self,
        key: crate::runtime_api::RuntimeKey,
        modifiers: crate::runtime_api::RuntimeKeyModifiers,
        repeat: u16,
        cx: &mut Context<Self>,
    ) -> Result<usize, crate::runtime_api::ApiError> {
        if let Some(reason) = &self.exited {
            return Err(crate::runtime_api::ApiError::new(
                "invalid_state",
                format!("pane has exited: {reason}"),
            ));
        }
        self.ensure_runtime_readable()?;
        let bytes = self.runtime_key_sequence(key, modifiers, repeat)?;
        let bytes_sent = bytes.len();
        self.write_input(bytes, cx);
        Ok(bytes_sent)
    }

    pub fn runtime_run(
        &mut self,
        command: String,
        cx: &mut Context<Self>,
    ) -> Result<u64, crate::runtime_api::ApiError> {
        crate::runtime_api::validate_command_line(&command)?;
        if self.ssh_destination.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "exit_code_unavailable",
                "pane.run is unavailable for native SSH panes because the remote integration does not report exit codes",
            ));
        }
        if let Some(reason) = &self.exited {
            return Err(crate::runtime_api::ApiError::new(
                "invalid_state",
                format!("pane has exited: {reason}"),
            ));
        }
        self.ensure_runtime_readable()?;
        if self.command_running || self.active_run.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "run_in_progress",
                "the pane is already running a command",
            ));
        }
        let mut bytes = command.as_bytes().to_vec();
        bytes.extend(self.runtime_key_sequence(
            crate::runtime_api::RuntimeKey::Enter,
            crate::runtime_api::RuntimeKeyModifiers::default(),
            1,
        )?);
        let run = crate::runtime_api::begin_runtime_run();
        let run_id = run.run_id;
        self.active_run = Some(run);
        self.last_run = None;
        self.command_running = true;
        self.suggest.last_committed.clone_from(&command);
        self.write_input(bytes, cx);
        cx.emit(TerminalViewEvent::TitleChanged);
        Ok(run_id)
    }

    pub fn runtime_active_run(&self) -> Option<crate::runtime_api::RuntimePaneRun> {
        self.active_run
    }

    pub fn runtime_last_run(&self) -> Option<crate::runtime_api::RuntimeRunOutcome> {
        self.last_run.clone()
    }

    pub fn runtime_prompt(
        &mut self,
        text: String,
        submit: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        crate::runtime_api::validate_prompt(&text)?;
        if let Some(reason) = &self.exited {
            return Err(crate::runtime_api::ApiError::new(
                "invalid_state",
                format!("pane has exited: {reason}"),
            ));
        }
        self.ensure_runtime_readable()?;
        if self.session.is_none() {
            return Err(crate::runtime_api::ApiError::new(
                "runtime_unavailable",
                "terminal session is unavailable for this pane",
            ));
        }
        let recognized_agent = submit && self.runtime_agent().is_some();
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            // Codex/Claude 可启用 kitty 或 Win32 输入协议；裸 CR 只在 legacy VT
            // 下等价于 Enter，必须按 pane 当前模式编码，才能真正提交输入框。
            bytes.extend(self.runtime_key_sequence(
                crate::runtime_api::RuntimeKey::Enter,
                crate::runtime_api::RuntimeKeyModifiers::default(),
                1,
            )?);
            self.suggest.last_committed.clone_from(&text);
        }
        self.write_input(bytes, cx);
        if submit {
            // 极短命令可能在 120ms runtime pump 的两拍之间完成。提交动作
            // 本身先建立 Running 边沿，保证 prompt --wait 的 after_seq 不会
            // 仍盯着提交前 Idle；真实 shell/hook 结束事件负责把它归位。
            self.awaiting_input = false;
            self.command_running = true;
            if recognized_agent {
                self.agent_status = crate::ai_agents::AgentStatus::Working;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
                self.agent_status_rule = None;
                self.idle_screen_streak = 0;
            }
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        Ok(())
    }
}
