//! Context-menu data shared by the legacy renderer and GPUI menus.

use super::color::Rgb;

/// Curated tab colors. `None` is rendered separately as the first, empty swatch.
pub const TAB_COLORS: [Rgb; 7] = [
    Rgb::new(224, 108, 117),
    Rgb::new(209, 154, 102),
    Rgb::new(229, 192, 123),
    Rgb::new(152, 195, 121),
    Rgb::new(86, 182, 194),
    Rgb::new(97, 175, 239),
    Rgb::new(198, 120, 221),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Tab(usize),
    Ssh(usize),
    Sftp(usize),
    SftpPanel,
    FileTree { row: usize, is_dir: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    ForkAiSession(usize),
    DuplicateTab(usize),
    ExportTab(usize),
    SplitTabRight(usize),
    SplitTabDown(usize),
    RenameTab(usize),
    CloseTab(usize),
    SetTabColor { index: usize, color: Option<Rgb> },
    ConnectSsh(usize),
    OpenSftp(usize),
    CopySshAddress(usize),
    EditSsh(usize),
    DeleteSsh(usize),
    DownloadSftp(usize),
    RenameSftp(usize),
    DeleteSftp(usize),
    RefreshSftp,
    UploadFilesSftp,
    UploadDirectorySftp,
    NewDirectorySftp,
    OpenFileTree(usize),
    RevealFileTree(usize),
    TerminalHereFileTree(usize),
    CopyFileTreePath(usize),
    DeleteFileTree(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuHit {
    Outside,
    Panel,
    Action(ContextMenuAction),
}
