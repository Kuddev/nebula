//! Application event protocol shared by the event loop, PTY proxies, and UI actions.

use std::path::PathBuf;
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
    /// 远程备份请求（设置→备份，口令确认后）。打包/加密/网络都在后台
    /// 线程；口令只在事件载荷里过一手，线程用完即弃。
    NebulaBackupRemote {
        upload: bool,
        passphrase: String,
        selection: crate::encrypted_backup::BackupSelection,
    },
    /// 后台远程备份线程完成（消息文本已含成功/失败语义，恢复成功的
    /// 提示里带「重启后应用」）。
    NebulaBackupRemoteDone {
        message: String,
        error: bool,
    },
    /// 设置→网络的本机代理握手扫描。请求在后台线程运行，结果回到目标窗口。
    LocalProxyScan,
    LocalProxyScanDone(Vec<crate::ssh_proxy::LocalProxyEndpoint>),
    /// 设置→网络的真实出网测试完成。`request_id` 用于丢弃设置变化前的旧结果。
    ProxyTestDone {
        request_id: u64,
        ok: bool,
        message: String,
        elapsed_ms: u64,
    },
    /// 设置→供应商的后台连通性测试结果。provider_id 与 request_id
    /// 共同防止切换供应商后旧请求覆盖当前状态。
    ProviderTestDone {
        request_id: u64,
        provider_id: String,
        ok: bool,
        message: String,
        elapsed_ms: u64,
    },
    NebulaTick,
    NebulaAttach,
    /// Authenticated runtime API request. The transport waits on the embedded
    /// one-shot response while all state mutation stays on this event thread.
    RuntimeControl(Arc<crate::runtime_api::RuntimeDispatch>),
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
    /// 在当前布局叶位置重建失败的 SSH pane，不经过关闭 tab 的路径。
    RetrySsh(String),
    OpenDoc(PathBuf),
    OpenSettings,
    Close,
    CloseIndex(usize),
    Duplicate(usize),
    /// Create a new local terminal and continue this tab's live AI session
    /// under a new independent session id.
    ForkAiSession(usize),
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
