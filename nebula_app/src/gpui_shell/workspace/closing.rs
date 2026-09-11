use super::*;

impl NebulaWorkspace {
    /// GPUI 的 should-close 回调必须同步返回：无繁忙进程时直接允许系统关闭；
    /// 有繁忙进程时先返回 false，再由对话框确认回调显式移除窗口。
    pub(super) fn should_close_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(windows)]
        windowing::quick_terminal_bounds_changed(self.runtime_window_id, window, cx);
        if self.window_close_pending {
            return false;
        }
        let persist_session = self.window_role == windowing::WindowRole::Regular;
        if persist_session && self.keep_session_on_close(window, cx) {
            return false;
        }
        if self.guard_file_window_close(window, cx) {
            return false;
        }
        self.close_window_after_documents(window, cx)
    }

    /// Continue after document save/discard confirmation. A true result lets
    /// the caller remove the window; false means cancellation or an async close.
    pub(super) fn close_window_after_documents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let persist_session = self.window_role == windowing::WindowRole::Regular;
        let Some(process) = self.busy_process_in_window(cx) else {
            if persist_session {
                self.finish_close_window(window, cx);
                return false;
            }
            return true;
        };
        if self.window_close_confirm_open {
            return false;
        }
        self.window_close_confirm_open = true;

        let body: SharedString = format!("{process} 仍在运行，关闭窗口会中止它。").into();
        let confirm_workspace = cx.entity().downgrade();
        let close_workspace = confirm_workspace.clone();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let confirm_workspace = confirm_workspace.clone();
            let close_workspace = close_workspace.clone();
            confirm_dialog(
                dialog,
                window,
                "关闭窗口？",
                body.clone(),
                "关闭",
                "取消",
                ButtonVariant::Danger,
            )
            .on_ok(move |_, window, cx| {
                let _ = confirm_workspace.update(cx, |workspace, cx| {
                    if persist_session {
                        workspace.finish_close_window(window, cx);
                    }
                    workspace.window_close_confirm_open = false;
                    // `remove_window` 是确认后的最终动作，不会重新触发
                    // should-close，从而避免再次弹出同一确认框。
                    if !persist_session {
                        window.remove_window();
                    }
                });
                true
            })
            .on_close(move |_, _, cx| {
                let _ = close_workspace.update(cx, |workspace, cx| {
                    workspace.window_close_confirm_open = false;
                    cx.notify();
                });
            })
        });
        false
    }

    fn finish_close_window(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.window_close_pending = true;
        let panes = self.prepare_session_save(cx);
        let handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            wait_for_session_ids(&panes, cx).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |workspace, cx| {
                    workspace.save_clean_window_session(cx);
                    window.remove_window();
                });
            });
        })
        .detach();
    }

    pub(super) fn prepare_session_save(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<Entity<TerminalView>> {
        let panes = self
            .tabs
            .iter()
            .filter_map(|tab| match tab {
                WorkspaceTab::Terminal { panes, .. } => Some(panes),
                _ => None,
            })
            .flatten()
            .map(|pane| pane.view.clone())
            .collect::<Vec<_>>();
        for pane in &panes {
            pane.update(cx, |view, cx| view.prepare_ai_session_save(cx));
        }
        panes
    }
}

pub(super) async fn wait_for_session_ids(panes: &[Entity<TerminalView>], cx: &mut gpui::AsyncApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let pending =
            cx.update(|cx| panes.iter().any(|pane| pane.read(cx).ai_session_save_pending()));
        if !pending || std::time::Instant::now() >= deadline {
            break;
        }
        cx.background_executor().timer(Duration::from_millis(25)).await;
    }
}
