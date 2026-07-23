//! Window, tab, and pane state shared by the window lifecycle and split layout.

#[cfg(not(windows))]
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use nebula_terminal::event_loop::{Msg, Notifier};
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::Term;
use nebula_terminal::tty;

use crate::config::ui_config::Profile;
use crate::display::color::Rgb;
use crate::display::{NebulaPaneState, SplitDirection};
use crate::event::{EventProxy, InlineSearchState, SearchState};
use crate::session;

/// Identifier for a pane, stable for the pane's lifetime and reused as the
/// terminal's event tag.
pub(super) type PaneId = u64;

/// Sentinel pane id for document-viewer tabs. A document tab intentionally has
/// no matching entry in the pane pool, so existing pane lookups degrade cleanly.
pub(super) const DOC_PANE_ID: PaneId = u64::MAX;

/// A single terminal session (one PTY + grid).
pub struct Pane {
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    pub search_state: SearchState,
    pub inline_search_state: InlineSearchState,
    pub id: PaneId,
    pub title: String,
    /// 原生 SSH Pane 的稳定连接目标。文件面板必须依据会话身份路由到 SFTP，
    /// 不能从终端标题或用户刚输入的命令反推，否则分屏和全屏 TUI 都会误判。
    pub ssh_destination: Option<String>,
    pub nebula_state: NebulaPaneState,
    /// Columns the welcome intro was printed at while the pane is pristine.
    pub intro_cols: Option<usize>,
    /// Shell process id used by the close-confirmation process scan.
    pub shell_pid: u32,
    /// Every event-proxy clone observes this route, which lets a detached pane
    /// move between windows without restarting its PTY.
    pub(super) window_route: Arc<AtomicU64>,
    #[cfg(not(windows))]
    pub master_fd: RawFd,
}

/// A tab's pane layout: a binary tree with panes at the leaves.
pub(super) enum Layout {
    Leaf(PaneId),
    Split {
        direction: SplitDirection,
        ratio: f32,
        /// PTY dimensions follow the committed ratio until drag release.
        preview_ratio: Option<f32>,
        dragging: bool,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

/// One entry in the tab bar: a pane layout plus tab-specific presentation state.
pub(super) struct TabEntry {
    pub(super) layout: Layout,
    pub(super) active_pane: PaneId,
    pub(super) has_bell: bool,
    pub(super) custom_name: Option<String>,
    pub(super) custom_color: Option<Rgb>,
    pub(super) launch: TabLaunch,
    pub(super) doc: Option<crate::display::markdown_view::DocView>,
    pub(super) settings: bool,
}

#[derive(Clone)]
pub(super) enum TabLaunch {
    Default,
    Profile(Profile),
    Shell { name: String, shell: tty::Shell },
    Ssh(String),
    Document(PathBuf),
    Settings,
}

/// How a new window context gets its initial tabs.
pub enum WindowBoot {
    Fresh,
    Restore(session::Session),
    Attach(DetachedWindow),
}

/// Tabs parked in the resident process while their PTYs keep running.
pub struct DetachedWindow {
    pub(super) panes: Vec<Pane>,
    pub(super) tabs: Vec<TabEntry>,
    pub(super) active_tab: usize,
    pub(super) next_pane_id: PaneId,
}

impl DetachedWindow {
    /// Drop a pane whose shell exited while detached. Stale layout leaves are
    /// pruned during attach, where the complete tree is available.
    pub fn reap_pane(&mut self, pane_id: u64) {
        self.panes.retain(|pane| pane.id != pane_id);
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

impl Drop for DetachedWindow {
    fn drop(&mut self) {
        // A detached window that is never re-attached still owns live PTYs.
        for pane in &self.panes {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
    }
}
