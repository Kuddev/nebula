use std::path::PathBuf;

use rfd::FileDialog;

use super::{FileFilter, Window};

pub(super) fn pick_file(owner: &Window, title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
    dialog(owner, title, filters).pick_file()
}

pub(super) fn pick_file_unowned(title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
    let mut dialog = FileDialog::new().set_title(title);
    for filter in filters {
        dialog = dialog.add_filter(filter.name, filter.extensions);
    }
    dialog.pick_file()
}

pub(super) fn pick_files(owner: &Window, title: &str, filters: &[FileFilter]) -> Vec<PathBuf> {
    dialog(owner, title, filters).pick_files().unwrap_or_default()
}

pub(super) fn pick_folder(owner: &Window, title: &str) -> Option<PathBuf> {
    dialog(owner, title, &[]).pick_folder()
}

pub(super) fn save_file(
    owner: &Window,
    title: &str,
    filters: &[FileFilter],
    default_name: &str,
) -> Option<PathBuf> {
    dialog(owner, title, filters).set_file_name(default_name).save_file()
}

fn dialog(owner: &Window, title: &str, filters: &[FileFilter]) -> FileDialog {
    let mut dialog = FileDialog::new().set_title(title).set_parent(owner.native_window());
    for filter in filters {
        dialog = dialog.add_filter(filter.name, filter.extensions);
    }
    dialog
}
