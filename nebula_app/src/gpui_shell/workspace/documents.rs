//! File routing and draft protection for tab/window lifecycles.

use super::super::file_editor::TextFileView;
use super::*;
use crate::i18n::Message;

impl WorkspaceTab {
    pub(super) fn file_editor(&self, cx: &App) -> Option<Entity<TextFileView>> {
        match self {
            Self::Document { view, .. } => Some(view.clone()),
            Self::Code { view, .. } => view.read(cx).file_editor(),
            _ => None,
        }
    }
}

impl NebulaWorkspace {
    pub(super) fn guard_file_tab_close(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(file) = self.tabs.get(index).and_then(|tab| tab.file_editor(cx)) else {
            return false;
        };
        if file.read(cx).is_saving() {
            return true;
        }
        if !file.read(cx).is_dirty() {
            return false;
        }
        self.confirm_files_close(vec![file], false, window, cx);
        true
    }

    pub(super) fn guard_file_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let files: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.file_editor(cx))
            .filter(|file| file.read(cx).is_dirty() || file.read(cx).is_saving())
            .collect();
        if files.is_empty() {
            return false;
        }
        if !files.iter().any(|file| file.read(cx).is_saving()) {
            self.confirm_files_close(files, true, window, cx);
        }
        true
    }

    fn confirm_files_close(
        &mut self,
        files: Vec<Entity<TextFileView>>,
        whole_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.window_close_confirm_open {
            return;
        }
        self.window_close_confirm_open = true;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let mut accepted = true;
            let mut discarded = Vec::new();
            for file in &files {
                let prompt = handle.update(cx, |_, window, cx| {
                    let language = crate::gpui_shell::config::ui_language(cx);
                    let draft = file.read(cx).draft(cx);
                    let prompt = window.prompt(
                        gpui::PromptLevel::Warning,
                        language.text(Message::EditorCloseTitle),
                        Some(&file.read(cx).source_label()),
                        &[
                            language.text(Message::EditorSave),
                            language.text(Message::EditorDiscard),
                            language.text(Message::EditorCancel),
                        ],
                        cx,
                    );
                    (prompt, draft)
                });
                let Ok((prompt, draft)) = prompt else {
                    accepted = false;
                    break;
                };
                match prompt.await {
                    Ok(0) => {
                        let task = file.update(cx, |file, cx| file.save(cx));
                        if !task.await {
                            accepted = false;
                            break;
                        }
                    },
                    Ok(1) => discarded.push((file.clone(), draft)),
                    _ => {
                        accepted = false;
                        break;
                    },
                }
            }
            let _ = handle.update(cx, |_, window, cx| {
                let _ =
                    this.update(cx, |workspace, cx| {
                        workspace.window_close_confirm_open = false;
                        if !accepted {
                            cx.notify();
                            return;
                        }
                        let relevant = if whole_window {
                            workspace
                                .tabs
                                .iter()
                                .filter_map(|tab| tab.file_editor(cx))
                                .collect::<Vec<_>>()
                        } else {
                            files.clone()
                        };
                        if relevant.iter().any(|file| {
                            let view = file.read(cx);
                            view.is_saving()
                                || (view.is_dirty()
                                    && !discarded.iter().any(|(approved, draft)| {
                                        approved == file && *draft == view.draft(cx)
                                    }))
                        }) {
                            cx.notify();
                            return;
                        }
                        if whole_window {
                            if workspace.close_window_after_documents(window, cx) {
                                window.remove_window();
                            }
                        } else if let Some(index) = workspace.tabs.iter().position(|tab| {
                            tab.file_editor(cx).is_some_and(|file| file == files[0])
                        }) {
                            workspace.finish_close_tab(index, window, cx);
                        }
                    });
            });
        })
        .detach();
    }

    /// 调试/验收后门：`NEBULA_GPUI_OPEN_DOC=路径` 时启动即打开该文档，
    /// 与文件树双击同一条路由（公式渲染等文档 UI 的免点击验收）。
    pub fn open_document_at_startup(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_document_path(path, window, cx);
    }

    /// 文件路由（旧壳 `input/chrome.rs` 双击合同）：图片 → 图片 tab；
    /// Markdown → 文档 tab（TextView 富渲染）；其余可读文本（txt/log/json
    /// 与源码）→ 代码 tab（行号 + 行级虚拟化，用户裁定 txt 同代码一样）；
    /// 都不认的交系统处理器。
    pub(super) fn open_document_path(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_markdown =
            path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
            });
        if crate::display::image_viewer::viewable_file(&path) {
            self.open_image_tab(path, window, cx);
        } else if is_markdown {
            self.open_doc_tab(path, window, cx);
        } else if crate::gpui_shell::code_tab::viewable_file(&path)
            || crate::display::markdown_view::viewable_file(&path)
        {
            self.open_code_tab(path, window, cx);
        } else {
            open_in_file_manager(&path);
        }
    }

    /// 同一路径复用已开 tab（激活 + 重读盘，旧壳 open_image_tab 同语义）。
    fn open_image_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Image { view } if view.read(cx).path == path),
        ) {
            if let Some(WorkspaceTab::Image { view }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload(cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::doc_tabs::ImageTabView::new(path, cx));
        self.insert_new_tab(WorkspaceTab::Image { view });
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn open_remote_document(
        &mut self,
        destination: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = crate::ssh_sftp::document::RemoteLocation { destination, path };
        if let Some(index) = self.tabs.iter().position(|tab| matches!(tab, WorkspaceTab::Document { view, .. } if view.read(cx).is_remote_location(&location))) {
            self.activate_tab(index, window, cx);
            return;
        }
        let view = cx.new(|cx| TextFileView::new_remote(location, window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_document_event);
        self.insert_new_tab(WorkspaceTab::Document { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    fn open_doc_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Document { view, .. } if view.read(cx).is_local_path(&path)),
        ) {
            if let Some(WorkspaceTab::Document { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| {
                    view.reload(window, cx);
                    cx.notify();
                });
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::doc_tabs::DocTabView::new(path, window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_document_event);
        self.insert_new_tab(WorkspaceTab::Document { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    fn open_code_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Code { view, .. } if view.read(cx).is_regular_path(&path)),
        ) {
            if let Some(WorkspaceTab::Code { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload(window, cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::code_tab::CodeTabView::new(path, window, cx));
        let subscription = cx.subscribe(&view, Self::on_code_tab_event);
        self.insert_new_tab(WorkspaceTab::Code { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 冲突文件仍属于代码 Tab，但以三栏合并形态打开；同一仓库、同一路径复用
    /// 已有 Tab，避免用户从冲突列表连续点击后堆出多个独立结果缓冲区。
    pub(super) fn open_git_merge_tab(
        &mut self,
        relative_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = self.side_panel.git_location() else { return };
        if let Some(ix) = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Code { view, .. }
                if view.read(cx).matches_git_merge(&location, &relative_path))
        }) {
            if let Some(WorkspaceTab::Code { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload_git_merge(window, cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| {
            crate::gpui_shell::code_tab::CodeTabView::new_git_merge(
                location,
                relative_path,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&view, Self::on_code_tab_event);
        self.insert_new_tab(WorkspaceTab::Code { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_document_event(
        &mut self,
        file: &Entity<crate::gpui_shell::doc_tabs::DocTabView>,
        event: &DocTabViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DocTabViewEvent::Changed => cx.notify(),
            DocTabViewEvent::SelectionContextMenuRequested { position, text } => {
                self.open_document_selection_context_menu(*position, text.clone(), window, cx);
            },
            DocTabViewEvent::ReaderFocusChanged { focused: true } => {
                self.enter_reader_focus(file.clone(), cx);
            },
            DocTabViewEvent::ReaderFocusChanged { focused: false } => {
                self.exit_reader_focus_for(file, cx);
            },
        }
    }

    /// Whether the active tab is currently using the temporary reader focus
    /// presentation. The entity check prevents a stale tab index from hiding
    /// chrome after a reorder or close.
    pub(super) fn reader_focus_active(&self, cx: &App) -> bool {
        let Some(state) = self.reader_focus.as_ref() else { return false };
        let entity_matches = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.file_editor(cx))
            .is_some_and(|file| file == state.file);
        Self::reader_focus_should_hide_chrome(entity_matches, state.file.read(cx).reader_focus())
    }

    /// Derived presentation contract: a focus record can hide chrome only if
    /// it still belongs to the active entity and that entity remains focused.
    /// This is deliberately independent of sidebar persistence state.
    pub(super) fn reader_focus_should_hide_chrome(
        active_entity_matches: bool,
        document_focus: bool,
    ) -> bool {
        active_entity_matches && document_focus
    }

    fn enter_reader_focus(
        &mut self,
        file: Entity<crate::gpui_shell::doc_tabs::DocTabView>,
        cx: &mut Context<Self>,
    ) {
        let is_active = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.file_editor(cx))
            .is_some_and(|active| active == file);
        if !is_active || self.reader_focus.is_some() {
            return;
        }
        // Keep workspace state untouched. Render derives the immersive view
        // from this Entity and temporarily omits surrounding chrome, so tab
        // changes and command actions cannot strand a collapsed sidebar.
        self.reader_focus = Some(super::ReaderFocusState { file });
        cx.notify();
    }

    fn exit_reader_focus_for(
        &mut self,
        file: &Entity<crate::gpui_shell::doc_tabs::DocTabView>,
        cx: &mut Context<Self>,
    ) {
        let matches = self.reader_focus.as_ref().is_some_and(|state| state.file == *file);
        if matches {
            self.restore_reader_focus(cx);
        }
    }

    /// Leave focus before activating/closing/reordering another tab. The
    /// document restores its own details panel; this restores workspace chrome.
    pub(super) fn clear_reader_focus(&mut self, cx: &mut Context<Self>) -> bool {
        if self.reader_focus.is_none() {
            return false;
        }
        self.restore_reader_focus(cx)
    }

    /// `insert_new_tab` and a few restore paths update `active` directly. Run
    /// this before render-derived layout so an old focused document cannot
    /// reappear in focus after opening a new tab and later returning to it.
    pub(super) fn clear_stale_reader_focus(&mut self, cx: &mut Context<Self>) -> bool {
        let stale = self.reader_focus.as_ref().is_some_and(|state| {
            let active_matches = self
                .tabs
                .get(self.active)
                .and_then(|tab| tab.file_editor(cx))
                .is_some_and(|file| file == state.file);
            !active_matches || !state.file.read(cx).reader_focus()
        });
        stale && self.restore_reader_focus(cx)
    }

    fn restore_reader_focus(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.reader_focus.take() else { return false };
        let _ = state.file.update(cx, |file, cx| file.clear_reader_focus(cx));
        cx.notify();
        true
    }

    fn on_code_tab_event(
        &mut self,
        _: Entity<crate::gpui_shell::code_tab::CodeTabView>,
        event: &CodeTabViewEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            CodeTabViewEvent::Changed => cx.notify(),
            CodeTabViewEvent::GitConflictResolved => {
                self.side_panel.request_refresh();
                cx.notify();
            },
        }
    }
}

#[cfg(test)]
mod reader_focus_contract_tests {
    use super::NebulaWorkspace;

    #[test]
    fn stale_focus_record_cannot_hide_a_new_active_tab() {
        assert!(!NebulaWorkspace::reader_focus_should_hide_chrome(false, true));
        assert!(!NebulaWorkspace::reader_focus_should_hide_chrome(true, false));
        assert!(NebulaWorkspace::reader_focus_should_hide_chrome(true, true));
    }
}
