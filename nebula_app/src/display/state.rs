//! Display-domain state models without rendering behavior.

use std::path::PathBuf;
use std::sync::Arc;

use nebula_terminal::index::{Point, Side};

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
    /// Wallpapers are normally confined to terminal content. Extending them
    /// under persistent controls is opt-in because low-contrast images can
    /// make caption buttons, tabs and SSH navigation harder to read.
    EnableBackgroundImageCoverChrome,
    /// 「拖拽调节侧栏」开启前的一次性告知：宽度拖动会实时重排终端，低配
    /// 机器或超大回滚缓冲下可能掉帧（用户裁定：开启必须先明确警告）。
    EnablePanelResize,
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
    /// 侧栏本地文件树的删除（回收站可撤销，仍需确认——树紧挨终端，误触
    /// 成本高）。路径在弹确认时已解析并快照，执行时不再回查行索引。
    DeleteFileTreePath {
        path: PathBuf,
        is_dir: bool,
    },
    /// Password entry for an encrypted backup export or restore. The
    /// passphrase itself deliberately lives only in `Display` and is never
    /// copied into the modal enum or persisted to settings.
    BackupPassphrase {
        restoring: bool,
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
    /// AI CLI 停下来等用户批准（claude 的 `Notification` hook）。和
    /// `finished_unseen` 分开：那个是"回合做完了，轮到你"，这个是"它卡在
    /// 半路上，不点头就不动"——后者才需要手掌徽章催人。
    pub needs_attention: bool,
    /// 上一条命令以非零码收尾且还没被看到。此前失败和成功共用一颗圆点，
    /// 标签上根本读不出"那条跑挂了"。
    pub failed_unseen: bool,
    /// 命令成功收尾的时刻，用来放那一下对勾闪现（见 `BADGE_FLASH`）。
    /// 闪完落回圆点——对勾说"刚成的"，圆点说"有结果没看"，是同一件事的
    /// 两个阶段。
    pub finished_at: Option<std::time::Instant>,
    pub pending_ssh_host: Option<String>,
    /// 助手错误恢复的建议条状态（spec 001）；`None` = 无条。
    pub ai_fix: Option<crate::ai_assistant::AiFixState>,
    /// 上次触发修复请求的时刻，实施 [`crate::ai_assistant::COOLDOWN`] 频控。
    pub ai_fix_cooldown: Option<std::time::Instant>,
    /// 可重建的公式布局缓存跟随 Pane，避免分屏之间复用错误的位置或字体尺寸。
    pub(super) terminal_math: TerminalMathState,
}

impl NebulaPaneState {
    pub(crate) fn terminal_math_source_point(
        &self,
        point: Point,
        side: Side,
        display_offset: usize,
    ) -> (Point, Side) {
        self.terminal_math.source_point(point, side, display_offset)
    }
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
