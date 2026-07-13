//! Terminal window context.

use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::mem;
#[cfg(not(windows))]
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use glutin::config::Config as GlutinConfig;
use glutin::display::GetGlDisplay;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use glutin::platform::x11::X11GlConfigExt;
use log::{error, info};
use serde_json as json;
use winit::event::{ElementState, Event as WinitEvent, Modifiers, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::WindowId;

use nebula_terminal::event::{Event as TerminalEvent, Notify};
use nebula_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::index::Direction;
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::test::TermSize;
use nebula_terminal::term::{Term, TermMode};
use nebula_terminal::tty;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::UiConfig;
use crate::display::window::Window;
use crate::display::{Display, NebulaPaneState};
use crate::event::{
    ActionContext, Event, EventProxy, EventType, InlineSearchState, Mouse, SearchState, TabRequest,
    TouchPurpose,
};
#[cfg(unix)]
use crate::logging::LOG_TARGET_IPC_CONFIG;
use crate::message_bar::MessageBuffer;
use crate::scheduler::{Scheduler, TimerId, Topic};
use crate::{input, renderer, session};

/// New-tab welcome page (Windows logo + fastfetch intro). Stateless helpers.
mod welcome;
mod nebula_fetch_art;
use welcome::nebula_fastfetch_intro_command_for;

/// Split-pane behaviour (toggle/resize/drag/focus); `impl WindowContext`.
mod split;

/// Identifier for a pane, stable for the pane's lifetime and reused as the
/// terminal's event tag. Panes live in [`WindowContext::panes`].
type PaneId = u64;

/// Sentinel pane id for document-viewer tabs. A doc tab owns no pane and no
/// PTY: its `Layout::Leaf(DOC_PANE_ID)` deliberately resolves to `None` in
/// every `pane()` lookup, which is exactly the degraded behaviour those call
/// sites already handle (no spinner, no close confirm, no PTY resize) — the
/// tab's content comes from `TabEntry::doc` instead.
const DOC_PANE_ID: PaneId = u64::MAX;

/// A single terminal session (one PTY + grid). It is a leaf of a tab's
/// [`Layout`] tree; the tab bar shows tabs, not panes.
pub struct Pane {
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    pub search_state: SearchState,
    pub inline_search_state: InlineSearchState,
    pub id: PaneId,
    pub title: String,
    pub nebula_state: NebulaPaneState,
    /// Columns the welcome intro was printed at, while the pane is pristine
    /// (no user input yet). Drives a re-print when a resize would reflow it;
    /// `None` once the user types or for panes without the intro.
    pub intro_cols: Option<usize>,
    /// Shell (PTY child) process id, for the close-confirmation process scan.
    pub shell_pid: u32,
    /// Which window this pane's PTY events route to (raw `WindowId` value),
    /// shared with every `EventProxy` clone in the Term + PTY I/O loop. A
    /// re-attach re-points all of them with one atomic store — the pane
    /// outlives its original window in detached (mux residency) mode.
    window_route: Arc<AtomicU64>,
    #[cfg(not(windows))]
    pub master_fd: RawFd,
}

/// A tab's pane layout: a binary tree with panes at the leaves and splits at
/// the internal nodes (a plain binary tree). A single pane is a bare
/// `Leaf`; splitting replaces a leaf with a `Split` of two sub-layouts.
enum Layout {
    Leaf(PaneId),
    Split {
        /// Orientation: panes side by side (left/right) or stacked (top/bottom).
        direction: crate::display::SplitDirection,
        /// Fraction of this node's extent assigned to `first` (left/top).
        ratio: f32,
        /// Divider preview while dragging this node. Pane PTYs keep `ratio`
        /// until release, otherwise full-screen TUIs repaint into a viewport
        /// that was never actually resized.
        preview_ratio: Option<f32>,
        /// Whether this node's divider is currently being dragged.
        dragging: bool,
        /// Left/top child.
        first: Box<Layout>,
        /// Right/bottom child.
        second: Box<Layout>,
    },
}

/// One entry in the tab bar: a pane layout plus which pane owns focus within it.
struct TabEntry {
    layout: Layout,
    active_pane: PaneId,
    /// A background tab rang its bell; shown as 🔔 in the tab bar until focused.
    has_bell: bool,
    /// Custom user-assigned name for this tab. When `None`, the label is derived
    /// from the working directory (Windows Terminal style).
    custom_name: Option<String>,
    /// Document viewer content (markdown/text/json). `Some` marks this as a
    /// doc tab: it has no pane, and the draw/input paths route to the viewer.
    doc: Option<crate::display::markdown_view::DocView>,
}

/// How a new window context gets its initial tabs.
pub enum WindowBoot {
    /// Spawn the default shell as a single fresh tab.
    Fresh,
    /// Cold restore from the session file: first tab's shell starts at the
    /// saved cwd, the remaining tabs are respawned after construction.
    Restore(session::Session),
    /// Adopt live detached panes — multiplexer-style re-attach. Their PTYs never
    /// stopped, so the window comes back mid-conversation.
    Attach(DetachedWindow),
}

/// Tabs stripped off a closed window, parked in the resident process with
/// their PTYs still running, waiting for a re-attach.
pub struct DetachedWindow {
    panes: Vec<Pane>,
    tabs: Vec<TabEntry>,
    active_tab: usize,
    next_pane_id: PaneId,
}

impl DetachedWindow {
    /// Drop a pane whose shell exited while detached. Its leaf stays in the
    /// layout; `finish_attach` prunes stale leaves with the full tree surgery.
    pub fn reap_pane(&mut self, pane_id: u64) {
        self.panes.retain(|pane| pane.id != pane_id);
    }

    /// No live panes left — nothing to re-attach.
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

impl Drop for DetachedWindow {
    fn drop(&mut self) {
        // Panes that never get re-attached (process quit, failed attach) must
        // not leak their PTYs. Re-attach empties `panes` first, so this is a
        // no-op on the happy path.
        for pane in &self.panes {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
    }
}

/// Event context for one individual Nebula window.
pub struct WindowContext {
    pub message_buffer: MessageBuffer,
    pub display: Display,
    pub dirty: bool,
    event_queue: Vec<WinitEvent<Event>>,
    /// Pool of all live panes in this window, indexed by lookup on `Pane::id`.
    panes: Vec<Pane>,
    /// Tab bar entries; `active_tab` indexes the visible one. Each tab owns a
    /// pane layout tree whose leaves reference panes in `panes`.
    tabs: Vec<TabEntry>,
    active_tab: usize,
    next_pane_id: PaneId,
    /// When set, this pane of the active tab is zoomed to fill the window
    /// (other panes hidden). Cleared by any layout/focus change.
    zoom: Option<PaneId>,
    /// Live divider-drag state: which split node (by tree path) is being
    /// resized, its orientation and content rect. `None` when not dragging.
    split_drag: Option<split::SplitDragState>,
    proxy: EventLoopProxy<Event>,
    cursor_blink_timed_out: bool,
    prev_bell_cmd: Option<Instant>,
    /// When the PTYs last learned their size. Drives the leading-edge check of
    /// the resize debounce: a lone resize (startup, maximize, sidebar toggle)
    /// passes through instantly; only a rapid follow-up — i.e. an interactive
    /// drag — defers to the settle timer.
    last_pty_resize: Option<Instant>,
    /// Current chrome clock cadence (1 Hz idle, 8 fps while a sidebar
    /// spinner animates); re-armed in `draw` when it changes.
    clock_interval: Duration,
    /// Last session snapshot written to disk, so the 1 Hz autosave can skip
    /// the write when nothing changed. `None` forces the next tick to write.
    last_saved_session: Option<session::Session>,
    /// Excluded from session persistence (the quick/Quake terminal is scratch
    /// space; its tabs must never overwrite the main window's session).
    pub session_exempt: bool,
    modifiers: Modifiers,
    mouse: Mouse,
    touch: TouchPurpose,
    occluded: bool,
    preserve_title: bool,
    window_config: ParsedOptions,
    config: Rc<UiConfig>,
    /// Stand-in pane for document-viewer tabs (see [`Self::create_doc_pane`]).
    /// Lives outside `panes` so id lookups keep treating doc tabs as
    /// pane-less; only the event pipeline borrows it.
    doc_pane: Pane,
}

impl WindowContext {
    /// Create initial window context that does bootstrapping the graphics API we're going to use.
    pub fn initial(
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Event>,
        config: Rc<UiConfig>,
        mut options: WindowOptions,
        boot: WindowBoot,
    ) -> Result<Self, Box<dyn Error>> {
        let raw_display_handle = event_loop.display_handle().unwrap().as_raw();

        let mut identity = config.window.identity.clone();
        options.window_identity.override_identity_config(&mut identity);

        // Windows has different order of GL platform initialization compared to any other platform;
        // it requires the window first.
        #[cfg(windows)]
        let window = Window::new(event_loop, &config, &identity, &mut options)?;
        #[cfg(windows)]
        crate::boot_trace("os window created");
        #[cfg(windows)]
        let raw_window_handle = Some(window.raw_window_handle());

        #[cfg(not(windows))]
        let raw_window_handle = None;

        let gl_display = renderer::platform::create_gl_display(
            raw_display_handle,
            raw_window_handle,
            config.debug.prefer_egl,
        )?;
        crate::boot_trace("gl display created (WGL ext probe)");
        let gl_config = renderer::platform::pick_gl_config(&gl_display, raw_window_handle)?;
        crate::boot_trace("gl display+config picked");

        #[cfg(not(windows))]
        let window = Window::new(
            event_loop,
            &config,
            &identity,
            &mut options,
            #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
            gl_config.x11_visual(),
        )?;

        // Create context.
        let gl_context =
            renderer::platform::create_gl_context(&gl_display, &gl_config, raw_window_handle)?;
        crate::boot_trace("gl context created");

        let display = Display::new(window, gl_context, &config, false)?;
        crate::boot_trace("display ready (fonts rasterized)");

        Self::new(display, config, options, proxy, boot)
    }

    /// Create additional context with the graphics platform other windows are using.
    pub fn additional(
        gl_config: &GlutinConfig,
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Event>,
        config: Rc<UiConfig>,
        mut options: WindowOptions,
        config_overrides: ParsedOptions,
        boot: WindowBoot,
    ) -> Result<Self, Box<dyn Error>> {
        let gl_display = gl_config.display();

        let mut identity = config.window.identity.clone();
        options.window_identity.override_identity_config(&mut identity);

        // Check if new window will be opened as a tab.
        // This must be done before `Window::new()`, which unsets `window_tabbing_id`.
        #[cfg(target_os = "macos")]
        let tabbed = options.window_tabbing_id.is_some();
        #[cfg(not(target_os = "macos"))]
        let tabbed = false;

        let window = Window::new(
            event_loop,
            &config,
            &identity,
            &mut options,
            #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
            gl_config.x11_visual(),
        )?;

        // Create context.
        let raw_window_handle = window.raw_window_handle();
        let gl_context =
            renderer::platform::create_gl_context(&gl_display, gl_config, Some(raw_window_handle))?;

        let display = Display::new(window, gl_context, &config, tabbed)?;

        let mut window_context = Self::new(display, config, options, proxy, boot)?;

        // Set the config overrides at startup.
        //
        // These are already applied to `config`, so no update is necessary.
        window_context.window_config = config_overrides;

        Ok(window_context)
    }

    /// Create a new terminal window context.
    fn new(
        display: Display,
        config: Rc<UiConfig>,
        options: WindowOptions,
        proxy: EventLoopProxy<Event>,
        boot: WindowBoot,
    ) -> Result<Self, Box<dyn Error>> {
        let preserve_title = options.window_identity.title.is_some();

        info!(
            "PTY dimensions: {:?} x {:?}",
            display.size_info.screen_lines(),
            display.size_info.columns()
        );

        let window_id = display.window.id();

        // Bootstrap the tab set: fresh/restored windows spawn their first
        // pane here; an attach adopts the detached panes wholesale.
        let mut restore = None;
        let (panes, tabs, active_tab, next_pane_id, fresh_first) = match boot {
            WindowBoot::Attach(mut detached) => {
                // Re-point every pane's PTY events at this window before any
                // of them fires again; the leftover DetachedWindow drops with
                // empty panes, so its PTY-shutdown Drop is a no-op.
                for pane in &detached.panes {
                    pane.window_route.store(window_id.into(), Ordering::Relaxed);
                }
                (
                    mem::take(&mut detached.panes),
                    mem::take(&mut detached.tabs),
                    detached.active_tab,
                    detached.next_pane_id,
                    None,
                )
            },
            other => {
                if let WindowBoot::Restore(session) = other {
                    restore = Some(session);
                }
                let mut pty_config = config.pty_config();
                options.terminal_options.override_pty_config(&mut pty_config);
                // Restored session: aim the first pane's shell at the saved
                // cwd; the remaining tabs are respawned once the context
                // exists. The CLI gate lives in `create_initial_window`; the
                // `is_none` check additionally keeps an explicit
                // --working-directory winning no matter what.
                if let Some(session) = &restore {
                    if pty_config.working_directory.is_none() {
                        if let Some(dir) =
                            session.tabs.first().and_then(|t| session::valid_dir(&t.cwd))
                        {
                            pty_config.working_directory = Some(dir);
                        }
                    }
                }
                let first_pane = Self::create_pane(
                    &display.size_info,
                    window_id,
                    &config,
                    pty_config,
                    &proxy,
                    0,
                )?;
                let first_id = first_pane.id;
                (
                    vec![first_pane],
                    vec![TabEntry {
                        layout: Layout::Leaf(first_id),
                        active_pane: first_id,
                        has_bell: false,
                        custom_name: None,
                        doc: None,
                    }],
                    0,
                    1,
                    Some(first_id),
                )
            },
        };
        let attached = fresh_first.is_none();

        // The pane stub every doc tab's events run against (never in `panes`).
        let doc_pane =
            Self::create_doc_pane(&display.size_info, display.window.id(), &config, &proxy);

        // Create context for the Nebula window.
        let context = WindowContext {
            preserve_title,
            panes,
            tabs,
            active_tab,
            next_pane_id,
            zoom: None,
            split_drag: None,
            proxy,
            display,
            config,
            doc_pane,
            cursor_blink_timed_out: Default::default(),
            prev_bell_cmd: Default::default(),
            last_pty_resize: None,
            clock_interval: Duration::from_secs(1),
            last_saved_session: None,
            session_exempt: false,
            message_buffer: Default::default(),
            window_config: Default::default(),
            event_queue: Default::default(),
            modifiers: Default::default(),
            occluded: Default::default(),
            mouse: Default::default(),
            touch: Default::default(),
            dirty: Default::default(),
        };
        let mut context = context;
        if let Some(first_id) = fresh_first {
            context.run_fastfetch_intro(first_id);
        }
        if let Some(session) = restore {
            context.restore_session_tabs(&session);
        }
        if attached {
            context.finish_attach();
        }
        Ok(context)
    }

    /// Spawn a new terminal session (PTY + grid + I/O loop) as a pane.
    fn create_pane(
        size_info: &crate::display::SizeInfo,
        window_id: WindowId,
        config: &UiConfig,
        mut pty_config: tty::Options,
        proxy: &EventLoopProxy<Event>,
        pane_id: PaneId,
    ) -> Result<Pane, Box<dyn Error>> {
        // Per-pane identity for AI-CLI lifecycle hooks: nebula-hook.exe reads
        // it and stamps its pipe messages, so turn state lands on the right
        // tab dot (see `ai_hook`).
        pty_config.env.insert(crate::ai_hook::PANE_ENV.into(), pane_id.to_string());

        let window_route = Arc::new(AtomicU64::new(window_id.into()));
        let event_proxy = EventProxy::new_tab(proxy.clone(), window_route.clone(), pane_id);

        // The terminal holds all display state, wrapped in a clonable mutex shared
        // with the PTY I/O loop.
        let terminal = Term::new(config.term_options(), size_info, event_proxy.clone());
        let terminal = Arc::new(FairMutex::new(terminal));

        // A working directory that no longer exists — deleted, on an unmounted
        // drive, or a PowerShell non-filesystem PSDrive (Cert:\, HKLM:\, Env:\)
        // reported over OSC — makes CreateProcessW fail with ERROR_DIRECTORY
        // (os error 267) and aborts the whole spawn. Fall back to the process
        // default cwd instead of failing the pane.
        if let Some(dir) = pty_config.working_directory.as_ref() {
            if !dir.is_dir() {
                log::warn!("Ignoring invalid working directory {dir:?}; using default");
                pty_config.working_directory = None;
            }
        }

        let initial_cwd = pty_config
            .working_directory
            .as_ref()
            .cloned()
            .or_else(|| std::env::current_dir().ok())
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        // The PTY forks the shell process and retains the master side.
        crate::boot_trace("conpty spawn begin");
        let pty = tty::new(&pty_config, (*size_info).into(), window_id.into())?;
        crate::boot_trace("conpty spawn done");

        #[cfg(not(windows))]
        let master_fd = pty.file().as_raw_fd();
        #[cfg(not(windows))]
        let shell_pid = pty.child().id();
        #[cfg(windows)]
        let shell_pid = pty.child_watcher().pid().map(|p| p.get()).unwrap_or(0);

        // PTY I/O runs on its own thread and updates the shared terminal state.
        let event_loop = PtyEventLoop::new(
            Arc::clone(&terminal),
            event_proxy.clone(),
            pty,
            pty_config.drain_on_exit,
            config.debug.ref_test,
        )?;

        let loop_tx = event_loop.channel();
        let _io_thread = event_loop.spawn();

        // Start cursor blinking, in case `Focused` isn't sent on startup.
        if config.cursor.style().blinking {
            event_proxy.send_event(TerminalEvent::CursorBlinkingChange.into());
        }

        let mut nebula_state = NebulaPaneState::default();
        nebula_state.cwd = initial_cwd;

        Ok(Pane {
            terminal,
            notifier: Notifier(loop_tx),
            search_state: Default::default(),
            inline_search_state: Default::default(),
            id: pane_id,
            title: String::from("shell"),
            nebula_state,
            intro_cols: None,
            shell_pid,
            window_route,
            #[cfg(not(windows))]
            master_fd,
        })
    }

    #[cfg(windows)]
    /// 创建由远端 PTY 通道驱动的 Pane，并复用本地终端的解析、渲染和事件协议。
    /// 这样传输层只负责字节流，输入、缩放与终端状态无需维护两套实现。
    fn create_ssh_pane(
        size_info: &crate::display::SizeInfo,
        window_id: WindowId,
        config: &UiConfig,
        proxy: &EventLoopProxy<Event>,
        pane_id: PaneId,
        destination: String,
    ) -> Result<Pane, Box<dyn Error>> {
        let window_route = Arc::new(AtomicU64::new(window_id.into()));
        let event_proxy = EventProxy::new_tab(proxy.clone(), window_route.clone(), pane_id);
        let terminal = Arc::new(FairMutex::new(Term::new(
            config.term_options(),
            size_info,
            event_proxy.clone(),
        )));
        let sender = crate::ssh_session::spawn_session(
            destination,
            (*size_info).into(),
            terminal.clone(),
            event_proxy.clone(),
        )?;
        if config.cursor.style().blinking {
            event_proxy.send_event(TerminalEvent::CursorBlinkingChange.into());
        }
        Ok(Pane {
            terminal,
            notifier: Notifier(sender),
            search_state: Default::default(),
            inline_search_state: Default::default(),
            id: pane_id,
            title: String::from("ssh"),
            nebula_state: NebulaPaneState::default(),
            intro_cols: None,
            shell_pid: 0,
            window_route,
            #[cfg(not(windows))]
            master_fd: -1,
        })
    }

    /// A pane-shaped stub for document-viewer tabs: a real (empty) `Term` so
    /// the shared event pipeline has state to borrow, but NO PTY behind it —
    /// the notifier is a sink, so keystrokes routed here are swallowed
    /// instead of reaching some other tab's shell. Never inserted into
    /// `panes`: every `pane(DOC_PANE_ID)` lookup stays `None`, keeping all
    /// the "no pane" degradations (no spinner, no close confirm, …) intact.
    fn create_doc_pane(
        size_info: &crate::display::SizeInfo,
        window_id: WindowId,
        config: &UiConfig,
        proxy: &EventLoopProxy<Event>,
    ) -> Pane {
        let window_route = Arc::new(AtomicU64::new(window_id.into()));
        let event_proxy = EventProxy::new_tab(proxy.clone(), window_route.clone(), DOC_PANE_ID);
        let terminal = Term::new(config.term_options(), size_info, event_proxy);
        Pane {
            terminal: Arc::new(FairMutex::new(terminal)),
            notifier: Notifier(nebula_terminal::event_loop::EventLoopSender::sink()),
            search_state: Default::default(),
            inline_search_state: Default::default(),
            id: DOC_PANE_ID,
            title: String::from("doc"),
            nebula_state: NebulaPaneState::default(),
            intro_cols: None,
            shell_pid: 0,
            window_route,
            #[cfg(not(windows))]
            master_fd: -1,
        }
    }

    /// Handle a Nebula tab request. Returns `true` if the window should close
    /// (i.e. the last tab was closed).
    pub fn handle_tab_request(&mut self, request: TabRequest) -> bool {
        use crate::display::NebulaConfirm;
        match request {
            TabRequest::New => {
                self.spawn_tab();
                false
            },
            TabRequest::NewProfile(index) => {
                self.spawn_tab_profile(index);
                false
            },
            TabRequest::NewShell { name, shell } => {
                self.spawn_tab_shell(name, shell);
                false
            },
            TabRequest::NewSsh(host) => {
                self.spawn_tab_ssh(host);
                false
            },
            TabRequest::OpenDoc(path) => {
                self.open_doc_tab(path);
                false
            },
            TabRequest::Close => {
                let id = self.focused_pane_id();
                // A pending confirm for this pane means the user re-triggered
                // the close (or pressed Enter, which re-dispatches this
                // request): proceed for real.
                let confirmed = matches!(
                    self.display.nebula_confirm,
                    Some(NebulaConfirm::ClosePane { pane_id, .. }) if pane_id == id
                );
                if confirmed {
                    self.display.nebula_confirm = None;
                } else if let Some(process) = self.busy_process_in(&[id]) {
                    self.display.nebula_confirm =
                        Some(NebulaConfirm::ClosePane { pane_id: id, process });
                    self.dirty = true;
                    return false;
                }
                self.close_focused_pane()
            },
            TabRequest::CloseIndex(index) => {
                let confirmed = matches!(
                    self.display.nebula_confirm,
                    Some(NebulaConfirm::CloseTab { index: i, .. }) if i == index
                );
                if confirmed {
                    self.display.nebula_confirm = None;
                } else {
                    let mut ids = Vec::new();
                    if let Some(tab) = self.tabs.get(index) {
                        tab.layout.leaves(&mut ids);
                    }
                    if let Some(process) = self.busy_process_in(&ids) {
                        self.display.nebula_confirm =
                            Some(NebulaConfirm::CloseTab { index, process });
                        self.dirty = true;
                        return false;
                    }
                }
                self.close_tab(index)
            },
            TabRequest::CloseWindow => {
                // A normal window close DETACHES: the PTYs live on in the
                // resident process, so a running claude/build is not lost
                // and needs no confirmation. When the close actually KILLS
                // the shells — the quick terminal (session_exempt), or the
                // user turned residency off in 设置→高级 — a busy process
                // (claude, a build…) gets the confirm dialog first.
                if self.session_exempt || !self.display.nebula_keep_session {
                    let confirmed = matches!(
                        self.display.nebula_confirm,
                        Some(NebulaConfirm::CloseWindow { .. })
                    );
                    if confirmed {
                        self.display.nebula_confirm = None;
                    } else {
                        let ids: Vec<_> = self.panes.iter().map(|p| p.id).collect();
                        if let Some(process) = self.busy_process_in(&ids) {
                            self.display.nebula_confirm =
                                Some(NebulaConfirm::CloseWindow { process });
                            self.dirty = true;
                            return false;
                        }
                    }
                }
                self.display.window.hold = false;
                true
            },
            TabRequest::SelectNext => {
                if !self.tabs.is_empty() {
                    self.select_tab((self.active_tab + 1) % self.tabs.len());
                }
                false
            },
            TabRequest::SelectPrev => {
                if !self.tabs.is_empty() {
                    let n = self.tabs.len();
                    self.select_tab((self.active_tab + n - 1) % n);
                }
                false
            },
            TabRequest::Select(index) => {
                self.select_tab(index);
                false
            },
            TabRequest::SelectLast => {
                if !self.tabs.is_empty() {
                    self.select_tab(self.tabs.len() - 1);
                }
                false
            },
            TabRequest::Move { from, to } => {
                self.move_tab(from, to);
                false
            },
            TabRequest::SplitToggle(direction) => {
                self.split_focused(direction);
                false
            },
            TabRequest::DockSplit { source, nav } => {
                self.dock_tab_into_active(source, nav);
                false
            },
            TabRequest::FocusSplit(nav) => {
                self.focus_split(nav);
                false
            },
            TabRequest::ToggleZoom => {
                self.toggle_zoom();
                false
            },
            TabRequest::BeginRename(index) => {
                if index < self.tabs.len() {
                    // Start editing: grab the current label (either custom name or cwd-derived)
                    let current_label = if let Some(custom) = &self.tabs[index].custom_name {
                        custom.clone()
                    } else {
                        self.pane(self.tabs[index].active_pane)
                            .map(Self::chrome_tab_label)
                            .unwrap_or_else(|| "Tab".to_owned())
                    };
                    self.display.nebula_tab_rename_caret = current_label.chars().count();
                    self.display.nebula_tab_rename = Some((index, current_label));
                    self.display.nebula_tab_rename_select_all = true;
                    self.dirty = true;
                }
                false
            },
            TabRequest::CommitRename(new_name) => {
                self.display.nebula_tab_rename_select_all = false;
                if let Some((index, _)) = self.display.nebula_tab_rename.take() {
                    if index < self.tabs.len() {
                        let trimmed = new_name.trim().to_owned();
                        self.tabs[index].custom_name = if trimmed.is_empty() {
                            None // Empty name reverts to auto-label
                        } else {
                            Some(trimmed)
                        };
                        self.sync_chrome_tabs();
                        self.dirty = true;
                    }
                }
                false
            },
            TabRequest::CancelRename => {
                self.display.nebula_tab_rename_select_all = false;
                if self.display.nebula_tab_rename.take().is_some() {
                    self.dirty = true;
                }
                false
            },
        }
    }

    /// Number of tabs in this window.
    #[inline]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Index of the active tab.
    #[inline]
    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    /// Spawn and activate a new tab (a single-pane layout) using the default shell.
    fn spawn_tab(&mut self) {
        let cwd = self.focused_cwd();
        // The default-shell setting (`shell=<id>` in nebula_settings.txt) may
        // name a detected shell the PTY layer doesn't bootstrap itself (cmd,
        // pwsh, nushell, a WSL distro). `resolve_id` returns `None` for the two
        // PTY-integrated executors (powershell/bash) so those keep their prompt
        // injection; anything else spawns as an explicit override here.
        let override_shell = Self::default_shell_override();
        let spawned = match override_shell {
            Some(shell) => self.spawn_pane_detached_with(cwd, self.display.size_info, Some(shell)),
            None => self.spawn_pane_detached(cwd, self.display.size_info),
        };
        if let Some(id) = spawned {
            // Insert right after the current tab (insert right next to the current tab)
            // rather than at the end of the bar.
            let at = (self.active_tab + 1).min(self.tabs.len());
            self.tabs.insert(at, TabEntry {
                layout: Layout::Leaf(id),
                active_pane: id,
                has_bell: false,
                custom_name: None,
                doc: None,
            });
            self.active_tab = at;
            self.resize_active_layout();
            self.dirty = true;
            self.run_fastfetch_intro(id);
        }
    }

    /// The default-shell override for a plain new tab, or `None` to use the PTY
    /// layer's own default (which owns the powershell/bash prompt bootstrap).
    fn default_shell_override() -> Option<nebula_terminal::tty::Shell> {
        let id = crate::display::nebula_settings_value("shell")
            .or_else(|| crate::display::nebula_settings_value("executor"))?;
        crate::shell_detect::resolve_id(&id).map(|shell| shell.shell())
    }

    /// Open a new tab running the quick-launch profile at `index` (custom
    /// command instead of the default shell). The tab is pre-named after the
    /// profile so an `ssh host` entry reads as its destination, not "ssh".
    fn spawn_tab_profile(&mut self, index: usize) {
        let Some(profile) = self.config.profiles.get(index).cloned() else { return };
        // Profile cwd wins when it exists; else inherit the focused pane's.
        let cwd = profile
            .cwd
            .as_ref()
            .filter(|p| p.is_dir())
            .cloned()
            .or_else(|| self.focused_cwd());
        let shell = profile.shell();
        if let Some(id) = self.spawn_pane_detached_with(cwd, self.display.size_info, Some(shell)) {
            let at = (self.active_tab + 1).min(self.tabs.len());
            self.tabs.insert(at, TabEntry {
                layout: Layout::Leaf(id),
                active_pane: id,
                has_bell: false,
                custom_name: Some(profile.name),
                doc: None,
            });
            self.active_tab = at;
            self.resize_active_layout();
            self.dirty = true;
        }
    }

    /// Open a new tab running a detected shell (the new-tab dropdown). Like
    /// `spawn_tab_profile` but the spec is passed in rather than looked up in
    /// the config, and the cwd inherits the focused pane's.
    fn spawn_tab_shell(&mut self, name: String, shell: nebula_terminal::tty::Shell) {
        if let Some(id) =
            self.spawn_pane_detached_with(self.focused_cwd(), self.display.size_info, Some(shell))
        {
            let at = (self.active_tab + 1).min(self.tabs.len());
            self.tabs.insert(at, TabEntry {
                layout: Layout::Leaf(id),
                active_pane: id,
                has_bell: false,
                custom_name: Some(name),
                doc: None,
            });
            self.active_tab = at;
            self.resize_active_layout();
            self.dirty = true;
        }
    }

    /// Open a saved SSH destination inside the configured default shell.
    /// `nebula ssh` is typed into that shell's PTY so OpenSSH remains inside
    /// Nebula's ConPTY instead of becoming the pane's GUI-subsystem root.
    fn spawn_tab_ssh(&mut self, host: String) {
        #[cfg(windows)]
        {
            let pane_id = self.next_pane_id;
            match Self::create_ssh_pane(
                &self.display.size_info,
                self.display.window.id(),
                &self.config,
                &self.proxy,
                pane_id,
                host.clone(),
            ) {
                Ok(pane) => {
                    self.next_pane_id += 1;
                    self.panes.push(pane);
                    let at = (self.active_tab + 1).min(self.tabs.len());
                    self.tabs.insert(at, TabEntry {
                        layout: Layout::Leaf(pane_id),
                        active_pane: pane_id,
                        has_bell: false,
                        custom_name: Some(host),
                        doc: None,
                    });
                    self.active_tab = at;
                    self.resize_active_layout();
                    self.dirty = true;
                    return;
                },
                Err(err) => {
                    error!("创建直连 SSH Pane 失败: {err}");
                    return;
                },
            }
        }

        #[cfg(not(windows))]
        {
        let Ok(exe) = std::env::current_exe() else {
            error!("Cannot locate nebula.exe for the SSH AskPass helper");
            return;
        };
        let shell_id = self.display.nebula_shell_id.clone().unwrap_or_else(|| {
            match self.display.nebula_shell {
                crate::display::NebulaShell::PowerShell => "powershell".into(),
                crate::display::NebulaShell::Bash => "bash".into(),
            }
        });
        let launch = match crate::ssh::build_pane_launch(&shell_id, &exe, &host) {
            Ok(launch) => launch,
            Err(err) => {
                error!("Refusing unsafe SSH destination {host:?}: {err}");
                return;
            },
        };
        let default_shell = Self::default_shell_override();
        if let Some(id) = self.spawn_pane_detached_with(
            self.focused_cwd(),
            self.display.size_info,
            default_shell,
        ) {
            let at = (self.active_tab + 1).min(self.tabs.len());
            self.tabs.insert(at, TabEntry {
                layout: Layout::Leaf(id),
                active_pane: id,
                has_bell: false,
                custom_name: Some(host),
                doc: None,
            });
            self.active_tab = at;
            self.resize_active_layout();
            self.dirty = true;
            if let Some(pane) = self.panes.iter().find(|pane| pane.id == id) {
                pane.notifier.notify(launch.command);
            }
        }
        }
    }

    /// Open `path` in a read-only document viewer tab. A tab already viewing
    /// this file is re-focused (and re-read, so the view is fresh) instead of
    /// duplicated — double-click twice shouldn't litter the bar.
    fn open_doc_tab(&mut self, path: std::path::PathBuf) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.doc.as_ref().is_some_and(|doc| doc.path == path))
        {
            if let Some(doc) = self.tabs[index].doc.as_mut() {
                doc.reload();
            }
            self.select_tab(index);
            self.dirty = true;
            return;
        }
        let doc = crate::display::markdown_view::DocView::open(path);
        // Nerd Font markdown mark in front of the file name; the label IS the
        // tab identity for doc tabs (no cwd to derive one from). Same codicon
        // as the file tree's markdown icon, so tab and tree read as one system.
        let label = format!("\u{eb1d} {}", doc.title);
        let at = (self.active_tab + 1).min(self.tabs.len());
        self.tabs.insert(at, TabEntry {
            layout: Layout::Leaf(DOC_PANE_ID),
            active_pane: DOC_PANE_ID,
            has_bell: false,
            custom_name: Some(label),
            doc: Some(doc),
        });
        self.active_tab = at;
        self.dirty = true;
    }

    /// Rebuild the remaining tabs of a restored session (its first tab became
    /// the initial pane) and refocus the tab that was active at close.
    fn restore_session_tabs(&mut self, session: &session::Session) {
        for tab in session.tabs.iter().skip(1) {
            // A vanished directory falls back to the default cwd, keeping the
            // tab count (and thus the saved active index) intact.
            let cwd = session::valid_dir(&tab.cwd);
            if let Some(id) = self.spawn_pane_detached(cwd, self.display.size_info) {
                self.tabs.push(TabEntry {
                    layout: Layout::Leaf(id),
                    active_pane: id,
                    has_bell: false,
                    custom_name: None,
                    doc: None,
                });
                self.run_fastfetch_intro(id);
            }
        }
        // Guarded: a failed spawn above leaves fewer tabs than were saved.
        if session.active_tab < self.tabs.len() {
            self.active_tab = session.active_tab;
        }
        self.dirty = true;
    }

    /// Whether any pane (and its PTY) is still alive in this window.
    pub fn has_live_panes(&self) -> bool {
        !self.panes.is_empty()
    }

    /// Strip the live tabs off this window for mux residency (detach): the
    /// panes' PTYs keep running in-process, ready for re-attach. The final
    /// session snapshot is written here and the Drop one is suppressed —
    /// after the take below, Drop would see zero tabs and wipe the file.
    pub fn detach_panes(&mut self) -> DetachedWindow {
        session::save(&self.session_snapshot());
        self.session_exempt = true;
        DetachedWindow {
            panes: mem::take(&mut self.panes),
            tabs: mem::take(&mut self.tabs),
            active_tab: self.active_tab,
            next_pane_id: self.next_pane_id,
        }
    }

    /// Post-adoption fixups for a re-attached window.
    fn finish_attach(&mut self) {
        // Prune stale leaves: a shell that exited during residency had its
        // pane reaped but its leaf kept. `close_pane` does the full tree
        // surgery (collapse split / drop empty tab / move focus); with the
        // pane already gone it touches no PTY.
        let live: std::collections::HashSet<PaneId> =
            self.panes.iter().map(|pane| pane.id).collect();
        let mut stale = Vec::new();
        for tab in &self.tabs {
            let mut ids = Vec::new();
            tab.layout.leaves(&mut ids);
            stale.extend(ids.into_iter().filter(|id| !live.contains(id)));
        }
        for id in stale {
            self.close_pane(id);
        }

        // Focus sanity: a tab's saved active pane may have been pruned.
        for tab in &mut self.tabs {
            let mut ids = Vec::new();
            tab.layout.leaves(&mut ids);
            if !ids.contains(&tab.active_pane) {
                if let Some(first) = ids.first() {
                    tab.active_pane = *first;
                }
            }
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }

        // The adopting window's geometry differs from the closed one's: size
        // the active tab now; background tabs resize on selection, as always.
        self.resize_active_layout();
        self.dirty = true;
    }

    /// Current tab list + per-tab cwd as a persistable session.
    fn session_snapshot(&self) -> session::Session {
        let tabs = self
            .tabs
            .iter()
            .map(|t| session::TabSession {
                cwd: self
                    .pane(t.active_pane)
                    .map(|p| p.nebula_state.cwd.trim().to_owned())
                    .unwrap_or_default(),
            })
            .collect();
        session::Session::new(self.active_tab, tabs)
    }

    /// 1 Hz autosave (piggybacks on the chrome clock tick): persist the session
    /// when it changed, so a crash or force-kill restores to within a second.
    /// Only the focused window writes — two open windows must not fight over
    /// the file every second; last-focused wins, which is also the window the
    /// user most plausibly wants back.
    pub fn autosave_session(&mut self) {
        if self.session_exempt {
            return;
        }
        let focused =
            self.pane(self.focused_pane_id()).is_some_and(|p| p.terminal.lock().is_focused);
        if !focused {
            return;
        }
        let snapshot = self.session_snapshot();
        if self.last_saved_session.as_ref() == Some(&snapshot) {
            return;
        }
        session::save(&snapshot);
        self.last_saved_session = Some(snapshot);
    }

    /// Drop the autosave dedup cache so the next tick rewrites the session
    /// file (another window's teardown just wrote ITS final snapshot over it).
    pub fn mark_session_dirty(&mut self) {
        self.last_saved_session = None;
    }

    /// Dock the whole layout of tab `source` into the active tab: the active
    /// layout becomes a 50/50 split with the docked tree on `nav`'s side, the
    /// source tab disappears from the bar, and focus follows the docked pane.
    /// Pure tree surgery — panes live in the window-level pool, so no PTY is
    /// touched beyond the resize at the end.
    fn dock_tab_into_active(&mut self, source: usize, nav: crate::display::SplitNav) {
        use crate::display::{SplitDirection, SplitNav};

        if source >= self.tabs.len() || source == self.active_tab || self.tabs.len() < 2 {
            return;
        }

        let src_entry = self.tabs.remove(source);
        if source < self.active_tab {
            self.active_tab -= 1;
        }

        let entry = &mut self.tabs[self.active_tab];
        // Temporarily park a placeholder leaf so the old tree can move.
        let old = mem::replace(&mut entry.layout, Layout::Leaf(src_entry.active_pane));
        let (direction, src_first) = match nav {
            SplitNav::Left => (SplitDirection::LeftRight, true),
            SplitNav::Right => (SplitDirection::LeftRight, false),
            SplitNav::Up => (SplitDirection::TopBottom, true),
            SplitNav::Down => (SplitDirection::TopBottom, false),
        };
        let (first, second) = if src_first {
            (src_entry.layout, old)
        } else {
            (old, src_entry.layout)
        };
        entry.layout = Layout::Split {
            direction,
            ratio: 0.5,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        };
        // Focus follows the docked pane (VS Code behaviour).
        entry.active_pane = src_entry.active_pane;

        // A zoomed pane would hide the fresh split; drop the zoom.
        self.zoom = None;

        // Structural change: grids AND PTYs need their sizes immediately.
        self.resize_active_layout();
        self.dirty = true;
    }

    /// Show a fastfetch-style welcome screen in a freshly-created pane.
    fn run_fastfetch_intro(&mut self, pane_id: PaneId) {
        if !self.display.nebula_fetch_enabled {
            return;
        }
        let cols = self.display.size_info.columns();
        if let Some(i) = self.panes.iter().position(|p| p.id == pane_id) {
            let pane = &mut self.panes[i];
            pane.intro_cols = Some(cols);
            pane.notifier
                .notify(nebula_fastfetch_intro_command_for(cols, self.display.nebula_shell));
        }
    }

    /// Spawn a new pane into the pool without attaching it to any tab. `cwd`
    /// overrides the shell's startup directory when set. Returns the new pane's
    /// id, or `None` if the shell failed to start.
    fn spawn_pane_detached(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        size_info: crate::display::SizeInfo,
    ) -> Option<PaneId> {
        self.spawn_pane_detached_with(cwd, size_info, None)
    }

    /// Like [`Self::spawn_pane_detached`] with an optional shell override
    /// (quick-launch profiles run their own command instead of the default).
    fn spawn_pane_detached_with(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        size_info: crate::display::SizeInfo,
        shell: Option<nebula_terminal::tty::Shell>,
    ) -> Option<PaneId> {
        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;

        let window_id = self.display.window.id();
        let mut pty_config = self.config.pty_config();
        // NOTE: the executor choice (PowerShell/Bash) is applied inside
        // `tty::windows::cmdline` from `nebula_settings.txt` whenever
        // `pty_config.shell` is `None` — it must NOT be overridden here, or the
        // bash path would lose its Nebula rcfile (OSC 7 cwd / prompt contract).
        // A profile override (`shell` param) intentionally bypasses that.
        if shell.is_some() {
            pty_config.shell = shell;
        }
        if cwd.is_some() {
            pty_config.working_directory = cwd;
        }
        match Self::create_pane(&size_info, window_id, &self.config, pty_config, &self.proxy, pane_id)
        {
            Ok(pane) => {
                self.panes.push(pane);
                Some(pane_id)
            },
            Err(err) => {
                error!("Failed to spawn pane: {err}");
                None
            },
        }
    }

    /// Look up a pane in the pool by id.
    fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// Index of a pane in the pool by id.
    fn pane_index(&self, id: PaneId) -> Option<usize> {
        self.panes.iter().position(|p| p.id == id)
    }

    /// Working directory of the focused pane (from the shell's `NEBULA|cwd|…`
    /// title report) for a new tab/split to inherit. `None` if unknown.
    fn focused_cwd(&self) -> Option<std::path::PathBuf> {
        let cwd = self.pane(self.focused_pane_id()).map(|p| p.nebula_state.cwd.clone())?;
        // Validate the shell-reported cwd still points at a real directory. A
        // stale or non-filesystem path would otherwise make the new pane's
        // CreateProcessW fail with ERROR_DIRECTORY.
        session::valid_dir(&cwd)
    }

    /// First busy (non-whitelisted) process running under any of `pane_ids`'
    /// shells, for the close-confirmation safety net. `None` = safe to close.
    fn busy_process_in(&self, pane_ids: &[PaneId]) -> Option<String> {
        pane_ids
            .iter()
            .filter_map(|id| self.pane(*id))
            .find_map(|pane| crate::process_tree::busy_child(pane.shell_pid))
    }

    /// Flag the tab containing `pane_id` as having rung its bell, unless it is
    /// the active tab (a bell in the visible tab needs no indicator).
    fn mark_pane_bell(&mut self, pane_id: PaneId) {
        let active = self.active_tab;
        let mut marked = false;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let mut ids = Vec::new();
            t.layout.leaves(&mut ids);
            if ids.contains(&pane_id) {
                if i != active && !t.has_bell {
                    t.has_bell = true;
                    marked = true;
                }
                break;
            }
        }
        if marked {
            // A bell in a BACKGROUND tab is invisible even with the window
            // focused (claude/codex finishing a turn there) — deliver the
            // system notification here. The window-unfocused case is handled
            // at the per-pane Bell event, so the two paths never double-ring.
            // Use the REAL window focus: a background pane's cached
            // `terminal.is_focused` starts true and may never see a focus
            // event, which would double-ring against the per-pane path.
            if self.display.window.has_focus() {
                let program = self
                    .pane(pane_id)
                    .and_then(|p| p.nebula_state.running_program.clone());
                crate::notify::deliver(
                    &self.display.window,
                    &crate::notify::Notification::Bell { program },
                    Some(pane_id),
                );
            }
            self.dirty = true;
        }
    }

    /// Apply a typed AI-CLI lifecycle event (claude/codex via the nebula-hook
    /// pipe) to its pane's turn state — the exact, edge-triggered version of
    /// what the BEL heuristics approximate. Returns `false` when the pane
    /// does not belong to this window so the processor can try the next one.
    pub fn handle_ai_hook(&mut self, ev: &crate::ai_hook::AiHookEvent) -> bool {
        // A missing pane id (env stripped by an intermediate layer) degrades
        // to the focused pane of the first window asked.
        let pane_id = ev.pane.unwrap_or_else(|| self.focused_pane_id());
        let Some(idx) = self.pane_index(pane_id) else { return false };

        // The hook names its client ("claude" / "codex") — ground truth for
        // the sidebar program icon, unlike the OSC 133 command-line sniffing
        // which misses wrapped launches and integration-less shells.
        {
            let state = &mut self.panes[idx].nebula_state;
            state.running_program = Some(ev.source.clone());
        }

        match ev.kind {
            crate::ai_hook::AiHookKind::PromptSubmit => {
                // A turn started: spinner resumes, stale dot is consumed.
                let state = &mut self.panes[idx].nebula_state;
                state.awaiting_input = false;
                state.finished_unseen = false;
                // No shell integration = no OSC 133;C ever ran: give the
                // spinner a start mark so the turn still animates.
                state.command_started.get_or_insert_with(std::time::Instant::now);
            },
            crate::ai_hook::AiHookKind::TurnDone
            | crate::ai_hook::AiHookKind::NeedsAttention => {
                {
                    let state = &mut self.panes[idx].nebula_state;
                    state.awaiting_input = true;
                    state.finished_unseen = true;
                }
                // Tab dot when the pane sits in a background tab (same rule
                // as mark_pane_bell; the visible tab shows the pane itself).
                let mut background_tab = false;
                let active = self.active_tab;
                for (i, tab) in self.tabs.iter_mut().enumerate() {
                    let mut ids = Vec::new();
                    tab.layout.leaves(&mut ids);
                    if ids.contains(&pane_id) {
                        if i != active {
                            tab.has_bell = true;
                            background_tab = true;
                        }
                        break;
                    }
                }
                // Toast policy in one place: unfocused window, or focused
                // window with the pane hidden in a background tab. The global
                // toast throttle absorbs the BEL/OSC-9 double fire when
                // claude's notif channel is active as well.
                let attention = ev.kind == crate::ai_hook::AiHookKind::NeedsAttention;
                if !self.display.window.has_focus() || background_tab {
                    crate::notify::deliver(
                        &self.display.window,
                        &crate::notify::Notification::AiTurn {
                            program: ev.source.clone(),
                            message: ev.message.clone(),
                            attention,
                        },
                        Some(pane_id),
                    );
                }
            },
        }

        self.dirty = true;
        self.display.window.request_redraw();
        true
    }

    /// Toast click landed: bring this window to the foreground and, when the
    /// toast named a pane, surface its tab and focus that split.
    pub fn focus_from_toast(&mut self, pane: Option<u64>) {
        if let Some(pane_id) = pane {
            let index = self.tabs.iter().position(|tab| {
                let mut ids = Vec::new();
                tab.layout.leaves(&mut ids);
                ids.contains(&pane_id)
            });
            if let Some(index) = index {
                if index != self.active_tab {
                    self.select_tab(index);
                }
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.active_pane = pane_id;
                }
            }
        }
        // Best-effort: Windows may downgrade a background process's focus
        // request to a taskbar flash; the click usually grants it.
        self.display.window.focus_window();
        self.dirty = true;
    }

    /// Switch the active tab, resizing its panes to the current window.
    fn select_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.active_tab = index;
        self.tabs[index].has_bell = false;
        self.zoom = None;
        self.resize_active_layout();
        self.dirty = true;
    }

    /// Confirm a typed-`ssh` login as connected and save its destination to
    /// the sidebar, once the session shows PTY activity beyond the fast-
    /// failure window. Called on Wakeup: the old flow only saved when the
    /// session ENDED (`SAVE_MIN_SESSION` at CommandDone), which left the
    /// sidebar empty exactly while the user was connected and looking at it.
    pub fn confirm_ssh_on_activity(&mut self, pane_id: Option<u64>) {
        let Some(index) = pane_id.and_then(|id| self.pane_index(id)) else { return };
        let state = &mut self.panes[index].nebula_state;
        if state.pending_ssh_host.is_some()
            && state
                .command_started
                .is_some_and(|started| started.elapsed() >= crate::ssh::SAVE_CONNECTED_AFTER)
        {
            if let Some(host) = state.pending_ssh_host.take() {
                self.display.nebula_save_ssh_host(&host);
                self.dirty = true;
            }
        }
    }

    /// Close the pane whose shell produced an `Exit` event, or the focused pane
    /// when `pane_id` is `None`. Returns `true` if the last tab closed (the
    /// window should close).
    pub fn close_tab_by_id(&mut self, pane_id: Option<u64>) -> bool {
        let id = pane_id.unwrap_or_else(|| self.focused_pane_id());
        self.close_pane(id)
    }

    /// Close an entire tab (all of its panes). Returns `true` if it was the last
    /// tab (the window should close).
    fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }

        let entry = self.tabs.remove(index);
        let mut ids = Vec::new();
        entry.layout.leaves(&mut ids);
        for id in ids {
            if let Some(i) = self.pane_index(id) {
                let pane = self.panes.remove(i);
                let _ = pane.notifier.0.send(Msg::Shutdown);
            }
        }

        if self.tabs.is_empty() {
            return true;
        }

        if self.active_tab > index {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.resize_active_layout();
        self.dirty = true;
        false
    }

    /// Update the terminal window to the latest config.
    pub fn update_config(&mut self, new_config: Rc<UiConfig>) {
        let old_config = mem::replace(&mut self.config, new_config);

        // Apply ipc config if there are overrides.
        self.config = self.window_config.override_config_rc(self.config.clone());

        self.display.update_config(&self.config);
        let focused = self.focused_pane_id();
        if let Some(pane) = self.pane(focused) {
            pane.terminal.lock().set_options(self.config.term_options());
        }

        // Reload cursor if its thickness has changed.
        if (old_config.cursor.thickness() - self.config.cursor.thickness()).abs() > f32::EPSILON {
            self.display.pending_update.set_cursor_dirty();
        }

        if old_config.font != self.config.font {
            let scale_factor = self.display.window.scale_factor as f32;
            // Do not update font size if it has been changed at runtime.
            if self.display.font_size == old_config.font.size().scale(scale_factor) {
                self.display.font_size = self.config.font.size().scale(scale_factor);
            }

            let font = self.config.font.clone().with_size(self.display.font_size);
            self.display.pending_update.set_font(font);
        }

        // Always reload the theme to account for auto-theme switching.
        self.display.window.set_theme(self.config.window.theme());

        // Update display if either padding options or resize increments were changed.
        let window_config = &old_config.window;
        if window_config.padding(1.) != self.config.window.padding(1.)
            || window_config.dynamic_padding != self.config.window.dynamic_padding
            || window_config.resize_increments != self.config.window.resize_increments
        {
            self.display.pending_update.dirty = true;
        }

        // Update title on config reload according to the following table.
        //
        // │cli │ dynamic_title │ current_title == old_config ││ set_title │
        // │ Y  │       _       │              _              ││     N     │
        // │ N  │       Y       │              Y              ││     Y     │
        // │ N  │       Y       │              N              ││     N     │
        // │ N  │       N       │              _              ││     Y     │
        if !self.preserve_title
            && (!self.config.window.dynamic_title
                || self.display.window.title() == old_config.window.identity.title)
        {
            self.display.window.set_title(self.config.window.identity.title.clone());
        }

        let opaque = self.config.window_opacity() >= 1.;

        // Disable shadows for transparent windows on macOS.
        #[cfg(target_os = "macos")]
        self.display.window.set_has_shadow(opaque);

        #[cfg(target_os = "macos")]
        self.display.window.set_option_as_alt(self.config.window.option_as_alt());

        // Change opacity and blur state.
        self.display.window.set_transparent(!opaque);
        self.display.window.set_blur(self.config.window.blur);

        // Update hint keys.
        self.display.hint_state.update_alphabet(self.config.hints.alphabet());

        // Update cursor blinking.
        let event = Event::new(TerminalEvent::CursorBlinkingChange.into(), None);
        self.event_queue.push(event.into());

        self.dirty = true;
    }

    /// Get reference to the window's configuration.
    #[cfg(unix)]
    pub fn config(&self) -> &UiConfig {
        &self.config
    }

    /// Clear the window config overrides.
    #[cfg(unix)]
    pub fn reset_window_config(&mut self, config: Rc<UiConfig>) {
        // Clear previous window errors.
        self.message_buffer.remove_target(LOG_TARGET_IPC_CONFIG);

        self.window_config.clear();

        // Reload current config to pull new IPC config.
        self.update_config(config);
    }

    /// Add new window config overrides.
    #[cfg(unix)]
    pub fn add_window_config(&mut self, config: Rc<UiConfig>, options: &ParsedOptions) {
        // Clear previous window errors.
        self.message_buffer.remove_target(LOG_TARGET_IPC_CONFIG);

        self.window_config.extend_from_slice(options);

        // Reload current config to pull new IPC config.
        self.update_config(config);
    }

    /// Draw the window.
    pub fn draw(&mut self, scheduler: &mut Scheduler) {
        self.display.window.requested_redraw = false;
        self.sync_chrome_tabs();
        // Right-side drawer follows the focused pane's cwd (throttled inside).
        let panel_cwd = self.focused_cwd();
        self.display.side_panel_sync(panel_cwd);

        // Chrome clock: 1 Hz normally, 8 fps while a sidebar spinner is
        // animating. Re-arm on cadence change.
        let clock_timer = TimerId::new(Topic::NebulaClock, self.display.window.id());
        let interval = if self.display.any_tab_running()
            || self.display.chrome_editor_active()
            || self.display.chrome_animating()
        {
            Duration::from_millis(125)
        } else {
            Duration::from_secs(1)
        };
        if self.clock_interval != interval {
            scheduler.unschedule(clock_timer);
            self.clock_interval = interval;
        }
        if !scheduler.scheduled(clock_timer) {
            let event = Event::new(EventType::NebulaTick, self.display.window.id());
            scheduler.schedule(event, interval, true, clock_timer);
        }

        if self.occluded {
            return;
        }

        self.dirty = false;

        // Force the display to process any pending display update.
        self.display.process_renderer_update();

        // Request immediate re-draw if visual bell animation is not finished yet.
        if !self.display.visual_bell.completed() {
            // We can get an OS redraw which bypasses nebula's frame throttling, thus
            // marking the window as dirty when we don't have frame yet.
            if self.display.window.has_frame {
                self.display.window.request_redraw();
            } else {
                self.dirty = true;
            }
        }

        // Chrome sidebar/drawer transitions need display-rate frames until settled.
        if self.display.chrome_animating() {
            if self.display.window.has_frame {
                self.display.window.request_redraw();
            } else {
                self.dirty = true;
            }
        }

        // Redraw the window: walk the active tab's layout tree and draw each
        // pane in its rectangle. A single-pane tab uses the simple full-window
        // path; multi-pane tabs draw every leaf then overlay dividers + dimming.
        let pane_rects = self.layout_geometry(false).0;
        let divider_rects = self.layout_geometry(true).1;
        let focused = self.focused_pane_id();

        // Document-viewer tab: no pane, no grid. Draw the doc into the tab's
        // content rect; `present_frame` inside lays the normal chrome on top.
        if let Some(doc) = self.tabs.get_mut(self.active_tab).and_then(|tab| tab.doc.as_mut()) {
            let view = pane_rects.first().map(|(_, view)| *view).unwrap_or(self.display.size_info);
            self.display.begin_pane_frame(&self.config);
            self.display.draw_doc_frame(doc, view, scheduler);
            return;
        }

        if pane_rects.len() <= 1 {
            let id = pane_rects.first().map(|(id, _)| *id).unwrap_or(focused);
            if let Some(idx) = self.pane_index(id) {
                let pane = &mut self.panes[idx];
                let terminal_arc = pane.terminal.clone();
                let terminal = terminal_arc.lock();
                self.display.draw(
                    terminal,
                    scheduler,
                    &self.message_buffer,
                    &self.config,
                    &mut pane.search_state,
                    &mut pane.nebula_state,
                );
            }
        } else {
            self.display.begin_pane_frame(&self.config);
            let mut dim_rects = Vec::new();
            for (i, (id, view)) in pane_rects.iter().enumerate() {
                let Some(idx) = self.pane_index(*id) else { continue };
                let is_focused = *id == focused;
                if !is_focused {
                    dim_rects.push((
                        view.padding_x(),
                        view.padding_y(),
                        view.width() - 2.0 * view.padding_x(),
                        view.height() - 2.0 * view.padding_y(),
                    ));
                }
                let pane = &mut self.panes[idx];
                let terminal_arc = pane.terminal.clone();
                let terminal = terminal_arc.lock();
                self.display.draw_pane_view(
                    terminal,
                    &self.message_buffer,
                    &self.config,
                    &mut pane.search_state,
                    &mut pane.nebula_state,
                    *view,
                    is_focused,
                    i == 0,
                );
            }
            self.display.draw_split_overlays(&dim_rects, &divider_rects);
            self.display.finish_pane_frame(scheduler);
        }

        // Startup profiling: the process-wide first completed frame.
        {
            use std::sync::atomic::AtomicBool;
            static FIRST_FRAME: AtomicBool = AtomicBool::new(false);
            if !FIRST_FRAME.swap(true, Ordering::Relaxed) {
                crate::boot_trace("first frame drawn");
            }
        }
    }

    /// Reorder the tab bar by moving the tab at index `from` to index `to`.
    /// With the pane pool the bar always lists every tab in storage order
    /// (displayed == storage index), so this is unconditional.
    fn move_tab(&mut self, from: usize, to: usize) {
        let len = self.tabs.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let entry = self.tabs.remove(from);
        self.tabs.insert(to, entry);
        // Keep the same tab focused: remap the active index through the move.
        self.active_tab = Self::shifted_index(self.active_tab, from, to);
        self.sync_chrome_tabs();
        self.dirty = true;
    }

    /// New position of `idx` after the element at `from` is removed and
    /// re-inserted at `to` (a single-element move within the vector).
    fn shifted_index(idx: usize, from: usize, to: usize) -> usize {
        if idx == from {
            to
        } else if from < to && idx > from && idx <= to {
            idx - 1
        } else if from > to && idx >= to && idx < from {
            idx + 1
        } else {
            idx
        }
    }

    fn sync_chrome_tabs(&mut self) {
        // The visible tab's activity is seen by definition — consume its
        // flag before it can render (dots are for background tabs only).
        if let Some(id) = self.tabs.get(self.active_tab).map(|t| t.active_pane) {
            if let Some(i) = self.pane_index(id) {
                self.panes[i].nebula_state.finished_unseen = false;
            }
        }

        let mut labels = Vec::with_capacity(self.tabs.len());
        let mut dots = Vec::with_capacity(self.tabs.len());
        let mut running = Vec::with_capacity(self.tabs.len());
        let mut logos = Vec::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            let pane = self.pane(tab.active_pane);
            let state = pane.map(|p| &p.nebula_state);
            // Use custom name if set, otherwise derive from cwd/title
            let mut label = if let Some(custom) = &tab.custom_name {
                custom.clone()
            } else {
                pane.map(Self::chrome_tab_label).unwrap_or_default()
            };
            // Program icon (Nerd Font) in front of the label while a command
            // runs — the sidebar shows WHAT each tab is busy with. AI clients
            // with a real brand logo (claude/codex) skip the glyph: the
            // display layer textures the actual mark into the icon slot.
            let logo =
                state.and_then(|s| s.running_program.as_deref()).and_then(crate::display::ai_logo);
            if let Some(program) = state.and_then(|s| s.running_program.as_deref()) {
                if logo.is_none() {
                    label = format!("{} {label}", crate::display::program_icon(program));
                }
            }
            logos.push(logo);
            labels.push(label);
            // Unseen-result dot: bell in a background tab, a tracked command
            // that finished unseen, or a tracked program parked at "waiting
            // for input" (claude between turns). The ring collapsing into a
            // dot IS the "turn finished, your move" signal — also on the
            // visible tab, where a merely-paused ring still read as busy.
            dots.push(tab.has_bell || state.is_some_and(|s| {
                s.finished_unseen || (s.command_started.is_some() && s.awaiting_input)
            }));
            // Spinner only while the command actually works; once it rang BEL
            // and waits for input the dot above takes over.
            running.push(
                state.is_some_and(|s| s.command_started.is_some() && !s.awaiting_input),
            );
        }
        let active = self.active_tab.min(labels.len().saturating_sub(1));
        // displayed == storage index always holds now, so the bar is reorderable.
        self.display.set_chrome_tabs(labels, dots, running, logos, active, true);
    }

    fn chrome_tab_label(pane: &Pane) -> String {
        let cwd = pane.nebula_state.cwd.trim();
        if !cwd.is_empty() {
            // Just the directory's own name: a full path wall-to-walls the
            // sidebar row and kills the design's breathing room. The last
            // meaningful component is what identifies the workspace anyway.
            let name = cwd
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(cwd);
            return name.to_owned();
        }

        if pane.title != "shell" && !pane.title.trim().is_empty() {
            return pane.title.clone();
        }

        std::env::current_dir()
            .ok()
            .and_then(|path| {
                path.file_name().map(|n| n.to_string_lossy().into_owned())
            })
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| ".".to_owned())
    }

    /// Process events for this terminal window.
    pub fn handle_event(
        &mut self,
        #[cfg(target_os = "macos")] event_loop: &ActiveEventLoop,
        event_proxy: &EventLoopProxy<Event>,
        clipboard: &mut Clipboard,
        scheduler: &mut Scheduler,
        event: WinitEvent<Event>,
    ) {
        match event {
            WinitEvent::AboutToWait
            | WinitEvent::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                // Skip further event handling with no staged updates.
                if self.event_queue.is_empty() {
                    return;
                }

                // Continue to process all pending events.
            },
            event => {
                self.event_queue.push(event);
                return;
            },
        }

        self.preprocess_split_mouse();

        // Flag background tabs whose panes rang a bell (🔔 in the tab bar).
        let bell_panes: Vec<u64> = self
            .event_queue
            .iter()
            .filter_map(|e| match e {
                WinitEvent::UserEvent(ev) => ev.terminal_bell_pane(),
                _ => None,
            })
            .collect();
        for pane_id in bell_panes {
            self.mark_pane_bell(pane_id);
        }

        // Any key press means the user is interacting again: resume the
        // focused pane's sidebar spinner (claude's next turn after its
        // wait-for-input bell). A stray clear is harmless — the next bell
        // pauses it again.
        let key_pressed = self.event_queue.iter().any(|e| {
            matches!(
                e,
                WinitEvent::WindowEvent {
                    event: WindowEvent::KeyboardInput { event: key, .. },
                    ..
                } if key.state == ElementState::Pressed
            )
        });
        if key_pressed {
            let focused = self.focused_pane_id();
            if let Some(i) = self.pane_index(focused) {
                self.panes[i].nebula_state.awaiting_input = false;
            }
        }

        // In a split, a left click moves keyboard focus to the clicked pane.
        // Resolve focus from the click position before routing this batch so the
        // click lands on the pane the user aimed at.
        if !matches!(self.active_layout(), Layout::Leaf(_)) {
            let ffm = self.config.mouse.focus_follows_mouse;
            // The click's real position is the latest CursorMoved in THIS batch:
            // winit's MouseInput carries no coordinates, and `self.mouse` still
            // holds the PREVIOUS batch's position — this batch's CursorMoved that
            // moved the pointer to the click hasn't been routed to the input
            // processor yet. Using the stale `self.mouse` here focuses the wrong
            // pane, so typed input lands in it (the "split typing bleeds into the
            // other pane" bug). Fall back to `self.mouse` only when the pointer
            // didn't move this batch (then it is already the current position).
            let latest_pos = self.event_queue.iter().rev().find_map(|e| match e {
                WinitEvent::WindowEvent {
                    event: WindowEvent::CursorMoved { position, .. }, ..
                } => Some((position.x as f32, position.y as f32)),
                _ => None,
            });
            let clicked = self.event_queue.iter().any(|e| {
                matches!(
                    e,
                    WinitEvent::WindowEvent {
                        event: WindowEvent::MouseInput {
                            state: ElementState::Pressed,
                            button: MouseButton::Left,
                            ..
                        },
                        ..
                    }
                )
            });
            // A left click always refocuses the clicked pane; focus-follows-mouse
            // also refocuses on plain pointer motion.
            let target = if clicked {
                latest_pos.or(Some((self.mouse.x as f32, self.mouse.y as f32)))
            } else if ffm {
                latest_pos
            } else {
                None
            };
            if let Some((px, py)) = target {
                if let Some(id) = self.pane_at_position(px, py) {
                    if self.tabs[self.active_tab].active_pane != id {
                        self.tabs[self.active_tab].active_pane = id;
                        self.dirty = true;
                    }
                }
            }
        }

        // Route each event to its own pane. A Terminal event names the pane
        // that produced it and must update THAT pane's state; window input
        // (keyboard, mouse) always belongs to the focused pane of the active
        // tab. Resolving one target for the whole batch let a background
        // pane's output drag the batch — keystrokes included — to itself,
        // typing into the wrong PTY.
        let focused_id = self.focused_pane_id();
        // A doc tab has no pane: its events run against `doc_pane` below so
        // chrome interaction (tab switching, closing, the sidebar) keeps
        // working; anything typed lands in the sink notifier.
        let doc_tab = self.tabs.get(self.active_tab).is_some_and(|tab| tab.doc.is_some());
        let focused = match self.pane_index(focused_id) {
            Some(index) => Some(index),
            None if doc_tab => None,
            None => return,
        };

        // Point input/hint hit-testing at the focused pane's rectangle so mouse
        // coordinates map into its (possibly partial) grid. `None` → full window.
        let pane_rects = self.layout_geometry(false).0;
        let pane_view = if pane_rects.len() > 1 {
            pane_rects.iter().find(|(id, _)| *id == focused_id).map(|(_, v)| *v)
        } else {
            None
        };
        self.display.nebula_pane_view = pane_view;

        let old_is_searching = focused
            .is_some_and(|index| self.panes[index].search_state.history_index.is_some());

        let target_of = |event: &WinitEvent<Event>| match event {
            WinitEvent::UserEvent(event) => event.terminal_tab_id().unwrap_or(focused_id),
            _ => focused_id,
        };
        // Consume the batch in order, one processor per run of consecutive
        // events sharing a target pane.
        let mut events = mem::take(&mut self.event_queue).into_iter().peekable();
        while let Some(event) = events.next() {
            let target_id = target_of(&event);
            let (pane, doc) = match self.pane_index(target_id) {
                Some(pane_idx) => (&mut self.panes[pane_idx], None),
                None if target_id == DOC_PANE_ID && doc_tab => (
                    &mut self.doc_pane,
                    self.tabs.get_mut(self.active_tab).and_then(|tab| tab.doc.as_mut()),
                ),
                None => {
                    // Source pane is gone (closed with output still in flight):
                    // drop its events, keep the rest of the batch.
                    while events.next_if(|event| target_of(event) == target_id).is_some() {}
                    continue;
                },
            };

            let terminal_arc = pane.terminal.clone();
            let mut terminal = terminal_arc.lock();
            let context = ActionContext {
                cursor_blink_timed_out: &mut self.cursor_blink_timed_out,
                prev_bell_cmd: &mut self.prev_bell_cmd,
                message_buffer: &mut self.message_buffer,
                inline_search_state: &mut pane.inline_search_state,
                search_state: &mut pane.search_state,
                nebula_state: &mut pane.nebula_state,
                doc,
                modifiers: &mut self.modifiers,
                notifier: &mut pane.notifier,
                display: &mut self.display,
                mouse: &mut self.mouse,
                touch: &mut self.touch,
                dirty: &mut self.dirty,
                occluded: &mut self.occluded,
                terminal: &mut terminal,
                #[cfg(not(windows))]
                master_fd: pane.master_fd,
                #[cfg(not(windows))]
                shell_pid: pane.shell_pid,
                preserve_title: self.preserve_title,
                config: &self.config,
                event_proxy,
                #[cfg(target_os = "macos")]
                event_loop,
                clipboard,
                scheduler,
            };
            let mut processor = input::Processor::new(context);
            processor.handle_event(event);
            while let Some(event) = events.next_if(|event| target_of(event) == target_id) {
                processor.handle_event(event);
            }
        }

        // Post-batch display housekeeping reads the focused pane's terminal
        // (the doc stub when a doc tab is active).
        let terminal_arc = match focused {
            Some(index) => self.panes[index].terminal.clone(),
            None => self.doc_pane.terminal.clone(),
        };
        let mut terminal = terminal_arc.lock();

        // Process DisplayUpdate events.
        if self.display.pending_update.dirty {
            let pane = match focused {
                Some(index) => &mut self.panes[index],
                None => &mut self.doc_pane,
            };
            Self::submit_display_update(
                &mut terminal,
                &mut self.display,
                &mut pane.notifier,
                &self.message_buffer,
                &mut pane.search_state,
                old_is_searching,
                &self.config,
            );
            self.dirty = true;

            // Deferred PTY resize: a lone resize (startup, maximize, sidebar
            // toggle) passes through IMMEDIATELY — startup latency is the
            // first principle. Only a rapid follow-up within the coalescing
            // window (an interactive drag) defers to the trailing-edge settle
            // timer, so ConPTY's per-resize viewport repaint fires once at
            // drag end instead of per tick.
            if self.display.nebula_pty_resize_pending {
                let now = Instant::now();
                let dragging = self
                    .last_pty_resize
                    .is_some_and(|t| now.duration_since(t) < Duration::from_millis(300));
                if dragging {
                    let timer =
                        TimerId::new(Topic::NebulaResizeSettle, self.display.window.id());
                    scheduler.unschedule(timer);
                    let event =
                        Event::new(EventType::NebulaResizeSettled, self.display.window.id());
                    scheduler.schedule(event, Duration::from_millis(150), false, timer);
                } else {
                    // Leading edge: flush now. `resize_active_layout_ptys`
                    // only touches notifiers/intro (no terminal lock), so the
                    // focused pane's guard held above stays safe.
                    self.display.nebula_pty_resize_pending = false;
                    self.last_pty_resize = Some(now);
                    self.resize_active_layout_ptys();
                }
            }

            // A window resize rebuilt `self.display.size_info`; re-derive every
            // pane's grid so a split tracks the new dimensions. PTY-side
            // notification waits for the settle timer above.
            if !matches!(self.active_layout(), Layout::Leaf(_)) {
                drop(terminal);
                self.resize_active_layout_grids();
                return;
            }
        }

        if self.dirty || self.mouse.hint_highlight_dirty {
            self.dirty |= self.display.update_highlighted_hints(
                &terminal,
                &self.config,
                &self.mouse,
                self.modifiers.state(),
            );
            self.mouse.hint_highlight_dirty = false;
        }

        // Don't call `request_redraw` when event is `RedrawRequested` since the `dirty` flag
        // represents the current frame, but redraw is for the next frame.
        if self.dirty
            && self.display.window.has_frame
            && !self.occluded
            && !matches!(event, WinitEvent::WindowEvent { event: WindowEvent::RedrawRequested, .. })
        {
            self.display.window.request_redraw();
        }
    }

    /// ID of this terminal context.
    pub fn id(&self) -> WindowId {
        self.display.window.id()
    }

    /// Write the ref test results to the disk.
    pub fn write_ref_test_results(&self) {
        // Dump grid state.
        let focused = self.focused_pane_id();
        let mut grid =
            self.pane(focused).expect("focused pane exists").terminal.lock().grid().clone();
        grid.initialize_all();
        grid.truncate();

        let serialized_grid = json::to_string(&grid).expect("serialize grid");

        let size_info = &self.display.size_info;
        let size = TermSize::new(size_info.columns(), size_info.screen_lines());
        let serialized_size = json::to_string(&size).expect("serialize size");

        let serialized_config = format!("{{\"history_size\":{}}}", grid.history_size());

        File::create("./grid.json")
            .and_then(|mut f| f.write_all(serialized_grid.as_bytes()))
            .expect("write grid.json");

        File::create("./size.json")
            .and_then(|mut f| f.write_all(serialized_size.as_bytes()))
            .expect("write size.json");

        File::create("./config.json")
            .and_then(|mut f| f.write_all(serialized_config.as_bytes()))
            .expect("write config.json");
    }

    /// Flush the deferred PTY resize once an interactive resize settles
    /// (`Topic::NebulaResizeSettle` fired): every pane's PTY learns its final
    /// size in one shot, and pristine panes re-print the welcome intro once —
    /// instead of per drag tick, which flooded the scrollback with ConPTY's
    /// per-resize viewport repaints.
    pub fn apply_settled_pty_resize(&mut self) {
        if !mem::take(&mut self.display.nebula_pty_resize_pending) {
            return;
        }
        self.last_pty_resize = Some(Instant::now());
        self.resize_active_layout_ptys();
    }

    /// Submit the pending changes to the `Display`.
    fn submit_display_update(
        terminal: &mut Term<EventProxy>,
        display: &mut Display,
        notifier: &mut Notifier,
        message_buffer: &MessageBuffer,
        search_state: &mut SearchState,
        old_is_searching: bool,
        config: &UiConfig,
    ) {
        // Compute cursor positions before resize.
        let num_lines = terminal.screen_lines();
        let cursor_at_bottom = terminal.grid().cursor.point.line + 1 == num_lines;
        let origin_at_bottom = if terminal.mode().contains(TermMode::VI) {
            terminal.vi_mode_cursor.point.line == num_lines - 1
        } else {
            search_state.direction == Direction::Left
        };

        display.handle_update(terminal, notifier, message_buffer, search_state, config);

        let new_is_searching = search_state.history_index.is_some();
        if !old_is_searching && new_is_searching {
            // Scroll on search start to make sure origin is visible with minimal viewport motion.
            let display_offset = terminal.grid().display_offset();
            if display_offset == 0 && cursor_at_bottom && !origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(1));
            } else if display_offset != 0 && origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(-1));
            }
        }
    }
}

impl Drop for WindowContext {
    fn drop(&mut self) {
        // Final session snapshot at teardown. Quitting by closing every tab
        // one by one reaches this with `tabs` already empty — persisting that
        // empty list is exactly what makes the next launch start clean.
        // Closing the whole window (X / Alt+F4 / shortcut) keeps the tabs, so
        // they restore. Crash/kill paths never get here and are covered by
        // the 1 Hz autosave instead.
        if !self.session_exempt {
            session::save(&self.session_snapshot());
        }

        // Shutdown every pane's PTY.
        for pane in &self.panes {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
    }
}
