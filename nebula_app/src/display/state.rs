//! Display-domain state models without rendering behavior.

use std::path::PathBuf;
use std::sync::Arc;

use super::terminal_math::TerminalMathState;

/// Which key accepts an inline suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcceptKey {
    Right,
    Tab,
    #[default]
    Both,
}

impl AcceptKey {
    pub(super) fn cycle(self) -> Self {
        match self {
            Self::Right => Self::Tab,
            Self::Tab => Self::Both,
            Self::Both => Self::Right,
        }
    }

    pub fn accepts_right(self) -> bool {
        matches!(self, Self::Right | Self::Both)
    }

    pub fn accepts_tab(self) -> bool {
        matches!(self, Self::Tab | Self::Both)
    }
}

/// Runtime-selected default executor for new terminal sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NebulaShell {
    #[default]
    PowerShell,
    Bash,
}

impl NebulaShell {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::Bash => "Bash",
        }
    }

    pub(super) fn settings_value(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Bash => "bash",
        }
    }

    pub(super) fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "powershell" | "pwsh" | "ps" => Some(Self::PowerShell),
            "bash" | "git-bash" | "gitbash" | "wsl" => Some(Self::Bash),
            _ => None,
        }
    }
}

/// A blocking window action awaiting user input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NebulaConfirm {
    InstallRequiredFont {
        directory: PathBuf,
    },
    ClosePane {
        pane_id: u64,
        process: String,
    },
    CloseTab {
        index: usize,
        process: String,
    },
    CloseWindow {
        process: String,
    },
    /// Binding paste data to its source pane prevents a window-global modal
    /// from routing a confirmed transaction into another split.
    Paste {
        pane_id: u64,
        text: String,
        bracketed: bool,
        lines: usize,
    },
    DeleteSsh {
        host: String,
        from_config: bool,
    },
    DeleteSftp {
        entry: crate::ssh_sftp::SftpEntry,
    },
}

impl NebulaConfirm {
    pub fn can_dismiss(&self) -> bool {
        true
    }

    pub fn paste_pane_id(&self) -> Option<u64> {
        match self {
            Self::Paste { pane_id, .. } => Some(*pane_id),
            _ => None,
        }
    }
}

/// One OSC 1337 image anchored to an absolute terminal-grid row.
#[derive(Debug, Clone)]
pub struct NebulaInlineImage {
    pub id: u64,
    pub abs_line: usize,
    pub width: f32,
    pub height: f32,
    pub rgba: Arc<Vec<u8>>,
    pub px_w: u32,
    pub px_h: u32,
}

/// Prompt metadata and overlays that must follow one concrete PTY/pane.
#[derive(Debug, Default, Clone)]
pub struct NebulaPaneState {
    pub cwd: String,
    pub branch: String,
    pub suggestion: String,
    pub(super) suggestion_key: String,
    pub line_buf: String,
    pub(crate) screen_line: String,
    pub touched: bool,
    pub inline_images: Vec<NebulaInlineImage>,
    pub command_started: Option<std::time::Instant>,
    pub running_program: Option<String>,
    pub last_committed: String,
    pub awaiting_input: bool,
    pub finished_unseen: bool,
    pub pending_ssh_host: Option<String>,
    /// 可重建的公式布局缓存跟随 Pane，避免分屏之间复用错误的位置或字体尺寸。
    pub(super) terminal_math: TerminalMathState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    LeftRight,
    TopBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitNav {
    Left,
    Right,
    Up,
    Down,
}
