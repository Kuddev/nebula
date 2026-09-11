//! Transfer requests, conflict confirmation and native file selection.

use super::*;
use crate::i18n::Message;

impl NebulaWorkspace {
    pub(super) fn remote_transfer_snapshot(&self) -> Option<SftpSnapshot> {
        self.remote_browser.transfer.as_ref().map(SftpController::snapshot)
    }

    pub(super) fn remote_transfer_working(&self) -> bool {
        self.remote_transfer_snapshot().is_some_and(|snapshot| snapshot.phase == SftpPhase::Working)
    }

    /// 为当前 pane/目录取得控制器，并启动一条常驻 wake 接收协程。
    ///
    /// 接收协程只持有 channel，不持有 controller；否则 controller 的 wake 闭包
    /// 持有 sender、协程再持有 controller，会形成直到进程退出才释放的环。
    pub(super) fn remote_transfer_controller(
        &mut self,
        target: &RemoteTransferTarget,
        cx: &mut Context<'_, Self>,
    ) -> Result<SftpController, String> {
        self.remote_browser.last_outcome = None;
        let destination = target.destination.clone();
        let path = target.path.clone();
        if destination.is_empty() || path.is_empty() {
            return Err(workspace_ui_language().text(Message::TransferNotReady).to_owned());
        }

        if let Some(controller) = self.remote_browser.transfer.as_ref() {
            let snapshot = controller.snapshot();
            if snapshot.phase == SftpPhase::Working {
                return Err(workspace_ui_language()
                    .format(Message::TransferBusyAt, &[("host", &snapshot.destination)]));
            }
        }

        // Each request owns a fresh wake subscription. A failed transfer from
        // another pane on the same host/path must not retain that pane's identity.
        self.remote_browser.transfer_id = self.remote_browser.transfer_id.wrapping_add(1).max(1);
        let transfer_id = self.remote_browser.transfer_id;
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let controller = SftpController::new_at(destination.clone(), path, wake)
            .map_err(|error| error.to_string())?;
        self.remote_browser.transfer = Some(controller.clone());

        let target = target.clone();
        cx.spawn(async move |this, cx| {
            while wake_rx.recv().await.is_some() {
                let finished = this
                    .update(cx, |workspace, cx| {
                        workspace.sync_remote_transfer(transfer_id, &target, cx)
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        })
        .detach();
        Ok(controller)
    }

    /// 把 controller 快照落回 GPUI 状态。返回 true 表示接收协程可以退出。
    pub(super) fn sync_remote_transfer(
        &mut self,
        transfer_id: u64,
        target: &RemoteTransferTarget,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if self.remote_browser.transfer_id != transfer_id {
            return true;
        }
        let Some(controller) = self.remote_browser.transfer.as_ref() else {
            return true;
        };
        let snapshot = controller.snapshot();
        if snapshot.destination != target.destination {
            return true;
        }

        let visible = self.remote_browser.pane == Some(target.pane)
            && self.remote_browser.destination == target.destination;
        if visible {
            match snapshot.phase {
                SftpPhase::Ready => {
                    // 用户可在传输期间继续浏览；只有仍停在传输起始目录时才替换
                    // 列表，否则完成结果会把用户从刚进入的目录拉回去。
                    if self.remote_transfer_target_matches(target)
                        && self.remote_browser.path == snapshot.path
                    {
                        self.remote_browser.entries = snapshot.entries.clone();
                        self.remote_browser.selected = None;
                    }
                    self.remote_browser.error = None;
                },
                SftpPhase::Error => self.remote_browser.error = snapshot.error.clone(),
                SftpPhase::Cancelled => {
                    self.remote_browser.error = None;
                    self.remote_browser.last_outcome = Some(Message::TransferCancelled);
                },
                SftpPhase::Working => {},
                SftpPhase::Connecting | SftpPhase::Loading => {},
            }
        }

        let finished = matches!(snapshot.phase, SftpPhase::Ready | SftpPhase::Cancelled);
        if finished {
            self.remote_browser.transfer = None;
        }
        cx.notify();
        finished
    }

    pub(super) fn start_remote_transfer(
        &mut self,
        pending: PendingRemoteTransfer,
        conflict: SftpConflictPolicy,
        target: &RemoteTransferTarget,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.remote_transfer_target_matches(target) {
            self.remote_browser.error =
                Some(workspace_ui_language().text(Message::TransferTargetChanged).to_owned());
            cx.notify();
            return;
        }
        let controller = match self.remote_transfer_controller(target, cx) {
            Ok(controller) => controller,
            Err(message) => {
                self.remote_browser.error = Some(message);
                cx.notify();
                return;
            },
        };
        let options =
            SftpTransferOptions { conflict, skip_unchanged: self.remote_browser.skip_unchanged };
        self.remote_browser.error = None;
        match pending {
            PendingRemoteTransfer::Upload(paths) => {
                controller.upload_paths_with_options(paths, options)
            },
            PendingRemoteTransfer::Download { entry, local_directory } => {
                controller.download_with_options(entry, local_directory, options)
            },
            PendingRemoteTransfer::Copy(source) => {
                controller.copy_from(source.source_destination, source.entry, options)
            },
        }
        cx.notify();
    }

    pub(super) fn current_remote_transfer_target(&self) -> Option<RemoteTransferTarget> {
        if self.remote_browser.loading || self.remote_browser.path.is_empty() {
            return None;
        }
        Some(RemoteTransferTarget {
            pane: self.remote_browser.pane?,
            destination: self.remote_browser.destination.clone(),
            path: self.remote_browser.path.clone(),
            navigation_generation: self.remote_browser.generation,
        })
    }

    pub(super) fn remote_transfer_target_matches(&self, target: &RemoteTransferTarget) -> bool {
        target.is_current(
            self.remote_browser.pane,
            &self.remote_browser.destination,
            self.remote_browser.generation,
        )
    }

    pub(super) fn remote_cancel_transfer(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(controller) = self.remote_browser.transfer.as_ref() {
            controller.cancel();
            cx.notify();
        }
    }

    pub(super) fn request_remote_transfer(
        &mut self,
        pending: PendingRemoteTransfer,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(target) = self.current_remote_transfer_target() else {
            self.remote_browser.error =
                Some(workspace_ui_language().text(Message::TransferNotReady).to_owned());
            cx.notify();
            return;
        };
        self.request_remote_transfer_at(pending, target, window, cx);
    }

    pub(super) fn request_remote_transfer_at(
        &mut self,
        pending: PendingRemoteTransfer,
        target: RemoteTransferTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.remote_transfer_target_matches(&target) {
            self.remote_browser.error =
                Some(workspace_ui_language().text(Message::TransferTargetChanged).to_owned());
            cx.notify();
            return;
        }
        if self.remote_transfer_working() || self.remote_browser.preflighting {
            self.remote_browser.error =
                Some(workspace_ui_language().text(Message::TransferBusy).to_owned());
            cx.notify();
            return;
        }
        self.remote_browser.preflight_id = self.remote_browser.preflight_id.wrapping_add(1);
        let request = self.remote_browser.preflight_id;
        self.remote_browser.preflighting = true;
        self.remote_browser.last_outcome = None;
        self.remote_browser.error = None;
        let skip_unchanged = self.remote_browser.skip_unchanged;
        let checked = pending.clone();
        let check_target = target.clone();
        let task = cx.background_executor().spawn(async move {
            let entries = if matches!(checked, PendingRemoteTransfer::Download { .. }) {
                Vec::new()
            } else {
                remote_call(move || async move {
                    crate::ssh_sftp::list_dir(&check_target.destination, &check_target.path).await
                })
                .await
                .ok_or_else(|| "Remote connection is unavailable".to_owned())??
            };
            Ok::<_, String>(pending_remote_conflicts(&checked, &entries, skip_unchanged))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |workspace, window, cx| {
                if workspace.remote_browser.preflight_id != request {
                    return;
                }
                workspace.remote_browser.preflighting = false;
                if !workspace.remote_transfer_target_matches(&target) {
                    workspace.remote_browser.error = Some(
                        workspace_ui_language().text(Message::TransferTargetChanged).to_owned(),
                    );
                    cx.notify();
                    return;
                }
                match result {
                    Ok((0, _, _)) => workspace.start_remote_transfer(
                        pending,
                        SftpConflictPolicy::Overwrite,
                        &target,
                        cx,
                    ),
                    Ok((count, overwrite, symlink)) => workspace.open_remote_conflict_dialog(
                        pending, target, count, overwrite, symlink, window, cx,
                    ),
                    Err(error) => workspace.remote_browser.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_remote_conflict_dialog(
        &mut self,
        pending: PendingRemoteTransfer,
        target: RemoteTransferTarget,
        conflicts: usize,
        overwrite_allowed: bool,
        follows_symlink: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = cx.entity().downgrade();
        let language = workspace_ui_language();
        let target_label = match &pending {
            PendingRemoteTransfer::Download { local_directory, .. } => {
                local_directory.display().to_string()
            },
            _ => format!("{}: {}", target.destination, target.path),
        };
        window.open_dialog(cx, move |dialog, window, _cx| {
            let skip_workspace = workspace.clone();
            let skip_pending = pending.clone();
            let skip_target = target.clone();
            let keep_workspace = workspace.clone();
            let keep_pending = pending.clone();
            let keep_target = target.clone();
            let overwrite_workspace = workspace.clone();
            let overwrite_pending = pending.clone();
            let overwrite_target = target.clone();
            let footer = DialogFooter::new()
                .child(
                    DialogClose::new().child(
                        Button::new("sftp-conflict-cancel")
                            .label(language.text(Message::TransferCancel)),
                    ),
                )
                .child(div().flex_1())
                .child(
                    DialogClose::new().child(
                        Button::new("sftp-conflict-skip")
                            .label(language.text(Message::TransferSkip))
                            .on_click(move |_, _, cx| {
                                let Some(workspace) = skip_workspace.upgrade() else { return };
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.start_remote_transfer(
                                        skip_pending.clone(),
                                        SftpConflictPolicy::Skip,
                                        &skip_target,
                                        cx,
                                    );
                                });
                            }),
                    ),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("sftp-conflict-keep-both")
                            .label(language.text(Message::TransferKeepBoth))
                            .on_click(move |_, _, cx| {
                                let Some(workspace) = keep_workspace.upgrade() else { return };
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.start_remote_transfer(
                                        keep_pending.clone(),
                                        SftpConflictPolicy::KeepBoth,
                                        &keep_target,
                                        cx,
                                    );
                                });
                            }),
                    ),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("sftp-conflict-overwrite")
                            .label(language.text(Message::TransferOverwrite))
                            .danger()
                            .disabled(!overwrite_allowed)
                            .on_click(move |_, _, cx| {
                                let Some(workspace) = overwrite_workspace.upgrade() else { return };
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.start_remote_transfer(
                                        overwrite_pending.clone(),
                                        SftpConflictPolicy::Overwrite,
                                        &overwrite_target,
                                        cx,
                                    );
                                });
                            }),
                    ),
                );

            center_modal_dialog(dialog, window, 220.0)
                .close_button(false)
                .overlay_closable(true)
                .title(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(language.text(Message::TransferConflictTitle)),
                )
                .footer(footer)
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(div().text_xs().text_color(_cx.theme().muted_foreground).child(
                            language.format(
                                Message::TransferDestination,
                                &[("destination", &target_label)],
                            ),
                        ))
                        .child(language.format(
                            Message::TransferConflictBody,
                            &[("count", &conflicts.to_string())],
                        ))
                        .when(!overwrite_allowed, |body| {
                            body.child(
                                div()
                                    .text_sm()
                                    .text_color(_cx.theme().danger)
                                    .child(language.text(Message::TransferIncompatible)),
                            )
                        })
                        .when(follows_symlink, |body| {
                            body.child(
                                div()
                                    .text_sm()
                                    .text_color(_cx.theme().danger)
                                    .child(language.text(Message::TransferSymlinkWarning)),
                            )
                        }),
                )
        });
    }

    pub(super) fn remote_copy_selected(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.selected_remote_entry() else { return };
        self.remote_browser.clipboard = Some(RemoteClipboard {
            source_destination: self.remote_browser.destination.clone(),
            entry: entry.clone(),
        });
        crate::gpui_shell::toast::toast(
            window,
            cx,
            crate::display::ToastKind::Info,
            workspace_ui_language().format(Message::TransferCopiedItem, &[("name", &entry.name)]),
        );
        cx.notify();
    }

    pub(super) fn remote_paste(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(source) = self.remote_browser.clipboard.clone() else { return };
        self.request_remote_transfer(PendingRemoteTransfer::Copy(source), window, cx);
    }

    pub(super) fn remote_pick_upload_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(target) = self.current_remote_transfer_target() else { return };
        let picked = crate::platform::file_picker::files(
            window,
            cx,
            workspace_ui_language().text(Message::TransferChooseFiles),
        );

        cx.spawn_in(window, async move |this, cx| {
            let paths = picked.await;
            if paths.is_empty() {
                return;
            }
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer_at(
                    PendingRemoteTransfer::Upload(paths),
                    target,
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    pub(super) fn remote_pick_upload_directory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(target) = self.current_remote_transfer_target() else { return };
        let picked = crate::platform::file_picker::directory(
            window,
            cx,
            workspace_ui_language().text(Message::TransferChooseUploadDirectory),
        );

        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer_at(
                    PendingRemoteTransfer::Upload(vec![path]),
                    target,
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    pub(super) fn remote_pick_download_directory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(target) = self.current_remote_transfer_target() else { return };
        let Some(entry) = self.selected_remote_entry() else { return };
        let picked = crate::platform::file_picker::directory(
            window,
            cx,
            workspace_ui_language().text(Message::TransferChooseDownloadDirectory),
        );

        cx.spawn_in(window, async move |this, cx| {
            let Some(local_directory) = picked.await else { return };
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer_at(
                    PendingRemoteTransfer::Download { entry, local_directory },
                    target,
                    window,
                    cx,
                );
            });
        })
        .detach();
    }
}

fn pending_remote_conflicts(
    pending: &PendingRemoteTransfer,
    entries: &[SftpEntry],
    skip_unchanged: bool,
) -> (usize, bool, bool) {
    match pending {
        PendingRemoteTransfer::Upload(paths) => {
            let mut conflicts = 0;
            let mut overwrite_allowed = true;
            let mut follows_symlink = false;
            let mut root_names = HashSet::with_capacity(paths.len());
            for path in paths {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let collides_with_batch = !root_names.insert(name.to_owned());
                let target = entries.iter().find(|entry| entry.name == name);
                let source = std::fs::symlink_metadata(path).ok();
                if !collides_with_batch
                    && skip_unchanged
                    && source
                        .as_ref()
                        .zip(target)
                        .is_some_and(|(source, target)| local_metadata_matches(source, target))
                {
                    continue;
                }
                if !collides_with_batch && target.is_none() {
                    continue;
                }
                conflicts += 1;
                follows_symlink |=
                    target.is_some_and(|target| target.kind == SftpEntryKind::Symlink);
                if collides_with_batch {
                    overwrite_allowed = false;
                }
                if let (Some(target), Some(source)) = (target, source) {
                    let compatible = (source.is_dir() && target.kind == SftpEntryKind::Directory)
                        || (source.is_file()
                            && matches!(target.kind, SftpEntryKind::File | SftpEntryKind::Symlink));
                    overwrite_allowed &= compatible;
                }
            }
            (conflicts, overwrite_allowed, follows_symlink)
        },
        PendingRemoteTransfer::Download { entry, local_directory } => {
            let target = local_directory.join(&entry.name);
            let Ok(metadata) = std::fs::symlink_metadata(target) else {
                return (0, true, false);
            };
            if skip_unchanged && local_metadata_matches(&metadata, entry) {
                return (0, true, false);
            }
            let compatible = match entry.kind {
                SftpEntryKind::Directory => metadata.is_dir(),
                SftpEntryKind::File => metadata.is_file() || metadata.file_type().is_symlink(),
                // 链接的目标类型必须由远端 lstat/readlink 后才能确定，交给
                // 内核的执行时校验，UI 不凭列表图标猜。
                SftpEntryKind::Symlink => true,
            };
            (1, compatible, metadata.file_type().is_symlink())
        },
        PendingRemoteTransfer::Copy(source) => {
            let Some(target) = entries.iter().find(|entry| entry.name == source.entry.name) else {
                return (0, true, false);
            };
            if skip_unchanged
                && source.entry.kind == SftpEntryKind::File
                && target.kind == SftpEntryKind::File
                && source.entry.size == target.size
                && source.entry.modified != 0
                && source.entry.modified == target.modified
            {
                return (0, true, false);
            }
            let compatible = match source.entry.kind {
                SftpEntryKind::Directory => target.kind == SftpEntryKind::Directory,
                SftpEntryKind::File => {
                    matches!(target.kind, SftpEntryKind::File | SftpEntryKind::Symlink)
                },
                SftpEntryKind::Symlink => true,
            };
            (1, compatible, target.kind == SftpEntryKind::Symlink)
        },
    }
}
