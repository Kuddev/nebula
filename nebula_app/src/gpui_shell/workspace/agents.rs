//! GPUI workspace 中与 AI Agent 生命周期直接相关的行为。
//!
//! Session 恢复、通用命令面板和 tab 展示仍留在父模块；这些路径同时服务
//! 普通终端与 SSH，不应为了缩短文件而拆散它们的状态事务。

use std::time::Duration;

use gpui::{Context, Window};
use nebula_split::SplitTree;

use super::{
    NebulaWorkspace, TabMeta, WorkspacePaletteAction, WorkspacePaletteRow, WorkspaceTab,
    new_tab_insert_index,
};

/// 精确 pane id 必须严格路由：已关闭 pane 的迟到事件不能污染当前活跃
/// pane；只有环境链路确实丢失 `NEBULA_PANE_ID` 的事件才允许回退到活跃
/// tab 的聚焦 pane。
pub(super) fn ai_hook_target_pane(
    pane_ids: &[u64],
    event_pane: Option<u64>,
    active_focused: Option<u64>,
) -> Option<u64> {
    match event_pane {
        Some(pane_id) => pane_ids.contains(&pane_id).then_some(pane_id),
        None => active_focused,
    }
}

/// AI 接续是会话回放的独立闸门：关掉时 pane 的 cwd/launch/layout 仍可
/// 恢复，但绝不把 AgentSession 转成会敲进新 shell 的命令。
pub(super) fn restored_agent_command(
    resume_ai: bool,
    agent: Option<&crate::session::AgentSession>,
) -> Option<String> {
    if resume_ai { agent.and_then(|agent| agent.resume_command()) } else { None }
}

pub(super) fn ai_session_palette_rows(
    sessions: impl IntoIterator<Item = crate::ai_sessions::AiSession>,
) -> Vec<WorkspacePaletteRow> {
    let sessions: Vec<_> = sessions.into_iter().collect();
    let lead_source = sessions.first().map(|session| session.source);
    let mut rows = Vec::new();
    for session in sessions {
        let group_order = usize::from(lead_source.is_some_and(|source| source != session.source));
        let group = crate::display::command_palette::source_group_label(session.source);
        let place = session.place_label();
        let time = crate::ai_sessions::relative_label(session.modified);
        let location = if place.is_empty() { time } else { format!("{place} · {time}") };
        let source = session.source.display_name();
        let search = format!("{} {} {}", session.title, session.project, session.source.label());
        let cwd = (!session.project.trim().is_empty())
            .then(|| std::path::PathBuf::from(session.project.trim()))
            .filter(|path| path.is_dir());
        if let Some(command) = session.resume_command() {
            rows.push(WorkspacePaletteRow {
                group_order,
                group: group.clone(),
                label: session.title.clone(),
                hint: format!("恢复 · {source} · {location}"),
                search: format!("恢复 resume {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
            });
        }
        if let Some(command) = session.fork_command() {
            rows.push(WorkspacePaletteRow {
                group_order,
                group: group.clone(),
                label: format!("分叉 · {}", session.title),
                hint: format!("{source} · {location}"),
                search: format!("分叉 fork {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
            });
        }
    }
    rows
}

impl NebulaWorkspace {
    pub(super) fn start_ai_hook_pump(
        receiver: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_this, cx| {
            loop {
                executor.timer(Duration::from_millis(75)).await;
                let mut events = Vec::new();
                while events.len() < 64 {
                    match receiver.try_recv() {
                        Ok(event) => events.push(event),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                if events.is_empty() {
                    continue;
                }
                cx.update(|cx| {
                    crate::gpui_shell::workspace::windowing::dispatch_ai_events(events, cx)
                });
            }
        })
        .detach();
    }

    /// 1 Hz 遍历所有终端 pane 跑屏幕看门狗（旧壳 `refresh_agent_screen_states`
    /// 的调度对应物）：纠正丢边的 hook 状态、给无 hook 客户端补位。非
    /// agent pane 在 view 侧一行判断就退出，代价可忽略。
    pub(super) fn start_agent_screen_watchdog(cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(1000)).await;
                let alive = this.update(cx, |workspace, cx| {
                    let views: Vec<_> = workspace
                        .tabs
                        .iter()
                        .filter_map(|tab| match tab {
                            WorkspaceTab::Terminal { panes, .. } => Some(panes.iter()),
                            _ => None,
                        })
                        .flatten()
                        .map(|pane| pane.view.clone())
                        .collect();
                    for view in views {
                        view.update(cx, |view, cx| view.refresh_agent_screen_state(cx));
                    }
                    workspace.publish_tray_agents(cx);
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    pub(crate) fn handle_ai_hook(
        &mut self,
        event: crate::ai_hook::AiHookEvent,
        cx: &mut Context<Self>,
    ) {
        let pane_ids: Vec<u64> = self
            .tabs
            .iter()
            .flat_map(|tab| match tab {
                WorkspaceTab::Terminal { panes, .. } => {
                    panes.iter().map(|pane| pane.id).collect::<Vec<_>>()
                },
                _ => Vec::new(),
            })
            .collect();
        let active_focused = self.tabs.get(self.active).and_then(|tab| match tab {
            WorkspaceTab::Terminal { focused, .. } => Some(*focused),
            _ => None,
        });
        let Some(target_id) = ai_hook_target_pane(&pane_ids, event.pane, active_focused) else {
            return;
        };
        let target = self.tabs.iter().find_map(|tab| match tab {
            WorkspaceTab::Terminal { panes, .. } => {
                panes.iter().find(|pane| pane.id == target_id).map(|pane| pane.view.clone())
            },
            _ => None,
        });
        if let Some(view) = target {
            view.update(cx, |view, cx| view.handle_ai_hook(&event, cx));
        }
    }

    /// 在新 tab 里继续一段进行中的 AI 会话（旧壳 `fork_ai_session` 合同）：
    /// 不克隆 PTY，而是按源 tab 的 shell 重开一个终端，把官方 fork 命令敲进
    /// 去。SSH tab 不分叉——往认证提示里注入命令会打错协议层；新 tab 继承
    /// 源 tab 的 cwd 与色标，命名「{Agent} 分叉」。
    pub(super) fn fork_ai_session(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.tabs.get(ix).and_then(WorkspaceTab::focused_view) else { return };
        let (command, cwd, agent) = {
            let view = view.read(cx);
            let Some(command) = view.ai_fork_command() else { return };
            let agent = view
                .ai_session
                .as_ref()
                .and_then(|identity| crate::ai_agents::AgentKind::parse(&identity.source));
            (command, view.local_cwd(), agent)
        };
        let launch_session = match self.meta(ix).launch {
            // Default 与旧壳一致取「分叉这一刻」的默认 shell，不是源 tab
            // 创建时的快照。
            None | Some(crate::session::LaunchSession::Default) => {
                Self::configured_local_launch(cx)
            },
            Some(shell @ crate::session::LaunchSession::Shell { .. }) => shell,
            // Profile 可能直接把 agent 当启动命令，SSH 会把命令注入认证
            // 提示——两者都不分叉（旧壳同合同）。
            Some(
                crate::session::LaunchSession::Profile { .. }
                | crate::session::LaunchSession::Ssh { .. },
            ) => return,
        };
        let color = self.meta(ix).color;
        self.activate_tab(ix, window, cx);

        let shell_tag = Self::launch_shell_tag(&launch_session);
        let grid = self.inherited_grid(cx);
        let launch = Self::terminal_launch_from_session(&launch_session, cwd);
        let pane = self.new_pane(grid, launch, Some(command), window, cx);
        let focused = pane.id;
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane.id),
            panes: vec![pane],
            focused,
            zoomed: false,
            broadcast: false,
        };
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(
            at,
            tab,
            TabMeta {
                custom_name: agent.map(|agent| format!("{} 分叉", agent.display_name())),
                color,
                shell_tag,
                launch: Some(launch_session),
                has_bell: false,
            },
        );
        self.active = at;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }
}
