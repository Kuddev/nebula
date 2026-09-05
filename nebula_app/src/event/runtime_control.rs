use super::*;

impl Processor {
    pub(super) fn runtime_snapshot(&self) -> RuntimeSnapshot {
        let mut windows: Vec<_> =
            self.windows.values().map(WindowContext::runtime_snapshot).collect();
        windows.sort_by_key(|window| window.id);
        RuntimeSnapshot::new(self.detached.len(), windows)
    }

    pub(super) fn publish_runtime_snapshot(&self) -> RuntimeSnapshot {
        self.runtime_hub.publish(self.runtime_snapshot())
    }

    /// Resolve a control target without relying on HashMap iteration order.
    /// Pane ids are window-local, so an omitted window is accepted only when
    /// the pane is unique across all live windows.
    pub(super) fn runtime_target_window(
        &self,
        window_id: Option<u64>,
        pane_id: Option<u64>,
    ) -> Result<WindowId, ApiError> {
        if let Some(window_id) = window_id {
            let id = WindowId::from(window_id);
            let Some(window) = self.windows.get(&id) else {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("window {window_id} does not exist"),
                ));
            };
            if let Some(pane_id) = pane_id
                && !window.runtime_contains_pane(pane_id)
            {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("pane {pane_id} does not belong to window {window_id}"),
                ));
            }
            return Ok(id);
        }

        if let Some(pane_id) = pane_id {
            let mut matches: Vec<_> = self
                .windows
                .iter()
                .filter_map(|(id, window)| window.runtime_contains_pane(pane_id).then_some(*id))
                .collect();
            matches.sort_by_key(|id| u64::from(*id));
            return match matches.as_slice() {
                [id] => Ok(*id),
                [] => {
                    Err(ApiError::new("target_not_found", format!("pane {pane_id} does not exist")))
                },
                _ => Err(ApiError::new(
                    "ambiguous_target",
                    format!("pane id {pane_id} exists in multiple windows; provide window_id"),
                )),
            };
        }

        self.windows
            .iter()
            .filter(|(_, window)| window.display.window.has_focus())
            .map(|(id, _)| *id)
            .next()
            .or_else(|| {
                self.windows
                    .iter()
                    .filter(|(_, window)| !window.session_exempt)
                    .map(|(id, _)| *id)
                    .min_by_key(|id| u64::from(*id))
            })
            .or_else(|| self.windows.keys().copied().min_by_key(|id| u64::from(*id)))
            .ok_or_else(|| ApiError::new("target_not_found", "no live Nebula window exists"))
    }

    pub(super) fn runtime_result(&self, action: Value) -> Result<Value, ApiError> {
        let snapshot = self.publish_runtime_snapshot();
        Ok(serde_json::json!({ "action": action, "snapshot": snapshot }))
    }

    /// Runtime close 已在 WindowContext 内逐 pane shutdown；这里只回收窗口级
    /// 调度状态。不能走普通 CloseWindow 的 detach 分支，否则 API 回报 closed
    /// 后 PTY 仍会作为驻留会话继续存在。
    pub(super) fn remove_runtime_window(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        let Some(closed) = self.windows.remove(&id) else { return };
        self.scheduler.unschedule_window(closed.id());
        if !closed.session_exempt {
            for window in self.windows.values_mut() {
                window.mark_session_dirty();
            }
        }
        if self.windows.is_empty() && self.detached.is_empty() && !self.cli_options.daemon {
            event_loop.exit();
        }
    }

    pub(super) fn execute_runtime_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: &RuntimeCommand,
    ) -> Result<Value, ApiError> {
        match command {
            RuntimeCommand::Snapshot => serde_json::to_value(self.publish_runtime_snapshot())
                .map_err(|error| ApiError::new("serialization_failed", error.to_string())),
            RuntimeCommand::NewWindow { cwd: _ } => {
                // GL backends require every current context to be released
                // before another window surface is created.
                for window in self.windows.values_mut() {
                    window.display.make_not_current();
                }
                let id = if self.gl_config.is_none() {
                    let before: Vec<_> = self.windows.keys().copied().collect();
                    self.create_initial_window(event_loop, WindowOptions::default())
                        .map_err(|error| ApiError::new("action_failed", error.to_string()))?;
                    self.windows.keys().copied().find(|id| !before.contains(id)).ok_or_else(
                        || ApiError::new("action_failed", "new window was not registered"),
                    )?
                } else {
                    self.create_window(event_loop, WindowOptions::default())
                        .map_err(|error| ApiError::new("action_failed", error.to_string()))?
                };
                self.runtime_result(serde_json::json!({ "window_id": u64::from(id) }))
            },
            RuntimeCommand::CloseWindow { window_id } => {
                let id = self.runtime_target_window(*window_id, None)?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_close_window()?;
                self.remove_runtime_window(event_loop, id);
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "closed": true
                }))
            },
            RuntimeCommand::Focus { window_id, pane_id } => {
                let id = self.runtime_target_window(*window_id, *pane_id)?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_focus(*pane_id)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::NewTab { window_id, cwd } => {
                let id = self.runtime_target_window(*window_id, None)?;
                let window = self.windows.get_mut(&id).expect("resolved runtime window exists");
                let pane_id = window.runtime_new_tab(cwd.clone())?;
                // 带目录的 tab.new 来自一次用户手势（Explorer 右键并入）：
                // 把窗口带到前台，不然标签开在了别人身后。无目录的编程调用
                // （agent/CLI）保持原来的不抢焦点语义。
                if cwd.is_some() {
                    window.runtime_focus(None)?;
                }
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::CloseTab { window_id, tab_index } => {
                let id = self.runtime_target_window(*window_id, None)?;
                let close_window = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_close_tab(*tab_index)?;
                if close_window {
                    self.remove_runtime_window(event_loop, id);
                }
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "tab_index": tab_index,
                    "closed": true
                }))
            },
            RuntimeCommand::RenameTab { window_id, tab_index, name } => {
                let id = self.runtime_target_window(*window_id, None)?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_rename_tab(*tab_index, name.clone())?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "tab_index": tab_index,
                    "name": name.trim()
                }))
            },
            RuntimeCommand::MoveTab { window_id, tab_index, to_index } => {
                let id = self.runtime_target_window(*window_id, None)?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_move_tab(*tab_index, *to_index)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "tab_index": tab_index,
                    "to_index": to_index
                }))
            },
            RuntimeCommand::Split { window_id, pane_id, direction } => {
                let id = self.runtime_target_window(*window_id, *pane_id)?;
                let (source_pane_id, pane_id) = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_split(*pane_id, *direction)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "source_pane_id": source_pane_id, "pane_id": pane_id
                }))
            },
            RuntimeCommand::ClosePane { window_id, pane_id } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let close_window = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_close_pane(*pane_id)?;
                if close_window {
                    self.remove_runtime_window(event_loop, id);
                }
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "closed": true
                }))
            },
            RuntimeCommand::ZoomPane { window_id, pane_id, zoomed } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_set_zoom(*pane_id, *zoomed)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "zoomed": zoomed
                }))
            },
            RuntimeCommand::ResizePane { window_id, pane_id, ratio } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_resize_pane(*pane_id, *ratio)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "ratio": ratio
                }))
            },
            RuntimeCommand::Prompt { window_id, pane_id, text, submit } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                self.windows.get_mut(&id).expect("resolved runtime window exists").runtime_prompt(
                    *pane_id,
                    text.clone(),
                    *submit,
                )?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "submitted": submit
                }))
            },
            RuntimeCommand::Paste { window_id, pane_id, text, submit } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                self.windows.get_mut(&id).expect("resolved runtime window exists").runtime_paste(
                    *pane_id,
                    text.clone(),
                    *submit,
                    false,
                )?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "submitted": submit,
                    "input": "paste"
                }))
            },
            RuntimeCommand::ReadPane { window_id, pane_id, lines } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let read = self
                    .windows
                    .get(&id)
                    .expect("resolved runtime window exists")
                    .runtime_read(*pane_id, *lines)?;
                serde_json::to_value(read)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::Procs { window_id, pane_id } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let processes = self
                    .windows
                    .get(&id)
                    .expect("resolved runtime window exists")
                    .runtime_procs(*pane_id)?;
                serde_json::to_value(processes)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let bytes_sent = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_send_key(*pane_id, *key, *modifiers, *repeat)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "key": key.as_str(),
                    "repeat": repeat,
                    "bytes_sent": bytes_sent
                }))
            },
            RuntimeCommand::Run { window_id, pane_id, command, .. } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let run_id = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_run(*pane_id, command.clone())?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "run_id": run_id
                }))
            },
            RuntimeCommand::Exec { .. } => Err(ApiError::new(
                "invalid_runtime_command",
                "pane.exec must be prepared before entering the synchronous UI dispatcher",
            )),
            RuntimeCommand::AgentStart { .. } | RuntimeCommand::AgentFork { .. } => {
                self.execute_agent_runtime_command(command)
            },
            RuntimeCommand::AgentPrompt { agent, generation, text, submit } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                let id = WindowId::from(managed.window_id);
                let Some(window) = self.windows.get_mut(&id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live window", managed.name),
                    ));
                };
                window.runtime_prompt(managed.pane_id, text.clone(), *submit)?;
                self.runtime_result(serde_json::json!({ "agent": managed }))
            },
            RuntimeCommand::AgentPaste { agent, generation, text, submit } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                let id = WindowId::from(managed.window_id);
                let Some(window) = self.windows.get_mut(&id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live window", managed.name),
                    ));
                };
                window.runtime_paste(managed.pane_id, text.clone(), *submit, true)?;
                self.runtime_result(serde_json::json!({ "agent": managed, "input": "paste" }))
            },
            RuntimeCommand::AgentRead { agent, generation, lines } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                let id = WindowId::from(managed.window_id);
                let Some(window) = self.windows.get(&id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live window", managed.name),
                    ));
                };
                let read = window.runtime_read(managed.pane_id, *lines)?;
                Ok(serde_json::json!({ "agent": managed, "read": read }))
            },
        }
    }

    pub(super) fn handle_runtime_control(
        &mut self,
        event_loop: &ActiveEventLoop,
        dispatch: &std::sync::Arc<RuntimeDispatch>,
    ) {
        if let RuntimeCommand::Exec { window_id, pane_id, argv, timeout_ms, max_output_bytes } =
            &dispatch.command
        {
            let prepared = self.runtime_target_window(*window_id, Some(*pane_id)).and_then(|id| {
                self.windows
                    .get(&id)
                    .expect("resolved runtime window exists")
                    .runtime_exec_context(*pane_id)
            });
            match prepared {
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
        let response = self.execute_runtime_command(event_loop, &dispatch.command);
        dispatch.respond(response);
    }
}
