//! GPUI 终端对 Runtime API 暴露的读取、输入与任务状态边界。

use gpui::{Context, EventEmitter as _};
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Line, Point as TermPoint};
use nebula_terminal::term::TermMode;

use super::{SidebarActivity, TerminalView, TerminalViewEvent};

/// OSC 9;4 是程序对自身状态的明确声明，比 BEL/标题/进程树推断可靠；但 Agent
/// hook 对 Claude/Codex 回合有更完整的语义，不能被遗留的进度码覆盖。
fn progress_sidebar_activity(
    progress: crate::taskbar::TaskProgress,
    agent_status: crate::ai_agents::AgentStatus,
) -> Option<SidebarActivity> {
    if agent_status != crate::ai_agents::AgentStatus::Unknown {
        return None;
    }
    match progress {
        crate::taskbar::TaskProgress::None => None,
        crate::taskbar::TaskProgress::Indeterminate | crate::taskbar::TaskProgress::Value(_) => {
            Some(SidebarActivity::Running)
        },
        crate::taskbar::TaskProgress::Error(_) => Some(SidebarActivity::CommandFailed),
        crate::taskbar::TaskProgress::Paused(_) => Some(SidebarActivity::Paused),
    }
}

impl TerminalView {
    /// Clear foreground Agent identity after an authoritative command end or
    /// after the submitted shell prompt is observed again. OSC 133;D remains
    /// the primary edge; the cached-prompt path calls the same reset so the two
    /// lifecycle routes cannot drift apart.
    pub(super) fn clear_foreground_agent_state(&mut self) -> bool {
        self.answers.close();
        let title_changed =
            self.running_program.take().is_some() || self.ai_session.take().is_some();
        self.primary_agent_pid = None;
        self.agent_status = crate::ai_agents::AgentStatus::Unknown;
        self.agent_status_source = crate::ai_agents::AgentStatusSource::Unknown;
        self.agent_status_rule = None;
        self.agent_hook_seen = false;
        self.agent_turn_active = false;
        self.idle_screen_streak = 0;
        self.agent_runtime_submit_pending = false;
        self.command_running = false;
        self.command_running_disproved = false;
        self.command_started = None;
        self.last_process_probe = None;
        self.pending_runtime_submit = None;
        self.suggest.pending_command_prompt = None;
        self.awaiting_input = false;
        title_changed
    }

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
        // 权威判定排在响铃兜底之前。`awaiting_input` 只由 BEL 置位，而 CC /
        // codex 回合结束同样响铃——原来它压在这段 match 之上，于是 hook 明确报
        // 出的 `Done` 每次都被改写成 `WaitingInput`，tab 上显示「在问你」。
        // 置位那头也有同一条闸（`AgentStatus::is_decided`），这里再挡一次是为了
        // 历史遗留的旗子改不了新判定。
        match self.agent_status {
            AgentStatus::Working => return RuntimeTaskState::Running,
            AgentStatus::Done => return RuntimeTaskState::Finished,
            AgentStatus::Idle => return RuntimeTaskState::Idle,
            AgentStatus::Blocked => unreachable!("handled above"),
            AgentStatus::Unknown => {},
        }
        if self.awaiting_input {
            return RuntimeTaskState::WaitingInput;
        }
        // `command_running` 是 OSC 133 的忠实记录，但 133;D 会因为宿主 shell
        // 被 cmd/wsl/ssh 接管而永不到达；`command_running_disproved` 是进程树
        // 给出的反证，看门狗每 2 秒复核一次。
        //
        // `running_program` 必须接受同一条判据。它是推断出来的（PowerShell 用
        // `NEBULA|` 标题上报、或从命令行首 token 提取），而接管终端的那个 shell
        // 不会再更新标题，于是这个值会永远停在最后一次推断上。少了下面这层
        // 判断，pane 就会一直转圈：`command_running` 那边早已被反证，Running
        // 却从这条 OR 分支漏了出来。
        //
        // 前台是交互式 shell 只说明「有个已知程序占着终端」，不说明有活儿在跑。
        // 真在那个 shell 里跑起活儿由上面 `command_running` 那条路负责——进程树
        // 会看到多出来的子进程，撤销反证。
        let program_is_work = self
            .running_program
            .as_deref()
            .is_some_and(|program| !crate::process_tree::is_interactive_shell_command(program));
        if (self.command_running && !self.command_running_disproved)
            || program_is_work
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
        Some(self.runtime_agent_for_kind(kind))
    }

    /// Send-to-Chat 只能投递给当前仍占据 pane 的 Agent。`ai_session` 会为会话
    /// 恢复/分叉保留历史身份，不能单独证明 CLI 仍在前台；进程树已经反证命令
    /// 结束时也必须立即排除，避免把多行引用送回普通 shell。
    pub fn runtime_chat_agent(&self) -> Option<crate::runtime_api::RuntimeAgent> {
        let kind = runtime_chat_agent_kind(
            self.running_program.as_deref(),
            self.command_running_disproved,
        )?;
        Some(self.runtime_agent_for_kind(kind))
    }

    fn runtime_agent_for_kind(
        &self,
        kind: crate::ai_agents::AgentKind,
    ) -> crate::runtime_api::RuntimeAgent {
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
        crate::runtime_api::RuntimeAgent {
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
        }
    }

    pub fn sidebar_activity(&self) -> SidebarActivity {
        let state = self.runtime_task_state();
        // pane 级故障永远优先；普通 CLI 才允许 OSC 9;4 覆盖弱推断。这里只改变
        // badge，不改变 RuntimeTaskState，避免进度协议干扰 agent.wait/自动回传。
        if state == crate::runtime_api::RuntimeTaskState::Failed {
            return SidebarActivity::Failed;
        }
        if let Some(activity) = progress_sidebar_activity(self.progress, self.agent_status) {
            return activity;
        }
        // 「上一条命令失败」盖在完成/空闲之上：那两个说的是「没在忙」，而退出码
        // 非 0 是一个你可能要处理的结果。真在跑就不画（`mark_command_running`
        // 起跑时已经清了旗子），pane 级故障走下面的 `Failed`，更严重。
        if self.last_command_failed
            && matches!(
                state,
                crate::runtime_api::RuntimeTaskState::Idle
                    | crate::runtime_api::RuntimeTaskState::Finished
            )
        {
            return SidebarActivity::CommandFailed;
        }
        match state {
            crate::runtime_api::RuntimeTaskState::Running => SidebarActivity::Running,
            crate::runtime_api::RuntimeTaskState::WaitingInput => SidebarActivity::WaitingInput,
            crate::runtime_api::RuntimeTaskState::Attention => SidebarActivity::Attention,
            // 刚完成的那一小会儿画对勾，之后沉降为圆点。
            crate::runtime_api::RuntimeTaskState::Finished => {
                if self.completed_at.is_some() {
                    SidebarActivity::Completed
                } else {
                    SidebarActivity::Done
                }
            },
            crate::runtime_api::RuntimeTaskState::Failed => unreachable!("handled above"),
            crate::runtime_api::RuntimeTaskState::Idle => SidebarActivity::Idle,
        }
    }

    /// 认出「刚刚进入完成」这个边沿，并让对勾在闪现窗口结束后自己沉降为圆点。
    ///
    /// 由 1Hz 的 agent 看门狗调用（`workspace::agents::start_agent_screen_watchdog`）：
    /// 那里本来就每秒遍历所有 pane，不用再养一个计时器。代价是边沿最多晚 1 秒
    /// 被看到、对勾实际停留 `COMPLETION_FLASH`..+1s——「短暂闪现」这个语义容得下
    /// 这个精度，换来的是零新增定时器。
    pub(crate) fn sync_activity_badges(&mut self, cx: &mut Context<Self>) {
        let state = self.runtime_task_state();
        if self.last_task_state != Some(state) {
            self.last_task_state = Some(state);
            self.completed_at = (state == crate::runtime_api::RuntimeTaskState::Finished)
                .then(std::time::Instant::now);
            cx.notify();
            return;
        }
        if self.completed_at.is_some_and(|at| at.elapsed() >= super::COMPLETION_FLASH) {
            self.completed_at = None;
            cx.notify();
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
        self.mark_command_running();
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
            self.mark_command_running();
            if recognized_agent {
                self.agent_status = crate::ai_agents::AgentStatus::Working;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
                self.agent_status_rule = None;
                self.agent_runtime_submit_pending = true;
                self.agent_turn_active = true;
                self.idle_screen_streak = 0;
            }
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        Ok(())
    }

    /// 三层护栏里唯一强制的那一层。
    ///
    /// 另两层都能被绕过，所以都只是护栏：第一层是便利层，Send-to-Chat 的目标
    /// 列表把远端 pane 明确标成远端主机，防的是手滑选错；第二层是自查层，远端
    /// pane 的环境里带 `NEBULA_PANE_REMOTE=1`，愿意自查的被调方可以自己拒绝。
    /// 这一层不同：判据是 pane 自己的身份，与调用方声明了什么无关，任何携带
    /// 本地上下文的写入都必须先过它。
    ///
    /// 规则只有一条：**程序不得把本地内容自动送进远端 pane**。用户在对话框里
    /// 当场选中一个 SSH pane 是知情的；agent 或 Recipe 把本地选区自动发到别人
    /// 的主机上不是——那是一次数据外传，而且没有任何人在看着。
    ///
    /// 注意这里不拦纯指令（`pane.run` / `pane.prompt`）：对 SSH pane 下命令本身
    /// 就是远端编排的正常用法，风险在于**内容**从本地流出，不在于写入动作。
    fn ensure_local_context_allowed(
        &self,
        origin: InputOrigin,
    ) -> Result<(), crate::runtime_api::ApiError> {
        match local_context_refusal(origin, self.ssh_destination.as_deref()) {
            Some(reason) => Err(crate::runtime_api::ApiError::new("remote_target_refused", reason)),
            None => Ok(()),
        }
    }

    /// 把受限 UTF-8 文本作为一整块 bracketed paste 写入 pane。Runtime API
    /// 调用属于程序来源，本地内容不得自动流向 SSH 目标。
    pub fn runtime_paste(
        &mut self,
        text: String,
        submit: bool,
        origin: InputOrigin,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        crate::runtime_api::validate_paste_text(&text)?;
        self.runtime_paste_inner(text, submit, false, origin, cx)
    }

    /// Agent 版本额外要求目标仍是当前活跃会话；managed generation 的校验在
    /// workspace 调度层完成，这里负责防止历史身份落回普通 shell。
    pub fn runtime_agent_paste(
        &mut self,
        text: String,
        submit: bool,
        origin: InputOrigin,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        crate::runtime_api::validate_paste_text(&text)?;
        self.runtime_paste_inner(text, submit, true, origin, cx)
    }

    /// Send-to-Chat 与公开 paste API 共享同一组终端安全边界；前者保留自己的
    /// 文案校验，但不能拥有一条更宽松的字节写入旁路。
    pub fn runtime_chat_message(
        &mut self,
        text: String,
        origin: InputOrigin,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        crate::runtime_api::validate_chat_message(&text)?;
        self.runtime_paste_inner(text, true, true, origin, cx)
    }

    fn runtime_paste_inner(
        &mut self,
        text: String,
        submit: bool,
        require_agent: bool,
        origin: InputOrigin,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        self.ensure_local_context_allowed(origin)?;
        let recognized_agent = self.runtime_chat_agent().is_some();
        if require_agent && !recognized_agent {
            return Err(crate::runtime_api::ApiError::new(
                "invalid_target",
                "multi-line Agent input requires a live Agent pane",
            ));
        }
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
        if self.pending_runtime_submit.is_some() {
            return Err(crate::runtime_api::ApiError::new(
                "input_in_progress",
                "the pane is still committing previous runtime input",
            ));
        }
        if !self.term_mode().contains(TermMode::BRACKETED_PASTE) {
            return Err(crate::runtime_api::ApiError::new(
                "unsafe_input_mode",
                "the target pane is not ready for safe multi-line input",
            ));
        }

        if submit {
            let submit_bytes = self.runtime_key_sequence(
                crate::runtime_api::RuntimeKey::Enter,
                crate::runtime_api::RuntimeKeyModifiers::default(),
                1,
            )?;
            self.pending_runtime_submit = Some(crate::display::state::RuntimeSubmitBarrier {
                baseline_screen: self.runtime_screen_snapshot().unwrap_or_default(),
                submit_bytes,
            });
        }
        self.paste_now_impl(&text, false, cx);
        if submit {
            self.awaiting_input = false;
            self.mark_command_running();
            if recognized_agent {
                self.agent_status = crate::ai_agents::AgentStatus::Working;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
                self.agent_status_rule = None;
                self.agent_runtime_submit_pending = true;
                self.agent_turn_active = true;
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

    /// 事件声明的 pane 与写管道进程的祖先链是否互相矛盾。
    ///
    /// 三种情况都返回 `false`（放行）：事件没有声明 pane（本就要回退焦点）、
    /// 拿不到客户端 pid（远端 SSH 走 OSC 通道，没有本地进程）、这个 pane 还
    /// 没有本地 shell，或者进程表给不出证据。只有明确证明「这个 pid 不在本
    /// pane 树内」时才判为矛盾。
    fn ai_hook_client_mismatched(&self, event: &crate::ai_hook::AiHookEvent) -> bool {
        let Some(client_pid) = event.client_pid else { return false };
        // 没有自报 pane 的事件不存在「声明与事实矛盾」，交给上层回退规则。
        if event.pane != Some(self.pane_id) {
            return false;
        }
        let Some(shell_pid) = self.session.as_ref().map(|session| session.shell_pid) else {
            return false;
        };
        crate::process_tree::is_within_tree(client_pid, shell_pid) == Some(false)
    }

    /// 这条事件是不是来自这个 pane 的主 agent（而非它 spawn 的嵌套子代理）。
    ///
    /// 判据是 agent 的进程 pid：第一个报到的就是主 agent——子代理必须由主 agent
    /// spawn，不可能先到。只有主 agent 能写 pane 的会话身份，子代理的状态边沿
    /// 照常生效（它在干活，spinner 该转）。
    ///
    /// 拿不到 pid 时一律当主 agent：远端 SSH、旧版 bridge、快照竞态都会落到这
    /// 里，宁可少一层区分，也不能把真实会话身份丢掉。
    fn claim_primary_agent(&mut self, event: &crate::ai_hook::AiHookEvent) -> bool {
        let Some(agent_pid) = event.agent_pid else { return true };
        match self.primary_agent_pid {
            Some(primary) => primary == agent_pid,
            None => {
                self.primary_agent_pid = Some(agent_pid);
                true
            },
        }
    }

    /// Apply one lifecycle event already routed to this pane by the workspace.
    ///
    /// 旧壳 `WindowContext::handle_ai_hook` 状态机的忠实移植：不能把所有
    /// 非 SessionEnd 事件压平成 running，否则 TurnDone 后 spinner 不会停。
    pub fn handle_ai_hook(
        &mut self,
        event: &crate::ai_hook::AiHookEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ai_agents::AgentStatus;
        use crate::ai_hook::AiHookKind;

        // 路由第二因子：写管道那个进程必须真的跑在这个 pane 的进程树里。
        // `NEBULA_PANE_ID` 是环境变量，任何进程都能设成别的 pane；祖先链不能
        // 伪造。只有拿到明确反证时才拒绝，`None`（查不到、竞态）一律放行。
        if self.ai_hook_client_mismatched(event) {
            log::warn!(
                "ai_hook: rejected event claiming pane {} from pid {:?} outside its process tree \
                 (source={} kind={:?})",
                self.pane_id,
                event.client_pid,
                event.source,
                event.kind
            );
            return false;
        }

        let verdict = crate::ai_hook::accept_for_pane(event, self.pane_id);
        if !verdict.accepted() {
            // 带原因：事件被静默丢掉是这条链路最难查的故障，用户只看到「通知
            // 没出现」。原因字段直接对应事件门里的那一条规则。
            log::debug!(
                "ai_hook: dropped event reason={verdict:?} source={} session={:?} pane={} \
                 kind={:?} bridge_seq={:?}",
                event.source,
                event.session_id,
                self.pane_id,
                event.kind,
                event.bridge_sequence
            );
            return false;
        }

        // SessionEnd 可能是第一次携带最终权威 id 的事件；必须先落盘再清理
        // pane 现场，否则 CLI 正常退出后反而无法冷恢复。
        //
        // 但嵌套子代理的会话不能落盘也不能上位：`claude -p` 起的子代理有自己的
        // 短命 session id，把它记成这个 pane 的身份，之后 `claude --resume` 就会
        // 指向一个早已结束的会话，真正活着的那个反而丢了。第一个报到的 agent
        // 进程就是主 agent（子代理必须由它 spawn，不可能先到）。
        let from_primary_agent = self.claim_primary_agent(event);
        if from_primary_agent && self.ssh_destination.is_none() && self.exited.is_none() {
            if self.answers.observe(event, self.pane_id) && let Some(reader) = &self.answer_reader {
                reader.update(cx, |reader, cx| reader.answer_arrived(cx));
            }
        }
        if event.kind == AiHookKind::NeedsAttention && let Some(reader) = &self.answer_reader {
            reader.update(cx, |reader, cx| reader.needs_attention(cx));
        }
        if from_primary_agent
            && let Some(id) = event.session_id.as_deref()
            && let Err(error) =
                crate::ai_sessions::record_hook_session(&event.source, id, &self.cwd, None)
        {
            log::warn!("agent session index: could not record {} {id}: {error}", event.source);
        }
        match event.kind {
            // 嵌套子代理结束了，主 agent 还在跑：绝不能清 pane 现场。这条 arm
            // 排在前面就是为了把「谁的 SessionEnd」分开——子代理的退出对 pane
            // 而言什么都不是。
            AiHookKind::SessionEnd if !from_primary_agent => {},
            AiHookKind::SessionEnd => {
                self.clear_foreground_agent_state();
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
                if from_primary_agent && let Some(id) = event.session_id.as_deref() {
                    self.ai_session = Some(crate::display::AiSessionIdentity {
                        source: event.source.clone(),
                        session_id: id.to_owned(),
                    });
                }
                match kind {
                    AiHookKind::SessionStart => {
                        self.agent_status = AgentStatus::Idle;
                        // 新会话还没开工，此前那点未读痕迹不该带进来。
                        self.agent_turn_active = false;
                    },
                    AiHookKind::PromptSubmit | AiHookKind::ToolComplete => {
                        self.agent_status = AgentStatus::Working;
                        self.agent_turn_active = true;
                    },
                    AiHookKind::TurnDone if event.active_background_tasks() > 0 => {
                        let active = event.active_background_tasks();
                        self.agent_status = AgentStatus::Working;
                        self.agent_turn_active = true;
                        self.agent_status_rule =
                            Some(format!("hook.background_tasks.active={active}"));
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
        if event.kind == AiHookKind::NeedsAttention
            && let Some(mut attention) = event.attention.clone()
        {
            attention.pane_id = Some(self.pane_id);
            cx.emit(TerminalViewEvent::AiAttention(attention));
        }
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
        true
    }

    /// TurnDone 到达时，屏幕上是否真的还挂着一个等人表态的框。
    ///
    /// 2026-08-22：此前这里把底部 15 行整段丢给一张裸关键词表（`(y/n)` /
    /// `do you want to proceed` / `enter to confirm` …），关键词落在**正文任何
    /// 位置**都算数——agent 打印的代码片段、上一轮已答完但还没滚走的权限框、
    /// 乃至讨论这套判据本身的聊天记录，都会让正常结束的回合挂上警告三角。
    /// 改走 per-agent manifest 的 blocked 规则：它们带 region 锚定
    /// （`after_last_horizontal_rule` / 底部 N 行），只认当前活动框。
    fn screen_tail_asks(&self) -> bool {
        let Some(program) = self.running_program.as_deref() else { return false };
        let Some(screen) = self.runtime_screen_snapshot() else { return false };
        crate::ai_agents::detect(program, &screen)
            .is_some_and(|detection| detection.status == crate::ai_agents::AgentStatus::Blocked)
    }

    /// 1 Hz 屏幕看门狗：hook 提供精确边沿，屏幕补偿丢失边沿和无 hook 客户端。
    pub fn refresh_agent_screen_state(&mut self, cx: &mut Context<Self>) {
        use crate::ai_agents::AgentStatus;

        // Wakeup 会被 synchronized-update 合并；1 Hz 看门狗提供可靠的
        // Grid 后置检查，确保 Runtime 文本回显后一定能补发 Enter。
        self.flush_pending_runtime_submit(cx);
        // 进程树对账必须排在下面那道早退之前：身份认不出来就早退，等于让
        // 这个 pane 永远退出屏幕检测。
        self.reconcile_shell_activity(cx);
        let Some(session) = &self.session else { return };
        let (prompt_restored, screen) = {
            let term = session.term.lock();
            let lines = term.screen_lines();
            if lines == 0 || term.columns() == 0 {
                return;
            }
            let prompt_restored =
                self.suggest.pending_command_prompt.as_deref().is_some_and(|expected| {
                    crate::display::nebula_shell_prompt_restored_from_raw_grid(
                        &term,
                        expected,
                        &self.suggest.suggest_env,
                    )
                });
            let take = lines.min(24);
            let start = TermPoint::new(Line((lines - take) as i32), Column(0));
            let end =
                TermPoint::new(Line(lines as i32 - 1), Column(term.columns().saturating_sub(1)));
            (prompt_restored, term.bounds_to_string(start, end))
        };
        if prompt_restored
            && self
                .running_program
                .as_deref()
                .and_then(crate::ai_agents::AgentKind::parse)
                .is_some()
        {
            log::debug!(
                "agent lifecycle: submitted shell prompt restored pane={} program={:?}",
                self.pane_id,
                self.running_program
            );
            if let Some(run) = self.active_run.take() {
                self.last_run =
                    Some(crate::runtime_api::RuntimeRunOutcome::command_done(run, None));
            }
            let title_changed = self.clear_foreground_agent_state();
            if title_changed {
                cx.emit(TerminalViewEvent::TitleChanged);
            }
            cx.notify();
            return;
        }
        let program = match self.running_program.clone() {
            Some(program) => program,
            None => {
                let Some(agent) = crate::ai_agents::identify(&screen) else {
                    self.idle_screen_streak = 0;
                    return;
                };
                let program = agent.slug().to_owned();
                log::debug!("agent identity from screen: pane={} program={program}", self.pane_id);
                self.running_program = Some(program.clone());
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Screen;
                self.agent_status_rule = None;
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
                program
            },
        };
        if crate::ai_agents::AgentKind::parse(&program).is_none() {
            return;
        }
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
                // hook 报出的 Done 是精确终态，屏幕不得改写。
                if self.agent_hook_seen && self.agent_status == AgentStatus::Done {
                    return;
                }
                self.idle_screen_streak = self.idle_screen_streak.saturating_add(1);
                match self.agent_status {
                    // Blocked 必须能自愈：问题框已经从屏幕上消失（用户答完了，
                    // 或者当初就是误判），继续挂警告三角就是假警报。此前它和
                    // Done 一起被 hook_seen 挡在门外，于是三角一旦点亮就再也
                    // 下不来，只能等下一个 hook 边沿——真实框还在时 blocked
                    // 规则会持续命中并走下面的分支，所以这里放行不会误灭。
                    AgentStatus::Blocked => {
                        if self.idle_screen_streak < 2 {
                            return;
                        }
                        AgentStatus::Done
                    },
                    // Working 降级更谨慎：hook 在场时 TurnDone 才是权威终态，
                    // 屏幕只在它迟迟不来时兜底，门槛拉到 5 拍；无 hook 的客户端
                    // 屏幕是唯一证据，维持 2 拍。
                    AgentStatus::Working => {
                        let threshold = if self.agent_hook_seen { 5 } else { 2 };
                        if self.idle_screen_streak < threshold {
                            return;
                        }
                        AgentStatus::Done
                    },
                    // 「干完了、你还没看」与「从没开工」必须分开：前者要留
                    // 蓝点。此前两者都落到 Idle，于是 hook 掉线的 pane 干完
                    // 整整一轮活也只显示 shell 标签，用户根本看不出哪个有结果
                    // （实测三个 claude tab 全是这样）。
                    _ if self.agent_turn_active => AgentStatus::Done,
                    _ => AgentStatus::Idle,
                }
            },
            status @ (AgentStatus::Working | AgentStatus::Blocked) => {
                self.idle_screen_streak = 0;
                if status == AgentStatus::Working {
                    self.agent_turn_active = true;
                }
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

    /// `command_running` 的统一置位口：进程树探测的节流窗口从这里起算，
    /// 上一条命令的反证同时作废（新命令开始，「树里没活儿」不再成立）。
    ///
    /// 上一条命令的失败标记与「刚完成」的对勾也在这里作废：新命令一起跑，旧结果
    /// 就不再是这个 pane 的现状。
    pub(super) fn mark_command_running(&mut self) {
        if !self.command_running {
            self.command_started = Some(std::time::Instant::now());
            self.last_process_probe = None;
        }
        self.command_running = true;
        self.command_running_disproved = false;
        self.last_command_failed = false;
        self.completed_at = None;
    }

    /// 进程树对账：补 agent 身份、给 `command_running` 做反证。
    ///
    /// 两件事都必须排在「`running_program` 为 None 就早退」之前。身份一旦
    /// 没认出来，看门狗就再也不看这个 pane 一眼——2026-08-22 实测的 codex
    /// pane 正是死在这里：codex 早已答完，屏幕上就是空闲输入框 `›`（codex.toml
    /// 的 prompt_idle 一匹就中），但 `running_program` 是 None，检测从未运行，
    /// 转圈一直挂着，侧栏也没有图标。
    ///
    /// 身份的第一来源是命令行首 token（`TermEvent::CommandStart` 那条路），
    /// 那是脆弱推断：会话恢复、shell 别名、`npx codex` 这类间接启动都会让它
    /// 落空。进程树是客观事实，只是慢一拍——而慢一拍在 1 Hz 看门狗里无所谓。
    fn reconcile_shell_activity(&mut self, cx: &mut Context<Self>) {
        if !self.command_running {
            self.command_running_disproved = false;
            return;
        }
        let Some(shell_pid) =
            self.session.as_ref().map(|session| session.shell_pid).filter(|pid| *pid != 0)
        else {
            return;
        };
        // 节流两道闸：命令先跑够 3 秒，且两次探测至少隔 2 秒。绝大多数命令
        // 活不到第一次探测，全机进程枚举因此不会落到 1 Hz 热路径上——
        // process_tree.rs 顶部专门告诫过不要那么做。
        let started = *self.command_started.get_or_insert_with(std::time::Instant::now);
        if started.elapsed() < std::time::Duration::from_secs(3) {
            return;
        }
        let probe_interval = std::time::Duration::from_secs(2);
        if self.last_process_probe.is_some_and(|at| at.elapsed() < probe_interval) {
            return;
        }
        self.last_process_probe = Some(std::time::Instant::now());

        if self.running_program.is_none()
            && let Some(agent) = crate::process_tree::agent_child(shell_pid)
        {
            log::debug!("agent identity from process tree: pane={} program={agent}", self.pane_id);
            self.running_program = Some(agent);
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
            return;
        }

        // 反证：树里只剩交互式 shell 与 console plumbing，没有真在跑的活儿。
        // 判据与关闭确认共用 `STATELESS` 白名单——两处对「算不算在跑活儿」
        // 必须同口径，否则同一个 cmd 会话会一边说「随便关」一边说「忙着呢」。
        let disproved = crate::process_tree::busy_child(shell_pid).is_none();
        if disproved != self.command_running_disproved {
            log::debug!(
                "command_running disproved={disproved} by process tree: pane={}",
                self.pane_id
            );
            self.command_running_disproved = disproved;
            cx.notify();
        }
        // 反证成立时，把推断出来的 `running_program` 里已经过期的那部分作废。
        // 它的来源都是推断（标题协议、命令行首 token），而进程树是客观事实：
        // 树里没有活儿，这个值就是上一条命令留下的残留（`npm run dev` 被 Ctrl+C
        // 掉、133;D 又没来，就是这种情形）。
        //
        // 交互式 shell 例外，不清：`cmd` 确实还占着这个 pane 的前台，那是要显示
        // 给用户的正确信息，而它不算「活儿」已经由 `runtime_task_state` 里的
        // `program_is_work` 判掉了。真在跑的 agent 也不会被误清——claude 等在提示
        // 符上时进程仍在树里，`busy_child` 看得见，反证根本不成立。
        if disproved
            && self
                .running_program
                .as_deref()
                .is_some_and(|program| !crate::process_tree::is_interactive_shell_command(program))
        {
            self.running_program = None;
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

/// 一次写入是谁发起的。安全判定不看写了什么，看谁让写的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// 用户在 UI 里当场选定了目标（Send-to-Chat 对话框、右键菜单）。选择本身
    /// 就是知情同意。
    User,
    /// 程序发起：runtime API、Recipe、agent 自己的 skill。没有人在看。
    Program,
}

/// 携带本地上下文的写入是否要被拒绝，以及拒绝理由。
///
/// 抽成纯函数是为了能被测试钉住——这是三层护栏里唯一强制的那一条判据，不能
/// 只存在于一个需要真实 `TerminalView` 才能触发的分支里。
fn local_context_refusal(origin: InputOrigin, ssh_destination: Option<&str>) -> Option<String> {
    let destination = ssh_destination?;
    (origin == InputOrigin::Program).then(|| {
        format!(
            "pane targets the remote host {destination}; local context must not be sent there \
             without an explicit user choice"
        )
    })
}

fn runtime_chat_agent_kind(
    running_program: Option<&str>,
    command_running_disproved: bool,
) -> Option<crate::ai_agents::AgentKind> {
    if command_running_disproved {
        return None;
    }
    crate::ai_agents::AgentKind::parse(running_program?)
}

#[cfg(test)]
mod tests {
    use super::{
        InputOrigin, SidebarActivity, local_context_refusal, progress_sidebar_activity,
        runtime_chat_agent_kind,
    };

    #[test]
    fn osc_progress_drives_cli_badges_but_never_overrides_agent_hooks() {
        use crate::ai_agents::AgentStatus;
        use crate::taskbar::TaskProgress;

        assert_eq!(
            progress_sidebar_activity(TaskProgress::Value(42), AgentStatus::Unknown),
            Some(SidebarActivity::Running)
        );
        assert_eq!(
            progress_sidebar_activity(TaskProgress::Indeterminate, AgentStatus::Unknown),
            Some(SidebarActivity::Running)
        );
        assert_eq!(
            progress_sidebar_activity(TaskProgress::Error(None), AgentStatus::Unknown),
            Some(SidebarActivity::CommandFailed)
        );
        assert_eq!(
            progress_sidebar_activity(TaskProgress::Paused(Some(7)), AgentStatus::Unknown),
            Some(SidebarActivity::Paused)
        );
        assert_eq!(progress_sidebar_activity(TaskProgress::None, AgentStatus::Unknown), None);
        for status in
            [AgentStatus::Working, AgentStatus::Done, AgentStatus::Idle, AgentStatus::Blocked]
        {
            assert_eq!(
                progress_sidebar_activity(TaskProgress::Indeterminate, status),
                None,
                "hook state {status:?} must remain authoritative"
            );
        }
    }

    #[test]
    fn historical_or_disproved_agent_is_not_a_chat_target() {
        // `ai_session` 不进入这条判据：没有当前前台程序时，历史身份不能成为
        // Send-to-Chat 目标。
        assert_eq!(runtime_chat_agent_kind(None, false), None);
        assert_eq!(runtime_chat_agent_kind(Some("codex"), true), None);
        assert_eq!(
            runtime_chat_agent_kind(Some("codex"), false),
            Some(crate::ai_agents::AgentKind::Codex)
        );
    }

    /// 强制层的判据：本地内容不得由程序自动送进远端 pane，用户当场选中则放行。
    #[test]
    fn program_writes_never_carry_local_context_to_a_remote_pane() {
        // 本地 pane：两种来源都放行。
        assert_eq!(local_context_refusal(InputOrigin::Program, None), None);
        assert_eq!(local_context_refusal(InputOrigin::User, None), None);

        // 远端 pane：用户当场选定是知情选择，放行。
        assert_eq!(local_context_refusal(InputOrigin::User, Some("build@10.0.0.7")), None);

        // 远端 pane + 程序发起：拒绝，且理由里必须点出是哪台主机——用户要能
        // 一眼看出内容本来会流去哪儿。
        let refusal = local_context_refusal(InputOrigin::Program, Some("build@10.0.0.7"))
            .expect("program writes to a remote pane must be refused");
        assert!(refusal.contains("build@10.0.0.7"), "拒绝理由要指名远端主机：{refusal}");
    }
}
