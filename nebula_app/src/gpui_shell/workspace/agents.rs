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
    let mut source_order = std::collections::HashMap::new();
    let mut rows = Vec::new();
    for session in sessions {
        // scan() 已按最近使用排序；以 provider 首次出现顺序建立稳定分组，既让
        // 最新 provider 置顶，也避免三种以上来源在时间序列中反复穿插表头。
        let group_order = if let Some(order) = source_order.get(&session.source) {
            *order
        } else {
            let order = source_order.len();
            source_order.insert(session.source, order);
            order
        };
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
                hint_style: super::WorkspacePaletteHintStyle::Metadata,
                search: format!("恢复 resume {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
                icon_path: None,
            });
        }
        if let Some(command) = session.fork_command() {
            rows.push(WorkspacePaletteRow {
                group_order,
                group: group.clone(),
                label: format!("分叉 · {}", session.title),
                hint: format!("{source} · {location}"),
                hint_style: super::WorkspacePaletteHintStyle::Metadata,
                search: format!("分叉 fork {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
                icon_path: None,
            });
        }
    }
    rows
}

/// 最小化查询。GPUI 只暴露 `is_window_active`（在不在前台），没有「像素还
/// 到不到屏幕」这一问，所以 Windows 直接问平台。非 Windows 退回只认显式
/// 隐藏：那里的帧回调由各自 compositor 在窗口不可见时自然停发。
#[cfg(windows)]
fn window_minimized(window: &Window) -> bool {
    let Some(hwnd) = super::windowing::native_hwnd(window) else { return false };
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(hwnd as *mut std::ffi::c_void) != 0
    }
}

#[cfg(not(windows))]
fn window_minimized(_window: &Window) -> bool {
    false
}

impl NebulaWorkspace {
    /// 只为当前可见的运行态徽章挂一帧。下一帧必须由随后的实际 render 续期，
    /// 因此任务结束、窗口不可见或徽章滚出视口时会自然断链，不留下常驻
    /// animation loop；由 GPUI 的帧源调度，才能跟随显示器刷新节奏。
    ///
    /// 断链判据是「像素到不了屏幕」而不是「窗口没聚焦」：一边看 Agent 跑、
    /// 一边在浏览器或编辑器里干活，正是这个转圈唯一的用武之地；失焦即冻结
    /// 会让它在最需要的场景里失效，而窗口就在旁边亮着不动，用户只能读成
    /// 「Agent 卡死了」。非活跃窗口的开销由 GPUI 帧循环自己压到 30fps
    /// （`next_frame_callbacks` 非空 + 窗口非活跃即节流），不必在这里再停
    /// 一次。
    pub(super) fn arm_activity_spinner_frame(&self, window: &mut Window, cx: &mut Context<Self>) {
        const PERIOD_SECS: f32 = 0.8;

        if self.spinner_frame_pending.get()
            || !self.spinner_visible.get()
            || self.spinner_offscreen(window)
        {
            return;
        }
        self.spinner_frame_pending.set(true);
        cx.on_next_frame(window, |workspace, window, cx| {
            workspace.spinner_frame_pending.set(false);
            if !workspace.spinner_visible.get() || workspace.spinner_offscreen(window) {
                return;
            }
            let now = std::time::Instant::now();
            let delta = now.saturating_duration_since(workspace.spinner_last);
            workspace.spinner_last = now;
            workspace.spinner_phase =
                (workspace.spinner_phase + delta.as_secs_f32() / PERIOD_SECS).rem_euclid(1.0);
            cx.notify();
        });
    }

    /// 转圈没有观众只有两种情况：托盘隐藏（Nebula 自己记账）和最小化。
    /// 活跃窗口不可能是最小化的，用 `spinner_window_active` 作门控省掉常态
    /// 下那次 user32 调用；这个标志脏成 false 也只是多问一次平台，不会误停。
    fn spinner_offscreen(&self, window: &Window) -> bool {
        self.window_hidden || (!self.spinner_window_active && window_minimized(window))
    }

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
                let retry_callbacks = this.update(cx, |workspace, cx| {
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
                        view.update(cx, |view, cx| {
                            view.refresh_agent_screen_state(cx);
                            // 徽章的时间轴也挂在这一拍上：认出「刚进入完成」的
                            // 边沿、并让对勾在闪现窗口结束后沉降为圆点。
                            view.sync_activity_badges(cx);
                        });
                    }
                    workspace.publish_tray_agents(cx);
                    workspace.runtime_hub.has_pending_delegation_callbacks()
                });
                let Ok(retry_callbacks) = retry_callbacks else { return };
                if retry_callbacks {
                    cx.update(|cx| {
                    // 回调可能早于发起方自己的 TurnDone 到达；每秒重投一次只会
                    // 释放身份仍匹配且已回到 Idle/Finished 的发起方。
                    crate::gpui_shell::workspace::windowing::publish_runtime_snapshot(cx);
                    crate::gpui_shell::workspace::windowing::dispatch_ready_delegation_callbacks(
                        cx,
                    );
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn handle_ai_hook(
        &mut self,
        event: crate::ai_hook::AiHookEvent,
        cx: &mut Context<Self>,
    ) -> bool {
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
            return false;
        };
        let target = self.tabs.iter().find_map(|tab| match tab {
            WorkspaceTab::Terminal { panes, .. } => {
                panes.iter().find(|pane| pane.id == target_id).map(|pane| pane.view.clone())
            },
            _ => None,
        });
        target.is_some_and(|view| view.update(cx, |view, cx| view.handle_ai_hook(&event, cx)))
    }

    pub(crate) fn deliver_delegation_callback(
        &mut self,
        callback: &crate::runtime_api::RuntimeDelegationCallback,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        let pane_id = callback.origin.pane_id;
        let Some(tab_ix) = self.tab_of_pane(pane_id) else {
            return Err(crate::runtime_api::ApiError::new(
                "origin_closed",
                format!("delegation origin pane {pane_id} no longer exists"),
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
        view.update(cx, |view, cx| {
            let agent = view.runtime_chat_agent().ok_or_else(|| {
                crate::runtime_api::ApiError::new(
                    "origin_replaced",
                    "the delegation origin is no longer occupied by an Agent",
                )
            })?;
            if agent.kind != callback.origin.kind
                || callback
                    .origin
                    .session_id
                    .as_deref()
                    .is_some_and(|expected| agent.session_id.as_deref() != Some(expected))
            {
                return Err(crate::runtime_api::ApiError::new(
                    "origin_replaced",
                    "the delegation origin now belongs to a different Agent session",
                ));
            }
            match view.runtime_task_state() {
                crate::runtime_api::RuntimeTaskState::Idle
                | crate::runtime_api::RuntimeTaskState::Finished => {},
                state => {
                    return Err(crate::runtime_api::ApiError::new(
                        "origin_not_ready",
                        format!("the delegation origin cannot accept a callback while {state:?}"),
                    ));
                },
            }
            view.runtime_prompt(callback.prompt.clone(), true, cx)
        })
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
