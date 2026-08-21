//! legacy 壳的命名 Agent 启动事务。

use serde_json::Value;

use super::Processor;
use crate::runtime_api::{ApiError, RuntimeCommand};

impl Processor {
    pub(super) fn route_ai_hook(&mut self, hook: &crate::ai_hook::AiHookEvent) {
        for window_context in self.windows.values_mut() {
            if window_context.handle_ai_hook(hook) {
                break;
            }
        }
    }

    pub(super) fn handle_terminal_wakeup(
        &mut self,
        window_id: &winit::window::WindowId,
        pane_id: Option<u64>,
    ) {
        crate::input::latency::pty_wakeup();
        let Some(window_context) = self.windows.get_mut(window_id) else { return };
        window_context.runtime_flush_pending_submit(pane_id);
        window_context.dirty = true;
        // 远端在快速失败窗口后仍有输出，才把手输的 ssh 视为真实连接。
        window_context.confirm_ssh_on_activity(pane_id);
        if window_context.display.window.has_frame {
            window_context.display.window.request_redraw();
        }
    }

    pub(super) fn execute_agent_runtime_command(
        &mut self,
        command: &RuntimeCommand,
    ) -> Result<Value, ApiError> {
        match command {
            RuntimeCommand::AgentStart {
                window_id,
                pane_id,
                name,
                kind,
                cwd,
                session_id,
                command,
                worktree,
            } => {
                let id = self.runtime_target_window(*window_id, *pane_id)?;
                self.runtime_hub.ensure_agent_name_available(name)?;
                let created_tab = pane_id.is_none();
                let pane_id = match pane_id {
                    Some(pane_id) => *pane_id,
                    None => self
                        .windows
                        .get_mut(&id)
                        .expect("resolved runtime window exists")
                        .runtime_new_tab(cwd.clone())?,
                };
                let agent = match self.runtime_hub.register_agent(
                    name.clone(),
                    *kind,
                    u64::from(id),
                    pane_id,
                    session_id.clone(),
                    worktree.clone(),
                ) {
                    Ok(agent) => agent,
                    Err(error) => {
                        if created_tab {
                            self.windows
                                .get_mut(&id)
                                .expect("resolved runtime window exists")
                                .runtime_discard_agent_tab(pane_id);
                        }
                        return Err(error);
                    },
                };
                let launch = if created_tab {
                    self.windows
                        .get_mut(&id)
                        .expect("resolved runtime window exists")
                        .runtime_set_tab_name(pane_id, name.clone())
                } else {
                    Ok(())
                }
                .and_then(|_| {
                    self.windows
                        .get_mut(&id)
                        .expect("resolved runtime window exists")
                        .runtime_prompt(pane_id, command.clone(), true)
                });
                if let Err(error) = launch {
                    self.runtime_hub.close_agent(&agent.agent_id, "launch_failed");
                    if created_tab {
                        self.windows
                            .get_mut(&id)
                            .expect("resolved runtime window exists")
                            .runtime_discard_agent_tab(pane_id);
                    }
                    return Err(error);
                }
                self.runtime_result(serde_json::json!({
                    "agent": agent,
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::AgentFork { .. } => Err(ApiError::new(
                "invalid_runtime_command",
                "agent.fork reached the UI before its Git worktree was prepared",
            )),
            _ => unreachable!("non-agent runtime command routed to agent handler"),
        }
    }
}
