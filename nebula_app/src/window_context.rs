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
use log::{error, info, warn};
use serde_json as json;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event as WinitEvent, Modifiers, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::WindowId;

use nebula_terminal::event::{Event as TerminalEvent, Notify};
use nebula_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::index::{Column, Direction, Line, Point};
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::test::TermSize;
use nebula_terminal::term::{Term, TermMode};
use nebula_terminal::tty;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::UiConfig;
use crate::config::ui_config::Profile;
use crate::display::window::Window;
use crate::display::{Display, NebulaPaneState};
use crate::event::{
    ActionContext, Event, EventProxy, EventType, Mouse, SearchState, TabRequest, TouchPurpose,
};
#[cfg(unix)]
use crate::logging::LOG_TARGET_IPC_CONFIG;
use crate::message_bar::MessageBuffer;
use crate::scheduler::{Scheduler, TimerId, Topic};
use crate::{input, renderer, session};

mod model;
mod nebula_fetch_art;
mod runtime;
/// New-tab welcome page (Windows logo + fastfetch intro). Stateless helpers.
mod welcome;
use welcome::nebula_fastfetch_intro_command_for;

use model::{DOC_PANE_ID, Layout, PaneId, TabEntry, TabLaunch};
pub use model::{DetachedWindow, Pane, WindowBoot};

/// Split-pane behaviour (toggle/resize/drag/focus); `impl WindowContext`.
mod split;

/// Mouse buttons whose press is an interaction with terminal content and must
/// therefore update split focus before the event is routed to a pane.
fn pane_focus_button(button: &MouseButton) -> bool {
    matches!(button, MouseButton::Left | MouseButton::Middle | MouseButton::Right)
}

/// Resolve window input while a modal is open. Multi-line paste is the only
/// confirmation that carries terminal data, so it stays bound to its source
/// pane; if that pane was reaped, the caller routes to normal focus and the
/// write-boundary pane-id guard drops the stale transaction.
fn routed_input_pane(
    confirm: Option<&crate::display::NebulaConfirm>,
    focused: PaneId,
    pane_exists: impl Fn(PaneId) -> bool,
) -> PaneId {
    confirm
        .and_then(crate::display::NebulaConfirm::paste_pane_id)
        .filter(|pane_id| pane_exists(*pane_id))
        .unwrap_or(focused)
}

fn select_initial_shell(
    configured: Option<tty::Shell>,
    user_default: Option<tty::Shell>,
    cli: Option<tty::Shell>,
) -> Option<tty::Shell> {
    cli.or(user_default).or(configured)
}

/// Validate a tree-provided cwd immediately before process creation. The tree
/// can disappear between drawing the action and handling its click; rejecting
/// that race avoids a platform-specific PTY/CreateProcess startup failure.
fn valid_new_tab_directory(path: &std::path::Path) -> bool {
    path.is_dir()
}

/// 一次标签插入的落点来源。由调用方声明意图，而不是让插入点自己猜：
/// 真正创建标签走 [`TabPlacement::Created`]（读新标签插入策略），会话恢复与
/// 工作区导入走 [`TabPlacement::AfterActive`]（保持各自记录的顺序）。
///
/// 这个区别必须由类型承载。恢复路径复用 `spawn_tab_*` 创建函数，光靠注释
/// 约定「恢复时别读策略」，下一个新增入口就会漏掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabPlacement {
    Created,
    AfterActive,
}

/// 新标签在标签顺序中的落点。所有插入点共用它，因此
/// `(active_tab + 1).min(len)` 这条计算只存在一处。
fn tab_insert_index(
    placement: TabPlacement,
    position: crate::display::NewTabPosition,
    active_tab: usize,
    tab_count: usize,
) -> usize {
    let after_active = active_tab.saturating_add(1).min(tab_count);
    match placement {
        TabPlacement::AfterActive => after_active,
        TabPlacement::Created => match position {
            crate::display::NewTabPosition::AfterCurrent => after_active,
            crate::display::NewTabPosition::End => tab_count,
        },
    }
}

/// Resolve a fresh tab's directory without allowing the global setting to
/// overwrite an explicit profile/command directory.
fn preferred_tab_cwd(
    explicit: Option<std::path::PathBuf>,
    startup: Option<std::path::PathBuf>,
    focused: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    explicit.or(startup).or(focused)
}

/// Initial-window precedence is deliberately different from a normal new
/// tab: an explicit CLI path and a restored session must survive a global
/// startup-directory change, while the setting still outranks static config.
fn preferred_initial_cwd(
    cli: Option<std::path::PathBuf>,
    restored: Option<std::path::PathBuf>,
    startup: Option<std::path::PathBuf>,
    configured: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    cli.or(restored).or(startup).or(configured)
}

/// Keep idle/editor redraw costs low while giving active task spinners a
/// display-rate clock. This changes only the timer cadence; no worker thread or
/// per-frame allocation is introduced.
#[inline]
fn chrome_clock_interval(
    spinner_running: bool,
    editor_active: bool,
    chrome_animating: bool,
) -> Duration {
    if spinner_running {
        Duration::from_micros(16_667)
    } else if editor_active || chrome_animating {
        Duration::from_millis(125)
    } else {
        Duration::from_secs(1)
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
    /// 上一帧的聚焦 pane。变化时给新聚焦终端补发 CursorBlinkingChange，
    /// blink 定时器按它的样式重新起表——否则从"不闪"的 pane 切到"该闪"
    /// 的 pane 后表根本没开，光标永远常亮。
    blink_focus_pane: Option<PaneId>,
    prev_bell_cmd: Option<Instant>,
    /// When the PTYs last learned their size. Drives the leading-edge check of
    /// the resize debounce: a lone resize (startup, maximize, sidebar toggle)
    /// passes through instantly; only a rapid follow-up — i.e. an interactive
    /// drag — defers to the settle timer.
    last_pty_resize: Option<Instant>,
    /// Current chrome clock cadence (1 Hz idle, 8 fps for finite chrome
    /// transitions, 60 fps while a task spinner runs).
    clock_interval: Duration,
    /// Last session snapshot written to disk, so the 1 Hz autosave can skip
    /// the write when nothing changed. `None` forces the next tick to write.
    last_saved_session: Option<session::Session>,
    /// Last normal inner size. Maximized/fullscreen resize events must not
    /// overwrite the dimensions Windows restores when leaving that state.
    windowed_size: LogicalSize<u32>,
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

        let display = Display::new(window, gl_context, &config, event_loop.system_theme(), false)?;
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

        let display = Display::new(window, gl_context, &config, event_loop.system_theme(), tabbed)?;

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
        // Startup no longer replays the saved window size (the session file's
        // window record is write-only), so the live inner size IS the last
        // normal size at this point.
        let windowed_size = display.window.inner_size().to_logical(display.window.scale_factor);

        // Bootstrap the tab set: fresh/restored windows spawn their first
        // pane here; an attach adopts the detached panes wholesale.
        let mut restore = None;
        let mut seed_pinned = false;
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
                let configured_cwd = pty_config.working_directory.clone();
                let cli_cwd = options
                    .terminal_options
                    .working_directory
                    .as_ref()
                    .filter(|path| path.is_dir())
                    .cloned();
                // A CLI-pinned directory means the user asked for this exact
                // tab: the restore below keeps it instead of dismantling it.
                seed_pinned = cli_cwd.is_some();
                let cli_shell = options.terminal_options.command().map(Into::into);
                pty_config.shell = select_initial_shell(
                    pty_config.shell.take(),
                    Self::default_shell_override(&config),
                    cli_shell,
                );
                options.terminal_options.override_pty_config(&mut pty_config);
                let restored_cwd = restore.as_ref().and_then(|session| {
                    session.tabs.first().and_then(|tab| session::valid_dir(&tab.cwd))
                });
                pty_config.working_directory = preferred_initial_cwd(
                    cli_cwd,
                    restored_cwd,
                    display.startup_directory(),
                    configured_cwd,
                );
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
                        custom_color: None,
                        launch: TabLaunch::Default,
                        doc: None,
                        image: None,
                        settings: false,
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
            blink_focus_pane: None,
            prev_bell_cmd: Default::default(),
            last_pty_resize: None,
            clock_interval: Duration::from_secs(1),
            last_saved_session: None,
            windowed_size,
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
            context.restore_session_tabs(&session, seed_pinned);
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
            ssh_destination: None,
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
            destination.clone(),
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
            ssh_destination: Some(destination),
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
            ssh_destination: None,
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
    /// 渲染门控看门狗（1 Hz 心跳调用）：Windows 的 `Occluded(false)` 与帧
    /// 回调都可能失约，卡住的 `occluded` / `has_frame` 会让整条 draw 路径
    /// 熄火——窗口"点什么都没反应"，最小化再复原才活过来（issue #21）。
    /// 窗口明明没最小化时强制解锁两道门；正常情况下它们本来就是开的，
    /// 这里是幂等空操作。
    pub fn unstick_render_gates_if_visible(&mut self) {
        if self.display.window.is_minimized().unwrap_or(false) {
            return;
        }
        if self.occluded || !self.display.window.has_frame {
            self.occluded = false;
            self.display.window.has_frame = true;
            self.dirty = true;
        }
    }

    pub fn handle_tab_request(&mut self, request: TabRequest) -> bool {
        use crate::display::NebulaConfirm;
        match request {
            TabRequest::New => {
                self.spawn_tab();
                false
            },
            TabRequest::NewAtDirectory(path) => {
                if valid_new_tab_directory(&path) {
                    self.spawn_tab_at(Some(path), TabPlacement::Created);
                } else {
                    warn!(
                        "Refusing to open a terminal at a missing or non-directory tree root: \
                         {path:?}"
                    );
                }
                false
            },
            TabRequest::NewProfile(profile) => {
                self.spawn_tab_profile_value(profile, TabPlacement::Created);
                false
            },
            TabRequest::NewShell { name, shell } => {
                self.spawn_tab_shell(name, shell, TabPlacement::Created);
                false
            },
            TabRequest::NewSsh(host) => {
                self.spawn_tab_ssh(host, TabPlacement::Created);
                false
            },
            TabRequest::OpenDoc(path) => {
                if crate::display::image_viewer::viewable_file(&path) {
                    self.open_image_tab(path);
                } else {
                    self.open_doc_tab(path);
                }
                false
            },
            TabRequest::OpenSettings => {
                self.open_settings_tab();
                false
            },
            TabRequest::Close => {
                if self
                    .tabs
                    .get(self.active_tab)
                    .is_some_and(|tab| tab.doc.is_some() || tab.image.is_some() || tab.settings)
                {
                    return self.close_tab(self.active_tab);
                }
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
            TabRequest::Duplicate(index) => {
                self.duplicate_tab(index);
                false
            },
            TabRequest::ExportWorkspace => {
                self.export_workspace(None);
                false
            },
            TabRequest::ExportTab(index) => {
                self.export_workspace(Some(index));
                false
            },
            TabRequest::ImportWorkspace => {
                self.import_workspace();
                false
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
            TabRequest::SplitIndex { index, direction } => {
                if self
                    .tabs
                    .get(index)
                    .is_some_and(|tab| tab.doc.is_none() && tab.image.is_none() && !tab.settings)
                {
                    self.select_tab(index);
                    self.split_focused(direction);
                }
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
            TabRequest::SetColor { index, color } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.custom_color = color;
                    self.sync_chrome_tabs();
                    self.mark_session_dirty();
                    self.dirty = true;
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

    /// 把一个新标签实体加入标签顺序并激活它，返回它的落点。
    ///
    /// 这是标签实体进入顺序的唯一入口（拖拽重排的 `move_tab` 除外——那里的
    /// 落点由用户手势直接给出）。`placement` 由调用方声明意图：真正创建标签
    /// 传 [`TabPlacement::Created`]，会话恢复与工作区导入传
    /// [`TabPlacement::AfterActive`]。
    fn insert_tab(&mut self, entry: TabEntry, placement: TabPlacement) -> usize {
        let at = tab_insert_index(
            placement,
            self.display.nebula_new_tab_position,
            self.active_tab,
            self.tabs.len(),
        );
        self.tabs.insert(at, entry);
        self.active_tab = at;
        at
    }

    /// Spawn and activate a new tab (a single-pane layout) using the default shell.
    fn spawn_tab(&mut self) {
        self.spawn_tab_at(
            self.display.startup_directory().or_else(|| self.focused_cwd()),
            TabPlacement::Created,
        );
    }

    /// Spawn a default-shell tab at an already validated explicit directory,
    /// or inherit the caller-provided cwd. Keeping this as the only insertion
    /// path guarantees tree-created terminals behave exactly like Ctrl+Shift+T.
    fn spawn_tab_at(&mut self, cwd: Option<std::path::PathBuf>, placement: TabPlacement) {
        // The default-shell setting (`shell=<id>` in nebula_settings.txt) may
        // name a detected shell the PTY layer doesn't bootstrap itself (cmd,
        // pwsh, nushell, a WSL distro). `resolve_id` returns `None` for the two
        // PTY-integrated executors (powershell/bash) so those keep their prompt
        // injection; anything else spawns as an explicit override here.
        let override_shell = Self::default_shell_override(&self.config);
        let spawned = match override_shell {
            Some(shell) => self.spawn_pane_detached_with(cwd, self.display.size_info, Some(shell)),
            None => self.spawn_pane_detached(cwd, self.display.size_info),
        };
        if let Some(id) = spawned {
            self.insert_tab(
                TabEntry {
                    layout: Layout::Leaf(id),
                    active_pane: id,
                    has_bell: false,
                    custom_name: None,
                    custom_color: None,
                    launch: TabLaunch::Default,
                    doc: None,
                    image: None,
                    settings: false,
                },
                placement,
            );
            self.resize_active_layout();
            self.dirty = true;
            self.run_fastfetch_intro(id);
        }
    }

    /// The default-shell override for a plain new tab, or `None` to use the PTY
    /// layer's own default (which owns the powershell/bash prompt bootstrap).
    fn default_shell_override(
        config: &crate::config::ui_config::UiConfig,
    ) -> Option<nebula_terminal::tty::Shell> {
        let id = crate::display::nebula_settings_value("shell")
            .or_else(|| crate::display::nebula_settings_value("executor"))?;
        if let Some(profile) = config
            .profiles
            .iter()
            .find(|profile| profile.settings_id().as_deref() == Some(id.as_str()))
        {
            return Some(profile.shell());
        }
        crate::shell_detect::resolve_id(&id).map(|shell| shell.shell())
    }

    /// Open a new tab running the quick-launch profile at `index` (custom
    /// command instead of the default shell). The tab is pre-named after the
    /// profile so an `ssh host` entry reads as its destination, not "ssh".
    fn spawn_tab_profile(&mut self, index: usize) {
        let Some(profile) = self.config.profiles.get(index).cloned() else { return };
        self.spawn_tab_profile_value(profile, TabPlacement::Created);
    }

    fn spawn_tab_profile_value(&mut self, profile: Profile, placement: TabPlacement) {
        // Profile cwd wins when it exists; else inherit the focused pane's.
        let cwd = preferred_tab_cwd(
            profile.cwd.as_ref().filter(|p| p.is_dir()).cloned(),
            self.display.startup_directory(),
            self.focused_cwd(),
        );
        let shell = profile.shell();
        if let Some(id) = self.spawn_pane_detached_with(cwd, self.display.size_info, Some(shell)) {
            self.insert_tab(
                TabEntry {
                    layout: Layout::Leaf(id),
                    active_pane: id,
                    has_bell: false,
                    custom_name: Some(profile.name.clone()),
                    custom_color: None,
                    launch: TabLaunch::Profile(profile),
                    doc: None,
                    image: None,
                    settings: false,
                },
                placement,
            );
            self.resize_active_layout();
            self.dirty = true;
        }
    }

    /// Open a new tab running a detected shell (the new-tab dropdown). Like
    /// `spawn_tab_profile` but the spec is passed in rather than looked up in
    /// the config, and the cwd inherits the focused pane's.
    fn spawn_tab_shell(
        &mut self,
        name: String,
        shell: nebula_terminal::tty::Shell,
        placement: TabPlacement,
    ) {
        if let Some(id) = self.spawn_pane_detached_with(
            self.display.startup_directory().or_else(|| self.focused_cwd()),
            self.display.size_info,
            Some(shell.clone()),
        ) {
            self.insert_tab(
                TabEntry {
                    layout: Layout::Leaf(id),
                    active_pane: id,
                    has_bell: false,
                    custom_name: Some(name.clone()),
                    custom_color: None,
                    launch: TabLaunch::Shell { name, shell },
                    doc: None,
                    image: None,
                    settings: false,
                },
                placement,
            );
            self.resize_active_layout();
            self.dirty = true;
        }
    }

    /// Open a saved SSH destination inside the configured default shell.
    /// `nebula ssh` is typed into that shell's PTY so OpenSSH remains inside
    /// Nebula's ConPTY instead of becoming the pane's GUI-subsystem root.
    fn spawn_tab_ssh(&mut self, host: String, placement: TabPlacement) {
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
                    self.insert_tab(
                        TabEntry {
                            layout: Layout::Leaf(pane_id),
                            active_pane: pane_id,
                            has_bell: false,
                            custom_name: Some(host.clone()),
                            custom_color: None,
                            launch: TabLaunch::Ssh(host),
                            doc: None,
                            image: None,
                            settings: false,
                        },
                        placement,
                    );
                    self.resize_active_layout();
                    self.dirty = true;
                    return;
                },
                Err(err) => {
                    error!("创建直连 SSH Pane 失败: {err}");
                    let user_error = crate::ux::UserFacingError::new(
                        format!("SSH {host} 连接创建失败"),
                        "无法创建 SSH 会话，地址、认证方式或本机 SSH 配置可能无效。",
                        "检查主机地址和认证配置，右键编辑该主机后重试。",
                    )
                    .retry(crate::ux::RetryAction::Retry)
                    .details(err.to_string());
                    self.message_buffer.push(crate::message_bar::Message::user_error(&user_error));
                    self.dirty = true;
                    self.display.window.request_redraw();
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
            let default_shell = Self::default_shell_override(&self.config);
            if let Some(id) = self.spawn_pane_detached_with(
                self.focused_cwd(),
                self.display.size_info,
                default_shell,
            ) {
                self.insert_tab(
                    TabEntry {
                        layout: Layout::Leaf(id),
                        active_pane: id,
                        has_bell: false,
                        custom_name: Some(host.clone()),
                        custom_color: None,
                        launch: TabLaunch::Ssh(host),
                        doc: None,
                        image: None,
                        settings: false,
                    },
                    placement,
                );
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
        if let Some(index) =
            self.tabs.iter().position(|tab| tab.doc.as_ref().is_some_and(|doc| doc.path == path))
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
        self.insert_tab(
            TabEntry {
                layout: Layout::Leaf(DOC_PANE_ID),
                active_pane: DOC_PANE_ID,
                has_bell: false,
                custom_name: Some(label),
                custom_color: None,
                launch: TabLaunch::Document(doc.path.clone()),
                doc: Some(doc),
                image: None,
                settings: false,
            },
            TabPlacement::Created,
        );
        self.display.set_special_tab_active(true);
        self.display.set_settings_tab_active(false);
        self.dirty = true;
    }

    fn open_image_tab(&mut self, path: std::path::PathBuf) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.image.as_ref().is_some_and(|image| image.path == path))
        {
            if let Some(image) = self.tabs[index].image.as_mut() {
                image.reload();
            }
            self.select_tab(index);
            self.dirty = true;
            return;
        }
        let image = crate::display::image_viewer::ImageView::open(path.clone());
        self.insert_tab(
            TabEntry {
                layout: Layout::Leaf(DOC_PANE_ID),
                active_pane: DOC_PANE_ID,
                has_bell: false,
                custom_name: Some(format!(
                    "{} {}",
                    crate::display::side_panel::file_type_icon(&image.title),
                    image.title
                )),
                custom_color: None,
                launch: TabLaunch::Image(path),
                doc: None,
                image: Some(image),
                settings: false,
            },
            TabPlacement::Created,
        );
        self.display.set_special_tab_active(true);
        self.display.set_settings_tab_active(false);
        self.dirty = true;
    }

    /// Open Settings as a real singleton tab. Re-focusing the existing tab
    /// avoids duplicate preference surfaces and never starts a shell process.
    fn open_settings_tab(&mut self) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.settings) {
            self.select_tab(index);
            self.display.set_settings_tab_active(true);
            self.dirty = true;
            return;
        }

        self.insert_tab(
            TabEntry {
                layout: Layout::Leaf(DOC_PANE_ID),
                active_pane: DOC_PANE_ID,
                has_bell: false,
                custom_name: Some("\u{eb51} 设置".to_owned()),
                custom_color: None,
                launch: TabLaunch::Settings,
                doc: None,
                image: None,
                settings: true,
            },
            TabPlacement::Created,
        );
        self.display.set_settings_tab_active(true);
        self.sync_chrome_tabs();
        self.dirty = true;
    }

    /// Duplicate the selected tab next to itself. We copy the launch identity,
    /// current cwd, user name and color, but intentionally not the live grid or
    /// split tree: a duplicate is a fresh process/session, matching Windows
    /// Terminal and avoiding shared PTY ownership.
    fn duplicate_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let launch = tab.launch.clone();
        let custom_name = tab.custom_name.clone();
        let custom_color = tab.custom_color;
        self.select_tab(index);
        let before = self.tabs.len();

        match launch {
            TabLaunch::Default => self.spawn_tab_at(self.focused_cwd(), TabPlacement::Created),
            TabLaunch::Profile(profile) => {
                self.spawn_tab_profile_value(profile, TabPlacement::Created)
            },
            TabLaunch::Shell { name, shell } => {
                self.spawn_tab_shell(name, shell, TabPlacement::Created)
            },
            TabLaunch::Ssh(host) => self.spawn_tab_ssh(host, TabPlacement::Created),
            TabLaunch::Document(path) => {
                let doc = crate::display::markdown_view::DocView::open(path.clone());
                let label = format!("\u{eb1d} {}", doc.title);
                self.insert_tab(
                    TabEntry {
                        layout: Layout::Leaf(DOC_PANE_ID),
                        active_pane: DOC_PANE_ID,
                        has_bell: false,
                        custom_name: Some(label),
                        custom_color: None,
                        launch: TabLaunch::Document(path),
                        doc: Some(doc),
                        image: None,
                        settings: false,
                    },
                    TabPlacement::Created,
                );
                self.dirty = true;
            },
            TabLaunch::Image(path) => self.open_image_tab(path),
            TabLaunch::Settings => self.open_settings_tab(),
        }

        if self.tabs.len() > before {
            if let Some(duplicate) = self.tabs.get_mut(self.active_tab) {
                duplicate.custom_name = custom_name;
                duplicate.custom_color = custom_color;
            }
            self.sync_chrome_tabs();
            self.mark_session_dirty();
        }
    }

    /// Export the whole window (`None`) or a single tab (`Some(index)`) as a
    /// workspace file — the same schema the crash-restore session uses, so
    /// "打开工作区" and session restore share one rebuild path.
    fn export_workspace(&mut self, tab_index: Option<usize>) {
        let exportable =
            |tab: &&TabEntry| tab.doc.is_none() && tab.image.is_none() && !tab.settings;
        let tabs: Vec<_> = match tab_index {
            Some(index) => self
                .tabs
                .get(index)
                .filter(exportable)
                .map(|tab| self.tab_session(tab))
                .into_iter()
                .collect(),
            None => self.tabs.iter().filter(exportable).map(|tab| self.tab_session(tab)).collect(),
        };
        let Some(first) = tabs.first() else { return };

        // Single-tab exports name the file after the tab; whole-window exports
        // after the workspace. Path separators and friends must not leak into
        // the suggested file name.
        let stem = match tab_index {
            Some(_) => first
                .custom_name
                .clone()
                .or_else(|| {
                    first.cwd.rsplit(['/', '\\']).find(|part| !part.is_empty()).map(str::to_owned)
                })
                .unwrap_or_else(|| "tab".to_owned()),
            None => "workspace".to_owned(),
        };
        let stem: String = stem
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                    '-'
                } else {
                    c
                }
            })
            .collect();

        let Some(path) =
            self.display.save_workspace_dialog(&format!("{stem}.nebula-workspace.json"))
        else {
            return;
        };
        let session = session::Session::new(0, tabs);
        if let Err(err) = session::save_to(&path, &session) {
            let user_error = crate::ux::UserFacingError::new(
                "工作区导出失败",
                "无法写入所选的工作区文件。",
                "确认该位置可写(或换一个目录)后重试。",
            )
            .details(err.to_string());
            self.message_buffer.push(crate::message_bar::Message::user_error(&user_error));
            self.dirty = true;
        }
    }

    /// Pick a workspace file and append its tabs — launch identity and split
    /// trees included — to this window, then focus the first imported tab.
    fn import_workspace(&mut self) {
        let Some(path) = self.display.pick_workspace_dialog() else { return };
        let Some(session) = session::load_from(&path) else {
            let user_error = crate::ux::UserFacingError::new(
                "工作区导入失败",
                "所选文件不是可识别的 Nebula 工作区。",
                "确认选择的是导出生成的 .nebula-workspace.json 文件。",
            );
            self.message_buffer.push(crate::message_bar::Message::user_error(&user_error));
            self.dirty = true;
            return;
        };

        let before = self.tabs.len();
        for tab in &session.tabs {
            self.append_session_tab(tab);
        }
        let added = self.tabs.len() - before;
        if added > 0 {
            // append_session_tab leaves the last new tab active; land on the
            // first one, matching the saved workspace's reading order.
            self.select_tab(self.active_tab + 1 - added);
            self.sync_chrome_tabs();
            self.mark_session_dirty();
        } else {
            let user_error = crate::ux::UserFacingError::new(
                "工作区导入失败",
                "工作区文件里没有可恢复的标签页。",
                "该文件可能为空,或其中的会话都无法启动。",
            );
            self.message_buffer.push(crate::message_bar::Message::user_error(&user_error));
            self.dirty = true;
        }
    }

    /// Rebuild every saved tab of a restored session and refocus the tab that
    /// was active at close. The boot path only spawned a seed pane so the
    /// window exists; the real tabs — launch identity and split tree included
    /// — are appended here through the same path workspace import uses, then
    /// the seed is dismantled unless the CLI pinned its working directory.
    fn restore_session_tabs(&mut self, session: &session::Session, keep_seed: bool) {
        let first_new = self.tabs.len();
        for tab in &session.tabs {
            self.append_session_tab(tab);
        }
        let restored = self.tabs.len() - first_new;
        if restored == 0 {
            // Every spawn failed: keep the seed rather than an empty window.
            return;
        }
        // Guarded: a failed spawn above leaves fewer tabs than were saved.
        if session.active_tab < restored {
            self.active_tab = first_new + session.active_tab;
        }
        if !keep_seed {
            // The seed is a plain single-pane tab at index 0 that never saw
            // user state — dismantle it in place instead of going through
            // close_tab's confirmation and focus logic.
            let seed = self.tabs.remove(0);
            let mut ids = Vec::new();
            seed.layout.leaves(&mut ids);
            for id in ids {
                if let Some(i) = self.pane_index(id) {
                    let pane = self.panes.remove(i);
                    let _ = pane.notifier.0.send(Msg::Shutdown);
                }
            }
            self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.resize_active_layout();
        self.sync_chrome_tabs();
        self.dirty = true;
    }

    /// Append one saved tab: spawn its first pane per launch identity, rebuild
    /// its split tree, and re-apply name/color/focus. Returns `false` when the
    /// first pane could not spawn — the tab is skipped so the session's
    /// remaining tabs still restore.
    fn append_session_tab(&mut self, tab: &session::TabSession) -> bool {
        let before = self.tabs.len();
        let launch = tab.launch.as_ref().unwrap_or(&session::LaunchSession::Default);
        match launch {
            session::LaunchSession::Default => {
                self.append_default_tab(tab);
            },
            session::LaunchSession::Shell { name, program, args } => {
                let shell = nebula_terminal::tty::Shell::new(program.clone(), args.clone());
                self.spawn_tab_shell(name.clone(), shell, TabPlacement::AfterActive);
            },
            session::LaunchSession::Profile { name, command, args, cwd, shell_id } => {
                self.spawn_tab_profile_value(
                    crate::config::ui_config::Profile {
                        name: name.clone(),
                        command: command.clone(),
                        args: args.clone(),
                        cwd: cwd.as_ref().map(std::path::PathBuf::from),
                        shell_id: shell_id.clone(),
                        terminal_profile_id: None,
                    },
                    TabPlacement::AfterActive,
                );
            },
            session::LaunchSession::Ssh { host } => {
                self.spawn_tab_ssh(host.clone(), TabPlacement::AfterActive)
            },
        }
        if self.tabs.len() == before {
            // Cross-platform degradation: a workspace made on another OS may
            // name a program this machine lacks (wsl.exe on Linux, a distro
            // shell on Windows). The tab must survive as a default shell in
            // its saved directory rather than silently vanish; the SSH path
            // reports its own failure and gets no fallback tab.
            let is_command = matches!(
                launch,
                session::LaunchSession::Shell { .. } | session::LaunchSession::Profile { .. }
            );
            if !(is_command && self.append_default_tab(tab)) {
                return false;
            }
        }

        // The new tab is active now. Grow the saved split tree around its
        // first pane, then re-apply the saved presentation.
        let first_pane = self.tabs[self.active_tab].active_pane;
        if let Some(saved) = tab.layout.as_ref().filter(|layout| layout.pane_count() > 1) {
            let mut seed = Some(first_pane);
            if let Some(built) = self.rebuild_layout(saved, &mut seed) {
                let entry = &mut self.tabs[self.active_tab];
                entry.layout = built;
                let mut leaves = Vec::new();
                entry.layout.leaves(&mut leaves);
                entry.active_pane =
                    leaves.get(tab.active_pane).or(leaves.first()).copied().unwrap_or(first_pane);
                self.resize_active_layout();
            }
        }
        let entry = &mut self.tabs[self.active_tab];
        if tab.custom_name.is_some() {
            // `None` must not clobber the launch-derived label (SSH host,
            // shell name) that spawn_tab_* already set.
            entry.custom_name = tab.custom_name.clone();
        }
        entry.custom_color = tab.color;
        true
    }

    /// Spawn a default-shell tab in a saved tab's first-leaf directory —
    /// both the `Default` launch path and the cross-platform fallback when a
    /// saved program does not exist on this machine.
    fn append_default_tab(&mut self, tab: &session::TabSession) -> bool {
        // The first pane adopts the tree's first leaf, so it must start in
        // that leaf's directory, not the focused pane's.
        let saved_cwd = tab.layout.as_ref().map(|layout| layout.first_cwd()).unwrap_or(&tab.cwd);
        let cwd = session::valid_dir(saved_cwd).or_else(|| self.display.startup_directory());
        let Some(id) = self.spawn_pane_detached(cwd, self.display.size_info) else {
            return false;
        };
        // 恢复不读新标签插入策略：保存的顺序才是权威，逐个追加在活动标签
        // 之后即可复现它。
        self.insert_tab(
            TabEntry {
                layout: Layout::Leaf(id),
                active_pane: id,
                has_bell: false,
                custom_name: None,
                custom_color: None,
                launch: TabLaunch::Default,
                doc: None,
                image: None,
                settings: false,
            },
            TabPlacement::AfterActive,
        );
        self.run_fastfetch_intro(id);
        true
    }

    /// Materialize a saved split tree. `seed` is the already-spawned first
    /// pane, adopted by the depth-first first leaf; every other leaf spawns a
    /// default shell in its saved cwd — matching live behaviour, where only a
    /// tab's first pane carries the launch identity. A leaf whose spawn fails
    /// collapses its parent to the surviving side, so a partially failed
    /// restore still yields a working tab.
    fn rebuild_layout(
        &mut self,
        node: &session::LayoutSession,
        seed: &mut Option<PaneId>,
    ) -> Option<Layout> {
        match node {
            session::LayoutSession::Pane { cwd } => {
                if let Some(id) = seed.take() {
                    return Some(Layout::Leaf(id));
                }
                let cwd = session::valid_dir(cwd).or_else(|| self.display.startup_directory());
                self.spawn_pane_detached(cwd, self.display.size_info).map(Layout::Leaf)
            },
            session::LayoutSession::Split { axis, ratio_permille, first, second } => {
                let first = self.rebuild_layout(first, seed);
                let second = self.rebuild_layout(second, seed);
                match (first, second) {
                    (Some(first), Some(second)) => Some(Layout::Split {
                        direction: match axis {
                            session::SplitAxis::LeftRight => {
                                crate::display::SplitDirection::LeftRight
                            },
                            session::SplitAxis::TopBottom => {
                                crate::display::SplitDirection::TopBottom
                            },
                        },
                        ratio: (f32::from(*ratio_permille) / 1000.0).clamp(0.05, 0.95),
                        preview_ratio: None,
                        dragging: false,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (first, second) => first.or(second),
                }
            },
        }
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
        session::save_final(&mut self.session_snapshot());
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
        let active_tab = self
            .tabs
            .iter()
            .take(self.active_tab)
            .filter(|tab| tab.doc.is_none() && tab.image.is_none() && !tab.settings)
            .count();
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tab.doc.is_none() && tab.image.is_none() && !tab.settings)
            .map(|tab| self.tab_session(tab))
            .collect();
        let mut session = session::Session::new(active_tab.min(tabs.len().saturating_sub(1)), tabs);
        let maximized = self.display.window.is_maximized();
        session.window = Some(if maximized {
            // Maximized: the live inner size is the whole monitor — remember
            // the last known NORMAL size instead.
            session::WindowState {
                width: self.windowed_size.width,
                height: self.windowed_size.height,
                maximized,
            }
        } else {
            // Normal state: take the current size straight from the window.
            // The cached bookkeeping once picked up a physical-domain value,
            // and a restored window then ballooned by the DPI factor on
            // every relaunch.
            let logical: LogicalSize<u32> =
                self.display.window.inner_size().to_logical(self.display.window.scale_factor);
            session::WindowState { width: logical.width, height: logical.height, maximized }
        });
        session
    }

    /// One tab as a persistable record: focused-pane cwd, launch identity and
    /// the full split tree. Shared by the session autosave and the workspace
    /// export so both always describe tabs identically.
    fn tab_session(&self, tab: &TabEntry) -> session::TabSession {
        let pane_cwd = |id: PaneId| {
            self.pane(id).map(|p| p.nebula_state.cwd.trim().to_owned()).unwrap_or_default()
        };
        let mut leaves = Vec::new();
        tab.layout.leaves(&mut leaves);
        session::TabSession {
            cwd: pane_cwd(tab.active_pane),
            custom_name: tab.custom_name.clone(),
            color: tab.custom_color,
            launch: Some(Self::launch_session(&tab.launch)),
            layout: Some(Self::layout_session(&tab.layout, &pane_cwd)),
            active_pane: leaves.iter().position(|id| *id == tab.active_pane).unwrap_or(0),
        }
    }

    /// The persistable subset of a tab's launch identity. Document/settings
    /// tabs are filtered out before this is called; mapping them to `Default`
    /// keeps the function total without giving them a session meaning.
    fn launch_session(launch: &TabLaunch) -> session::LaunchSession {
        match launch {
            TabLaunch::Default
            | TabLaunch::Document(_)
            | TabLaunch::Image(_)
            | TabLaunch::Settings => session::LaunchSession::Default,
            TabLaunch::Shell { name, shell } => session::LaunchSession::Shell {
                name: name.clone(),
                program: shell.program().to_owned(),
                args: shell.args().to_vec(),
            },
            TabLaunch::Profile(profile) => session::LaunchSession::Profile {
                name: profile.name.clone(),
                command: profile.command.clone(),
                args: profile.args.clone(),
                cwd: profile.cwd.as_ref().map(|path| path.to_string_lossy().into_owned()),
                shell_id: profile.shell_id.clone(),
            },
            TabLaunch::Ssh(host) => session::LaunchSession::Ssh { host: host.clone() },
        }
    }

    /// Serialize a layout tree, resolving each leaf to its pane's cwd.
    fn layout_session(
        layout: &Layout,
        pane_cwd: &impl Fn(PaneId) -> String,
    ) -> session::LayoutSession {
        match layout {
            Layout::Leaf(id) => session::LayoutSession::Pane { cwd: pane_cwd(*id) },
            Layout::Split { direction, ratio, first, second, .. } => {
                session::LayoutSession::Split {
                    axis: match direction {
                        crate::display::SplitDirection::LeftRight => session::SplitAxis::LeftRight,
                        crate::display::SplitDirection::TopBottom => session::SplitAxis::TopBottom,
                    },
                    ratio_permille: (ratio.clamp(0.0, 1.0) * 1000.0).round() as u16,
                    first: Box::new(Self::layout_session(first, pane_cwd)),
                    second: Box::new(Self::layout_session(second, pane_cwd)),
                }
            },
        }
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

        if source >= self.tabs.len()
            || source == self.active_tab
            || self.tabs.len() < 2
            || self.tabs[source].doc.is_some()
            || self.tabs[source].settings
            || self.tabs[self.active_tab].doc.is_some()
            || self.tabs[self.active_tab].settings
        {
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
        let (first, second) =
            if src_first { (src_entry.layout, old) } else { (old, src_entry.layout) };
        entry.layout = Layout::Split {
            direction,
            ratio: 0.5,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        };
        // Focus follows the pane that accepted the dock operation.
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
        match Self::create_pane(
            &size_info,
            window_id,
            &self.config,
            pty_config,
            &self.proxy,
            pane_id,
        ) {
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

    /// 把后台 SSH runtime 的阶段上报转给 display，顺带补上这个 pane 的目标
    /// 地址——事件本身只带 pane 身份，地址在 pane 上。
    pub fn ssh_connect_stage(&mut self, pane: PaneId, stage: crate::ssh_session::SshStage) {
        let destination = self
            .pane_index(pane)
            .and_then(|index| self.panes[index].ssh_destination.clone())
            .unwrap_or_default();
        self.display.ssh_connect_stage(pane, destination, stage);
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

    /// The focused pane's cwd mapped through `\\wsl$` when the pane belongs
    /// to a WSL tab reporting a Linux path — so the directory tree can follow
    /// a WSL shell. Only the drawer uses this: spawning terminals in a UNC
    /// directory has its own semantics and is deliberately not affected.
    fn focused_wsl_cwd(&self) -> Option<std::path::PathBuf> {
        let raw = self.pane(self.focused_pane_id()).map(|p| p.nebula_state.cwd.clone())?;
        let raw = raw.trim();
        if !raw.starts_with('/') {
            return None;
        }
        let tab = self.tabs.iter().find(|tab| {
            let mut ids = Vec::new();
            tab.layout.leaves(&mut ids);
            ids.contains(&self.focused_pane_id())
        })?;
        let TabLaunch::Shell { shell, .. } = &tab.launch else { return None };
        let program = std::path::Path::new(shell.program())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if program != "wsl" {
            return None;
        }
        // `wsl -d <distro>` / `--distribution <distro>`; a bare `wsl` launch
        // uses the default distro whose name we cannot know — skip mapping
        // and let the tree keep its last known root.
        let args = shell.args();
        let distro = args
            .iter()
            .position(|arg| arg == "-d" || arg == "--distribution")
            .and_then(|index| args.get(index + 1))?;
        let unc = format!("\\\\wsl$\\{distro}{}", raw.replace('/', "\\"));
        let path = std::path::PathBuf::from(unc);
        path.is_dir().then_some(path)
    }

    /// Name of the first busy program under any of `pane_ids`, for the close
    /// confirm modal — or `None` when every pane is safe to kill.
    ///
    /// 2026-07-27 用户反馈"node.exe 仍在运行"：`busy_child` 只认得进程快照里
    /// 的 exe 名，而 Claude Code / codex 这类 CLI 是被 node 托管的，快照里就
    /// 只剩宿主解释器。Nebula 另有一份更准的身份——pane 的 `running_program`
    /// （AI hook 直报 `claude`，或 OSC 133 从命令行解析），侧栏图标画的就是
    /// 它。所以：由 `busy_child` 判定"忙不忙"，由 `running_program` 决定"叫
    /// 什么"；后者缺席（无 shell 集成）时退回擦掉 `.exe` 的进程名。
    fn busy_process_in(&self, pane_ids: &[PaneId]) -> Option<String> {
        pane_ids.iter().filter_map(|id| self.pane(*id)).find_map(|pane| {
            let exe = crate::process_tree::busy_child(pane.shell_pid)?;
            let known = pane.nebula_state.running_program.as_deref();
            Some(known.map_or_else(
                || crate::process_tree::display_name(&exe),
                |program| program.to_owned(),
            ))
        })
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
                let program =
                    self.pane(pane_id).and_then(|p| p.nebula_state.running_program.clone());
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
            crate::ai_hook::AiHookKind::TurnDone | crate::ai_hook::AiHookKind::NeedsAttention => {
                // codex 的 notify 只有"回合完成"一种事件：弹出交互式提问时
                // 它发的也是 turn-complete，事件流分不出"说完了"和"在等你
                // 回答"。回合结束的瞬间看一眼屏幕尾部——还挂着选择框或确认
                // 提示，就按「等你批准」处理（蓝点升级成手掌）。
                let screen_asks = ev.kind == crate::ai_hook::AiHookKind::TurnDone && {
                    let term = self.panes[idx].terminal.lock();
                    let lines = term.screen_lines();
                    let take = lines.min(15);
                    let start = Point::new(Line((lines - take) as i32), Column(0));
                    let end = Point::new(
                        Line(lines as i32 - 1),
                        Column(term.columns().saturating_sub(1)),
                    );
                    crate::ai_hook::tail_looks_like_question(&term.bounds_to_string(start, end))
                };
                {
                    let state = &mut self.panes[idx].nebula_state;
                    state.awaiting_input = true;
                    state.finished_unseen = true;
                    // 「等你批准」是比「回合完成」更强的状态：它不是通知你
                    // 结果，是挡在半路要你点头。徽章上分成手掌与圆点两种
                    // 墨迹，此前两者共用一个点，界面上根本分不出来。
                    if ev.kind == crate::ai_hook::AiHookKind::NeedsAttention || screen_asks {
                        state.needs_attention = true;
                    }
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

    /// 后台修复请求（spec 001）的结果落地：pane 归属本窗口即认领（返回
    /// true，AiHook 同款路由契约）。写入前校验 seq——条子已被用户撤掉、或
    /// 新失败已顶掉旧请求时，迟到的响应直接丢弃。
    pub fn handle_ai_fix(
        &mut self,
        pane_id: u64,
        seq: u64,
        fix: &Option<crate::ai_assistant::AiFix>,
    ) -> bool {
        let Some(idx) = self.pane_index(pane_id) else { return false };
        let state = &mut self.panes[idx].nebula_state;
        if state.ai_fix.as_ref().is_some_and(|current| current.seq() == seq) {
            state.ai_fix =
                fix.clone().map(|fix| crate::ai_assistant::AiFixState::Ready { seq, fix });
            self.dirty = true;
            self.display.window.request_redraw();
        }
        true
    }

    /// 同步线程收尾（spec 003）：消息进 message bar；拉到新历史时热加载
    /// （ghost 补全立即吃到另一台机器的命令）。settings 变化不用管——
    /// mtime 监视在下一帧自动 reload。
    pub fn handle_sync_done(&mut self, message: &str, error: bool, history_changed: bool) {
        // 设置页的状态行（按钮下方）是第一现场；message_bar 兜底通知
        // 没开设置页的窗口。
        self.display.sync_action_done(message, error);
        let ty = if error {
            crate::message_bar::MessageType::Error
        } else {
            crate::message_bar::MessageType::Warning
        };
        self.message_buffer.push(crate::message_bar::Message::new(format!("同步：{message}"), ty));
        if history_changed {
            self.display.reload_nebula_history();
        }
        self.dirty = true;
        self.display.window.request_redraw();
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
        self.display.set_special_tab_active(
            self.tabs[index].doc.is_some()
                || self.tabs[index].image.is_some()
                || self.tabs[index].settings,
        );
        self.display.set_settings_tab_active(self.tabs[index].settings);
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
            // 连接中的 tab 被关掉：丢弃它的卡片状态，否则 HashMap 会随
            // 会话累积，而那个 pane 再也不会回来。
            self.display.forget_ssh_connect(id);
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
        let special = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.doc.is_some() || tab.image.is_some() || tab.settings);
        self.display.set_special_tab_active(special);
        self.display.set_settings_tab_active(
            self.tabs.get(self.active_tab).is_some_and(|tab| tab.settings),
        );
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

            let font =
                self.display.effective_font(&self.config.font).with_size(self.display.font_size);
            self.display.pending_update.set_font(font);
        }

        // Keep the decoration override in sync without suppressing winit's
        // OS theme events while Nebula's automatic theme mode is enabled.
        self.display.update_window_theme_override(self.config.window.theme());

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
        // 模糊开关的权威在 nebula_settings.txt（设置面板写的就是它），
        // 基础配置侧的 `window.blur` 只是同名字段，跟它没有同步。
        self.display.window.set_blur(self.display.nebula_blur);

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
        // The drawer follows the focused pane: its VIEW routes to SFTP only
        // while an SSH pane with the matching destination is focused, and the
        // directory tree follows the focused pane's cwd (throttled inside).
        let focused_ssh =
            self.pane(self.focused_pane_id()).and_then(|pane| pane.ssh_destination.clone());
        self.display.route_side_panel(focused_ssh.as_deref());
        let panel_cwd = self.focused_cwd().or_else(|| self.focused_wsl_cwd());
        // 命令面板的「工作目录」组也认这个值（WSL 路径已映射成 `\\wsl$\…`，
        // 复制出去和丢给资源管理器都能用）。抽屉是节流的，这里不能顺手复用
        // 它的内部状态——面板要的是**当前**目录，不是抽屉上次同步到的那个。
        self.display.nebula_focused_cwd = panel_cwd.clone();
        self.display.side_panel_sync(panel_cwd);

        // Chrome clock: 1 Hz idle, 8 fps for finite UI transitions, and
        // display-rate only while a task spinner is running. Re-arm whenever
        // the cadence class changes.
        let clock_timer = TimerId::new(Topic::NebulaClock, self.display.window.id());
        let interval = chrome_clock_interval(
            self.display.any_tab_running()
                || self.display.ssh_test_running()
                || self.display.any_tab_flashing(),
            self.display.chrome_editor_active(),
            self.display.chrome_animating(),
        );
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
        // 助手建议条（spec 001）跟随焦点 pane：绘制层只认 Display 自己的
        // 快照字段（SSH 撤销条同款模式），此处每帧同步一次。
        self.display.nebula_ai_fix_bar =
            self.pane_index(focused).and_then(|idx| self.panes[idx].nebula_state.ai_fix.clone());

        // Settings is rendered inside the normal tab content card; it is not
        // a modal and therefore keeps the tab/sidebar chrome fully usable.
        if self.tabs.get(self.active_tab).is_some_and(|tab| tab.settings) {
            self.display.begin_pane_frame(&self.config);
            self.display.draw_settings_frame(scheduler);
            return;
        }

        // Document-viewer tab: no pane, no grid. Draw the doc into the tab's
        // content rect; `present_frame` inside lays the normal chrome on top.
        if let Some(image) = self.tabs.get(self.active_tab).and_then(|tab| tab.image.clone()) {
            let view = pane_rects.first().map(|(_, view)| *view).unwrap_or(self.display.size_info);
            self.display.begin_pane_frame(&self.config);
            self.display.draw_image_frame(&image, view, scheduler);
            return;
        }

        if let Some(doc) = self.tabs.get_mut(self.active_tab).and_then(|tab| tab.doc.as_mut()) {
            let view = pane_rects.first().map(|(_, view)| *view).unwrap_or(self.display.size_info);
            self.display.begin_pane_frame(&self.config);
            self.display.draw_doc_frame(doc, view, scheduler);
            return;
        }

        // 连接卡片只画在聚焦 pane 里，display 侧只有几何、没有身份。
        self.display.set_focused_pane(focused);

        // 焦点 pane 变了：blink 定时器还按旧终端的样式在跑（或没跑）。给
        // 新聚焦终端补发一次 CursorBlinkingChange，让它按自己的样式起表。
        if self.blink_focus_pane != Some(focused) {
            self.blink_focus_pane = Some(focused);
            if let Some(idx) = self.pane_index(focused) {
                let pane = &self.panes[idx];
                EventProxy::new_tab(self.proxy.clone(), pane.window_route.clone(), pane.id)
                    .send_event(TerminalEvent::CursorBlinkingChange.into());
            }
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
            // The whole-window clear must not be tied to pane_rects[0]: a
            // layout leaf whose pane is gone (or a doc sentinel) is skipped
            // below, and skipping the clearing pane would leave every later
            // frame compositing over stale buffer contents (ghost frames).
            let mut cleared = false;
            // Pane focus AND window focus together decide the cursor's
            // focused look — a focused pane in an unfocused window must show
            // the hollow unfocused cursor, exactly like the single-pane path.
            let window_focused = self.display.window.has_focus();
            for (id, view) in pane_rects.iter() {
                let Some(idx) = self.pane_index(*id) else { continue };
                let is_focused = *id == focused;
                if !is_focused {
                    dim_rects.push((
                        view.padding_x(),
                        view.padding_y(),
                        // Split views use asymmetric padding: the sidebar is
                        // included on the left while the right keeps only the
                        // normal content margin. Using `2 * padding_x` here
                        // dropped the entire asymmetric difference from the
                        // dim veil, leaving a bright uncovered strip.
                        view.width() - view.padding_x() - view.padding_right(),
                        view.height() - view.padding_y() - view.padding_bottom(),
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
                    is_focused && window_focused,
                    !cleared,
                );
                cleared = true;
            }
            if !cleared {
                crate::display::nebula_debug_log(format!(
                    "render_clear_missing active_tab={} layout_panes={} live_panes={} focused={focused}",
                    self.active_tab,
                    pane_rects.len(),
                    self.panes.len(),
                ));
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
        let special = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.doc.is_some() || tab.image.is_some() || tab.settings);
        self.display.set_special_tab_active(special);
        self.display.set_settings_tab_active(
            self.tabs.get(self.active_tab).is_some_and(|tab| tab.settings),
        );
        // The visible tab's activity is seen by definition — consume its
        // flag before it can render (dots are for background tabs only).
        if let Some(id) = self.tabs.get(self.active_tab).map(|t| t.active_pane) {
            if let Some(i) = self.pane_index(id) {
                self.panes[i].nebula_state.finished_unseen = false;
                self.panes[i].nebula_state.needs_attention = false;
                self.panes[i].nebula_state.failed_unseen = false;
            }
        }

        let mut labels = Vec::with_capacity(self.tabs.len());
        let mut colors = Vec::with_capacity(self.tabs.len());
        let mut dots = Vec::with_capacity(self.tabs.len());
        let mut running = Vec::with_capacity(self.tabs.len());
        let mut attention = Vec::with_capacity(self.tabs.len());
        let mut failed = Vec::with_capacity(self.tabs.len());
        let mut flashing = Vec::with_capacity(self.tabs.len());
        let mut logos = Vec::with_capacity(self.tabs.len());
        let mut shells = Vec::with_capacity(self.tabs.len());
        // 静默行右侧的 shell 短标；Default 启动的 tab 用当前默认 shell 的。
        let default_tag = self.display.default_shell_tag();
        let ui_language = self.display.ui_language();
        for tab in &self.tabs {
            let pane = self.pane(tab.active_pane);
            let state = pane.map(|p| &p.nebula_state);
            // Use custom name if set, otherwise derive from cwd/title
            let mut label = if tab.settings {
                format!("\u{eb51} {}", ui_language.pick("设置", "Settings"))
            } else if let Some(custom) = &tab.custom_name {
                custom.clone()
            } else {
                pane.map(Self::chrome_tab_label).unwrap_or_default()
            };
            // Program icon (Nerd Font) in front of the label while a command
            // runs — the sidebar shows WHAT each tab is busy with. AI clients
            // with a real brand logo skip the glyph: the
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
            colors.push(tab.custom_color);
            shells.push(match &tab.launch {
                TabLaunch::Default => default_tag.clone(),
                TabLaunch::Shell { name, .. } => crate::shell_detect::shell_short_tag(name),
                // SSH 行的身份是目标主机（标签本身就写着），短标只说环境。
                TabLaunch::Ssh(_) => "ssh".to_owned(),
                TabLaunch::Profile(_)
                | TabLaunch::Document(_)
                | TabLaunch::Image(_)
                | TabLaunch::Settings => String::new(),
            });
            // Unseen-result dot: bell in a background tab, a tracked command
            // that finished unseen, or a tracked program parked at "waiting
            // for input" (claude between turns). The ring collapsing into a
            // dot IS the "turn finished, your move" signal — also on the
            // visible tab, where a merely-paused ring still read as busy.
            dots.push(
                tab.has_bell
                    || state.is_some_and(|s| {
                        s.finished_unseen || (s.command_started.is_some() && s.awaiting_input)
                    }),
            );
            // Spinner only while the command actually works; once it rang BEL
            // and waits for input the dot above takes over.
            running.push(state.is_some_and(|s| s.command_started.is_some() && !s.awaiting_input));
            attention.push(state.is_some_and(|s| s.needs_attention));
            failed.push(state.is_some_and(|s| s.failed_unseen));
            // 对勾只在成功收尾后的一小段里亮着，随后落回圆点。
            flashing.push(state.is_some_and(|s| {
                s.finished_at.is_some_and(|at| at.elapsed() < crate::display::BADGE_FLASH)
            }));
        }
        let active = self.active_tab.min(labels.len().saturating_sub(1));
        // displayed == storage index always holds now, so the bar is reorderable.
        self.display.set_chrome_tabs(
            labels, colors, dots, running, attention, failed, flashing, logos, shells, active, true,
        );
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
            .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| ".".to_owned())
    }

    /// Commit the final DPI and physical size held by the native move tracker.
    /// Applying the factor first keeps the logical windowed bounds correct and
    /// collapses the cross-monitor work into one display update.
    pub fn apply_pending_native_transition(&mut self) {
        if self.display.window.native_live_move() {
            return;
        }

        if let Some(scale_factor) = self.display.window.take_pending_scale_factor() {
            let start = Instant::now();
            self.display.apply_scale_factor_change(scale_factor, &self.config);
            crate::display::nebula_debug_log(format!(
                "winmove pending_scale {scale_factor} applied in {:?}",
                start.elapsed()
            ));
            self.dirty = true;
        }

        if let Some(size) = self.display.window.take_pending_inner_size() {
            crate::display::nebula_debug_log(format!(
                "winmove pending_size {}x{} applied",
                size.width, size.height
            ));
            if self.display.window.allows_drag_resize() {
                self.windowed_size = size.to_logical(self.display.window.scale_factor);
            }
            self.display.pending_update.set_dimensions(size);
            self.dirty = true;
        }
    }

    /// Process events for this terminal window.
    pub fn handle_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event_proxy: &EventLoopProxy<Event>,
        clipboard: &mut Clipboard,
        scheduler: &mut Scheduler,
        event: WinitEvent<Event>,
    ) {
        // `Window::theme()` can retain a stale manual override. The event-loop
        // query is system-wide and lets automatic mode react immediately.
        self.display.sync_system_theme(event_loop.system_theme());

        match event {
            WinitEvent::AboutToWait
            | WinitEvent::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                // Skip further event handling with no staged updates.
                // A native DPI transition can stage a Display update without
                // adding a synthetic winit event, so the pending flag is part
                // of this fast-path decision.
                if self.event_queue.is_empty() && !self.display.pending_update.dirty {
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
                // 打字即表态：人已经在这个 pane 上动手了，徽章再催就是噪声。
                self.panes[i].nebula_state.needs_attention = false;
            }
        }

        // In a split, a terminal-content mouse press moves keyboard focus to
        // the clicked pane. Right-click paste and middle-click selection paste
        // must target the pane under the pointer as well; otherwise they use
        // the previous keyboard focus and write into a neighbouring terminal.
        // Resolve focus from the click position before routing this batch so the
        // click lands on the pane the user aimed at.
        if self.display.nebula_confirm.is_none() && !matches!(self.active_layout(), Layout::Leaf(_))
        {
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
                    event: WindowEvent::CursorMoved { position, .. },
                    ..
                } => Some((position.x as f32, position.y as f32)),
                _ => None,
            });
            let clicked = self.event_queue.iter().any(|e| {
                matches!(
                    e,
                    WinitEvent::WindowEvent {
                        event: WindowEvent::MouseInput { state: ElementState::Pressed, button, .. },
                        ..
                    } if pane_focus_button(button)
                )
            });
            // A terminal mouse press always refocuses the clicked pane;
            // focus-follows-mouse also refocuses on plain pointer motion.
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
        // Multi-line paste confirmation is a transaction bound to the pane
        // that opened it. Route both keyboard Enter and a modal-button click
        // to that pane even when the centered button lies over another split.
        let normal_focus = self.focused_pane_id();
        let focused_id =
            routed_input_pane(self.display.nebula_confirm.as_ref(), normal_focus, |pane_id| {
                self.pane_index(pane_id).is_some()
            });
        // A doc tab has no pane: its events run against `doc_pane` below so
        // chrome interaction (tab switching, closing, the sidebar) keeps
        // working; anything typed lands in the sink notifier.
        let special_tab = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.doc.is_some() || tab.image.is_some() || tab.settings);
        let focused = match self.pane_index(focused_id) {
            Some(index) => Some(index),
            None if special_tab => None,
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

        let old_is_searching =
            focused.is_some_and(|index| self.panes[index].search_state.history_index.is_some());

        let target_of = |event: &WinitEvent<Event>| match event {
            WinitEvent::UserEvent(event) => event.terminal_tab_id().unwrap_or(focused_id),
            _ => focused_id,
        };
        // Consume the batch in order, one processor per run of consecutive
        // events sharing a target pane.
        let mut events = mem::take(&mut self.event_queue).into_iter().peekable();
        while let Some(event) = events.next() {
            let target_id = target_of(&event);
            let (pane, doc, image) = match self.pane_index(target_id) {
                Some(pane_idx) => (&mut self.panes[pane_idx], None, None),
                None if target_id == DOC_PANE_ID && special_tab => {
                    let tab = &mut self.tabs[self.active_tab];
                    (&mut self.doc_pane, tab.doc.as_mut(), tab.image.as_mut())
                },
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
                pane_id: pane.id,
                cursor_blink_timed_out: &mut self.cursor_blink_timed_out,
                prev_bell_cmd: &mut self.prev_bell_cmd,
                message_buffer: &mut self.message_buffer,
                inline_search_state: &mut pane.inline_search_state,
                search_state: &mut pane.search_state,
                nebula_state: &mut pane.nebula_state,
                ssh_destination: pane.ssh_destination.as_deref(),
                doc,
                image,
                modifiers: &mut self.modifiers,
                notifier: &mut pane.notifier,
                display: &mut self.display,
                windowed_size: &mut self.windowed_size,
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

        if self.display.pending_update.terminal_colors_dirty() {
            // 主题切换必须覆盖所有 tab、分屏和文档占位终端。这里尚未取得焦点
            // terminal 的锁，可逐个清理 OSC 覆盖而不产生重复加锁死锁。
            for pane in &self.panes {
                pane.terminal.lock().reset_dynamic_colors();
            }
            self.doc_pane.terminal.lock().reset_dynamic_colors();
            self.dirty = true;
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
            let update_start = Instant::now();
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
            crate::display::nebula_debug_log(format!(
                "winmove display_update in {:?}",
                update_start.elapsed()
            ));
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
                    let timer = TimerId::new(Topic::NebulaResizeSettle, self.display.window.id());
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
            let visual_point =
                self.mouse.point(&self.display.pane_view(), terminal.grid().display_offset());
            let pane = match focused {
                Some(index) => &self.panes[index],
                None => &self.doc_pane,
            };
            let hint_point = pane
                .nebula_state
                .terminal_math_source_point(
                    visual_point,
                    self.mouse.cell_side,
                    terminal.grid().display_offset(),
                )
                .0;
            self.dirty |= self.display.update_highlighted_hints(
                &terminal,
                &self.config,
                &self.mouse,
                hint_point,
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
        // the 1 Hz autosave instead — which is also why this one (and only
        // this one) stamps `clean_exit`: reaching Drop IS the definition of
        // a clean exit.
        if !self.session_exempt {
            session::save_final(&mut self.session_snapshot());
        }

        // Shutdown every pane's PTY.
        for pane in &self.panes {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
    }
}

#[cfg(test)]
mod startup_shell_tests {
    use nebula_terminal::tty::Shell;

    use super::{
        TabPlacement, chrome_clock_interval, preferred_initial_cwd, preferred_tab_cwd,
        routed_input_pane, select_initial_shell, tab_insert_index, valid_new_tab_directory,
    };
    use crate::display::NewTabPosition;

    fn shell(program: &str) -> Shell {
        Shell::new(program.to_owned(), Vec::new())
    }

    #[test]
    fn created_tabs_land_after_the_active_tab_by_default() {
        let at = NewTabPosition::AfterCurrent;
        assert_eq!(tab_insert_index(TabPlacement::Created, at, 0, 3), 1);
        assert_eq!(tab_insert_index(TabPlacement::Created, at, 1, 3), 2);
        assert_eq!(tab_insert_index(TabPlacement::Created, at, 2, 3), 3);
    }

    #[test]
    fn created_tabs_land_at_the_end_when_the_user_chose_end() {
        let end = NewTabPosition::End;
        assert_eq!(tab_insert_index(TabPlacement::Created, end, 0, 3), 3);
        assert_eq!(tab_insert_index(TabPlacement::Created, end, 1, 3), 3);
        assert_eq!(tab_insert_index(TabPlacement::Created, end, 2, 3), 3);
    }

    #[test]
    fn restored_tabs_ignore_the_creation_strategy() {
        // 会话恢复与工作区导入保持既有的「当前标签之后」行为，即使用户把
        // 新标签插入策略选成了列表末尾——否则恢复会重排保存的顺序。
        assert_eq!(tab_insert_index(TabPlacement::AfterActive, NewTabPosition::End, 1, 4), 2);
        assert_eq!(
            tab_insert_index(TabPlacement::AfterActive, NewTabPosition::AfterCurrent, 1, 4),
            2
        );
    }

    #[test]
    fn insertion_never_runs_past_the_end_of_the_tab_list() {
        // 活动下标可能短暂领先于实际长度（关闭标签后的过渡态）；落点必须
        // 仍是合法的 Vec::insert 位置。
        assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::AfterCurrent, 9, 2), 2);
        assert_eq!(tab_insert_index(TabPlacement::AfterActive, NewTabPosition::End, 9, 2), 2);
        assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::AfterCurrent, 0, 0), 0);
        assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::End, 0, 0), 0);
    }

    /// 落点只能有一个来源。九处各自硬编码 `(active_tab + 1).min(len)` 是这个
    /// 功能存在的原因——只靠约定「新入口记得读策略」，下一个入口就会漏掉。
    ///
    /// 允许的两处：`insert_tab`（唯一的创建/恢复插入口）与 `move_tab`
    /// （拖拽重排，落点由用户手势直接给出，不经策略）。新增第三处会让本测试
    /// 变红——那说明它应该改走 `insert_tab`。
    #[test]
    fn tab_insertion_has_exactly_two_homes() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/window_context.rs"),
        )
        .expect("window_context.rs 必须可读，否则本约束静默放水");
        // 只扫产品代码。规则本身是用字符串字面量表达的，扫描自己会把规则
        // 连同它的失败信息一起算成违规。
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        let insertions = production.matches("self.tabs.insert(").count();
        assert_eq!(
            insertions, 2,
            "找到 {insertions} 处 self.tabs.insert(，应为 2 处（insert_tab 与 move_tab）。\n\
             新的标签插入请调用 insert_tab(entry, placement)，由它决定落点。"
        );
    }

    #[test]
    fn chrome_clock_uses_fast_cadence_only_for_running_tabs() {
        assert_eq!(
            chrome_clock_interval(true, false, false),
            std::time::Duration::from_micros(16_667)
        );
        assert_eq!(
            chrome_clock_interval(false, true, false),
            std::time::Duration::from_millis(125)
        );
        assert_eq!(
            chrome_clock_interval(false, false, true),
            std::time::Duration::from_millis(125)
        );
        assert_eq!(chrome_clock_interval(false, false, false), std::time::Duration::from_secs(1));
    }

    #[test]
    fn startup_shell_uses_user_default_instead_of_the_base_pty_shell() {
        let selected =
            select_initial_shell(Some(shell("powershell.exe")), Some(shell("pwsh.exe")), None);
        assert_eq!(selected, Some(shell("pwsh.exe")));
    }

    #[test]
    fn explicit_cli_command_still_wins_over_the_user_default() {
        let selected = select_initial_shell(
            Some(shell("powershell.exe")),
            Some(shell("pwsh.exe")),
            Some(shell("nu.exe")),
        );
        assert_eq!(selected, Some(shell("nu.exe")));
    }

    #[test]
    fn tree_terminal_cwd_accepts_only_an_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("项目 空间");
        std::fs::create_dir(&directory).unwrap();
        let file = temp.path().join("not-a-directory.txt");
        std::fs::write(&file, b"x").unwrap();

        assert!(valid_new_tab_directory(&directory));
        assert!(!valid_new_tab_directory(&file));
        assert!(!valid_new_tab_directory(&temp.path().join("missing")));
    }

    #[test]
    fn explicit_tab_directory_precedes_startup_and_focused_directories() {
        let explicit = std::path::PathBuf::from("D:/profile");
        let startup = std::path::PathBuf::from("D:/startup");
        let focused = std::path::PathBuf::from("D:/focused");

        assert_eq!(
            preferred_tab_cwd(Some(explicit.clone()), Some(startup.clone()), Some(focused.clone())),
            Some(explicit)
        );
        assert_eq!(preferred_tab_cwd(None, Some(startup.clone()), Some(focused)), Some(startup));
    }

    #[test]
    fn initial_directory_precedence_keeps_cli_and_restore_before_startup() {
        let cli = std::path::PathBuf::from("D:/cli");
        let restored = std::path::PathBuf::from("D:/restore");
        let startup = std::path::PathBuf::from("D:/startup");
        let configured = std::path::PathBuf::from("D:/config");

        assert_eq!(
            preferred_initial_cwd(
                Some(cli.clone()),
                Some(restored.clone()),
                Some(startup.clone()),
                Some(configured.clone())
            ),
            Some(cli)
        );
        assert_eq!(
            preferred_initial_cwd(
                None,
                Some(restored.clone()),
                Some(startup.clone()),
                Some(configured)
            ),
            Some(restored)
        );
        assert_eq!(preferred_initial_cwd(None, None, Some(startup.clone()), None), Some(startup));
    }

    #[test]
    fn multiline_paste_modal_routes_to_its_originating_pane_only() {
        let paste = crate::display::NebulaConfirm::Paste {
            pane_id: 7,
            text: "one\ntwo".to_owned(),
            bracketed: true,
            lines: 2,
        };
        assert_eq!(routed_input_pane(Some(&paste), 9, |pane_id| pane_id == 7), 7);
        assert_eq!(routed_input_pane(Some(&paste), 9, |_| false), 9);

        let close =
            crate::display::NebulaConfirm::ClosePane { pane_id: 7, process: "cargo".to_owned() };
        assert_eq!(routed_input_pane(Some(&close), 9, |_| true), 9);
    }
}
