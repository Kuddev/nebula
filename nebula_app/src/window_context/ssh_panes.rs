//! SSH pane construction and configuration at the legacy window boundary.

use super::*;

pub(super) fn apply_terminal_config(pane: &Pane, config: &UiConfig) {
    let mut options = config.term_options();
    if pane.ssh_destination.is_some() {
        options = crate::ssh_session::terminal_config(options);
    }
    pane.terminal.lock().set_options(options);
}

#[cfg(windows)]
impl WindowContext {
    /// 创建由远端 PTY 通道驱动的 Pane，并复用本地终端的解析、渲染和事件协议。
    /// 这样传输层只负责字节流，输入、缩放与终端状态无需维护两套实现。
    pub(super) fn create_ssh_pane(
        size_info: &crate::display::SizeInfo,
        window_id: WindowId,
        config: &UiConfig,
        proxy: &EventLoopProxy<Event>,
        pane_id: PaneId,
        destination: String,
        remote_cwd: Option<String>,
    ) -> Result<Pane, Box<dyn Error>> {
        let window_route = Arc::new(AtomicU64::new(window_id.into()));
        let event_proxy = EventProxy::new_tab(proxy.clone(), window_route.clone(), pane_id);
        let terminal = Arc::new(FairMutex::new(Term::new(
            crate::ssh_session::terminal_config(config.term_options()),
            size_info,
            event_proxy.clone(),
        )));
        let sender = crate::ssh_session::spawn_session_at(
            destination.clone(),
            remote_cwd,
            (*size_info).into(),
            terminal.clone(),
            event_proxy.clone(),
        )?;
        if config.cursor.style().blinking {
            event_proxy.send_event(TerminalEvent::CursorBlinkingChange.into());
        }
        Ok(Pane {
            terminal,
            notifier: Notifier(sender),
            search_state: Default::default(),
            inline_search_state: Default::default(),
            id: pane_id,
            title: String::from("ssh"),
            exec_context: None,
            ssh_destination: Some(destination),
            nebula_state: NebulaPaneState::default(),
            intro_cols: None,
            shell_pid: 0,
            window_route,
        })
    }
}
