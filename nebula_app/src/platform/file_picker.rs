//! Native path selection with one asynchronous cancellation contract.
//!
//! Windows keeps the existing owner-bound dialogs on their worker thread;
//! other hosts use GPUI's native picker. Transfer targets remain owned by the
//! caller, so selecting a path cannot retarget an in-flight upload/download.

use std::future::Future;
use std::path::PathBuf;

use futures::channel::oneshot;
use gpui::{App, Window};

#[derive(Clone, Copy)]
enum PickerKind {
    Files,
    Directory,
}

pub(crate) fn files(
    window: &Window,
    cx: &App,
    title: &'static str,
) -> impl Future<Output = Vec<PathBuf>> + 'static {
    prompt_paths(window, cx, PickerKind::Files, title)
}

pub(crate) fn directory(
    window: &Window,
    cx: &App,
    title: &'static str,
) -> impl Future<Output = Option<PathBuf>> + 'static {
    let picked = prompt_paths(window, cx, PickerKind::Directory, title);
    async move { picked.await.into_iter().next() }
}

async fn selected_paths<E>(
    picked: oneshot::Receiver<Result<Option<Vec<PathBuf>>, E>>,
) -> Vec<PathBuf> {
    // Preserve the transfer UI's existing no-op on cancel, picker failure,
    // or a picker worker disappearing before it can return a selection.
    picked.await.ok().and_then(Result::ok).flatten().unwrap_or_default()
}

fn prompt_paths(
    window: &Window,
    cx: &App,
    kind: PickerKind,
    title: &'static str,
) -> impl Future<Output = Vec<PathBuf>> + 'static {
    #[cfg(windows)]
    {
        let _ = cx;
        let owner = native_owner(window);
        let (tx, rx) = oneshot::channel();
        std::thread::spawn(move || {
            let paths = match kind {
                PickerKind::Files => {
                    Some(crate::display::file_dialog::pick_upload_files_with_hwnd(owner as _))
                },
                PickerKind::Directory => {
                    crate::display::file_dialog::pick_folder_with_hwnd(owner as _, title)
                        .map(|path| vec![path])
                },
            };
            let _ = tx.send(Ok(paths));
        });
        selected_paths::<String>(rx)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        selected_paths(cx.prompt_for_paths(prompt_options(kind, title)))
    }
}

#[cfg(any(not(windows), test))]
fn prompt_options(kind: PickerKind, title: &'static str) -> gpui::PathPromptOptions {
    let files = matches!(kind, PickerKind::Files);
    gpui::PathPromptOptions {
        files,
        directories: !files,
        multiple: files,
        prompt: Some(title.into()),
    }
}

#[cfg(windows)]
fn native_owner(window: &Window) -> usize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window)
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
            _ => None,
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;

    use super::*;

    #[test]
    fn files_and_directories_preserve_native_selection_modes() {
        let files = prompt_options(PickerKind::Files, "Choose files");
        assert!(files.files && files.multiple && !files.directories);
        assert_eq!(files.prompt.as_deref(), Some("Choose files"));
        let directory = prompt_options(PickerKind::Directory, "Choose folder");
        assert!(directory.directories && !directory.files && !directory.multiple);
        assert_eq!(directory.prompt.as_deref(), Some("Choose folder"));
    }

    #[test]
    fn selected_paths_preserve_order_unicode_and_spaces() {
        let paths = vec![PathBuf::from("project with spaces/文档.md"), PathBuf::from("other.txt")];
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(Some(paths.clone()))).unwrap();
        assert_eq!(block_on(selected_paths::<String>(rx)), paths);
    }

    #[test]
    fn cancelled_failed_and_dropped_pickers_never_start_a_transfer() {
        for result in [Ok(None), Ok(Some(Vec::new())), Err("picker failed".to_owned())] {
            let (tx, rx) = oneshot::channel();
            tx.send(result).unwrap();
            assert!(block_on(selected_paths(rx)).is_empty());
        }
        let (tx, rx) = oneshot::channel();
        drop(tx);
        assert!(block_on(selected_paths::<String>(rx)).is_empty());
    }
}
