//! GPUI 终端对 Runtime API 暴露的读取、输入与任务状态边界。

use gpui::{Context, EventEmitter as _};
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Line, Point as TermPoint};

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
        if self.pending_runtime_submit.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        let bytes =
            crate::input::terminal_input::build_runtime_text_sequence(&command, self.term_mode());
        let submit_bytes = self.runtime_key_sequence(
            crate::runtime_api::RuntimeKey::Enter,
            crate::runtime_api::RuntimeKeyModifiers::default(),
            1,
        )?;
        self.pending_runtime_submit = Some(crate::display::state::RuntimeSubmitBarrier {
            baseline_screen: self.runtime_screen_snapshot().unwrap_or_default(),
            submit_bytes,
        });
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
        if submit && self.pending_runtime_submit.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        let recognized_agent = submit && self.runtime_agent().is_some();
        let mut bytes =
            crate::input::terminal_input::build_runtime_text_sequence(&text, self.term_mode());
        if submit {
            // Codex/Claude 可启用 kitty 或 Win32 输入协议；裸 CR 只在 legacy VT
            // 下等价于 Enter。Win32 模式下文本也已编码为 VK_PACKET 记录，
            // 整个提交因此是一条同质协议流，不依赖 ConPTY 的读取边界。
            let submit_bytes = self.runtime_key_sequence(
                crate::runtime_api::RuntimeKey::Enter,
                crate::runtime_api::RuntimeKeyModifiers::default(),
                1,
            )?;
            if text.is_empty() {
                bytes.extend(submit_bytes);
            } else {
                self.pending_runtime_submit = Some(crate::display::state::RuntimeSubmitBarrier {
                    baseline_screen: self.runtime_screen_snapshot().unwrap_or_default(),
                    submit_bytes,
                });
            }
        }
        if submit {
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
                self.agent_runtime_submit_pending = true;
                self.idle_screen_streak = 0;
            }
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        Ok(())
    }

    pub fn ai_fork_command(&self) -> Option<String> {
        let identity = self.ai_session.as_ref()?;
        crate::ai_agents::AgentKind::parse(&identity.source)?.fork_command(&identity.session_id)
    }

    /// 冷恢复把快照里的 hook 身份种回 pane，右键分叉不必再等下一条事件。
    pub fn seed_ai_session(&mut self, source: String, session_id: String, cx: &mut Context<Self>) {
        if session_id.is_empty() {
            return;
        }
        self.ai_session = Some(crate::display::AiSessionIdentity { source, session_id });
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }

    /// Queue a full shell command. Like cold resume, input may arrive before
    /// the first prompt; ConPTY preserves ordering until the shell reads it.
    pub fn run_command(&mut self, command: String, cx: &mut Context<Self>) {
        if command.is_empty() || self.exited.is_some() {
            return;
        }
        let mut bytes = command.into_bytes();
        bytes.push(b'\r');
        self.write_input(bytes, cx);
    }

    /// Apply one lifecycle event already routed to this pane by the workspace.
    ///
    /// 旧壳 `WindowContext::handle_ai_hook` 状态机的忠实移植：不能把所有
    /// 非 SessionEnd 事件压平成 running，否则 TurnDone 后 spinner 不会停。
    pub fn handle_ai_hook(&mut self, event: &crate::ai_hook::AiHookEvent, cx: &mut Context<Self>) {
        use crate::ai_agents::AgentStatus;
        use crate::ai_hook::AiHookKind;

        // SessionEnd 可能是第一次携带最终权威 id 的事件；必须先落盘再清理
        // pane 现场，否则 CLI 正常退出后反而无法冷恢复。
        if let Some(id) = event.session_id.as_deref() {
            if let Err(error) =
                crate::ai_sessions::record_hook_session(&event.source, id, &self.cwd, None)
            {
                log::warn!("agent session index: could not record {} {id}: {error}", event.source);
            }
        }
        match event.kind {
            AiHookKind::SessionEnd => {
                self.ai_session = None;
                self.running_program = None;
                self.agent_status = AgentStatus::Unknown;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Unknown;
                self.agent_status_rule = None;
                self.agent_hook_seen = false;
                self.idle_screen_streak = 0;
                self.agent_runtime_submit_pending = false;
            },
            kind => {
                self.running_program = Some(event.source.clone());
                self.agent_hook_seen = true;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Hook;
                self.agent_status_rule = None;
                // 精确边沿抵达 = 屏幕检测的空闲计数作废（上一回合攒下的
                // 拍数不能把新回合的第一个空闲闪现立即降级）。
                self.idle_screen_streak = 0;
                if !matches!(kind, AiHookKind::SessionStart) {
                    self.agent_runtime_submit_pending = false;
                }
                if let Some(id) = event.session_id.as_deref() {
                    self.ai_session = Some(crate::display::AiSessionIdentity {
                        source: event.source.clone(),
                        session_id: id.to_owned(),
                    });
                }
                match kind {
                    AiHookKind::SessionStart => self.agent_status = AgentStatus::Idle,
                    AiHookKind::PromptSubmit | AiHookKind::ToolComplete => {
                        self.agent_status = AgentStatus::Working;
                    },
                    AiHookKind::TurnDone | AiHookKind::NeedsAttention => {
                        let screen_asks =
                            event.kind == AiHookKind::TurnDone && self.screen_tail_asks();
                        self.agent_status =
                            if event.kind == AiHookKind::NeedsAttention || screen_asks {
                                AgentStatus::Blocked
                            } else {
                                AgentStatus::Done
                            };
                    },
                    AiHookKind::SessionEnd => unreachable!("handled above"),
                }
            },
        }
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }

    fn screen_tail_asks(&self) -> bool {
        let Some(session) = &self.session else { return false };
        let term = session.term.lock();
        let lines = term.screen_lines();
        let take = lines.min(15);
        let start = TermPoint::new(Line((lines - take) as i32), Column(0));
        let end = TermPoint::new(Line(lines as i32 - 1), Column(term.columns().saturating_sub(1)));
        crate::ai_hook::tail_looks_like_question(&term.bounds_to_string(start, end))
    }

    /// 1 Hz 屏幕看门狗：hook 提供精确边沿，屏幕补偿丢失边沿和无 hook 客户端。
    pub fn refresh_agent_screen_state(&mut self, cx: &mut Context<Self>) {
        use crate::ai_agents::AgentStatus;

        // Wakeup 会被 synchronized-update 合并；1 Hz 看门狗提供可靠的
        // Grid 后置检查，确保 Runtime 文本回显后一定能补发 Enter。
        self.flush_pending_runtime_submit(cx);
        let Some(program) = self.running_program.clone() else {
            self.idle_screen_streak = 0;
            return;
        };
        if crate::ai_agents::AgentKind::parse(&program).is_none() {
            return;
        }
        let Some(session) = &self.session else { return };
        let screen = {
            let term = session.term.lock();
            let lines = term.screen_lines();
            if lines == 0 || term.columns() == 0 {
                return;
            }
            let take = lines.min(24);
            let start = TermPoint::new(Line((lines - take) as i32), Column(0));
            let end =
                TermPoint::new(Line(lines as i32 - 1), Column(term.columns().saturating_sub(1)));
            term.bounds_to_string(start, end)
        };
        let Some(detection) = crate::ai_agents::detect(&program, &screen) else { return };

        if detection.status == AgentStatus::Idle && self.agent_runtime_submit_pending {
            self.idle_screen_streak = 0;
            return;
        }
        if matches!(detection.status, AgentStatus::Working | AgentStatus::Blocked) {
            self.agent_runtime_submit_pending = false;
        }

        let next = match detection.status {
            AgentStatus::Idle => {
                if self.agent_hook_seen
                    && matches!(self.agent_status, AgentStatus::Done | AgentStatus::Blocked)
                {
                    return;
                }
                if self.agent_status == AgentStatus::Working {
                    self.idle_screen_streak = self.idle_screen_streak.saturating_add(1);
                    if self.idle_screen_streak < 2 {
                        return;
                    }
                    AgentStatus::Done
                } else {
                    AgentStatus::Idle
                }
            },
            status @ (AgentStatus::Working | AgentStatus::Blocked) => {
                self.idle_screen_streak = 0;
                status
            },
            AgentStatus::Done | AgentStatus::Unknown => {
                self.idle_screen_streak = 0;
                return;
            },
        };
        if next != self.agent_status {
            log::debug!(
                "agent screen state: pane={} program={} {:?}->{next:?} rule={}",
                self.pane_id,
                program,
                self.agent_status,
                detection.rule_id
            );
            self.agent_status = next;
            self.agent_status_source = crate::ai_agents::AgentStatusSource::Screen;
            self.agent_status_rule = Some(detection.rule_id);
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
    }

    fn runtime_screen_snapshot(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.term.lock();
        let lines = term.screen_lines();
        if lines == 0 || term.columns() == 0 {
            return None;
        }
        let start = TermPoint::new(Line(0), Column(0));
        let end = TermPoint::new(Line(lines as i32 - 1), Column(term.columns().saturating_sub(1)));
        Some(term.bounds_to_string(start, end))
    }

    pub(super) fn flush_pending_runtime_submit(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_runtime_submit.as_ref() else { return };
        let Some(screen) = self.runtime_screen_snapshot() else { return };
        if screen == pending.baseline_screen {
            return;
        }
        let pending = self.pending_runtime_submit.take().expect("checked above");
        self.write_input(pending.submit_bytes, cx);
    }
}
