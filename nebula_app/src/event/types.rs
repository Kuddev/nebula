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
    NebulaTick,
    NebulaAttach,
    NebulaResizeSettled,
    SshDeleteUndoExpired,
    SftpUpdated,
    AiHook(crate::ai_hook::AiHookEvent),
    FocusWindow {
        pane: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabRequest {
    New,
    NewAtDirectory(PathBuf),
    NewProfile(usize),
    NewShell { name: String, shell: nebula_terminal::tty::Shell },
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
    Move { from: usize, to: usize },
    SplitToggle(crate::display::SplitDirection),
    SplitIndex { index: usize, direction: crate::display::SplitDirection },
    DockSplit { source: usize, nav: crate::display::SplitNav },
    FocusSplit(crate::display::SplitNav),
    ToggleZoom,
    BeginRename(usize),
    CommitRename(String),
    SetColor { index: usize, color: Option<crate::display::color::Rgb> },
    CancelRename,
}

impl From<TerminalEvent> for EventType {
    fn from(event: TerminalEvent) -> Self {
        Self::Terminal(event)
    }
}
