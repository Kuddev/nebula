//! Process window events.

use crate::ConfigMonitor;
use glutin::config::GetGlConfig;
use std::borrow::Cow;
use std::cmp::min;
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::Debug;
#[cfg(not(windows))]
use std::os::unix::io::RawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{env, f32, mem};

use ahash::RandomState;
use crossfont::Size as FontSize;
use glutin::config::Config as GlutinConfig;
use glutin::display::GetGlDisplay;
use log::{debug, error, info, warn};
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, Event as WinitEvent, Ime, Modifiers, MouseButton, StartCause,
    Touch as TouchEvent, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop, EventLoopProxy};
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::WindowId;

use global_hotkey::hotkey::{Code, HotKey, Modifiers as HotKeyModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use nebula_terminal::event::{Event as TerminalEvent, EventListener, Notify};
use nebula_terminal::event_loop::Notifier;
use nebula_terminal::grid::{BidirectionalIterator, Dimensions, Scroll};
use nebula_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use nebula_terminal::selection::{Selection, SelectionType};
use nebula_terminal::term::cell::Flags;
use nebula_terminal::term::search::{Match, RegexSearch};
use nebula_terminal::term::{self, ClipboardType, Term, TermMode};
use nebula_terminal::vte::ansi::NamedColor;

#[cfg(unix)]
use crate::cli::{IpcConfig, ParsedOptions};
use crate::cli::{Options as CliOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::reload::ReloadWorker;
use crate::config::ui_config::{HintAction, HintInternalAction};
use crate::config::{self, UiConfig};
#[cfg(not(windows))]
use crate::daemon::foreground_process_path;
use crate::daemon::spawn_daemon;
use crate::display::NebulaPaneState;
use crate::display::color::Rgb;
use crate::display::hint::HintMatch;
use crate::display::window::{ImeInhibitor, Window};
use crate::display::{Display, Preedit, SizeInfo};
use crate::input::{self, ActionContext as _, FONT_SIZE_STEP};
use crate::logging::{LOG_TARGET_CONFIG, LOG_TARGET_WINIT};
use crate::message_bar::{Message, MessageBuffer};
#[cfg(unix)]
use crate::polling::ipc::{self, SocketReply};
use crate::scheduler::{Scheduler, TimerId, Topic};
use crate::window_context::{DetachedWindow, WindowBoot, WindowContext};

/// Duration after the last user input until an unlimited search is performed.
pub const TYPING_SEARCH_DELAY: Duration = Duration::from_millis(500);

/// Maximum number of lines for the blocking search while still typing the search regex.
const MAX_SEARCH_WHILE_TYPING: Option<usize> = Some(1000);

/// Maximum number of search terms stored in the history.
const MAX_SEARCH_HISTORY_SIZE: usize = 255;

/// Touch zoom speed.
const TOUCH_ZOOM_FACTOR: f32 = 0.01;

/// Cooldown between invocations of the bell command.
const BELL_CMD_COOLDOWN: Duration = Duration::from_millis(100);

/// The event processor.
///
/// Stores some state from received events and dispatches actions when they are
/// triggered.
pub struct Processor {
    pub config_monitor: Option<ConfigMonitor>,

    clipboard: Clipboard,
    scheduler: Scheduler,
    initial_window_options: Option<WindowOptions>,
    initial_window_error: Option<Box<dyn Error>>,
    windows: HashMap<WindowId, WindowContext, RandomState>,
    proxy: EventLoopProxy<Event>,
    gl_config: Option<GlutinConfig>,
    #[cfg(unix)]
    global_ipc_options: ParsedOptions,
    cli_options: CliOptions,
    config: Rc<UiConfig>,
    // Lua 回调与模块状态属于当前成功代次，失败重载不能提前释放它。
    lua_generation: Option<config::lua::LuaGeneration>,
    config_source: Option<config::source::ConfigSource>,
    config_reload_worker: ReloadWorker,
    /// The quick (Quake) terminal window, once created.
    quick_terminal: Option<WindowId>,
    /// Whether the quick terminal is currently shown (target state).
    quick_visible: bool,
    /// Renderer-independent quick-terminal motion state.
    quick_motion: crate::motion::Tween,
    quick_motion_clock: crate::motion::MotionClock,
    /// Global hotkey manager, kept alive so its registration stays active.
    global_hotkey: Option<GlobalHotKeyManager>,
    /// Id of the registered quick-terminal toggle hotkey.
    quick_hotkey_id: Option<u32>,
    /// Tabs of closed windows kept alive for re-attach (multiplexer-style): their
    /// PTYs never stopped, so `claude` and friends survive the window. LIFO —
    /// an attach request adopts the most recently closed window first.
    detached: Vec<DetachedWindow>,
}

impl Processor {
    /// Create a new event processor.
    pub fn new(
        loaded_config: config::LoadedConfig,
        cli_options: CliOptions,
        event_loop: &EventLoop<Event>,
    ) -> Processor {
        let proxy = event_loop.create_proxy();
        let reload_proxy = proxy.clone();
        let config_reload_worker = ReloadWorker::new(move || {
            let event = Event::new(EventType::ConfigReloadReady, None);
            let _ = reload_proxy.send_event(event);
        });
        let scheduler = Scheduler::new(proxy.clone());
        let initial_window_options = Some(cli_options.window_options.clone());

        // Disable all device events, since we don't care about them.
        event_loop.listen_device_events(DeviceEvents::Never);

        // SAFETY: Since this takes a pointer to the winit event loop, it MUST be dropped first,
        // which is done in `loop_exiting`.
        let clipboard = unsafe { Clipboard::new(event_loop.display_handle().unwrap().as_raw()) };

        // Create a config monitor.
        //
        // The monitor watches the config file for changes and reloads it. Pending
        // config changes are processed in the main loop.
        let mut config_monitor = None;
        if loaded_config.live_config_reload() {
            config_monitor =
                ConfigMonitor::new(loaded_config.config_paths.clone(), event_loop.create_proxy());
        }

        let config::LoadedConfig { config, source: config_source, lua_generation } = loaded_config;

        // Register the global quick-terminal toggle hotkey (Ctrl+`).
        let (global_hotkey, quick_hotkey_id) = Self::init_quick_hotkey();

        Processor {
            initial_window_options,
            initial_window_error: None,
            cli_options,
            proxy,
            scheduler,
            gl_config: None,
            config: Rc::new(config),
            lua_generation,
            config_source,
            config_reload_worker,
            clipboard,
            windows: Default::default(),
            #[cfg(unix)]
            global_ipc_options: Default::default(),
            config_monitor,
            quick_terminal: None,
            quick_visible: false,
            quick_motion: crate::motion::Tween::new(1.0),
            quick_motion_clock: crate::motion::MotionClock::default(),
            global_hotkey,
            quick_hotkey_id,
            detached: Vec::new(),
        }
    }

    /// Create the global hotkey manager and register the quick-terminal toggle
    /// (Ctrl+`). Returns `(None, None)` if the platform rejects it, so the rest
    /// of the terminal keeps working without a quick terminal.
    fn init_quick_hotkey() -> (Option<GlobalHotKeyManager>, Option<u32>) {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                warn!("Quick terminal disabled: global hotkey init failed: {err}");
                return (None, None);
            },
        };
        let hotkey = HotKey::new(Some(HotKeyModifiers::CONTROL), Code::Backquote);
        let id = hotkey.id();
        match manager.register(hotkey) {
            Ok(()) => (Some(manager), Some(id)),
            Err(err) => {
                // Non-fatal and common in dev: a hard-killed previous instance
                // never ran Drop to release Ctrl+`, or another app already owns
                // it. The terminal works fine without the quick-terminal hotkey,
                // so log quietly instead of nagging the on-screen message bar.
                debug!("Quick terminal hotkey (Ctrl+`) not registered: {err}");
                (Some(manager), None)
            },
        }
    }

    /// Create initial window and load GL platform.
    ///
    /// This will initialize the OpenGL Api and pick a config that
    /// will be used for the rest of the windows.
    pub fn create_initial_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_options: WindowOptions,
    ) -> Result<(), Box<dyn Error>> {
        // Session restore (tab list + cwds) for a plain launch. An explicit
        // -e/--working-directory means the user asked for something specific:
        // start exactly that instead of yesterday's tabs.
        let plain_launch = window_options.terminal_options.working_directory.is_none()
            && window_options.terminal_options.command().is_none();
        let boot = if plain_launch {
            crate::session::load()
                .filter(crate::session::should_restore)
                .map(|mut session| {
                    // Count this launch against the crash-loop breaker; the
                    // first successful autosave (1 Hz tick) resets it.
                    crate::session::mark_boot_attempt(&mut session);
                    session
                })
                .map_or(WindowBoot::Fresh, WindowBoot::Restore)
        } else {
            WindowBoot::Fresh
        };

        let window_context = WindowContext::initial(
            event_loop,
            self.proxy.clone(),
            self.config.clone(),
            window_options,
            boot,
        )?;

        self.gl_config = Some(window_context.display.gl_context().config());
        self.windows.insert(window_context.id(), window_context);

        Ok(())
    }

    /// Create a new terminal window.
    pub fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        options: WindowOptions,
    ) -> Result<WindowId, Box<dyn Error>> {
        self.create_window_boot(event_loop, options, WindowBoot::Fresh)
    }

    /// Create a new terminal window with an explicit boot mode (fresh shell
    /// or adopting detached panes).
    fn create_window_boot(
        &mut self,
        event_loop: &ActiveEventLoop,
        options: WindowOptions,
        boot: WindowBoot,
    ) -> Result<WindowId, Box<dyn Error>> {
        let gl_config = self.gl_config.as_ref().unwrap();

        // Override config with CLI/IPC options.
        let mut config_overrides = options.config_overrides();
        #[cfg(unix)]
        config_overrides.extend_from_slice(&self.global_ipc_options);
        let mut config = self.config.clone();
        config = config_overrides.override_config_rc(config);

        let window_context = WindowContext::additional(
            gl_config,
            event_loop,
            self.proxy.clone(),
            config,
            options,
            config_overrides,
            boot,
        )?;

        let id = window_context.id();
        self.windows.insert(id, window_context);
        Ok(id)
    }

    /// A second launch (via the mux socket) asked this resident instance to
    /// surface. Priority: re-attach detached tabs > focus an existing window
    /// > open a fresh one.
    fn handle_attach_request(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(detached) = self.detached.pop() {
            match self.create_window_boot(
                event_loop,
                WindowOptions::default(),
                WindowBoot::Attach(detached),
            ) {
                Ok(_) => return,
                // The panes are gone with the failed boot (their PTYs shut
                // down by DetachedWindow's Drop); still surface SOMETHING.
                Err(err) => error!("Failed to re-attach detached tabs: {err}"),
            }
        }
        if let Some(window_context) = self.windows.values().find(|w| !w.session_exempt) {
            window_context.display.window.focus_window();
            return;
        }
        if self.gl_config.is_some() {
            if let Err(err) = self.create_window(event_loop, WindowOptions::default()) {
                error!("Could not open window on attach request: {err:?}");
            }
        }
    }

    /// Drop a detached pane whose shell exited while its window was closed,
    /// pruning residency entries that have nothing left alive.
    fn reap_detached_pane(&mut self, pane_id: Option<u64>) {
        let Some(pane_id) = pane_id else { return };
        for detached in &mut self.detached {
            detached.reap_pane(pane_id);
        }
        self.detached.retain(|detached| !detached.is_empty());
    }

    /// Drain global-hotkey events and toggle the quick terminal on a press.
    fn poll_quick_hotkey(&mut self, event_loop: &ActiveEventLoop) {
        let Some(hotkey_id) = self.quick_hotkey_id else { return };
        let mut toggle = false;
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == hotkey_id && event.state == HotKeyState::Pressed {
                toggle = true;
            }
        }
        if toggle {
            self.toggle_quick_terminal(event_loop);
        }
    }

    /// Show/hide the quick (Quake) terminal with a slide animation, creating it
    /// on first use.
    fn toggle_quick_terminal(&mut self, event_loop: &ActiveEventLoop) {
        // Existing quick terminal: flip the target state and start a slide.
        if let Some(id) = self.quick_terminal {
            if self.windows.contains_key(&id) {
                self.quick_visible = !self.quick_visible;
                let show = self.quick_visible;
                if show {
                    if let Some(wc) = self.windows.get(&id) {
                        // A fully hidden window starts above the edge. Reversing
                        // an active exit keeps its current position.
                        if !self.quick_motion.is_active() {
                            wc.display.window.set_quick_terminal_slide(1.0);
                            self.quick_motion.snap_to(1.0);
                        }
                        wc.display.window.set_visible(true);
                        wc.display.window.focus_window();
                    }
                }
                // Slide-out keeps the window visible until the animation ends.
                self.quick_motion.animate_role(
                    if show { 0.0 } else { 1.0 },
                    if show {
                        crate::motion::MotionRole::Enter
                    } else {
                        crate::motion::MotionRole::Exit
                    },
                    crate::motion::MotionPolicy::Full,
                );
                return;
            }
            // The window was closed by the user; fall through and recreate it.
            self.quick_terminal = None;
        }

        // The shared GL config only exists after the first normal window.
        if self.gl_config.is_none() {
            return;
        }

        match self.create_window(event_loop, WindowOptions::default()) {
            Ok(id) => {
                self.quick_terminal = Some(id);
                self.quick_visible = true;
                if let Some(wc) = self.windows.get_mut(&id) {
                    // Scratch space: the quick terminal never reads or writes
                    // the session file.
                    wc.session_exempt = true;
                    wc.display.window.configure_quick_terminal();
                    wc.display.window.set_quick_terminal_slide(1.0);
                    wc.display.window.focus_window();
                }
                self.quick_motion.snap_to(1.0);
                self.quick_motion.animate_role(
                    0.0,
                    crate::motion::MotionRole::Enter,
                    crate::motion::MotionPolicy::Full,
                );
            },
            Err(err) => error!("Failed to create quick terminal: {err}"),
        }
    }

    /// Advance the quick-terminal slide one frame. Returns `true` while
    /// animating (so the loop keeps polling). Motion Runtime owns timing and
    /// easing; this path only applies the resulting normalized position.
    fn animate_quick_terminal(&mut self) -> bool {
        if !self.quick_motion.is_active() {
            return false;
        }
        let Some(id) = self.quick_terminal else {
            self.quick_motion.snap_to(1.0);
            return false;
        };
        self.quick_motion.step(self.quick_motion_clock.tick());
        let hidden = self.quick_motion.value().clamp(0.0, 1.0);

        if let Some(wc) = self.windows.get(&id) {
            wc.display.window.set_quick_terminal_slide(hidden);
        }

        if !self.quick_motion.is_active() {
            if self.quick_motion.target() >= 1.0 {
                // Slide-out finished: actually hide the window.
                if let Some(wc) = self.windows.get(&id) {
                    wc.display.window.set_visible(false);
                }
            }
            return false;
        }
        true
    }

    /// Run the event loop.
    ///
    /// The result is exit code generate from the loop.
    pub fn run(&mut self, event_loop: EventLoop<Event>) -> Result<(), Box<dyn Error>> {
        let result = event_loop.run_app(self);
        match self.initial_window_error.take() {
            Some(initial_window_error) => Err(initial_window_error),
            _ => result.map_err(Into::into),
        }
    }

    /// Check if an event is irrelevant and can be skipped.
    fn skip_window_event(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::KeyboardInput { is_synthetic: true, .. }
                | WindowEvent::ActivationTokenDone { .. }
                | WindowEvent::DoubleTapGesture { .. }
                | WindowEvent::TouchpadPressure { .. }
                | WindowEvent::RotationGesture { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::PinchGesture { .. }
                | WindowEvent::AxisMotion { .. }
                | WindowEvent::PanGesture { .. }
                | WindowEvent::HoveredFileCancelled
                | WindowEvent::Destroyed
                | WindowEvent::HoveredFile(_)
                | WindowEvent::Moved(_)
        )
    }
}

impl ApplicationHandler<Event> for Processor {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause != StartCause::Init || self.cli_options.daemon {
            return;
        }

        if let Some(window_options) = self.initial_window_options.take() {
            if let Err(err) = self.create_initial_window(event_loop, window_options) {
                self.initial_window_error = Some(err);
                event_loop.exit();
                return;
            }
        }

        info!("Initialisation complete");
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.config.debug.print_events {
            info!(target: LOG_TARGET_WINIT, "{event:?}");
        }

        // Ignore all events we do not care about.
        if Self::skip_window_event(&event) {
            return;
        }

        let window_context = match self.windows.get_mut(&window_id) {
            Some(window_context) => window_context,
            None => return,
        };

        let is_redraw = matches!(event, WindowEvent::RedrawRequested);

        window_context.handle_event(
            #[cfg(target_os = "macos")]
            _event_loop,
            &self.proxy,
            &mut self.clipboard,
            &mut self.scheduler,
            WinitEvent::WindowEvent { window_id, event },
        );

        if is_redraw {
            window_context.draw(&mut self.scheduler);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        if self.config.debug.print_events {
            info!(target: LOG_TARGET_WINIT, "{event:?}");
        }

        // Handle events which don't mandate the WindowId.
        let tab_id = event.tab_id;
        match (event.payload, event.window_id.as_ref()) {
            // AI-CLI lifecycle events (nebula-hook pipe) route by pane id, so
            // the windows resolve them themselves; the owner claims it.
            (EventType::AiHook(hook), _) => {
                for window_context in self.windows.values_mut() {
                    if window_context.handle_ai_hook(&hook) {
                        break;
                    }
                }
            },
            // Toast click: surface the window (and pane) the toast came from.
            // Must be consumed here — the generic Some(window_id) forwarding
            // below would park it in a window's event queue instead.
            (EventType::FocusWindow { pane }, window_id) => {
                let id = window_id.copied();
                let window_context = match id {
                    Some(id) if self.windows.contains_key(&id) => self.windows.get_mut(&id),
                    _ => self.windows.values_mut().next(),
                };
                if let Some(window_context) = window_context {
                    window_context.focus_from_toast(pane);
                }
            },
            // Process IPC config update.
            #[cfg(unix)]
            (EventType::IpcConfig(ipc_config), window_id) => {
                // Try and parse options as toml.
                let mut options = ParsedOptions::from_options(&ipc_config.options);

                // Override IPC config for each window with matching ID.
                for (_, window_context) in self
                    .windows
                    .iter_mut()
                    .filter(|(id, _)| window_id.is_none() || window_id == Some(*id))
                {
                    if ipc_config.reset {
                        window_context.reset_window_config(self.config.clone());
                    } else {
                        window_context.add_window_config(self.config.clone(), &options);
                    }
                }

                // Persist global options for future windows.
                if window_id.is_none() {
                    if ipc_config.reset {
                        self.global_ipc_options.clear();
                    } else {
                        self.global_ipc_options.append(&mut options);
                    }
                }
            },
            // Process IPC config requests.
            #[cfg(unix)]
            (EventType::IpcGetConfig(stream), window_id) => {
                // Get the config for the requested window ID.
                let config = match self.windows.iter().find(|(id, _)| window_id == Some(*id)) {
                    Some((_, window_context)) => window_context.config(),
                    None => &self.global_ipc_options.override_config_rc(self.config.clone()),
                };

                // Convert config to JSON format.
                let config_json = match serde_json::to_string(&config) {
                    Ok(config_json) => config_json,
                    Err(err) => {
                        error!("Failed config serialization: {err}");
                        return;
                    },
                };

                // Send JSON config to the socket.
                if let Ok(mut stream) = stream.try_clone() {
                    ipc::send_reply(&mut stream, SocketReply::GetConfig(config_json));
                }
            },
            (EventType::ConfigReload(path), _) => {
                // Clear config logs from message bar for all terminals.
                for window_context in self.windows.values_mut() {
                    if !window_context.message_buffer.is_empty() {
                        window_context.message_buffer.remove_target(LOG_TARGET_CONFIG);
                        window_context.display.pending_update.dirty = true;
                    }
                }

                match config::source::source_for_path(path, true) {
                    Ok(source) => {
                        self.config_reload_worker.request(source);
                    },
                    Err(error) => error!("Unable to reload configuration: {error}"),
                }
            },
            (EventType::ConfigReloadReady, _) => {
                // 失败结果只产生诊断；当前配置和 Lua 代次保持不变。
                if let Some(result) = self.config_reload_worker.take_latest()
                    && let Ok(mut loaded) = result.loaded
                {
                    self.cli_options.override_config(&mut loaded.config);
                    self.lua_generation = loaded.lua_generation;
                    self.config_source = loaded.source;
                    self.config = Rc::new(loaded.config);

                    // Restart config monitor if imports changed.
                    if let Some(monitor) = self.config_monitor.take() {
                        let paths = &self.config.config_paths;
                        self.config_monitor = if monitor.needs_restart(paths) {
                            monitor.shutdown();
                            ConfigMonitor::new(paths.clone(), self.proxy.clone())
                        } else {
                            Some(monitor)
                        };
                    }

                    for window_context in self.windows.values_mut() {
                        window_context.update_config(self.config.clone());
                    }
                }
            },
            // Create a new terminal window.
            (EventType::CreateWindow(options), _) => {
                // XXX Ensure that no context is current when creating a new window,
                // otherwise it may lock the backing buffer of the
                // surface of current context when asking
                // e.g. EGL on Wayland to create a new context.
                for window_context in self.windows.values_mut() {
                    window_context.display.make_not_current();
                }

                if self.gl_config.is_none() {
                    // Handle initial window creation in daemon mode.
                    if let Err(err) = self.create_initial_window(event_loop, options) {
                        self.initial_window_error = Some(err);
                        event_loop.exit();
                    }
                } else if let Err(err) = self.create_window(event_loop, options) {
                    error!("Could not open window: {err:?}");
                }
            },
            // Shutdown all windows.
            #[cfg(unix)]
            (EventType::Shutdown, _) => event_loop.exit(),
            // A second launch handed over to this resident instance.
            (EventType::NebulaAttach, _) => self.handle_attach_request(event_loop),
            // Process events affecting all windows.
            (payload, None) => {
                let event = WinitEvent::UserEvent(Event::new(payload, None));
                for window_context in self.windows.values_mut() {
                    window_context.handle_event(
                        #[cfg(target_os = "macos")]
                        event_loop,
                        &self.proxy,
                        &mut self.clipboard,
                        &mut self.scheduler,
                        event.clone(),
                    );
                }
            },
            (EventType::Terminal(TerminalEvent::Wakeup), Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.dirty = true;
                    // A typed `ssh` login is confirmed by remote output still
                    // arriving after the fast-failure window — save it to the
                    // sidebar now, not when the session eventually exits.
                    window_context.confirm_ssh_on_activity(tab_id);
                    if window_context.display.window.has_frame {
                        window_context.display.window.request_redraw();
                    }
                }
            },
            (EventType::Terminal(TerminalEvent::Exit), Some(window_id)) => {
                // Close the tab whose shell exited; only close the window when
                // it was the last tab (respecting the hold option).
                let close_window = match self.windows.get_mut(window_id) {
                    Some(window_context) if !window_context.display.window.hold => {
                        let close = window_context.close_tab_by_id(tab_id);
                        if !close {
                            window_context.dirty = true;
                            window_context.display.window.request_redraw();
                        }
                        close
                    },
                    Some(_) => return,
                    None => {
                        // A shell exited in a DETACHED pane (its window is
                        // gone): reap it from the residency pool. Once nothing
                        // is left to re-attach, the resident process has no
                        // reason to live.
                        self.reap_detached_pane(tab_id);
                        if self.windows.is_empty()
                            && self.detached.is_empty()
                            && !self.cli_options.daemon
                        {
                            event_loop.exit();
                        }
                        return;
                    },
                };

                if !close_window {
                    return;
                }

                let window_context = match self.windows.remove(window_id) {
                    Some(window_context) => window_context,
                    None => return,
                };

                // Unschedule pending events.
                self.scheduler.unschedule_window(window_context.id());

                // The closed window's Drop writes its final session snapshot;
                // force the surviving windows to reclaim the file on their
                // next autosave tick.
                if !window_context.session_exempt {
                    for window in self.windows.values_mut() {
                        window.mark_session_dirty();
                    }
                }

                // Shutdown if no more terminals are open (and none detached).
                if self.windows.is_empty() && self.detached.is_empty() && !self.cli_options.daemon {
                    // Write ref tests of last window to disk.
                    if self.config.debug.ref_test {
                        window_context.write_ref_test_results();
                    }

                    event_loop.exit();
                }
            },
            // NOTE: This event bypasses batching to minimize input latency.
            (EventType::Frame, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.window.has_frame = true;
                    if window_context.dirty {
                        window_context.display.window.request_redraw();
                    }
                }
            },
            (EventType::NebulaTick, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    // Piggyback session persistence on the 1 Hz chrome clock.
                    window_context.autosave_session();
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::NebulaResizeSettled, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.apply_settled_pty_resize();
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::SshDeleteUndoExpired, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.expire_ssh_delete_undo();
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::SftpUpdated, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::NebulaTab(request), Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    let close_window = window_context.handle_tab_request(request);
                    if close_window {
                        if let Some(mut closed) = self.windows.remove(window_id) {
                            // A window-level close with live panes = detach
                            // (multiplexer-style): the PTYs keep running in this
                            // resident process, ready for re-attach. Quitting
                            // tab by tab reaches here with zero panes and
                            // falls through to a plain close. 设置→高级 lets
                            // users opt out: with keep_session off, closing
                            // the window kills its shells like a plain
                            // terminal (no resident server).
                            if closed.has_live_panes()
                                && !closed.session_exempt
                                && closed.display.nebula_keep_session
                            {
                                self.detached.push(closed.detach_panes());
                            }
                            // Same reclaim dance as the Exit path above: the
                            // closed window's Drop snapshot must not stick
                            // while other windows live on.
                            if !closed.session_exempt {
                                for window in self.windows.values_mut() {
                                    window.mark_session_dirty();
                                }
                            }
                        }
                        if self.windows.is_empty() && self.detached.is_empty() {
                            event_loop.exit();
                        }
                    } else {
                        window_context.dirty = true;
                        window_context.display.window.request_redraw();
                    }
                }
            },
            (payload, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.handle_event(
                        #[cfg(target_os = "macos")]
                        event_loop,
                        &self.proxy,
                        &mut self.clipboard,
                        &mut self.scheduler,
                        WinitEvent::UserEvent(Event {
                            window_id: Some(*window_id),
                            tab_id,
                            payload,
                        }),
                    );
                }
            },
        };
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.config.debug.print_events {
            info!(target: LOG_TARGET_WINIT, "About to wait");
        }

        // Poll the global quick-terminal toggle hotkey.
        self.poll_quick_hotkey(event_loop);

        // Advance the quick-terminal slide one frame; `true` = still animating.
        let quick_animating = self.animate_quick_terminal();

        // Dispatch event to all windows.
        for window_context in self.windows.values_mut() {
            window_context.handle_event(
                #[cfg(target_os = "macos")]
                event_loop,
                &self.proxy,
                &mut self.clipboard,
                &mut self.scheduler,
                WinitEvent::AboutToWait,
            );
        }

        // Update the scheduler after event processing to ensure
        // the event loop deadline is as accurate as possible.
        let control_flow = match self.scheduler.update() {
            Some(instant) => ControlFlow::WaitUntil(instant),
            None => ControlFlow::Wait,
        };
        // While the quick terminal slides, keep the loop hot so the eased
        // position is re-derived every frame instead of parking on Wait.
        event_loop.set_control_flow(if quick_animating { ControlFlow::Poll } else { control_flow });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.config.debug.print_events {
            info!("Exiting the event loop");
        }

        match self.gl_config.take().map(|config| config.display()) {
            #[cfg(not(target_os = "macos"))]
            Some(glutin::display::Display::Egl(display)) => {
                // Ensure that all the windows are dropped, so the destructors for
                // Renderer and contexts ran.
                self.windows.clear();

                // SAFETY: the display is being destroyed after destroying all the
                // windows, thus no attempt to access the EGL state will be made.
                unsafe {
                    display.terminate();
                }
            },
            _ => (),
        }

        // SAFETY: The clipboard must be dropped before the event loop, so use the nop clipboard
        // as a safe placeholder.
        self.clipboard = Clipboard::new_nop();
    }
}

/// Nebula events.
#[derive(Debug, Clone)]
pub struct Event {
    /// Limit event to a specific window.
    window_id: Option<WindowId>,

    /// Limit event to a specific tab within the window (Nebula tabs).
    tab_id: Option<u64>,

    /// Event payload.
    payload: EventType,
}

impl Event {
    pub fn new<I: Into<Option<WindowId>>>(payload: EventType, window_id: I) -> Self {
        Self { window_id: window_id.into(), tab_id: None, payload }
    }

    /// Tab id attached to a terminal-originated event.
    pub(crate) fn terminal_tab_id(&self) -> Option<u64> {
        matches!(self.payload, EventType::Terminal(_)).then_some(self.tab_id).flatten()
    }

    /// Pane id of a terminal `Bell` event, for per-tab bell indicators.
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

/// Nebula events.
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
    /// Nebula tab management request for a window.
    NebulaTab(TabRequest),
    /// Periodic tick to redraw the chrome clock.
    NebulaTick,
    /// A second `nebula` launch asked the resident instance to surface: re-open
    /// a window for detached tabs, or focus an existing one (single instance).
    NebulaAttach,
    /// An interactive resize settled (no size change for the debounce window):
    /// flush the deferred PTY resizes + welcome-intro reprint in one shot.
    NebulaResizeSettled,
    /// The SSH deletion grace period ended; commit its delayed credential cleanup.
    SshDeleteUndoExpired,
    /// An SFTP background operation published a new local UI snapshot.
    SftpUpdated,
    /// Typed AI-CLI lifecycle event from the nebula-hook pipe (see `ai_hook`).
    AiHook(crate::ai_hook::AiHookEvent),
    /// A toast was clicked: focus the originating window and, when known,
    /// surface the pane's tab. The window rides in `Event::window_id`.
    FocusWindow {
        pane: Option<u64>,
    },
}

/// Nebula tab management actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabRequest {
    New,
    /// Open a new tab running the quick-launch profile at this config index
    /// (custom command instead of the default shell, e.g. an ssh jump).
    NewProfile(usize),
    /// Open a new tab running a detected shell (the new-tab dropdown). Carries
    /// the spec directly — detected shells aren't config-indexed.
    NewShell {
        name: String,
        shell: nebula_terminal::tty::Shell,
    },
    /// Open a new tab connected to this `~/.ssh/config` host alias, through
    /// `nebula ssh` so remote shell integration bootstraps automatically.
    NewSsh(String),
    /// Open this file (markdown/text/json) in a read-only viewer tab —
    /// double-clicking a viewable file in the directory tree lands here.
    OpenDoc(std::path::PathBuf),
    /// Open or focus the singleton Settings tab. It owns no PTY.
    OpenSettings,
    Close,
    CloseIndex(usize),
    /// Duplicate a tab using the launch identity captured when it was opened.
    Duplicate(usize),
    CloseWindow,
    SelectNext,
    SelectPrev,
    Select(usize),
    /// Jump to the rightmost tab.
    SelectLast,
    /// Reorder: move the tab at displayed index `from` to displayed index `to`.
    Move {
        from: usize,
        to: usize,
    },
    /// Toggle a split (left/right or top/bottom) with an independent shell.
    SplitToggle(crate::display::SplitDirection),
    /// Select a specific tab and split it. Context menus can target inactive
    /// tabs, so this must be atomic rather than relying on two queued events.
    SplitIndex {
        index: usize,
        direction: crate::display::SplitDirection,
    },
    /// Dock the whole layout of tab `source` into the ACTIVE tab, splitting it
    /// on `nav`'s side (drag a sidebar tab into the terminal area to drop).
    DockSplit {
        source: usize,
        nav: crate::display::SplitNav,
    },
    /// Move keyboard focus to the split pane in the given direction.
    FocusSplit(crate::display::SplitNav),
    /// Toggle zoom (temporary full-window) of the focused split pane.
    ToggleZoom,
    /// Begin renaming the tab at the given index.
    BeginRename(usize),
    /// Commit the rename with the provided new name.
    CommitRename(String),
    /// Set a tab's custom light-strip color; `None` follows the theme accent.
    SetColor {
        index: usize,
        color: Option<crate::display::color::Rgb>,
    },
    /// Cancel the current rename operation.
    CancelRename,
}

impl From<TerminalEvent> for EventType {
    fn from(event: TerminalEvent) -> Self {
        Self::Terminal(event)
    }
}

/// Regex search state.
pub struct SearchState {
    /// Search direction.
    pub direction: Direction,

    /// Current position in the search history.
    pub history_index: Option<usize>,

    /// Change in display offset since the beginning of the search.
    display_offset_delta: i32,

    /// Search origin in viewport coordinates relative to original display offset.
    origin: Point,

    /// Focused match during active search.
    focused_match: Option<Match>,

    /// Search regex and history.
    ///
    /// During an active search, the first element is the user's current input.
    ///
    /// While going through history, the [`SearchState::history_index`] will point to the element
    /// in history which is currently being previewed.
    history: VecDeque<String>,

    /// Compiled search automatons.
    dfas: Option<RegexSearch>,
}

impl SearchState {
    /// Search regex text if a search is active.
    pub fn regex(&self) -> Option<&String> {
        self.history_index.and_then(|index| self.history.get(index))
    }

    /// Direction of the search from the search origin.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Focused match during vi-less search.
    pub fn focused_match(&self) -> Option<&Match> {
        self.focused_match.as_ref()
    }

    /// Clear the focused match.
    pub fn clear_focused_match(&mut self) {
        self.focused_match = None;
    }

    /// Active search dfas.
    pub fn dfas(&mut self) -> Option<&mut RegexSearch> {
        self.dfas.as_mut()
    }

    /// Search regex text if a search is active.
    fn regex_mut(&mut self) -> Option<&mut String> {
        self.history_index.and_then(move |index| self.history.get_mut(index))
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            display_offset_delta: Default::default(),
            focused_match: Default::default(),
            history_index: Default::default(),
            history: Default::default(),
            origin: Default::default(),
            dfas: Default::default(),
        }
    }
}

/// Vi inline search state.
pub struct InlineSearchState {
    /// Whether inline search is currently waiting for search character input.
    pub char_pending: bool,
    pub character: Option<char>,

    direction: Direction,
    stop_short: bool,
}

impl Default for InlineSearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            char_pending: Default::default(),
            stop_short: Default::default(),
            character: Default::default(),
        }
    }
}

pub struct ActionContext<'a, N, T> {
    pub notifier: &'a mut N,
    pub terminal: &'a mut Term<T>,
    pub clipboard: &'a mut Clipboard,
    pub mouse: &'a mut Mouse,
    pub touch: &'a mut TouchPurpose,
    pub modifiers: &'a mut Modifiers,
    pub display: &'a mut Display,
    pub nebula_state: &'a mut NebulaPaneState,
    /// Document shown by the active tab, when it is a viewer tab — wheel and
    /// navigation keys scroll this instead of a grid. `None` on pane tabs.
    pub doc: Option<&'a mut crate::display::markdown_view::DocView>,
    pub message_buffer: &'a mut MessageBuffer,
    pub config: &'a UiConfig,
    pub cursor_blink_timed_out: &'a mut bool,
    pub prev_bell_cmd: &'a mut Option<Instant>,
    #[cfg(target_os = "macos")]
    pub event_loop: &'a ActiveEventLoop,
    pub event_proxy: &'a EventLoopProxy<Event>,
    pub scheduler: &'a mut Scheduler,
    pub search_state: &'a mut SearchState,
    pub inline_search_state: &'a mut InlineSearchState,
    pub dirty: &'a mut bool,
    pub occluded: &'a mut bool,
    pub preserve_title: bool,
    #[cfg(not(windows))]
    pub master_fd: RawFd,
    #[cfg(not(windows))]
    pub shell_pid: u32,
}

impl<'a, N: Notify + 'a, T: EventListener> input::ActionContext<T> for ActionContext<'a, N, T> {
    #[inline]
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, val: B) {
        self.notifier.notify(val);
    }

    #[inline]
    fn doc_view(&mut self) -> Option<&mut crate::display::markdown_view::DocView> {
        self.doc.as_deref_mut()
    }

    /// Request a redraw.
    #[inline]
    fn mark_dirty(&mut self) {
        *self.dirty = true;
    }

    #[inline]
    fn size_info(&self) -> SizeInfo {
        // In split mode this is the focused pane's view, so mouse/selection
        // coordinates map into the focused grid rather than the full window.
        self.display.pane_view()
    }

    fn scroll(&mut self, scroll: Scroll) {
        let old_offset = self.terminal.grid().display_offset() as i32;

        let old_vi_cursor = self.terminal.vi_mode_cursor;
        self.terminal.scroll_display(scroll);

        let lines_changed = old_offset - self.terminal.grid().display_offset() as i32;

        // Keep track of manual display offset changes during search.
        if self.search_active() {
            self.search_state.display_offset_delta += lines_changed;
        }

        let vi_mode = self.terminal.mode().contains(TermMode::VI);

        // Update selection.
        if vi_mode && self.terminal.selection.as_ref().is_some_and(|s| !s.is_empty()) {
            self.update_selection(self.terminal.vi_mode_cursor.point, Side::Right);
        } else if self.mouse.left_button_state == ElementState::Pressed
            || self.mouse.right_button_state == ElementState::Pressed
        {
            let display_offset = self.terminal.grid().display_offset();
            let point = self.mouse.point(&self.size_info(), display_offset);
            self.update_selection(point, self.mouse.cell_side);
        }

        // Scrolling inside Vi mode moves the cursor, so start typing.
        if vi_mode {
            self.on_typing_start();
        }

        // Update dirty if actually scrolled or moved Vi cursor in Vi mode.
        *self.dirty |=
            lines_changed != 0 || (vi_mode && old_vi_cursor != self.terminal.vi_mode_cursor);
    }

    // Copy text selection.
    fn copy_selection(&mut self, ty: ClipboardType) {
        let text = match self.terminal.selection_to_string().filter(|s| !s.is_empty()) {
            Some(text) => text,
            None => return,
        };

        if ty == ClipboardType::Selection && self.config.selection.save_to_clipboard {
            self.clipboard.store(ClipboardType::Clipboard, text.clone());
        }
        self.clipboard.store(ty, text);
    }

    fn selection_is_empty(&self) -> bool {
        self.terminal.selection.as_ref().is_none_or(Selection::is_empty)
    }

    fn clear_selection(&mut self) {
        // Clear the selection on the terminal.
        let selection = self.terminal.selection.take();
        if selection.is_some() {
            crate::display::nebula_debug_log(format!(
                "pointer_selection_clear id={} non_empty={}",
                self.mouse.debug_press_id,
                selection.as_ref().is_some_and(|selection| !selection.is_empty())
            ));
        }
        // Mark the terminal as dirty when selection wasn't empty.
        *self.dirty |= selection.is_some_and(|s| !s.is_empty());
    }

    fn update_selection(&mut self, mut point: Point, side: Side) {
        let mut selection = match self.terminal.selection.take() {
            Some(selection) => selection,
            None => {
                crate::display::nebula_debug_log(format!(
                    "pointer_selection_update_ignored id={} point={point:?} side={side:?} reason=no-selection",
                    self.mouse.debug_press_id
                ));
                return;
            },
        };

        // Treat motion over message bar like motion over the last line.
        point.line = min(point.line, self.terminal.bottommost_line());

        // Update selection.
        selection.update(point, side);
        self.mouse.debug_selection_updates = self.mouse.debug_selection_updates.saturating_add(1);
        let update = self.mouse.debug_selection_updates;
        if update <= 3 || update % 10 == 0 {
            crate::display::nebula_debug_log(format!(
                "pointer_selection_update id={} update={} point={point:?} side={side:?} type={:?}",
                self.mouse.debug_press_id, update, selection.ty
            ));
        }

        // Move vi cursor and expand selection.
        if self.terminal.mode().contains(TermMode::VI) && !self.search_active() {
            self.terminal.vi_mode_cursor.point = point;
            selection.include_all();
        }

        self.terminal.selection = Some(selection);
        *self.dirty = true;
    }

    fn start_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        crate::display::nebula_debug_log(format!(
            "pointer_selection_start id={} type={ty:?} point={point:?} side={side:?} xy=({}, {})",
            self.mouse.debug_press_id, self.mouse.x, self.mouse.y
        ));
        self.terminal.selection = Some(Selection::new(ty, point, side));
        *self.dirty = true;

        self.copy_selection(ClipboardType::Selection);
    }

    fn toggle_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        match &mut self.terminal.selection {
            Some(selection) if selection.ty == ty && !selection.is_empty() => {
                self.clear_selection();
            },
            Some(selection) if !selection.is_empty() => {
                selection.ty = ty;
                *self.dirty = true;

                self.copy_selection(ClipboardType::Selection);
            },
            _ => self.start_selection(ty, point, side),
        }
    }

    #[inline]
    fn mouse_mode(&self) -> bool {
        self.terminal.mode().intersects(TermMode::MOUSE_MODE)
            && !self.terminal.mode().contains(TermMode::VI)
    }

    #[inline]
    fn mouse_mut(&mut self) -> &mut Mouse {
        self.mouse
    }

    #[inline]
    fn mouse(&self) -> &Mouse {
        self.mouse
    }

    #[inline]
    fn touch_purpose(&mut self) -> &mut TouchPurpose {
        self.touch
    }

    #[inline]
    fn modifiers(&mut self) -> &mut Modifiers {
        self.modifiers
    }

    #[inline]
    fn window(&mut self) -> &mut Window {
        &mut self.display.window
    }

    #[inline]
    fn display(&mut self) -> &mut Display {
        self.display
    }

    #[inline]
    fn terminal(&self) -> &Term<T> {
        self.terminal
    }

    #[inline]
    fn terminal_mut(&mut self) -> &mut Term<T> {
        self.terminal
    }

    #[inline]
    fn nebula_accept(&self) -> crate::display::AcceptKey {
        self.display.nebula_accept
    }

    #[inline]
    fn nebula_take_suggestion(&mut self) -> String {
        mem::take(&mut self.nebula_state.suggestion)
    }

    #[inline]
    fn nebula_input_char(&mut self, c: char) {
        Display::nebula_input_char(self.nebula_state, c);
    }

    #[inline]
    fn nebula_input_text(&mut self, text: &str) {
        Display::nebula_input_text(self.nebula_state, text);
    }

    #[inline]
    fn nebula_input_backspace(&mut self) {
        Display::nebula_input_backspace(self.nebula_state);
    }

    #[inline]
    fn nebula_delete_word(&mut self) {
        Display::nebula_input_delete_word(self.nebula_state);
    }

    #[inline]
    fn nebula_commit_line(&mut self) {
        // Snapshot the input straight off the grid at Enter time: the shell
        // hasn't processed the newline yet, so the row still shows the full
        // line, while the cached `screen_line` is one draw behind and commits
        // a truncated command on type-fast-then-Enter.
        #[cfg(windows)]
        if !self.terminal.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI)
            && self.search_state.regex().is_none()
        {
            let cursor = self.terminal.grid().cursor.point;
            match Display::nebula_input_from_raw_grid(self.terminal, cursor) {
                Some(line) => self.nebula_state.screen_line = line,
                // A failed read means the cached copy is stale too — an
                // earlier partial line must not get recorded as this command.
                None => self.nebula_state.screen_line.clear(),
            }
        }
        self.display.nebula_commit_line(self.nebula_state);
    }

    #[inline]
    fn nebula_clear_line(&mut self) {
        Display::nebula_clear_line(self.nebula_state);
    }

    fn nebula_tab(&self, request: TabRequest) {
        let _ = self.event_proxy.send_event(Event {
            window_id: Some(self.display.window.id()),
            tab_id: None,
            payload: EventType::NebulaTab(request),
        });
    }

    fn nebula_open_sftp(&mut self, destination: String) {
        if let Err(err) = self.display.open_sftp_panel(destination, self.event_proxy.clone()) {
            log::error!("{err}");
        }
    }

    fn spawn_new_instance(&mut self) {
        let mut env_args = env::args();
        let nebula = env_args.next().unwrap();

        let mut args: Vec<String> = Vec::new();

        // Reuse the arguments passed to Nebula for the new instance.
        #[allow(clippy::while_let_on_iterator)]
        while let Some(arg) = env_args.next() {
            // New instances shouldn't inherit command.
            if arg == "-e" || arg == "--command" {
                break;
            }

            // On unix, the working directory of the foreground shell is used by `start_daemon`.
            #[cfg(not(windows))]
            if arg == "--working-directory" {
                let _ = env_args.next();
                continue;
            }

            args.push(arg);
        }

        self.spawn_daemon(&nebula, &args);
    }

    #[cfg(not(windows))]
    fn create_new_window(&mut self, #[cfg(target_os = "macos")] tabbing_id: Option<String>) {
        let mut options = WindowOptions::default();
        options.terminal_options.working_directory =
            foreground_process_path(self.master_fd, self.shell_pid).ok();

        #[cfg(target_os = "macos")]
        {
            options.window_tabbing_id = tabbing_id;
        }

        let _ = self.event_proxy.send_event(Event::new(EventType::CreateWindow(options), None));
    }

    #[cfg(windows)]
    fn create_new_window(&mut self) {
        let _ = self
            .event_proxy
            .send_event(Event::new(EventType::CreateWindow(WindowOptions::default()), None));
    }

    fn spawn_daemon<I, S>(&self, program: &str, args: I)
    where
        I: IntoIterator<Item = S> + Debug + Copy,
        S: AsRef<OsStr>,
    {
        #[cfg(not(windows))]
        let result = spawn_daemon(program, args, self.master_fd, self.shell_pid);
        #[cfg(windows)]
        let result = spawn_daemon(program, args);

        match result {
            Ok(_) => debug!("Launched {program} with args {args:?}"),
            Err(err) => warn!("Unable to launch {program} with args {args:?}: {err}"),
        }
    }

    fn change_font_size(&mut self, delta: f32) {
        // Round to pick integral px steps, since fonts look better on them.
        let new_size = self.display.font_size.as_px().round() + delta;
        self.display.font_size = FontSize::from_px(new_size);
        let font = self.display.effective_font(&self.config.font).with_size(self.display.font_size);
        self.display.pending_update.set_font(font);
    }

    fn reset_font_size(&mut self) {
        let scale_factor = self.display.window.scale_factor as f32;
        self.display.font_size = self.config.font.size().scale(scale_factor);
        let font = self.display.effective_font(&self.config.font).with_size(self.display.font_size);
        self.display.pending_update.set_font(font);
    }

    #[inline]
    fn pop_message(&mut self) {
        if !self.message_buffer.is_empty() {
            self.display.pending_update.dirty = true;
            self.message_buffer.pop();
        }
    }

    #[inline]
    fn start_search(&mut self, direction: Direction) {
        // Only create new history entry if the previous regex wasn't empty.
        if self.search_state.history.front().is_none_or(|regex| !regex.is_empty()) {
            self.search_state.history.push_front(String::new());
            self.search_state.history.truncate(MAX_SEARCH_HISTORY_SIZE);
        }

        self.search_state.history_index = Some(0);
        self.search_state.direction = direction;
        self.search_state.focused_match = None;

        // Store original search position as origin and reset location.
        if self.terminal.mode().contains(TermMode::VI) {
            self.search_state.origin = self.terminal.vi_mode_cursor.point;
            self.search_state.display_offset_delta = 0;

            // Adjust origin for content moving upward on search start.
            if self.terminal.grid().cursor.point.line + 1 == self.terminal.screen_lines() {
                self.search_state.origin.line -= 1;
            }
        } else {
            let viewport_top = Line(-(self.terminal.grid().display_offset() as i32)) - 1;
            let viewport_bottom = viewport_top + self.terminal.bottommost_line();
            let last_column = self.terminal.last_column();
            self.search_state.origin = match direction {
                Direction::Right => Point::new(viewport_top, Column(0)),
                Direction::Left => Point::new(viewport_bottom, last_column),
            };
        }

        // Remove vi mode IME inhibitor, so the user can input the target character.
        self.window().set_ime_inhibitor(ImeInhibitor::VI, false);

        self.display.damage_tracker.frame().mark_fully_damaged();
        self.display.pending_update.dirty = true;
    }

    #[inline]
    fn start_seeded_search(&mut self, direction: Direction, text: String) {
        let origin = self.terminal.vi_mode_cursor.point;

        // Start new search.
        self.clear_selection();
        self.start_search(direction);

        // Enter initial selection text.
        for c in text.chars() {
            if let '$' | '('..='+' | '?' | '['..='^' | '{'..='}' = c {
                self.search_input('\\');
            }
            self.search_input(c);
        }

        // Leave search mode.
        self.confirm_search();

        if !self.terminal.mode().contains(TermMode::VI) {
            return;
        }

        // Find the target vi cursor point by going to the next match to the right of the origin,
        // then jump to the next search match in the target direction.
        let target = self.search_next(origin, Direction::Right, Side::Right).and_then(|rm| {
            let regex_match = match direction {
                Direction::Right => {
                    let origin = rm.end().add(self.terminal, Boundary::None, 1);
                    self.search_next(origin, Direction::Right, Side::Left)?
                },
                Direction::Left => {
                    let origin = rm.start().sub(self.terminal, Boundary::None, 1);
                    self.search_next(origin, Direction::Left, Side::Left)?
                },
            };
            Some(*regex_match.start())
        });

        // Move the vi cursor to the target position.
        if let Some(target) = target {
            self.terminal_mut().vi_goto_point(target);
            self.mark_dirty();
        }
    }

    #[inline]
    fn confirm_search(&mut self) {
        // Just cancel search when not in vi mode.
        if !self.terminal.mode().contains(TermMode::VI) {
            self.cancel_search();
            return;
        }

        // Force unlimited search if the previous one was interrupted.
        let timer_id = TimerId::new(Topic::DelayedSearch, self.display.window.id());
        if self.scheduler.scheduled(timer_id) {
            self.goto_match(None);
        }

        self.exit_search();
    }

    #[inline]
    fn cancel_search(&mut self) {
        if self.terminal.mode().contains(TermMode::VI) {
            // Recover pre-search state in vi mode.
            self.search_reset_state();
        } else if let Some(focused_match) = &self.search_state.focused_match {
            // Create a selection for the focused match.
            let start = *focused_match.start();
            let end = *focused_match.end();
            self.start_selection(SelectionType::Simple, start, Side::Left);
            self.update_selection(end, Side::Right);
            self.copy_selection(ClipboardType::Selection);
        }

        self.search_state.dfas = None;

        self.exit_search();
    }

    #[inline]
    fn search_input(&mut self, c: char) {
        match self.search_state.history_index {
            Some(0) => (),
            // When currently in history, replace active regex with history on change.
            Some(index) => {
                self.search_state.history[0] = self.search_state.history[index].clone();
                self.search_state.history_index = Some(0);
            },
            None => return,
        }
        let regex = &mut self.search_state.history[0];

        match c {
            // Handle backspace/ctrl+h.
            '\x08' | '\x7f' => {
                let _ = regex.pop();
            },
            // Add ascii and unicode text.
            ' '..='~' | '\u{a0}'..='\u{10ffff}' => regex.push(c),
            // Ignore non-printable characters.
            _ => return,
        }

        if !self.terminal.mode().contains(TermMode::VI) {
            // Clear selection so we do not obstruct any matches.
            self.terminal.selection = None;
        }

        self.update_search();
    }

    #[inline]
    fn search_pop_word(&mut self) {
        if let Some(regex) = self.search_state.regex_mut() {
            *regex = regex.trim_end().to_owned();
            regex.truncate(regex.rfind(' ').map_or(0, |i| i + 1));
            self.update_search();
        }
    }

    /// Go to the previous regex in the search history.
    #[inline]
    fn search_history_previous(&mut self) {
        let index = match &mut self.search_state.history_index {
            None => return,
            Some(index) if *index + 1 >= self.search_state.history.len() => return,
            Some(index) => index,
        };

        *index += 1;
        self.update_search();
    }

    /// Go to the previous regex in the search history.
    #[inline]
    fn search_history_next(&mut self) {
        let index = match &mut self.search_state.history_index {
            Some(0) | None => return,
            Some(index) => index,
        };

        *index -= 1;
        self.update_search();
    }

    #[inline]
    fn advance_search_origin(&mut self, direction: Direction) {
        // Use focused match as new search origin if available.
        if let Some(focused_match) = &self.search_state.focused_match {
            let new_origin = match direction {
                Direction::Right => focused_match.end().add(self.terminal, Boundary::None, 1),
                Direction::Left => focused_match.start().sub(self.terminal, Boundary::None, 1),
            };

            self.terminal.scroll_to_point(new_origin);

            self.search_state.display_offset_delta = 0;
            self.search_state.origin = new_origin;
        }

        // Search for the next match using the supplied direction.
        let search_direction = mem::replace(&mut self.search_state.direction, direction);
        self.goto_match(None);
        self.search_state.direction = search_direction;

        // If we found a match, we set the search origin right in front of it to make sure that
        // after modifications to the regex the search is started without moving the focused match
        // around.
        let focused_match = match &self.search_state.focused_match {
            Some(focused_match) => focused_match,
            None => return,
        };

        // Set new origin to the left/right of the match, depending on search direction.
        let new_origin = match self.search_state.direction {
            Direction::Right => *focused_match.start(),
            Direction::Left => *focused_match.end(),
        };

        // Store the search origin with display offset by checking how far we need to scroll to it.
        let old_display_offset = self.terminal.grid().display_offset() as i32;
        self.terminal.scroll_to_point(new_origin);
        let new_display_offset = self.terminal.grid().display_offset() as i32;
        self.search_state.display_offset_delta = new_display_offset - old_display_offset;

        // Store origin and scroll back to the match.
        self.terminal.scroll_display(Scroll::Delta(-self.search_state.display_offset_delta));
        self.search_state.origin = new_origin;
    }

    /// Find the next search match.
    fn search_next(&mut self, origin: Point, direction: Direction, side: Side) -> Option<Match> {
        self.search_state
            .dfas
            .as_mut()
            .and_then(|dfas| self.terminal.search_next(dfas, origin, direction, side, None))
    }

    #[inline]
    fn search_direction(&self) -> Direction {
        self.search_state.direction
    }

    #[inline]
    fn search_active(&self) -> bool {
        self.search_state.history_index.is_some()
    }

    /// Handle keyboard typing start.
    ///
    /// This will temporarily disable some features like terminal cursor blinking or the mouse
    /// cursor.
    ///
    /// All features are re-enabled again automatically.
    #[inline]
    fn on_typing_start(&mut self) {
        // Disable cursor blinking.
        let timer_id = TimerId::new(Topic::BlinkCursor, self.display.window.id());
        if self.scheduler.unschedule(timer_id).is_some() {
            self.schedule_blinking();

            // Mark the cursor as visible and queue redraw if the cursor was hidden.
            if mem::take(&mut self.display.cursor_hidden) {
                *self.dirty = true;
            }
        } else if *self.cursor_blink_timed_out {
            self.update_cursor_blinking();
        }

        // Hide mouse cursor.
        if self.config.mouse.hide_when_typing && self.display.window.mouse_visible() {
            self.display.window.set_mouse_visible(false);

            // Request hint highlights update, since the mouse may have been hovering a hint.
            self.mouse.hint_highlight_dirty = true
        }
    }

    /// Process a new character for keyboard hints.
    fn hint_input(&mut self, c: char) {
        if let Some(hint) = self.display.hint_state.keyboard_input(self.terminal, c) {
            self.mouse.block_hint_launcher = false;
            self.trigger_hint(&hint);
        }
        *self.dirty = true;
    }

    /// Open a filesystem path with the system default handler (the drawer's
    /// double-click). `explorer.exe` handles files AND folders, and sidesteps
    /// `cmd /c start` mangling spaces/unicode (same as file:// hints).
    fn open_path(&mut self, path: &std::path::Path) {
        #[cfg(windows)]
        self.spawn_daemon("explorer.exe", &[path.as_os_str()]);
        #[cfg(not(windows))]
        self.spawn_daemon("xdg-open", &[path.as_os_str()]);
    }

    /// Trigger a hint action.
    fn trigger_hint(&mut self, hint: &HintMatch) {
        crate::display::nebula_link_log(format!(
            "trigger_hint block={} hyperlink={}",
            self.mouse.block_hint_launcher,
            hint.hyperlink().is_some()
        ));
        if self.mouse.block_hint_launcher {
            return;
        }

        let hint_bounds = hint.bounds();
        let text = match hint.text(self.terminal) {
            Some(text) => text,
            None => return,
        };

        match &hint.action() {
            // Launch an external program.
            HintAction::Command(command) => {
                // On Windows, a `file://` OSC 8 link (our clickable `ls`) is
                // opened via `explorer.exe` with a translated native path. This
                // sidesteps `cmd /c start` mangling spaces/unicode and lets
                // WSL/MSYS posix paths (`/mnt/c/…`, `/d/…`) actually resolve.
                #[cfg(windows)]
                if let Some(path) = crate::file_uri::file_uri_to_local_path(&text) {
                    crate::display::nebula_link_log(format!(
                        "trigger_hint file-uri explorer path={path:?} (from {text:?})"
                    ));
                    self.spawn_daemon("explorer.exe", &[path.as_os_str()]);
                    return;
                }

                let mut args = command.args().to_vec();
                args.push(text.into());
                crate::display::nebula_link_log(format!(
                    "trigger_hint spawn program={:?} args={args:?}",
                    command.program()
                ));
                self.spawn_daemon(command.program(), &args);
            },
            // Copy the text to the clipboard.
            HintAction::Action(HintInternalAction::Copy) => {
                self.clipboard.store(ClipboardType::Clipboard, text);
            },
            // Write the text to the PTY/search.
            HintAction::Action(HintInternalAction::Paste) => self.paste(&text, true),
            // Select the text.
            HintAction::Action(HintInternalAction::Select) => {
                self.start_selection(SelectionType::Simple, *hint_bounds.start(), Side::Left);
                self.update_selection(*hint_bounds.end(), Side::Right);
                self.copy_selection(ClipboardType::Selection);
            },
            // Move the vi mode cursor.
            HintAction::Action(HintInternalAction::MoveViModeCursor) => {
                // Enter vi mode if we're not in it already.
                if !self.terminal.mode().contains(TermMode::VI) {
                    self.terminal.toggle_vi_mode();
                }

                self.terminal.vi_goto_point(*hint_bounds.start());
                self.mark_dirty();
            },
        }
    }

    /// Expand the selection to the current mouse cursor position.
    #[inline]
    fn expand_selection(&mut self) {
        let control = self.modifiers().state().control_key();
        let selection_type = match self.mouse().click_state {
            ClickState::None => return,
            _ if control => SelectionType::Block,
            ClickState::Click => SelectionType::Simple,
            ClickState::DoubleClick => SelectionType::Semantic,
            ClickState::TripleClick => SelectionType::Lines,
        };

        // Load mouse point, treating message bar and padding as the closest cell.
        let display_offset = self.terminal().grid().display_offset();
        let point = self.mouse().point(&self.size_info(), display_offset);

        let cell_side = self.mouse().cell_side;

        let selection = match &mut self.terminal_mut().selection {
            Some(selection) => selection,
            None => return,
        };

        selection.ty = selection_type;
        self.update_selection(point, cell_side);

        // Move vi mode cursor to mouse click position.
        if self.terminal().mode().contains(TermMode::VI) && !self.search_active() {
            self.terminal_mut().vi_mode_cursor.point = point;
        }
    }

    /// Get the semantic word at the specified point.
    fn semantic_word(&self, point: Point) -> String {
        let terminal = self.terminal();
        let grid = terminal.grid();

        // Find the next semantic word boundary to the right.
        let mut end = terminal.semantic_search_right(point);

        // Get point at which skipping over semantic characters has led us back to the
        // original character.
        let start_cell = &grid[point];
        let search_end = if start_cell.flags.intersects(Flags::LEADING_WIDE_CHAR_SPACER) {
            point.add(terminal, Boundary::None, 2)
        } else if start_cell.flags.intersects(Flags::WIDE_CHAR) {
            point.add(terminal, Boundary::None, 1)
        } else {
            point
        };

        // Keep moving until we're not on top of a semantic escape character.
        let semantic_chars = terminal.semantic_escape_chars();
        loop {
            let cell = &grid[end];

            // Get cell's character, taking wide characters into account.
            let c = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                grid[end.sub(terminal, Boundary::None, 1)].c
            } else {
                cell.c
            };

            if !semantic_chars.contains(c) {
                break;
            }

            end = terminal.semantic_search_right(end.add(terminal, Boundary::None, 1));

            // Stop if the entire grid is only semantic escape characters.
            if end == search_end {
                return String::new();
            }
        }

        // Find the beginning of the semantic word.
        let start = terminal.semantic_search_left(end);

        terminal.bounds_to_string(start, end)
    }

    /// Handle beginning of terminal text input.
    fn on_terminal_input_start(&mut self) {
        self.on_typing_start();
        self.clear_selection();

        if self.terminal().grid().display_offset() != 0 {
            self.scroll(Scroll::Bottom);
        }
    }

    /// Paste a text into the terminal.
    fn paste(&mut self, text: &str, bracketed: bool) {
        // Multi-line paste confirmation (#18): anything with a newline would
        // start executing in most shells the moment it lands. Search and
        // pending-char inputs are exempt (they consume text locally).
        let goes_to_pty = !self.search_active() && !self.inline_search_state.char_pending;
        if goes_to_pty
            && self.display.nebula_confirm.is_none()
            && (text.contains('\n') || text.contains('\r'))
        {
            let lines = text.lines().count().max(2);
            self.display.nebula_confirm = Some(crate::display::NebulaConfirm::Paste {
                text: text.to_owned(),
                bracketed,
                lines,
            });
            *self.dirty = true;
            return;
        }
        self.paste_now(text, bracketed);
    }

    fn paste_now(&mut self, text: &str, bracketed: bool) {
        if self.search_active() {
            for c in text.chars() {
                self.search_input(c);
            }
        } else if self.inline_search_state.char_pending {
            self.inline_search_input(text);
        } else if bracketed && self.terminal().mode().contains(TermMode::BRACKETED_PASTE) {
            self.on_terminal_input_start();

            self.write_to_pty(&b"\x1b[200~"[..]);

            // Write filtered escape sequences.
            //
            // We remove `\x1b` to ensure it's impossible for the pasted text to write the bracketed
            // paste end escape `\x1b[201~` and `\x03` since some shells incorrectly terminate
            // bracketed paste when they receive it.
            let filtered = text.replace(['\x1b', '\x03'], "");
            self.nebula_input_text(&filtered);
            self.write_to_pty(filtered.into_bytes());

            self.write_to_pty(&b"\x1b[201~"[..]);
        } else {
            self.on_terminal_input_start();

            let payload = if bracketed {
                // In non-bracketed (ie: normal) mode, terminal applications cannot distinguish
                // pasted data from keystrokes.
                //
                // In theory, we should construct the keystrokes needed to produce the data we are
                // pasting... since that's neither practical nor sensible (and probably an
                // impossible task to solve in a general way), we'll just replace line breaks
                // (windows and unix style) with a single carriage return (\r, which is what the
                // Enter key produces).
                text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
            } else {
                // When we explicitly disable bracketed paste don't manipulate with the input,
                // so we pass user input as is.
                text.to_owned().into_bytes()
            };

            if bracketed {
                if let Ok(text) = std::str::from_utf8(&payload) {
                    self.nebula_input_text(text);
                } else {
                    self.nebula_clear_line();
                }
            }
            self.write_to_pty(payload);
        }
    }

    /// Toggle the vi mode status.
    #[inline]
    fn toggle_vi_mode(&mut self) {
        let was_in_vi_mode = self.terminal.mode().contains(TermMode::VI);
        if was_in_vi_mode {
            // If we had search running when leaving Vi mode we should mark terminal fully damaged
            // to cleanup highlighted results.
            if self.search_state.dfas.take().is_some() {
                self.display.damage_tracker.frame().mark_fully_damaged();
            }
        } else {
            self.clear_selection();
        }

        if self.search_active() {
            self.cancel_search();
        }

        // We don't want IME in Vi mode.
        self.window().set_ime_inhibitor(ImeInhibitor::VI, !was_in_vi_mode);

        self.terminal.toggle_vi_mode();

        *self.dirty = true;
    }

    /// Get vi inline search state.
    fn inline_search_state(&mut self) -> &mut InlineSearchState {
        self.inline_search_state
    }

    /// Start vi mode inline search.
    fn start_inline_search(&mut self, direction: Direction, stop_short: bool) {
        self.inline_search_state.stop_short = stop_short;
        self.inline_search_state.direction = direction;
        self.inline_search_state.char_pending = true;
        self.inline_search_state.character = None;
    }

    /// Jump to the next matching character in the line.
    fn inline_search_next(&mut self) {
        let direction = self.inline_search_state.direction;
        self.inline_search(direction);
    }

    /// Jump to the next matching character in the line.
    fn inline_search_previous(&mut self) {
        let direction = self.inline_search_state.direction.opposite();
        self.inline_search(direction);
    }

    /// Process input during inline search.
    fn inline_search_input(&mut self, text: &str) {
        // Ignore input with empty text, like modifier keys.
        let c = match text.chars().next() {
            Some(c) => c,
            None => return,
        };

        self.inline_search_state.char_pending = false;
        self.inline_search_state.character = Some(c);
        self.window().set_ime_inhibitor(ImeInhibitor::VI, true);

        // Immediately move to the captured character.
        self.inline_search_next();
    }

    fn message(&self) -> Option<&Message> {
        self.message_buffer.message()
    }

    fn config(&self) -> &UiConfig {
        self.config
    }

    #[cfg(target_os = "macos")]
    fn event_loop(&self) -> &ActiveEventLoop {
        self.event_loop
    }

    fn clipboard_mut(&mut self) -> &mut Clipboard {
        self.clipboard
    }

    fn scheduler_mut(&mut self) -> &mut Scheduler {
        self.scheduler
    }
}

impl<'a, N: Notify + 'a, T: EventListener> ActionContext<'a, N, T> {
    fn update_search(&mut self) {
        let regex = match self.search_state.regex() {
            Some(regex) => regex,
            None => return,
        };

        // Hide cursor while typing into the search bar.
        if self.config.mouse.hide_when_typing {
            self.display.window.set_mouse_visible(false);
        }

        if regex.is_empty() {
            // Stop search if there's nothing to search for.
            self.search_reset_state();
            self.search_state.dfas = None;
        } else {
            // Create search dfas for the new regex string.
            self.search_state.dfas = RegexSearch::new(regex).ok();

            // Update search highlighting.
            self.goto_match(MAX_SEARCH_WHILE_TYPING);
        }

        *self.dirty = true;
    }

    /// Reset terminal to the state before search was started.
    fn search_reset_state(&mut self) {
        // Unschedule pending timers.
        let timer_id = TimerId::new(Topic::DelayedSearch, self.display.window.id());
        self.scheduler.unschedule(timer_id);

        // Clear focused match.
        self.search_state.focused_match = None;

        // The viewport reset logic is only needed for vi mode, since without it our origin is
        // always at the current display offset instead of at the vi cursor position which we need
        // to recover to.
        if !self.terminal.mode().contains(TermMode::VI) {
            return;
        }

        // Reset display offset and cursor position.
        self.terminal.vi_mode_cursor.point = self.search_state.origin;
        self.terminal.scroll_display(Scroll::Delta(self.search_state.display_offset_delta));
        self.search_state.display_offset_delta = 0;

        *self.dirty = true;
    }

    /// Jump to the first regex match from the search origin.
    fn goto_match(&mut self, mut limit: Option<usize>) {
        let dfas = match &mut self.search_state.dfas {
            Some(dfas) => dfas,
            None => return,
        };

        // Limit search only when enough lines are available to run into the limit.
        limit = limit.filter(|&limit| limit <= self.terminal.total_lines());

        // Jump to the next match.
        let direction = self.search_state.direction;
        let clamped_origin = self.search_state.origin.grid_clamp(self.terminal, Boundary::Grid);
        match self.terminal.search_next(dfas, clamped_origin, direction, Side::Left, limit) {
            Some(regex_match) => {
                let old_offset = self.terminal.grid().display_offset() as i32;

                if self.terminal.mode().contains(TermMode::VI) {
                    // Move vi cursor to the start of the match.
                    self.terminal.vi_goto_point(*regex_match.start());
                } else {
                    // Select the match when vi mode is not active.
                    self.terminal.scroll_to_point(*regex_match.start());
                }

                // Update the focused match.
                self.search_state.focused_match = Some(regex_match);

                // Store number of lines the viewport had to be moved.
                let display_offset = self.terminal.grid().display_offset();
                self.search_state.display_offset_delta += old_offset - display_offset as i32;

                // Since we found a result, we require no delayed re-search.
                let timer_id = TimerId::new(Topic::DelayedSearch, self.display.window.id());
                self.scheduler.unschedule(timer_id);
            },
            // Reset viewport only when we know there is no match, to prevent unnecessary jumping.
            None if limit.is_none() => self.search_reset_state(),
            None => {
                // Schedule delayed search if we ran into our search limit.
                let timer_id = TimerId::new(Topic::DelayedSearch, self.display.window.id());
                if !self.scheduler.scheduled(timer_id) {
                    let event = Event::new(EventType::SearchNext, self.display.window.id());
                    self.scheduler.schedule(event, TYPING_SEARCH_DELAY, false, timer_id);
                }

                // Clear focused match.
                self.search_state.focused_match = None;
            },
        }

        *self.dirty = true;
    }

    /// Cleanup the search state.
    fn exit_search(&mut self) {
        let vi_mode = self.terminal.mode().contains(TermMode::VI);
        self.window().set_ime_inhibitor(ImeInhibitor::VI, vi_mode);

        self.display.damage_tracker.frame().mark_fully_damaged();
        self.display.pending_update.dirty = true;
        self.search_state.history_index = None;

        // Clear focused match.
        self.search_state.focused_match = None;
    }

    /// Update the cursor blinking state.
    fn update_cursor_blinking(&mut self) {
        // Get config cursor style.
        let mut cursor_style = self.config.cursor.style;
        let vi_mode = self.terminal.mode().contains(TermMode::VI);
        if vi_mode {
            cursor_style = self.config.cursor.vi_mode_style.unwrap_or(cursor_style);
        }

        // Check terminal cursor style.
        let terminal_blinking = self.terminal.cursor_style().blinking;
        let mut blinking = cursor_style.blinking_override().unwrap_or(terminal_blinking);
        blinking &= (vi_mode || self.terminal().mode().contains(TermMode::SHOW_CURSOR))
            && self.display().ime.preedit().is_none();

        // Update cursor blinking state.
        let window_id = self.display.window.id();
        self.scheduler.unschedule(TimerId::new(Topic::BlinkCursor, window_id));
        self.scheduler.unschedule(TimerId::new(Topic::BlinkTimeout, window_id));

        // Reset blinking timeout.
        *self.cursor_blink_timed_out = false;

        if blinking && self.terminal.is_focused {
            self.schedule_blinking();
            self.schedule_blinking_timeout();
        } else {
            self.display.cursor_hidden = false;
            *self.dirty = true;
        }
    }

    fn schedule_blinking(&mut self) {
        let window_id = self.display.window.id();
        let timer_id = TimerId::new(Topic::BlinkCursor, window_id);
        let event = Event::new(EventType::BlinkCursor, window_id);
        let blinking_interval = Duration::from_millis(self.config.cursor.blink_interval());
        self.scheduler.schedule(event, blinking_interval, true, timer_id);
    }

    fn schedule_blinking_timeout(&mut self) {
        let blinking_timeout = self.config.cursor.blink_timeout();
        if blinking_timeout == Duration::ZERO {
            return;
        }

        let window_id = self.display.window.id();
        let event = Event::new(EventType::BlinkCursorTimeout, window_id);
        let timer_id = TimerId::new(Topic::BlinkTimeout, window_id);

        self.scheduler.schedule(event, blinking_timeout, false, timer_id);
    }

    /// Perform vi mode inline search in the specified direction.
    fn inline_search(&mut self, direction: Direction) {
        let c = match self.inline_search_state.character {
            Some(c) => c,
            None => return,
        };
        let mut buf = [0; 4];
        let search_character = c.encode_utf8(&mut buf);

        // Find next match in this line.
        let vi_point = self.terminal.vi_mode_cursor.point;
        let point = match direction {
            Direction::Right => self.terminal.inline_search_right(vi_point, search_character),
            Direction::Left => self.terminal.inline_search_left(vi_point, search_character),
        };

        // Jump to point if there's a match.
        if let Ok(mut point) = point {
            if self.inline_search_state.stop_short {
                let grid = self.terminal.grid();
                point = match direction {
                    Direction::Right => {
                        grid.iter_from(point).prev().map_or(point, |cell| cell.point)
                    },
                    Direction::Left => {
                        grid.iter_from(point).next().map_or(point, |cell| cell.point)
                    },
                };
            }

            self.terminal.vi_goto_point(point);
            self.mark_dirty();
        }
    }
}

/// Identified purpose of the touch input.
#[derive(Default, Debug)]
pub enum TouchPurpose {
    #[default]
    None,
    Select(TouchEvent),
    Scroll(TouchEvent),
    Zoom(TouchZoom),
    ZoomPendingSlot(TouchEvent),
    Tap(TouchEvent),
    Invalid(HashSet<u64, RandomState>),
}

/// Touch zooming state.
#[derive(Debug)]
pub struct TouchZoom {
    slots: (TouchEvent, TouchEvent),
    fractions: f32,
}

impl TouchZoom {
    pub fn new(slots: (TouchEvent, TouchEvent)) -> Self {
        Self { slots, fractions: Default::default() }
    }

    /// Get slot distance change since last update.
    pub fn font_delta(&mut self, slot: TouchEvent) -> f32 {
        let old_distance = self.distance();

        // Update touch slots.
        if slot.id == self.slots.0.id {
            self.slots.0 = slot;
        } else {
            self.slots.1 = slot;
        }

        // Calculate font change in `FONT_SIZE_STEP` increments.
        let delta = (self.distance() - old_distance) * TOUCH_ZOOM_FACTOR + self.fractions;
        let font_delta = (delta.abs() / FONT_SIZE_STEP).floor() * FONT_SIZE_STEP * delta.signum();
        self.fractions = delta - font_delta;

        font_delta
    }

    /// Get active touch slots.
    pub fn slots(&self) -> (TouchEvent, TouchEvent) {
        self.slots
    }

    /// Calculate distance between slots.
    fn distance(&self) -> f32 {
        let delta_x = self.slots.0.location.x - self.slots.1.location.x;
        let delta_y = self.slots.0.location.y - self.slots.1.location.y;
        delta_x.hypot(delta_y) as f32
    }
}

/// State of the mouse.
#[derive(Debug)]
pub struct Mouse {
    pub left_button_state: ElementState,
    pub middle_button_state: ElementState,
    pub right_button_state: ElementState,
    pub last_click_timestamp: Instant,
    pub last_click_button: MouseButton,
    /// Pixel where the last press landed, for multi-click detection: a press
    /// only upgrades to double/triple when it lands within half a cell of the
    /// previous one (Windows Terminal's `_numberOfClicks` distance gate).
    pub last_click_pos: (usize, usize),
    pub click_state: ClickState,
    pub accumulated_scroll: AccumulatedScroll,
    pub cell_side: Side,
    pub block_hint_launcher: bool,
    pub hint_highlight_dirty: bool,
    pub inside_text_area: bool,
    /// Pixel where the last left press landed. Drag-selection engages only
    /// once the pointer travels a threshold away from here, so a plain
    /// click (with sub-pixel jitter) never leaves a stray selection behind.
    pub drag_origin: Option<(usize, usize)>,
    /// Whether the current press crossed the drag threshold.
    pub drag_active: bool,
    /// Selection armed by the left press but not yet started (Windows
    /// Terminal model: a click never creates a selection — only a drag past
    /// the threshold does). Holds the would-be type/anchor so `mouse_moved`
    /// can start the selection from the ORIGINAL press cell once the pointer
    /// commits to a drag. Cleared on release.
    pub pending_selection: Option<(nebula_terminal::selection::SelectionType, Point, Side)>,
    /// Correlates pointer diagnostics across press, motion, selection, and release.
    pub debug_press_id: u64,
    /// Limits selection-update diagnostics without losing the drag's progression.
    pub debug_selection_updates: u32,
    /// Prevents a promoted tab drag from logging once per pointer-motion event.
    pub debug_tab_drag_logged: bool,
    pub x: usize,
    pub y: usize,
}

impl Default for Mouse {
    fn default() -> Mouse {
        Mouse {
            last_click_timestamp: Instant::now(),
            last_click_button: MouseButton::Left,
            last_click_pos: (0, 0),
            left_button_state: ElementState::Released,
            middle_button_state: ElementState::Released,
            right_button_state: ElementState::Released,
            click_state: ClickState::None,
            cell_side: Side::Left,
            hint_highlight_dirty: Default::default(),
            block_hint_launcher: Default::default(),
            inside_text_area: Default::default(),
            accumulated_scroll: Default::default(),
            drag_origin: Default::default(),
            drag_active: Default::default(),
            pending_selection: Default::default(),
            debug_press_id: Default::default(),
            debug_selection_updates: Default::default(),
            debug_tab_drag_logged: Default::default(),
            x: Default::default(),
            y: Default::default(),
        }
    }
}

impl Mouse {
    /// Convert mouse pixel coordinates to viewport point.
    ///
    /// If the coordinates are outside of the terminal grid, like positions inside the padding, the
    /// coordinates will be clamped to the closest grid coordinates.
    #[inline]
    pub fn point(&self, size: &SizeInfo, display_offset: usize) -> Point {
        let col = self.x.saturating_sub(size.padding_x() as usize) / (size.cell_width() as usize);
        let col = min(Column(col), size.last_column());

        let line = self.y.saturating_sub(size.padding_y() as usize) / (size.cell_height() as usize);
        let line = min(line, size.bottommost_line().0 as usize);

        term::viewport_to_point(display_offset, Point::new(line, col))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClickState {
    None,
    Click,
    DoubleClick,
    TripleClick,
}

/// The amount of scroll accumulated from the pointer events.
#[derive(Default, Debug)]
pub struct AccumulatedScroll {
    /// Scroll we should perform along `x` axis.
    pub x: f64,

    /// Scroll we should perform along `y` axis.
    pub y: f64,
}

impl input::Processor<EventProxy, ActionContext<'_, Notifier, EventProxy>> {
    /// Handle events from winit.
    pub fn handle_event(&mut self, event: WinitEvent<Event>) {
        match event {
            WinitEvent::UserEvent(Event { payload, tab_id, .. }) => match payload {
                EventType::SearchNext => self.ctx.goto_match(None),
                // Tab requests are handled at the window-context level.
                EventType::NebulaTab(_) => (),
                // Clock ticks are handled at the window-context level.
                EventType::NebulaTick | EventType::NebulaAttach => (),
                // Resize settling is handled at the window-context level.
                EventType::NebulaResizeSettled
                | EventType::SshDeleteUndoExpired
                | EventType::SftpUpdated => (),
                // AI hook events are handled at the Processor level (they may
                // target any window's pane); FocusWindow likewise.
                EventType::AiHook(_) | EventType::FocusWindow { .. } => (),
                EventType::Scroll(scroll) => self.ctx.scroll(scroll),
                EventType::BlinkCursor => {
                    // Only change state when timeout isn't reached, since we could get
                    // BlinkCursor and BlinkCursorTimeout events at the same time.
                    if !*self.ctx.cursor_blink_timed_out {
                        self.ctx.display.cursor_hidden ^= true;
                        *self.ctx.dirty = true;
                    }
                },
                EventType::BlinkCursorTimeout => {
                    // Disable blinking after timeout reached.
                    let timer_id = TimerId::new(Topic::BlinkCursor, self.ctx.display.window.id());
                    self.ctx.scheduler.unschedule(timer_id);
                    *self.ctx.cursor_blink_timed_out = true;
                    self.ctx.display.cursor_hidden = false;
                    *self.ctx.dirty = true;
                },
                // Add message only if it's not already queued.
                EventType::Message(message) if !self.ctx.message_buffer.is_queued(&message) => {
                    self.ctx.message_buffer.push(message);
                    self.ctx.display.pending_update.dirty = true;
                },
                EventType::Terminal(event) => match event {
                    TerminalEvent::Title(title) => {
                        // Nebula encodes cwd/branch in a `NEBULA|cwd|branch` title
                        // for the glass powerline instead of the window title. A
                        // remote `nebula ssh` shell appends a 4th `program` field
                        // (`NEBULA|cwd|branch|program`): the local screen-scrape
                        // that normally feeds `running_program` can't see through
                        // the SSH pipe, so the remote reports the program identity
                        // here instead — empty at the prompt, the command name
                        // while one runs. A local shell sends only 3 fields, so
                        // the 4th is absent and `running_program` is left to the
                        // existing OSC-133;C/last_committed path untouched.
                        if let Some(rest) = title.strip_prefix("NEBULA|") {
                            let mut parts = rest.splitn(3, '|');
                            self.ctx.nebula_state.cwd = parts.next().unwrap_or("").to_owned();
                            self.ctx.nebula_state.branch = parts.next().unwrap_or("").to_owned();
                            if let Some(program) = parts.next() {
                                self.ctx.nebula_state.running_program = if program.is_empty() {
                                    None
                                } else {
                                    Some(program.to_owned())
                                };
                                // A 4-field title only ever comes from the
                                // remote `nebula ssh` integration, so the
                                // typed ssh login is confirmed connected:
                                // save its destination to the sidebar now
                                // instead of waiting out SAVE_MIN_SESSION.
                                if let Some(host) = self.ctx.nebula_state.pending_ssh_host.take() {
                                    self.ctx.display.nebula_save_ssh_host(&host);
                                }
                            }
                            *self.ctx.dirty = true;
                        } else {
                            // A non-NEBULA title while a command is in flight
                            // can only come from the program on the PTY — the
                            // local shell integration only retitles at its
                            // prompt. For a typed `ssh` login that means the
                            // remote shell is up (Ubuntu-style PS1 retitles on
                            // login): confirm and save the destination without
                            // waiting for the session to end.
                            if self.ctx.nebula_state.command_started.is_some() {
                                if let Some(host) = self.ctx.nebula_state.pending_ssh_host.take() {
                                    self.ctx.display.nebula_save_ssh_host(&host);
                                    *self.ctx.dirty = true;
                                }
                            }
                            if !self.ctx.preserve_title && self.ctx.config.window.dynamic_title {
                                self.ctx.window().set_title(title);
                            }
                        }
                    },
                    TerminalEvent::ResetTitle => {
                        let window_config = &self.ctx.config.window;
                        if !self.ctx.preserve_title && window_config.dynamic_title {
                            self.ctx.display.window.set_title(window_config.identity.title.clone());
                        }
                    },
                    TerminalEvent::CwdReport(cwd) => {
                        // Standard OSC 7 / 9;9 directory report. Update cwd only,
                        // leaving any branch captured from a `NEBULA|cwd|branch`
                        // title intact, so the two channels coexist.
                        if self.ctx.nebula_state.cwd != cwd {
                            self.ctx.nebula_state.cwd = cwd;
                            *self.ctx.dirty = true;
                        }
                    },
                    TerminalEvent::InlineImage { png, abs_line, width, height } => {
                        // Decode off the PTY thread (here, on the UI loop) and
                        // anchor the pixels to the pane. Textures upload lazily
                        // on first draw.
                        match crate::renderer::image::decode_png_bytes(&png) {
                            Ok((px_w, px_h, rgba)) => {
                                use std::sync::atomic::{AtomicU64, Ordering};
                                static NEXT_INLINE_IMAGE_ID: AtomicU64 = AtomicU64::new(1);
                                let id = NEXT_INLINE_IMAGE_ID.fetch_add(1, Ordering::Relaxed);
                                let images = &mut self.ctx.nebula_state.inline_images;
                                images.push(crate::display::NebulaInlineImage {
                                    id,
                                    abs_line,
                                    width,
                                    height,
                                    rgba: std::sync::Arc::new(rgba),
                                    px_w,
                                    px_h,
                                });
                                // VRAM/heap guard against imgcat runaway loops.
                                if images.len() > 16 {
                                    images.remove(0);
                                }
                                *self.ctx.dirty = true;
                            },
                            Err(err) => {
                                warn!("inline image decode failed: {err}");
                            },
                        }
                    },
                    TerminalEvent::CommandStart => {
                        self.ctx.nebula_state.command_started = Some(Instant::now());
                        // Program identity for the sidebar tab icon, from the
                        // line captured at Enter (buffers are cleared by now).
                        self.ctx.nebula_state.running_program =
                            crate::display::extract_program(&self.ctx.nebula_state.last_committed);
                        // Arm the ssh host auto-save: when this command is an
                        // interactive ssh login, hold its destination until a
                        // remote NEBULA| title or a long-enough session
                        // (CommandDone) confirms the connection was real.
                        self.ctx.nebula_state.pending_ssh_host =
                            crate::ssh::ssh_destination(&self.ctx.nebula_state.last_committed);
                        self.ctx.nebula_state.awaiting_input = false;
                    },
                    TerminalEvent::CommandDone => {
                        // Take (not just clear) the program: the toast below
                        // names it, and reading the field after the reset used
                        // to hand the toast a permanent `None`.
                        let program = self.ctx.nebula_state.running_program.take();
                        let pending_ssh = self.ctx.nebula_state.pending_ssh_host.take();
                        self.ctx.nebula_state.awaiting_input = false;
                        // Long commands (npm/cargo builds...) notify when the
                        // window is in the background; quick ones stay silent.
                        if let Some(started) = self.ctx.nebula_state.command_started.take() {
                            let duration = started.elapsed();
                            // An ssh session that lived this long was a real
                            // connection even without the remote integration's
                            // NEBULA| title: save the host to the sidebar.
                            if duration >= crate::ssh::SAVE_MIN_SESSION {
                                if let Some(host) = pending_ssh {
                                    self.ctx.display.nebula_save_ssh_host(&host);
                                }
                            }
                            if duration >= crate::notify::COMMAND_NOTIFY_MIN {
                                // Sidebar dot until the tab gets looked at
                                // (cleared instantly for the visible tab).
                                self.ctx.nebula_state.finished_unseen = true;
                                if !self.ctx.display.window.has_focus() {
                                    crate::notify::deliver(
                                        &self.ctx.display.window,
                                        &crate::notify::Notification::CommandDone {
                                            duration,
                                            program,
                                        },
                                        tab_id,
                                    );
                                }
                            }
                        }
                    },
                    TerminalEvent::Notify(body) => {
                        // Program-initiated (OSC 9) notifications only matter
                        // when the user isn't already looking at the pane.
                        if !self.ctx.display.window.has_focus() {
                            crate::notify::deliver(
                                &self.ctx.display.window,
                                &crate::notify::Notification::Text {
                                    body,
                                    program: self.ctx.nebula_state.running_program.clone(),
                                },
                                tab_id,
                            );
                        }
                    },
                    TerminalEvent::AiHookEnvelope(_) => (),
                    TerminalEvent::Bell => {
                        // Claude Code / Codex ring BEL when a turn finishes, so
                        // an unfocused bell is the primary "AI task done"
                        // signal: always request attention + sound, without
                        // gating on the (rarely set) URGENCY_HINTS mode.
                        //
                        // CRITICAL: Query the window's CURRENT focus state directly
                        // via winit, not the cached terminal.is_focused flag. The
                        // cached flag is updated by WindowEvent::Focused, which may
                        // arrive AFTER the BEL if the user switches windows quickly.
                        if !self.ctx.display.window.has_focus() {
                            crate::notify::deliver(
                                &self.ctx.display.window,
                                &crate::notify::Notification::Bell {
                                    program: self.ctx.nebula_state.running_program.clone(),
                                },
                                tab_id,
                            );
                        }

                        // A bell from a tracked program (claude finishing a
                        // turn) means it now waits for input: pause the
                        // sidebar spinner until the user types again.
                        if self.ctx.nebula_state.running_program.is_some() {
                            self.ctx.nebula_state.awaiting_input = true;
                        }

                        // Ring visual bell.
                        self.ctx.display.visual_bell.ring();

                        // Execute bell command.
                        if let Some(bell_command) = &self.ctx.config.bell.command {
                            if self
                                .ctx
                                .prev_bell_cmd
                                .is_none_or(|i| i.elapsed() >= BELL_CMD_COOLDOWN)
                            {
                                self.ctx.spawn_daemon(bell_command.program(), bell_command.args());

                                *self.ctx.prev_bell_cmd = Some(Instant::now());
                            }
                        }
                    },
                    TerminalEvent::ClipboardStore(clipboard_type, content) => {
                        if self.ctx.terminal.is_focused {
                            self.ctx.clipboard.store(clipboard_type, content);
                        }
                    },
                    TerminalEvent::ClipboardLoad(clipboard_type, format) => {
                        if self.ctx.terminal.is_focused {
                            let text = format(self.ctx.clipboard.load(clipboard_type).as_str());
                            self.ctx.write_to_pty(text.into_bytes());
                        }
                    },
                    TerminalEvent::ColorRequest(index, format) => {
                        if crate::display::replays_untrusted_terminal_output(
                            &self.ctx.nebula_state.last_committed,
                        ) {
                            return;
                        }
                        let color = match self.ctx.terminal().colors()[index] {
                            Some(color) => Rgb(color),
                            // Ignore cursor color requests unless it was changed.
                            None if index == NamedColor::Cursor as usize => return,
                            None => self.ctx.display.colors[index],
                        };
                        self.ctx.write_to_pty(format(color.0).into_bytes());
                    },
                    TerminalEvent::TextAreaSizeRequest(format) => {
                        let text = format(self.ctx.size_info().into());
                        self.ctx.write_to_pty(text.into_bytes());
                    },
                    TerminalEvent::PtyWrite(text) => self.ctx.write_to_pty(text.into_bytes()),
                    TerminalEvent::MouseCursorDirty => self.reset_mouse_cursor(),
                    TerminalEvent::CursorBlinkingChange => self.ctx.update_cursor_blinking(),
                    TerminalEvent::Exit | TerminalEvent::ChildExit(_) | TerminalEvent::Wakeup => (),
                },
                #[cfg(unix)]
                EventType::IpcConfig(_) | EventType::IpcGetConfig(..) | EventType::Shutdown => (),
                EventType::Message(_)
                | EventType::ConfigReload(_)
                | EventType::ConfigReloadReady
                | EventType::CreateWindow(_)
                | EventType::Frame => (),
            },
            WinitEvent::WindowEvent { event, .. } => {
                match event {
                    WindowEvent::CloseRequested => {
                        // User asked to close the window, so no need to hold it.
                        // This is a window-level action: close every tab/pane at once,
                        // not only the currently focused PTY.
                        self.ctx.window().hold = false;
                        self.ctx.nebula_tab(TabRequest::CloseWindow);
                    },
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let old_scale_factor =
                            mem::replace(&mut self.ctx.window().scale_factor, scale_factor);

                        // Rescale font size for the new factor.
                        let font_scale = scale_factor as f32 / old_scale_factor as f32;
                        self.ctx.display.font_size = self.ctx.display.font_size.scale(font_scale);

                        let font = self.ctx.display.effective_font(&self.ctx.config.font);
                        let font_size = self.ctx.display.font_size;
                        self.ctx.display.pending_update.set_font(font.with_size(font_size));
                    },
                    WindowEvent::Resized(size) => {
                        // Ignore unreasonably small resizes. A borderless window on
                        // Windows reports a tiny size (~237x39) when minimized instead
                        // of 0x0; honoring it would collapse the terminal grid to a
                        // single row and lose the visible content on restore.
                        if size.width < 100 || size.height < 100 {
                            return;
                        }

                        self.ctx.display.pending_update.set_dimensions(size);
                    },
                    WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                        // mouse-hide-while-typing: hide the cursor on any key
                        // press; any mouse movement/click/wheel below shows it
                        // again. Hide the pointer while typing.
                        if self.ctx.config.mouse.hide_when_typing
                            && event.state == ElementState::Pressed
                        {
                            self.ctx.window().set_mouse_visible(false);
                        }
                        self.key_input(event);
                    },
                    WindowEvent::ModifiersChanged(modifiers) => self.modifiers_input(modifiers),
                    WindowEvent::MouseInput { state, button, .. } => {
                        self.ctx.window().set_mouse_visible(true);
                        self.mouse_input(state, button);
                    },
                    WindowEvent::CursorMoved { position, .. } => {
                        self.ctx.window().set_mouse_visible(true);
                        self.mouse_moved(position);
                    },
                    WindowEvent::MouseWheel { delta, phase, .. } => {
                        self.ctx.window().set_mouse_visible(true);
                        self.mouse_wheel_input(delta, phase);
                    },
                    WindowEvent::Touch(touch) => self.touch(touch),
                    WindowEvent::Focused(is_focused) => {
                        log::info!("WindowEvent::Focused({})", is_focused);
                        self.ctx.terminal.is_focused = is_focused;

                        // Losing window focus ends any chrome text editing —
                        // a rename box left open under another window reads
                        // as a hang (its caret froze), and stray keystrokes
                        // later would edit a name the user forgot about.
                        if !is_focused {
                            if self.ctx.display.nebula_tab_rename.take().is_some() {
                                self.ctx.display.nebula_tab_rename_select_all = false;
                            }
                            let panel = &mut self.ctx.display.nebula_side_panel;
                            panel.search_unfocus(false);
                            panel.commit_unfocus();
                            if let Some(panel) = self.ctx.display.nebula_sftp_panel.as_mut() {
                                panel.editor_unfocus();
                            }
                        }

                        // Nebula: always redraw on focus change, and clear the
                        // occluded flag when refocused. On Windows `Occluded(false)`
                        // is unreliable, so without this the draw path stays gated
                        // off and terminal content vanishes after backgrounding.
                        *self.ctx.dirty = true;
                        if is_focused {
                            *self.ctx.occluded = false;
                            // Bypass frame throttling and force an immediate
                            // repaint; otherwise content stays blank after the
                            // window returns from the background on Windows.
                            self.ctx.window().has_frame = true;
                            self.ctx.window().request_redraw();
                            self.ctx.window().set_urgent(false);
                        }

                        self.ctx.update_cursor_blinking();
                        self.on_focus_change(is_focused);

                        // Ensure IME is disabled while unfocused.
                        self.ctx.window().set_ime_inhibitor(ImeInhibitor::FOCUS, !is_focused);
                    },
                    WindowEvent::Occluded(occluded) => {
                        *self.ctx.occluded = occluded;

                        // Force a full redraw when the window becomes visible again.
                        if !occluded {
                            *self.ctx.dirty = true;
                        }
                    },
                    WindowEvent::DroppedFile(path) => {
                        let path: String = path.to_string_lossy().into();
                        self.ctx.paste(&(path + " "), true);
                    },
                    WindowEvent::CursorLeft { .. } => {
                        self.ctx.mouse.inside_text_area = false;
                        self.ctx.display().set_chrome_hover(
                            crate::display::ChromeHit::None,
                            crate::display::SettingsHit::None,
                        );

                        if self.ctx.display().highlighted_hint.is_some() {
                            *self.ctx.dirty = true;
                        }
                    },
                    WindowEvent::Ime(ime) => {
                        match ime {
                            Ime::Commit(text) => {
                                *self.ctx.dirty = true;
                                // Tab rename owns committed text while editing: on
                                // Windows (and any IME), printable characters are
                                // delivered here, NOT through key_input — so the
                                // rename buffer must consume them here or typing
                                // silently pastes into the shell behind the box.
                                if self.ctx.display.nebula_tab_rename.is_some() {
                                    // Caret-aware insert (type-to-overwrite on a
                                    // pending select-all) — same code path as the
                                    // non-IME keyboard fallback.
                                    self.ctx.display.tab_rename_insert(&text);
                                } else if self.ctx.display.nebula_sftp_panel.as_ref().is_some_and(
                                    crate::display::sftp_panel::SftpPanel::editor_active,
                                ) {
                                    if let Some(panel) = self.ctx.display.nebula_sftp_panel.as_mut()
                                    {
                                        panel.editor_insert(&text);
                                    }
                                } else if self.ctx.display.nebula_side_panel.search_focus {
                                    // Side-panel filter box: same IME contract as
                                    // tab rename — committed text must land in the
                                    // box, not paste into the shell behind it.
                                    self.ctx.display.nebula_side_panel.search_input(&text);
                                } else if self.ctx.display.nebula_side_panel.commit_focus {
                                    self.ctx.display.nebula_side_panel.commit_input(&text);
                                } else if self.ctx.display.nebula_ssh_editor.is_some() {
                                    self.ctx.display.ssh_editor_insert(&text);
                                } else if self.ctx.display.command_palette_open() {
                                    self.ctx.display.palette_input_text(&text);
                                } else {
                                    // Don't use bracketed paste for single char input.
                                    self.ctx.paste(&text, text.chars().count() > 1);
                                }
                                self.ctx.update_cursor_blinking();
                            },
                            Ime::Preedit(text, cursor_offset) => {
                                let preedit =
                                    (!text.is_empty()).then(|| Preedit::new(text, cursor_offset));

                                if self.ctx.display.ime.preedit() != preedit.as_ref() {
                                    self.ctx.display.ime.set_preedit(preedit);
                                    self.ctx.update_cursor_blinking();
                                    *self.ctx.dirty = true;
                                }
                            },
                            Ime::Enabled => {
                                self.ctx.display.ime.set_enabled(true);
                                *self.ctx.dirty = true;
                            },
                            Ime::Disabled => {
                                self.ctx.display.ime.set_enabled(false);
                                *self.ctx.dirty = true;
                            },
                        }
                    },
                    WindowEvent::ThemeChanged(theme) => {
                        self.ctx.display.system_theme_changed(theme);
                        *self.ctx.dirty = true;
                    },
                    WindowEvent::KeyboardInput { is_synthetic: true, .. }
                    | WindowEvent::ActivationTokenDone { .. }
                    | WindowEvent::DoubleTapGesture { .. }
                    | WindowEvent::TouchpadPressure { .. }
                    | WindowEvent::RotationGesture { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::PinchGesture { .. }
                    | WindowEvent::AxisMotion { .. }
                    | WindowEvent::PanGesture { .. }
                    | WindowEvent::HoveredFileCancelled
                    | WindowEvent::Destroyed
                    | WindowEvent::HoveredFile(_)
                    | WindowEvent::RedrawRequested
                    | WindowEvent::Moved(_) => (),
                }
            },
            WinitEvent::Suspended
            | WinitEvent::NewEvents { .. }
            | WinitEvent::DeviceEvent { .. }
            | WinitEvent::LoopExiting
            | WinitEvent::Resumed
            | WinitEvent::MemoryWarning
            | WinitEvent::AboutToWait => (),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<Event>,
    /// Routing target: which window this proxy's events address, as a raw
    /// `WindowId` value. Shared with the owning pane (see
    /// [`crate::window_context::Pane`]) so a re-attach can re-point every
    /// clone (Term, PTY I/O loop) at the new window in one atomic store.
    window_id: Arc<AtomicU64>,
    tab_id: Option<u64>,
}

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<Event>, window_id: WindowId) -> Self {
        Self { proxy, window_id: Arc::new(AtomicU64::new(window_id.into())), tab_id: None }
    }

    /// Event proxy bound to a specific Nebula tab. `route` is shared with the
    /// pane, so detached panes can be re-pointed at an adopting window.
    pub fn new_tab(proxy: EventLoopProxy<Event>, route: Arc<AtomicU64>, tab_id: u64) -> Self {
        Self { proxy, window_id: route, tab_id: Some(tab_id) }
    }

    /// Current routing target.
    fn target(&self) -> WindowId {
        WindowId::from(self.window_id.load(Ordering::Relaxed))
    }

    /// Send an event to the event loop.
    pub fn send_event(&self, event: EventType) {
        let _ = self.proxy.send_event(Event {
            window_id: Some(self.target()),
            tab_id: self.tab_id,
            payload: event,
        });
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TerminalEvent) {
        if let TerminalEvent::AiHookEnvelope(envelope) = event {
            if let Some(hook) = crate::ai_hook::parse_remote_envelope(&envelope, self.tab_id) {
                self.send_event(EventType::AiHook(hook));
            }
            return;
        }
        let _ = self.proxy.send_event(Event {
            window_id: Some(self.target()),
            tab_id: self.tab_id,
            payload: event.into(),
        });
    }
}
