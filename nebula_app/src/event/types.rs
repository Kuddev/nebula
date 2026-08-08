//! Application event protocol shared by the event loop, PTY proxies, and UI actions.

use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
use winit::event::Event as WinitEvent;
use winit::window::WindowId;

use nebula_terminal::event::Event as TerminalEvent;
use nebula_terminal::grid::Scroll;

#[cfg(unix)]
use crate::cli::IpcConfig;
use crate::cli::WindowOptions;
use crate::message_bar::Message;

#[derive(Debug, Clone)]
pub struct Event {
    pub(super) window_id: Option<WindowId>,
    pub(super) tab_id: Option<u64>,
    pub(super) payload: EventType,
}

impl Event {
    pub fn new<I: Into<Option<WindowId>>>(payload: EventType, window_id: I) -> Self {
        Self { window_id: window_id.into(), tab_id: None, payload }
    }

    pub(crate) fn terminal_tab_id(&self) -> Option<u64> {
        matches!(self.payload, EventType::Terminal(_)).then_some(self.tab_id).flatten()
    }

    pub(crate) fn terminal_bell_pane(&self) -> Option<u64> {
        matches!(self.payload, EventType::Terminal(TerminalEvent::Bell))
            .then_some(self.tab_id)
            .flatten()
    }
}

impl From<Event> for WinitEvent<Event> {
    fn from(event: Event) -> Self {
        WinitEvent::UserEvent(event)
    }
}

#[derive(Debug, Clone)]
pub enum EventType {
    Terminal(TerminalEvent),
    ConfigReload(PathBuf),
    ConfigReloadReady,
    /// A Settings import changed `terminal_profiles.json`; refresh all live
    /// window configs without waiting for a process restart.
    TerminalProfilesChanged,
    Message(Message),
    Scroll(Scroll),
    CreateWindow(WindowOptions),
    #[cfg(unix)]
    IpcConfig(IpcConfig),
    #[cfg(unix)]
    IpcGetConfig(Arc<UnixStream>),
    BlinkCursor,
    BlinkCursorTimeout,
    SearchNext,
    #[cfg(unix)]
    Shutdown,
    Frame,
    NebulaTab(TabRequest),
    /// WebDAV 同步请求（命令面板）。true = 推送，false = 拉取。
    NebulaSync {
        push: bool,
    },
    /// 后台同步线程完成（spec 003）。`history_changed` 提示各窗口热加载
    /// 命令历史；settings 变化走 mtime 监视，无需专门通知。
    NebulaSyncDone {
        message: String,
        error: bool,
        history_changed: bool,
    },
    NebulaTick,
    NebulaAttach,
    NebulaResizeSettled,
    SshDeleteUndoExpired,
    /// 设置页捕获到新的快速终端全局快捷键。
    QuickTerminalHotkeyChanged {
        hotkey: String,
    },
    /// SSH 编辑器「测试连接」完成（后台 runtime → 窗口线程）。`destination`
    /// 用于丢弃过期结果：草稿已改就当无事发生。
    SshTestDone {
        request_id: u64,
        destination: String,
        ok: bool,
        message: String,
        elapsed_ms: u64,
    },
    /// 直连 SSH 会话的连接阶段推进（后台 runtime → 窗口线程）。事件自带
    /// `tab_id`，接收侧据此定位 pane，无需在负载里重复 pane id。
    SshConnect(crate::ssh_session::SshStage),
    SftpUpdated,
    AiHook(crate::ai_hook::AiHookEvent),
    /// 助手修复请求完成（后台线程 → 主循环）。`fix: None` = 沉默（失败、
    /// 无 key、模型认为无解——三者同款处理，建议条直接消失）。
    AiFixReady {
        pane: u64,
        seq: u64,
        fix: Option<crate::ai_assistant::AiFix>,
    },
    FocusWindow {
        pane: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabRequest {
    New,
    NewAtDirectory(PathBuf),
    /// Launch the supplied profile snapshot. Carrying the value avoids a
    /// stale index when imported profiles are refreshed while a menu is open.
    NewProfile(crate::config::ui_config::Profile),
    NewShell {
        name: String,
        shell: nebula_terminal::tty::Shell,
    },
    NewSsh(String),
    OpenDoc(PathBuf),
    OpenSettings,
    Close,
    CloseIndex(usize),
    Duplicate(usize),
    CloseWindow,
    SelectNext,
    SelectPrev,
    Select(usize),
    SelectLast,
    Move {
        from: usize,
        to: usize,
    },
    SplitToggle(crate::display::SplitDirection),
    SplitIndex {
        index: usize,
        direction: crate::display::SplitDirection,
    },
    DockSplit {
        source: usize,
        nav: crate::display::SplitNav,
    },
    FocusSplit(crate::display::SplitNav),
    ToggleZoom,
    BeginRename(usize),
    CommitRename(String),
    SetColor {
        index: usize,
        color: Option<crate::display::color::Rgb>,
    },
    CancelRename,
    /// Save every terminal tab as a workspace file.
    ExportWorkspace,
    /// Save one tab (by index) as a workspace file.
    ExportTab(usize),
    /// Pick a workspace file and append its tabs to this window.
    ImportWorkspace,
}

impl From<TerminalEvent> for EventType {
    fn from(event: TerminalEvent) -> Self {
        Self::Terminal(event)
    }
}
