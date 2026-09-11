//! File gesture routing and the handoff to the Windows drag worker.

use super::*;
use std::sync::{Mutex, OnceLock};

struct ExportBroker {
    receive: Mutex<
        Option<
            tokio::sync::oneshot::Receiver<
                Result<Arc<crate::ssh_sftp::export::PreparedExport>, String>,
            >,
        >,
    >,
    prepared: OnceLock<Result<Arc<crate::ssh_sftp::export::PreparedExport>, String>>,
}

impl ExportBroker {
    fn get(&self) -> std::io::Result<Arc<crate::ssh_sftp::export::PreparedExport>> {
        self.prepared
            .get_or_init(|| {
                self.receive
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .ok_or_else(|| "File drag preparation was already consumed".to_owned())?
                    .blocking_recv()
                    .map_err(|_| "File drag preparation ended".to_owned())?
            })
            .clone()
            .map_err(std::io::Error::other)
    }

    fn finish(&self, result: &Result<DragOutcome, String>) {
        if let Some(Ok(prepared)) = self.prepared.get() {
            prepared.finish(match result {
                Ok(DragOutcome::Copied) => Ok(()),
                Ok(DragOutcome::Cancelled) => Err("File drag cancelled".into()),
                Err(error) => Err(error.clone()),
            });
        }
    }
}

use crate::i18n::Message;
use crate::platform::file_drag::{DragFile, DragOutcome, MaterializedFile};

#[derive(Clone)]
pub(super) struct RemoteFileDrag {
    pub(super) source: RemoteTransferTarget,
    pub(super) entry: SftpEntry,
}

impl NebulaWorkspace {
    pub(super) fn drop_upload_paths(
        &mut self,
        paths: Vec<PathBuf>,
        directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let Some(mut target) = self.current_remote_transfer_target() else {
            return;
        };
        if let Some(directory) = directory {
            target.path = directory;
        }
        self.request_remote_transfer_at(PendingRemoteTransfer::Upload(paths), target, window, cx);
    }

    pub(super) fn begin_native_download_drag(
        &mut self,
        drag: RemoteFileDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::platform::file_drag::supported() || !cx.stop_active_drag(window) {
            return;
        }
        if !self.remote_transfer_target_matches(&drag.source) {
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
        let controller = match self.remote_transfer_controller(&drag.source, cx) {
            Ok(controller) => controller,
            Err(error) => {
                self.remote_browser.error = Some(error);
                cx.notify();
                return;
            },
        };
        let ready = controller.prepare_export(drag.entry, self.remote_browser.entries.clone());
        crate::platform::file_drag::release_pointer();
        let (done, receive) = futures::channel::oneshot::channel();
        let cancel = controller.cancellation_probe();
        let origin = crate::platform::file_drag::DragOrigin::capture();
        let worker_controller = controller.clone();
        let worker = std::thread::Builder::new().name("pebrel-file-drag".into()).spawn(move || {
            let broker = Arc::new(ExportBroker {
                receive: Mutex::new(Some(ready)),
                prepared: OnceLock::new(),
            });
            let describe = broker.clone();
            let contents = broker.clone();
            let stream_cancel = cancel.clone();
            // Enter OLE immediately. Network work is requested lazily by the
            // shell, so a short drag is not lost while SFTP prepares a directory.
            let result = crate::platform::file_drag::run(
                origin,
                Arc::new(move || {
                    Ok(describe
                        .get()?
                        .files
                        .iter()
                        .map(|file| DragFile {
                            relative_path: file.relative_path.clone(),
                            directory: file.directory,
                            size: file.size,
                        })
                        .collect())
                }),
                Arc::new(move |index| {
                    let (path, lease) = contents.get()?.materialize(index)?;
                    Ok(MaterializedFile { path, lease, cancelled: stream_cancel.clone() })
                }),
                cancel,
            )
            .map_err(|error| error.to_string());
            if !matches!(result, Ok(DragOutcome::Copied)) {
                worker_controller.cancel();
            }
            broker.finish(&result);
            let _ = done.send(result);
        });
        if let Err(error) = worker {
            controller.cancel();
            self.remote_browser.error = Some(error.to_string());
            cx.notify();
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = receive.await.unwrap_or_else(|_| Err("File drag worker ended".into()));
            let _ = this.update_in(cx, |_, window, cx| {
                let language = workspace_ui_language();
                let (kind, message) = match result {
                    Ok(DragOutcome::Copied) => (
                        crate::display::ToastKind::Success,
                        language.text(Message::TransferDownloaded).to_owned(),
                    ),
                    Ok(DragOutcome::Cancelled) => (
                        crate::display::ToastKind::Info,
                        language.text(Message::TransferCancelled).to_owned(),
                    ),
                    Err(error) => (
                        crate::display::ToastKind::Warning,
                        format!("{}: {error}", language.text(Message::TransferFailed)),
                    ),
                };
                crate::gpui_shell::toast::toast(window, cx, kind, message);
            });
        })
        .detach();
        cx.notify();
    }
}
