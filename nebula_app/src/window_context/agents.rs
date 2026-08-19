//! legacy 壳向托盘投影的 Agent 视图。

use super::WindowContext;

impl WindowContext {
    /// 托盘与侧栏读取同一 pane 事实源，不维护第二份 Agent 状态。
    pub fn tray_agents(&self) -> Vec<crate::tray::TrayAgent> {
        self.panes
            .iter()
            .filter_map(|pane| {
                let state = &pane.nebula_state;
                let program = state
                    .running_program
                    .as_deref()
                    .filter(|program| crate::ai_agents::AgentKind::parse(program).is_some())?;
                // 项目目录比 pane id 更适合作为托盘中的人工识别信息。
                let place = std::path::Path::new(state.cwd.trim())
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let label = if place.is_empty() {
                    program.to_owned()
                } else {
                    format!("{program} · {place}")
                };
                Some(crate::tray::TrayAgent {
                    window: self.display.window.id(),
                    pane: pane.id,
                    label,
                    needs_attention: state.needs_attention,
                })
            })
            .collect()
    }
}
