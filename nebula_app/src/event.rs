//! Process window events.

use crate::ConfigMonitor;
use glutin::config::GetGlConfig;
use std::borrow::Cow;
use std::cmp::min;
use std::collections::HashMap;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::Debug;
#[cfg(not(windows))]
use std::os::unix::io::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{env, f32, mem};

use ahash::RandomState;
use crossfont::Size as FontSize;
use glutin::config::Config as GlutinConfig;
use glutin::display::GetGlDisplay;
use log::{debug, error, info, warn};
use serde_json::Value;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event as WinitEvent, Ime, Modifiers, StartCause, WindowEvent};
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
use nebula_terminal::term::{ClipboardType, Term, TermMode};
use nebula_terminal::vte::ansi::NamedColor;

#[cfg(unix)]
use crate::cli::ParsedOptions;
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
use crate::display::{Display, Preedit, SizeInfo, ToastKind, UiLanguage};
use crate::input::{self, ActionContext as _};
use crate::logging::{LOG_TARGET_CONFIG, LOG_TARGET_WINIT};
use crate::message_bar::{Message, MessageBuffer, MessageType};
#[cfg(unix)]
use crate::polling::ipc::{self, SocketReply};
use crate::runtime_api::{ApiError, RuntimeCommand, RuntimeDispatch, RuntimeHub, RuntimeSnapshot};
use crate::scheduler::{Scheduler, TimerId, Topic};
use crate::window_context::{DetachedWindow, WindowBoot, WindowContext};
use crate::window_transition::{NativeWindowStage, NativeWindowStageTracker};

mod agent_runtime;
mod input_state;
mod proxy;
mod search_state;
mod types;

pub use input_state::{ClickState, Mouse, TouchPurpose, TouchZoom};
pub use proxy::EventProxy;
pub use search_state::{InlineSearchState, SearchState};
pub use types::{Event, EventType, TabRequest};

/// Duration after the last user input until an unlimited search is performed.
pub const TYPING_SEARCH_DELAY: Duration = Duration::from_millis(500);

/// Maximum number of lines for the blocking search while still typing the search regex.
const MAX_SEARCH_WHILE_TYPING: Option<usize> = Some(1000);

/// Maximum number of search terms stored in the history.
const MAX_SEARCH_HISTORY_SIZE: usize = 255;

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
    native_window_stages: NativeWindowStageTracker,
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
    /// Registered quick-terminal toggle hotkey and the persisted spelling shown
    /// in settings. Keeping the full value lets failed replacements restore it.
    quick_hotkey: Option<HotKey>,
    quick_hotkey_combo: String,
    /// Tabs of closed windows kept alive for re-attach (multiplexer-style): their
    /// PTYs never stopped, so `claude` and friends survive the window. LIFO —
    /// an attach request adopts the most recently closed window first.
    detached: Vec<DetachedWindow>,
    /// Canonical state projection observed by CLI clients and subscribers.
    runtime_hub: RuntimeHub,
}

impl Processor {
    /// Create a new event processor.
    pub fn new(
        loaded_config: config::LoadedConfig,
        cli_options: CliOptions,
        event_loop: &EventLoop<Event>,
        native_window_stages: NativeWindowStageTracker,
        runtime_hub: RuntimeHub,
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

        // Register the persisted global quick-terminal toggle hotkey before any
        // window is shown. Invalid hand-edited values fall back to the default.
        let quick_hotkey_combo = crate::display::quick_terminal_hotkey_from_settings(&config);
        let (global_hotkey, quick_hotkey) = Self::init_quick_hotkey(&quick_hotkey_combo);

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
            native_window_stages,
            #[cfg(unix)]
            global_ipc_options: Default::default(),
            config_monitor,
            quick_terminal: None,
            quick_visible: false,
            quick_motion: crate::motion::Tween::new(1.0),
            quick_motion_clock: crate::motion::MotionClock::default(),
            global_hotkey,
            quick_hotkey,
            quick_hotkey_combo,
            detached: Vec::new(),
            runtime_hub,
        }
    }

    /// Apply native move/resize stages before handling the next winit event.
    /// The native hook only records these two low-frequency markers; all
    /// window scans and state changes stay on the normal application path.
    fn drain_native_window_stages(&mut self) {
        self.native_window_stages.drain(|event| {
            for window_context in self.windows.values_mut() {
                if window_context.display.window.native_window_handle_id() != Some(event.hwnd) {
                    continue;
                }

                match event.stage {
                    NativeWindowStage::EnterSizeMove => {
                        crate::display::nebula_debug_log("winmove enter_size_move");
                        window_context.display.window.set_native_live_move(true);
                    },
                    NativeWindowStage::ExitSizeMove => {
                        crate::display::nebula_debug_log("winmove exit_size_move");
                        window_context.display.window.set_native_live_move(false);
                        window_context.apply_pending_native_transition();
                    },
                }
                break;
            }
        });
    }

    /// Create the global hotkey manager and register the quick-terminal toggle
    /// (Ctrl+`). Returns `(None, None)` if the platform rejects it, so the rest
    /// of the terminal keeps working without a quick terminal.
    fn init_quick_hotkey(combo: &str) -> (Option<GlobalHotKeyManager>, Option<HotKey>) {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                warn!("Quick terminal disabled: global hotkey init failed: {err}");
                return (None, None);
            },
        };
        let hotkey = combo
            .parse::<HotKey>()
            .unwrap_or_else(|_| HotKey::new(Some(HotKeyModifiers::CONTROL), Code::Backquote));
        match manager.register(hotkey) {
            Ok(()) => (Some(manager), Some(hotkey)),
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
        // 恢复行为归设置·高级→会话管（默认开）。关掉只是不回放——快照照写，
        // 工作区导出与崩溃现场诊断都还在。
        //
        // 两类提示分流：恢复成功是**已经结束、没有待办**的事实，走自动消失的
        // toast；断路器那条带着隔离文件路径，用户可能要去把它捞出来，必须留在
        // 消息栏等他自己关掉。
        let mut restored_notice = None;
        let mut blocked_notice = None;
        let restore = if plain_launch && crate::display::restore_session_enabled() {
            match crate::session::load() {
                Some(mut session) if crate::session::should_restore(&session) => {
                    if crate::session::was_crash(&session) {
                        restored_notice = Some(format!(
                            "已从上次异常退出恢复 {} 个标签（进程未正常收尾）。",
                            session.tabs.len()
                        ));
                    }
                    // Count this launch against the crash-loop breaker; the
                    // first successful autosave (1 Hz tick) resets it.
                    crate::session::mark_boot_attempt(&mut session);
                    Some(session)
                },
                // 断路器跳闸：连续三次启动都没活到第一次自动保存。把这份
                // 会话隔离出去再干净启动，否则一秒后的自动保存就会盖掉这份
                // 「一恢复就崩」的唯一现场。
                Some(session) if !session.tabs.is_empty() => {
                    blocked_notice = Some(match crate::session::quarantine() {
                        Some(path) => format!(
                            "连续三次启动失败，已跳过会话恢复；上次的会话保存在 {}。",
                            path.display()
                        ),
                        None => "连续三次启动失败，已跳过会话恢复。".to_owned(),
                    });
                    None
                },
                _ => None,
            }
        } else {
            None
        };
        let boot = restore.map_or(WindowBoot::Fresh, WindowBoot::Restore);

        let mut window_context = WindowContext::initial(
            event_loop,
            self.proxy.clone(),
            self.config.clone(),
            window_options,
            boot,
        )?;

        // 恢复成功：说完就走。断路器：留在消息栏，路径要能被读到。
        if let Some(text) = restored_notice {
            window_context.display.push_toast(text, ToastKind::Success);
        }
        if let Some(text) = blocked_notice {
            window_context.message_buffer.push(Message::new(text, MessageType::Warning));
        }

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

        // Arm the 1 Hz chrome clock / render-gate watchdog right now, before
        // the first frame. `draw()` also (re)schedules it, but `draw()` only
        // runs on a `RedrawRequested`, and a redraw request is gated behind
        // `has_frame && !occluded`. If a startup occlusion misreport or a lost
        // frame callback closes one of those gates before the first draw ever
        // lands, the very watchdog that exists to reopen them
        // (`unstick_render_gates_if_visible`) would never be armed — the window
        // stays visible but frozen, repainting only after a manual
        // minimize/restore (issues #21 and #32). Scheduling here breaks that
        // bootstrap deadlock so recovery always happens within one tick. The
        // interval matches the idle chrome-clock cadence, so the first `draw()`
        // finds the timer already in place and leaves it untouched.
        let clock_timer = TimerId::new(Topic::NebulaClock, id);
        if !self.scheduler.scheduled(clock_timer) {
            let tick = Event::new(EventType::NebulaTick, id);
            self.scheduler.schedule(tick, Duration::from_secs(1), true, clock_timer);
        }

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

    fn runtime_snapshot(&self) -> RuntimeSnapshot {
        let mut windows: Vec<_> =
            self.windows.values().map(WindowContext::runtime_snapshot).collect();
        windows.sort_by_key(|window| window.id);
        RuntimeSnapshot::new(self.detached.len(), windows)
    }

    fn publish_runtime_snapshot(&self) -> RuntimeSnapshot {
        self.runtime_hub.publish(self.runtime_snapshot())
    }

    /// Resolve a control target without relying on HashMap iteration order.
    /// Pane ids are window-local, so an omitted window is accepted only when
    /// the pane is unique across all live windows.
    fn runtime_target_window(
        &self,
        window_id: Option<u64>,
        pane_id: Option<u64>,
    ) -> Result<WindowId, ApiError> {
        if let Some(window_id) = window_id {
            let id = WindowId::from(window_id);
            let Some(window) = self.windows.get(&id) else {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("window {window_id} does not exist"),
                ));
            };
            if let Some(pane_id) = pane_id
                && !window.runtime_contains_pane(pane_id)
            {
                return Err(ApiError::new(
                    "target_not_found",
                    format!("pane {pane_id} does not belong to window {window_id}"),
                ));
            }
            return Ok(id);
        }

        if let Some(pane_id) = pane_id {
            let mut matches: Vec<_> = self
                .windows
                .iter()
                .filter_map(|(id, window)| window.runtime_contains_pane(pane_id).then_some(*id))
                .collect();
            matches.sort_by_key(|id| u64::from(*id));
            return match matches.as_slice() {
                [id] => Ok(*id),
                [] => {
                    Err(ApiError::new("target_not_found", format!("pane {pane_id} does not exist")))
                },
                _ => Err(ApiError::new(
                    "ambiguous_target",
                    format!("pane id {pane_id} exists in multiple windows; provide window_id"),
                )),
            };
        }

        self.windows
            .iter()
            .filter(|(_, window)| window.display.window.has_focus())
            .map(|(id, _)| *id)
            .next()
            .or_else(|| {
                self.windows
                    .iter()
                    .filter(|(_, window)| !window.session_exempt)
                    .map(|(id, _)| *id)
                    .min_by_key(|id| u64::from(*id))
            })
            .or_else(|| self.windows.keys().copied().min_by_key(|id| u64::from(*id)))
            .ok_or_else(|| ApiError::new("target_not_found", "no live Nebula window exists"))
    }

    fn runtime_result(&self, action: Value) -> Result<Value, ApiError> {
        let snapshot = self.publish_runtime_snapshot();
        Ok(serde_json::json!({ "action": action, "snapshot": snapshot }))
    }

    fn execute_runtime_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: &RuntimeCommand,
    ) -> Result<Value, ApiError> {
        match command {
            RuntimeCommand::Snapshot => serde_json::to_value(self.publish_runtime_snapshot())
                .map_err(|error| ApiError::new("serialization_failed", error.to_string())),
            RuntimeCommand::NewWindow => {
                // GL backends require every current context to be released
                // before another window surface is created.
                for window in self.windows.values_mut() {
                    window.display.make_not_current();
                }
                let id = if self.gl_config.is_none() {
                    let before: Vec<_> = self.windows.keys().copied().collect();
                    self.create_initial_window(event_loop, WindowOptions::default())
                        .map_err(|error| ApiError::new("action_failed", error.to_string()))?;
                    self.windows.keys().copied().find(|id| !before.contains(id)).ok_or_else(
                        || ApiError::new("action_failed", "new window was not registered"),
                    )?
                } else {
                    self.create_window(event_loop, WindowOptions::default())
                        .map_err(|error| ApiError::new("action_failed", error.to_string()))?
                };
                self.runtime_result(serde_json::json!({ "window_id": u64::from(id) }))
            },
            RuntimeCommand::Focus { window_id, pane_id } => {
                let id = self.runtime_target_window(*window_id, *pane_id)?;
                self.windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_focus(*pane_id)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::NewTab { window_id, cwd } => {
                let id = self.runtime_target_window(*window_id, None)?;
                let window = self.windows.get_mut(&id).expect("resolved runtime window exists");
                let pane_id = window.runtime_new_tab(cwd.clone())?;
                // 带目录的 tab.new 来自一次用户手势（Explorer 右键并入）：
                // 把窗口带到前台，不然标签开在了别人身后。无目录的编程调用
                // （agent/CLI）保持原来的不抢焦点语义。
                if cwd.is_some() {
                    window.runtime_focus(None)?;
                }
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::Split { window_id, direction } => {
                let id = self.runtime_target_window(*window_id, None)?;
                let pane_id = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_split(*direction)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id
                }))
            },
            RuntimeCommand::Prompt { window_id, pane_id, text, submit } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                self.windows.get_mut(&id).expect("resolved runtime window exists").runtime_prompt(
                    *pane_id,
                    text.clone(),
                    *submit,
                )?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "submitted": submit
                }))
            },
            RuntimeCommand::ReadPane { window_id, pane_id, lines } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let read = self
                    .windows
                    .get(&id)
                    .expect("resolved runtime window exists")
                    .runtime_read(*pane_id, *lines)?;
                serde_json::to_value(read)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::Procs { window_id, pane_id } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let processes = self
                    .windows
                    .get(&id)
                    .expect("resolved runtime window exists")
                    .runtime_procs(*pane_id)?;
                serde_json::to_value(processes)
                    .map_err(|error| ApiError::new("serialization_failed", error.to_string()))
            },
            RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let bytes_sent = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_send_key(*pane_id, *key, *modifiers, *repeat)?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "key": key.as_str(),
                    "repeat": repeat,
                    "bytes_sent": bytes_sent
                }))
            },
            RuntimeCommand::Run { window_id, pane_id, command, .. } => {
                let id = self.runtime_target_window(*window_id, Some(*pane_id))?;
                let run_id = self
                    .windows
                    .get_mut(&id)
                    .expect("resolved runtime window exists")
                    .runtime_run(*pane_id, command.clone())?;
                self.runtime_result(serde_json::json!({
                    "window_id": u64::from(id),
                    "pane_id": pane_id,
                    "run_id": run_id
                }))
            },
            RuntimeCommand::AgentStart { .. } | RuntimeCommand::AgentFork { .. } => {
                self.execute_agent_runtime_command(command)
            },
            RuntimeCommand::AgentPrompt { agent, generation, text, submit } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                let id = WindowId::from(managed.window_id);
                let Some(window) = self.windows.get_mut(&id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live window", managed.name),
                    ));
                };
                window.runtime_prompt(managed.pane_id, text.clone(), *submit)?;
                self.runtime_result(serde_json::json!({ "agent": managed }))
            },
            RuntimeCommand::AgentRead { agent, generation, lines } => {
                let managed = self.runtime_hub.active_agent(agent, *generation)?;
                let id = WindowId::from(managed.window_id);
                let Some(window) = self.windows.get(&id) else {
                    return Err(ApiError::new(
                        "agent_closed",
                        format!("agent {:?} no longer has a live window", managed.name),
                    ));
                };
                let read = window.runtime_read(managed.pane_id, *lines)?;
                Ok(serde_json::json!({ "agent": managed, "read": read }))
            },
        }
    }

    fn handle_runtime_control(&mut self, event_loop: &ActiveEventLoop, dispatch: &RuntimeDispatch) {
        let response = self.execute_runtime_command(event_loop, &dispatch.command);
        dispatch.respond(response);
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
        let Some(hotkey_id) = self.quick_hotkey.map(|hotkey| hotkey.id()) else { return };
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

    /// Replace the global quick-terminal shortcut transactionally. Registering
    /// the candidate before releasing the old key keeps the existing shortcut
    /// alive when the OS rejects a conflicting or malformed candidate.
    fn apply_quick_terminal_hotkey(&mut self, requested: &str) -> Result<(), String> {
        let new_hotkey =
            requested.parse::<HotKey>().map_err(|err| format!("快捷键格式无效：{err}"))?;
        if self.quick_hotkey == Some(new_hotkey) {
            self.quick_hotkey_combo = requested.to_owned();
            return Ok(());
        }

        let mut manager = self.global_hotkey.take().or_else(|| GlobalHotKeyManager::new().ok());
        let Some(manager_ref) = manager.as_mut() else {
            return Err("系统全局快捷键管理器初始化失败".to_owned());
        };
        if let Err(err) = manager_ref.register(new_hotkey) {
            self.global_hotkey = manager;
            return Err(format!("快捷键注册失败：{err}"));
        }
        if let Some(old_hotkey) = self.quick_hotkey {
            if let Err(err) = manager_ref.unregister(old_hotkey) {
                let _ = manager_ref.unregister(new_hotkey);
                self.global_hotkey = manager;
                return Err(format!("释放旧快捷键失败：{err}"));
            }
        }
        self.global_hotkey = manager;
        self.quick_hotkey = Some(new_hotkey);
        self.quick_hotkey_combo = requested.to_owned();
        Ok(())
    }

    /// Settings files are shared by windows. A non-originating window can
    /// notice a changed hotkey during its mtime reload, so drain one staged
    /// request during the main wait cycle even when no key event follows.
    fn flush_quick_hotkey_requests(&mut self) {
        let mut pending = None;
        for (window_id, window_context) in &mut self.windows {
            if let Some(hotkey) = window_context.display.take_quick_hotkey_request() {
                pending = Some((*window_id, hotkey));
                break;
            }
        }
        let Some((window_id, hotkey)) = pending else { return };
        let old = self.quick_hotkey_combo.clone();
        let result = self.apply_quick_terminal_hotkey(&hotkey);
        if let Some(window_context) = self.windows.get_mut(&window_id) {
            match result {
                Ok(()) => {
                    window_context.display.quick_hotkey_registration_done(&hotkey, true, None, &old)
                },
                Err(err) => window_context.display.quick_hotkey_registration_done(
                    &hotkey,
                    false,
                    Some(&err),
                    &old,
                ),
            }
            window_context.dirty = true;
            window_context.display.window.request_redraw();
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
        // A native stage can precede the winit event it affects (notably DPI
        // changes), so consume it before filtering or routing this event.
        self.drain_native_window_stages();

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
            _event_loop,
            &self.proxy,
            &mut self.clipboard,
            &mut self.scheduler,
            WinitEvent::WindowEvent { window_id, event },
        );

        if is_redraw {
            let start = std::time::Instant::now();
            window_context.draw(&mut self.scheduler);
            crate::input::latency::frame_drawn();
            let elapsed = start.elapsed();
            if elapsed.as_millis() >= 8 {
                crate::display::nebula_debug_log(format!("winmove slow_draw {elapsed:?}"));
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        if self.config.debug.print_events {
            info!(target: LOG_TARGET_WINIT, "{event:?}");
        }

        // Handle events which don't mandate the WindowId.
        let tab_id = event.tab_id;
        match (event.payload, event.window_id.as_ref()) {
            (EventType::RuntimeControl(dispatch), _) => {
                self.handle_runtime_control(event_loop, &dispatch)
            },
            // AI-CLI lifecycle events (nebula-hook pipe) route by pane id, so
            // the windows resolve them themselves; the owner claims it.
            (EventType::AiHook(hook), _) => {
                self.route_ai_hook(&hook);
            },
            // Assistant fix results route by pane id the same way.
            (EventType::AiFixReady { pane, seq, fix }, _) => {
                for window_context in self.windows.values_mut() {
                    if window_context.handle_ai_fix(pane, seq, &fix) {
                        break;
                    }
                }
            },
            // WebDAV 同步（spec 003）：网络与 Argon2 派生都在后台 OS 线程
            // 阻塞完成，主循环只发起与收尾——终端渲染不等加密。
            (EventType::NebulaSync { push }, _) => {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let result = if push { crate::sync::push() } else { crate::sync::pull() };
                    crate::sync::warn_result(&result);
                    let (message, error, history_changed) = match result {
                        Ok(outcome) => (outcome.message, false, outcome.history_changed),
                        Err(err) => (err, true, false),
                    };
                    let _ = proxy.send_event(crate::event::Event::new(
                        EventType::NebulaSyncDone { message, error, history_changed },
                        None,
                    ));
                });
            },
            (EventType::NebulaSyncDone { message, error, history_changed }, _) => {
                for window_context in self.windows.values_mut() {
                    window_context.handle_sync_done(&message, error, history_changed);
                }
            },
            // 远程备份（设置→备份）：打包、Argon2 派生与网络都在后台 OS
            // 线程阻塞完成，主循环只发起与收尾——与 WebDAV 同步同一模型。
            (EventType::NebulaBackupRemote { upload, passphrase, selection }, _) => {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let result = if upload {
                        crate::encrypted_backup::collect(selection)
                            .and_then(|archive| {
                                crate::encrypted_backup::seal(&archive, &passphrase)
                            })
                            .and_then(|packet| crate::backup_remote::push(&packet))
                    } else {
                        crate::backup_remote::pull_latest().and_then(|(name, packet)| {
                            crate::encrypted_backup::restore(&packet, &passphrase)
                                .map(|()| format!("已从远端恢复 {name}，重启后应用全部设置"))
                        })
                    };
                    crate::backup_remote::warn_result(&result);
                    let (message, error) = match result {
                        Ok(message) => (message, false),
                        Err(err) => (err, true),
                    };
                    let _ = proxy.send_event(crate::event::Event::new(
                        EventType::NebulaBackupRemoteDone { message, error },
                        None,
                    ));
                });
            },
            (EventType::NebulaBackupRemoteDone { message, error }, _) => {
                for window_context in self.windows.values_mut() {
                    window_context.handle_backup_remote_done(&message, error);
                }
            },
            (EventType::LocalProxyScan, Some(window_id)) => {
                let proxy = self.proxy.clone();
                let window_id = *window_id;
                std::thread::spawn(move || {
                    let found = crate::ssh_proxy::scan_local_proxies(&[]);
                    let _ = proxy.send_event(crate::event::Event::new(
                        EventType::LocalProxyScanDone(found),
                        window_id,
                    ));
                });
            },
            (EventType::LocalProxyScanDone(found), Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.local_proxy_scan_done(found);
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::ProxyTestDone { request_id, ok, message, elapsed_ms }, Some(window_id)) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.proxy_test_done(request_id, ok, &message, elapsed_ms);
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (
                EventType::ProviderTestDone { request_id, provider_id, ok, message, elapsed_ms },
                Some(window_id),
            ) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.provider_test_done(
                        request_id,
                        &provider_id,
                        ok,
                        &message,
                        elapsed_ms,
                    );
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (EventType::QuickTerminalHotkeyChanged { hotkey }, Some(window_id)) => {
                let old = self.quick_hotkey_combo.clone();
                let result = self.apply_quick_terminal_hotkey(&hotkey);
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    match result {
                        Ok(()) => window_context
                            .display
                            .quick_hotkey_registration_done(&hotkey, true, None, &old),
                        Err(err) => window_context.display.quick_hotkey_registration_done(
                            &hotkey,
                            false,
                            Some(&err),
                            &old,
                        ),
                    }
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            (
                EventType::SshTestDone { request_id, destination, ok, message, elapsed_ms },
                Some(window_id),
            ) => {
                if let Some(window_context) = self.windows.get_mut(window_id) {
                    window_context.display.ssh_test_done(
                        request_id,
                        &destination,
                        ok,
                        &message,
                        elapsed_ms,
                    );
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
            },
            // 连接阶段推进：只更新拥有该 pane 的窗口。事件自带 tab_id，
            // 没有 tab_id 的（不该出现）直接丢弃而不是误绘到别的 pane 上。
            (EventType::SshConnect(stage), Some(window_id)) => {
                if let (Some(window_context), Some(pane)) =
                    (self.windows.get_mut(window_id), tab_id)
                {
                    window_context.ssh_connect_stage(pane, stage);
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
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
                    config::merge_terminal_profiles(&mut loaded.config);
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
            (EventType::TerminalProfilesChanged, _) => {
                // The imported profile store is deliberately separate from
                // the user's config file. Rebuild the shared config snapshot
                // in-place so every existing window and future tab sees the
                // new profile without a restart.
                let mut config = (*self.config).clone();
                config::merge_terminal_profiles(&mut config);
                self.config = Rc::new(config);
                for window_context in self.windows.values_mut() {
                    window_context.update_config(self.config.clone());
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
                        event_loop,
                        &self.proxy,
                        &mut self.clipboard,
                        &mut self.scheduler,
                        event.clone(),
                    );
                }
            },
            (EventType::Terminal(TerminalEvent::Wakeup), Some(window_id)) => {
                self.handle_terminal_wakeup(window_id, tab_id);
            },
            (EventType::Terminal(TerminalEvent::Exit), Some(window_id)) => {
                if let Some(pane_id) = tab_id {
                    self.runtime_hub.record_pane_exited(u64::from(*window_id), pane_id);
                }
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
                    // Agent screen semantics (blocked/working/idle) are a
                    // cheap 1 Hz fallback under exact lifecycle hooks.
                    window_context.refresh_agent_screen_states();
                    // Piggyback session persistence on the 1 Hz chrome clock.
                    window_context.autosave_session();
                    // 渲染门控看门狗:被误报的遮挡/丢失的帧回调在这里解锁
                    // (issue #21"启动后点什么都没反应")。
                    window_context.unstick_render_gates_if_visible();
                    window_context.dirty = true;
                    window_context.display.window.request_redraw();
                }
                // 托盘 agent 清单同样搭 1 Hz 时钟：跨窗口聚合，tray::update
                // 内容不变时自去抖。多窗口各自的 tick 都会走到这里，最先到
                // 的那个完成本秒的发布，其余是廉价 no-op。
                let agents = self
                    .windows
                    .values()
                    .flat_map(|window_context| window_context.tray_agents())
                    .collect();
                crate::tray::update(agents);
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
        // WM_EXITSIZEMOVE may not have a corresponding WindowEvent. Drain it
        // here so the final pending DPI is committed before the loop sleeps.
        self.drain_native_window_stages();

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
                event_loop,
                &self.proxy,
                &mut self.clipboard,
                &mut self.scheduler,
                WinitEvent::AboutToWait,
            );
        }
        self.flush_quick_hotkey_requests();
        // This is the single projection boundary for GUI, PTY, hook, and CLI
        // changes. RuntimeHub deduplicates identical semantic snapshots.
        self.publish_runtime_snapshot();

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

pub struct ActionContext<'a, N, T> {
    pub pane_id: u64,
    pub notifier: &'a mut N,
    pub terminal: &'a mut Term<T>,
    pub clipboard: &'a mut Clipboard,
    pub mouse: &'a mut Mouse,
    pub touch: &'a mut TouchPurpose,
    pub modifiers: &'a mut Modifiers,
    pub display: &'a mut Display,
    pub windowed_size: &'a mut LogicalSize<u32>,
    pub nebula_state: &'a mut NebulaPaneState,
    pub ssh_destination: Option<&'a str>,
    /// Document shown by the active tab, when it is a viewer tab — wheel and
    /// navigation keys scroll this instead of a grid. `None` on pane tabs.
    pub doc: Option<&'a mut crate::display::markdown_view::DocView>,
    /// Standalone image shown by the active tab. Pointer wheel/drag events
    /// update this state instead of reaching the document stub terminal.
    pub image: Option<&'a mut crate::display::image_viewer::ImageView>,
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
    fn pane_id(&self) -> u64 {
        self.pane_id
    }

    #[inline]
    fn nebula_special_tab_active(&self) -> bool {
        self.display.nebula_special_tab_active
    }

    #[inline]
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, val: B) {
        self.notifier.notify(val);
    }

    #[inline]
    fn doc_view(&mut self) -> Option<&mut crate::display::markdown_view::DocView> {
        self.doc.as_deref_mut()
    }

    #[inline]
    fn image_view(&mut self) -> Option<&mut crate::display::image_viewer::ImageView> {
        self.image.as_deref_mut()
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

    fn terminal_math_source_point(&self, point: Point, side: Side) -> (Point, Side) {
        // Formula projection spans live in rendered-viewport coordinates,
        // which can be a crop of the grid while a resize commit is pending.
        self.nebula_state.terminal_math_source_point(
            point,
            side,
            self.terminal.viewport_origin_for(self.size_info().screen_lines()),
        )
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
            let point = self.mouse.point(&self.size_info(), &*self.terminal);
            let (point, side) = self.terminal_math_source_point(point, self.mouse.cell_side);
            self.update_selection(point, side);
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

        // 交互设置「选中即复制」：开启时任何选区立即进系统剪贴板；
        // 关闭时选择只保留在终端内，复制交给右键（复制/粘贴）路径。
        if ty == ClipboardType::Selection && self.display.nebula_copy_on_select {
            self.clipboard.store(ClipboardType::Clipboard, text.clone());
        }
        self.clipboard.store(ty, text.clone());
        // Explicit clipboard copies are user actions worth acknowledging.
        // Selection storage is intentionally silent: with copy-on-select it
        // can fire for every mouse-motion update and would spam the toast rail.
        if ty == ClipboardType::Clipboard {
            self.notify_copy(&text);
        }
    }

    /// Show the copy confirmation in the current UI language. Keeping this in
    /// the event layer lets keyboard, context-menu and right-click copies share
    /// exactly one notification path while paste remains silent.
    fn notify_copy(&mut self, text: &str) {
        let lines = text.lines().count().max(1);
        let language = self.display.ui_language();
        let message = match language {
            UiLanguage::ZhCn => format!("已复制 {lines} 行到剪贴板"),
            UiLanguage::EnUs => format!("Copied {lines} lines to clipboard"),
        };
        self.display.push_toast(message, ToastKind::Info);
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
    fn nebula_completion_popup_active(&self) -> bool {
        !self.nebula_state.completion_items.is_empty()
    }

    fn nebula_completion_popup_move(&mut self, delta: isize) {
        let len = self.nebula_state.completion_items.len();
        if len == 0 {
            return;
        }
        let current = self.nebula_state.completion_selected as isize;
        self.nebula_state.completion_selected = (current + delta).rem_euclid(len as isize) as usize;
        *self.dirty = true;
    }

    fn nebula_completion_popup_take(&mut self) -> Option<String> {
        let state = &mut self.nebula_state;
        let insert =
            state.completion_items.get(state.completion_selected).map(|item| item.insert.clone());
        state.completion_items.clear();
        state.completion_selected = 0;
        *self.dirty = true;
        insert
    }

    fn nebula_completion_popup_dismiss(&mut self) -> bool {
        let state = &mut self.nebula_state;
        if state.completion_items.is_empty() {
            return false;
        }
        // Items go, the recompute key stays: the cache guard in
        // `nebula_update_suggestion` then keeps the list closed until the
        // line itself changes.
        state.completion_items.clear();
        state.completion_selected = 0;
        *self.dirty = true;
        true
    }

    fn nebula_take_ai_fix(&mut self) -> Option<String> {
        use crate::ai_assistant::AiFixState;
        match self.nebula_state.ai_fix.take() {
            Some(AiFixState::Ready { fix, .. }) => Some(fix.command),
            other => {
                // Pending 放回去：分析中的请求不因误按 Ctrl+. 而丢。
                self.nebula_state.ai_fix = other;
                None
            },
        }
    }

    fn nebula_dismiss_ai_fix(&mut self) -> bool {
        self.nebula_state.ai_fix.take().is_some()
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

    fn refresh_terminal_profiles(&mut self) {
        let _ = self.event_proxy.send_event(Event {
            window_id: None,
            tab_id: None,
            payload: EventType::TerminalProfilesChanged,
        });
    }

    fn nebula_sync(&self, push: bool) {
        let _ = self.event_proxy.send_event(Event {
            window_id: Some(self.display.window.id()),
            tab_id: None,
            payload: EventType::NebulaSync { push },
        });
    }

    fn nebula_backup_remote(&self, request: crate::display::RemoteBackupRequest) {
        let _ = self.event_proxy.send_event(Event {
            window_id: Some(self.display.window.id()),
            tab_id: None,
            payload: EventType::NebulaBackupRemote {
                upload: request.upload,
                passphrase: request.passphrase,
                selection: request.selection,
            },
        });
    }

    fn nebula_local_proxy_scan(&mut self) {
        if !self.display.take_local_proxy_scan_request() {
            return;
        }
        let _ = self
            .event_proxy
            .send_event(Event::new(EventType::LocalProxyScan, self.display.window.id()));
    }

    fn nebula_proxy_test(&mut self) {
        let Some(request_id) = self.display.take_proxy_test_request() else { return };
        if let Err(err) = crate::ssh_session::spawn_proxy_test(
            request_id,
            self.event_proxy.clone(),
            self.display.window.id(),
        ) {
            self.display.proxy_test_done(request_id, false, &format!("无法启动网络测试：{err}"), 0);
        }
    }

    fn nebula_provider_test(&mut self) {
        let Some(request) = self.display.take_provider_test_request() else { return };
        let request_id = request.request_id;
        let provider_id = request.provider.id.clone();
        if let Err(err) = crate::ai_providers::spawn_test(
            request,
            self.event_proxy.clone(),
            self.display.window.id(),
        ) {
            self.display.provider_test_done(
                request_id,
                &provider_id,
                false,
                &format!("无法启动供应商测试：{err}"),
                0,
            );
        }
    }

    fn nebula_quick_hotkey_changed(&mut self) {
        let Some(hotkey) = self.display.take_quick_hotkey_request() else { return };
        let _ = self.event_proxy.send_event(Event::new(
            EventType::QuickTerminalHotkeyChanged { hotkey },
            self.display.window.id(),
        ));
    }

    /// SSH 编辑器「测试连接」：display 侧点击时暂存的请求在这里被取走，
    /// 交给共享 SSH runtime 执行；结果以 [`EventType::SshTestDone`] 回流。
    fn nebula_ssh_test(&mut self) {
        let Some(request) = self.display.take_ssh_test_request() else { return };
        let request_id = request.request_id;
        let destination = request.destination.clone();
        if let Err(err) = crate::ssh_session::spawn_test(
            request,
            self.event_proxy.clone(),
            self.display.window.id(),
        ) {
            self.display.ssh_test_done(
                request_id,
                &destination,
                false,
                &format!("无法启动测试任务：{err}"),
                0,
            );
        }
    }

    fn nebula_open_sftp(&mut self, destination: String) {
        if let Err(err) = self.display.open_sftp_panel(destination, self.event_proxy.clone()) {
            log::error!("{err}");
        }
    }

    fn nebula_ssh_destination(&self) -> Option<&str> {
        self.ssh_destination
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
        let scale = self.display.window.scale_factor as f32;
        // Hard bounds keep runaway zooms recoverable. Without them a stuck
        // modifier or trackpad burst can scroll the terminal to 180 px+,
        // where a ±1-step notch changes the size by under 1 % and zooming
        // back out reads as "broken". Logical 4–64 px covers everything from
        // dense logs to presentations.
        let (min_px, max_px) = (4.0 * scale, 64.0 * scale);
        // Round to pick integral px steps, since fonts look better on them.
        let new_size = (self.display.font_size.as_px().round() + delta).clamp(min_px, max_px);
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

    fn apply_default_cursor_style(&mut self) {
        // 用户在设置页显式选了光标样式：先清掉 shell 早前用 DECSCUSR 钉住
        // 的覆盖（PSReadLine/starship 启动时常发），否则新默认被旧覆盖压
        // 住、"改了不生效"；之后 vim 等程序再发 DECSCUSR 仍可正常覆盖。
        self.terminal.reset_cursor_style_override();
        self.update_cursor_blinking();
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

    /// 资源管理器里定位到条目本身（文件树右键「在资源管理器中显示」）。
    /// `/select,` 与路径必须是同一个参数，逗号后直接拼路径。
    fn reveal_in_file_manager(&mut self, path: &std::path::Path) {
        #[cfg(windows)]
        {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(path.as_os_str());
            self.spawn_daemon("explorer.exe", &[arg.as_os_str()]);
        }
        #[cfg(not(windows))]
        if let Some(parent) = path.parent() {
            self.spawn_daemon("xdg-open", &[parent.as_os_str()]);
        }
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
                self.clipboard.store(ClipboardType::Clipboard, text.clone());
                self.notify_copy(&text);
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
        let point = self.mouse().point(&self.size_info(), self.terminal());
        let (point, cell_side) = self.terminal_math_source_point(point, self.mouse().cell_side);

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

    /// 剪贴板截图转路径粘贴（无文本时的回退）。本地 pane 同步落盘临时 PNG
    /// 直接粘；SSH pane 交给 SFTP 后台上传，路径经 runtime Prompt 通道回粘
    /// ——期间不阻塞任何输入。
    fn paste_clipboard_image(&mut self) -> bool {
        let Some(png) = crate::clipboard::clipboard_image_png() else {
            return false;
        };
        if let Some(destination) = self.ssh_destination {
            crate::ssh_sftp::upload_clipboard_image(
                destination.to_owned(),
                png,
                self.pane_id,
                self.event_proxy.clone(),
            );
            return true;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("nebula-paste-{stamp}.png"));
        if std::fs::write(&path, &png).is_err() {
            return false;
        }
        self.paste(&path.display().to_string(), true);
        true
    }

    /// Paste a text into the terminal.
    fn paste(&mut self, text: &str, bracketed: bool) {
        // Multi-line paste confirmation (#18): a newline heading to a bare
        // shell starts executing the moment it lands. But an app in
        // bracketed-paste mode — codex, vim, a REPL, modern PSReadLine —
        // receives the whole paste as one chunk (wrapped below in
        // `\x1b[200~`…`\x1b[201~`) and decides what to do with the newlines
        // itself, so it is *not* executed line by line and the warning both
        // misleads and gets in the way (#35). Confirm only for the genuinely
        // dangerous case: newlines going to a shell that is not bracketing.
        // Search and pending-char inputs are exempt (they consume text
        // locally).
        let goes_to_pty = !self.search_active() && !self.inline_search_state.char_pending;
        let bracketing = bracketed && self.terminal().mode().contains(TermMode::BRACKETED_PASTE);
        if goes_to_pty
            && !bracketing
            && self.display.nebula_confirm.is_none()
            && (text.contains('\n') || text.contains('\r'))
        {
            let lines = text.lines().count().max(2);
            self.display.nebula_confirm = Some(crate::display::NebulaConfirm::Paste {
                pane_id: self.pane_id,
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
    /// 当前聚焦终端此刻是否应该闪烁光标——blink 定时与每个 tick 的自检共
    /// 用这一个判定,两处口径不可能分叉。
    fn cursor_should_blink(&mut self) -> bool {
        // Push the settings default (shape + blink) into the terminal first:
        // `Term::cursor_style()` falls back to it whenever no DECSCUSR escape
        // has overridden the style, so vim's mode cursor keeps working while
        // plain shells follow the user's choice immediately.
        self.terminal.set_default_cursor_style(self.display.nebula_default_cursor_style());
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
        // 用 winit 的实时焦点而不是 `terminal.is_focused` 缓存:切 pane / 切
        // tab 后新 Term 的缓存可能从未见过 Focused 事件,残留 false 会让
        // 闪烁"有时不闪";残留 true 则让失焦窗口继续闪。
        blinking && self.display.window.has_focus()
    }

    fn update_cursor_blinking(&mut self) {
        let blinking = self.cursor_should_blink();

        // Update cursor blinking state.
        let window_id = self.display.window.id();
        self.scheduler.unschedule(TimerId::new(Topic::BlinkCursor, window_id));
        self.scheduler.unschedule(TimerId::new(Topic::BlinkTimeout, window_id));

        // Reset blinking timeout.
        *self.cursor_blink_timed_out = false;

        if blinking {
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

impl input::Processor<EventProxy, ActionContext<'_, Notifier, EventProxy>> {
    /// 助手错误恢复（spec 001 阶段一）触发点：便宜的门（冷却、开关、规则
    /// 表）都过了才抓输出、开线程。任何一门不过都安静返回——这条路径跑在
    /// 每次命令失败上，不能吵。
    fn maybe_request_ai_fix(&mut self, exit_code: i32, program: Option<&str>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);

        if let Some(last) = self.ctx.nebula_state.ai_fix_cooldown {
            if last.elapsed() < crate::ai_assistant::COOLDOWN {
                return;
            }
        }
        let cfg = crate::ai_assistant::AssistantConfig::load();
        if !cfg.enabled {
            return;
        }
        let command = self.ctx.nebula_state.last_committed.clone();
        if !crate::ai_assistant::should_suggest(
            exit_code,
            &command,
            program,
            &cfg.ignored_exit_codes,
        ) {
            return;
        }
        self.ctx.nebula_state.ai_fix_cooldown = Some(Instant::now());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let request = crate::ai_assistant::FixRequest {
            pane: self.ctx.pane_id,
            seq,
            command,
            exit_code,
            cwd: self.ctx.nebula_state.cwd.clone(),
            branch: self.ctx.nebula_state.branch.clone(),
            output_tail: crate::ai_assistant::redact_secrets(&self.grid_output_tail(24, 2000)),
        };
        self.ctx.nebula_state.ai_fix = Some(crate::ai_assistant::AiFixState::Pending { seq });
        crate::ai_assistant::spawn_fix_request(self.ctx.event_proxy.clone(), cfg, request);
        self.ctx.mark_dirty();
    }

    /// The failed command's on-screen output: up to `max_lines` rows ending at
    /// the cursor (OSC 133;D arrives before the next prompt paints, so the
    /// cursor still sits at the end of the output), tail-capped at `max_chars`
    /// — the newest lines carry the actual error. Shell-side integrations cannot
    /// see this rendered context, so the terminal grid remains the authoritative source.
    fn grid_output_tail(&self, max_lines: usize, max_chars: usize) -> String {
        use nebula_terminal::index::{Column, Line};
        use nebula_terminal::term::cell::Flags as CellFlags;

        let grid = self.ctx.terminal.grid();
        let cursor_line = grid.cursor.point.line.0;
        let columns = self.ctx.terminal.columns();
        let first = (cursor_line + 1 - max_lines as i32).max(0);
        let mut lines: Vec<String> = Vec::new();
        for l in first..=cursor_line {
            let row = &grid[Line(l)];
            let mut text = String::with_capacity(columns);
            for c in 0..columns {
                let cell = &row[Column(c)];
                // Wide chars own two cells; the spacer half would double every
                // CJK glyph as a stray space.
                if cell.flags.contains(CellFlags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
            }
            lines.push(text.trim_end().to_owned());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        while lines.first().is_some_and(String::is_empty) {
            lines.remove(0);
        }
        let text = lines.join("\n");
        let overflow = text.chars().count().saturating_sub(max_chars);
        if overflow > 0 { text.chars().skip(overflow).collect() } else { text }
    }

    /// Handle events from winit.
    pub fn handle_event(&mut self, event: WinitEvent<Event>) {
        match event {
            WinitEvent::UserEvent(Event { payload, tab_id, .. }) => match payload {
                EventType::SearchNext => self.ctx.goto_match(None),
                // Tab requests are handled at the window-context level.
                EventType::NebulaTab(_) => (),
                // Clock ticks are handled at the window-context level.
                EventType::NebulaTick | EventType::NebulaAttach | EventType::RuntimeControl(_) => {
                    ()
                },
                // Resize settling is handled at the window-context level.
                EventType::NebulaResizeSettled
                | EventType::SshDeleteUndoExpired
                | EventType::QuickTerminalHotkeyChanged { .. }
                | EventType::ProxyTestDone { .. }
                | EventType::ProviderTestDone { .. }
                | EventType::SshTestDone { .. }
                | EventType::SshConnect(_)
                | EventType::SftpUpdated => (),
                // AI hook events are handled at the Processor level (they may
                // target any window's pane); FocusWindow, fix results and
                // WebDAV sync likewise.
                EventType::AiHook(_)
                | EventType::AiFixReady { .. }
                | EventType::NebulaSync { .. }
                | EventType::NebulaSyncDone { .. }
                | EventType::NebulaBackupRemote { .. }
                | EventType::NebulaBackupRemoteDone { .. }
                | EventType::LocalProxyScan
                | EventType::LocalProxyScanDone(_)
                | EventType::FocusWindow { .. } => (),
                EventType::Scroll(scroll) => self.ctx.scroll(scroll),
                EventType::BlinkCursor => {
                    // 切 tab / 切 pane 后 timer 可能还按旧终端的口味在跑；
                    // 每个 tick 都对照当前聚焦终端自检，不该闪就地停表并把
                    // 光标恢复常亮——"残留闪烁"没有活过一个周期的机会。
                    if !self.ctx.cursor_should_blink() {
                        self.ctx.update_cursor_blinking();
                    } else if !*self.ctx.cursor_blink_timed_out {
                        // Only change state when timeout isn't reached, since we could get
                        // BlinkCursor and BlinkCursorTimeout events at the same time.
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
                            let cwd = parts.next().unwrap_or("").to_owned();
                            if self.ctx.nebula_state.cwd != cwd {
                                self.ctx.nebula_state.cwd.clone_from(&cwd);
                                self.ctx.display.nebula_record_directory(&cwd);
                            }
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
                            self.ctx.nebula_state.cwd.clone_from(&cwd);
                            self.ctx.display.nebula_record_directory(&cwd);
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
                            crate::display::extract_program(&self.ctx.nebula_state.last_committed)
                                .map(|program| {
                                    crate::ai_agents::AgentKind::parse(&program)
                                        .map(|agent| agent.slug().to_owned())
                                        .unwrap_or(program)
                                });
                        self.ctx.nebula_state.agent_hook_seen = false;
                        self.ctx.nebula_state.agent_status_rule = None;
                        self.ctx.nebula_state.agent_status_source =
                            crate::ai_agents::AgentStatusSource::Process;
                        self.ctx.nebula_state.agent_status = if self
                            .ctx
                            .nebula_state
                            .running_program
                            .as_deref()
                            .and_then(crate::ai_agents::AgentKind::parse)
                            .is_some()
                        {
                            crate::ai_agents::AgentStatus::Working
                        } else {
                            crate::ai_agents::AgentStatus::Unknown
                        };
                        // Arm the ssh host auto-save: when this command is an
                        // interactive ssh login, hold its destination until a
                        // remote NEBULA| title or a long-enough session
                        // (CommandDone) confirms the connection was real.
                        self.ctx.nebula_state.pending_ssh_host =
                            crate::ssh::ssh_destination(&self.ctx.nebula_state.last_committed);
                        self.ctx.nebula_state.awaiting_input = false;
                        if let Some(run) = &mut self.ctx.nebula_state.active_run
                            && run.phase == crate::runtime_api::RuntimeRunPhase::Submitted
                        {
                            run.phase = crate::runtime_api::RuntimeRunPhase::Started;
                        }
                    },
                    TerminalEvent::CommandDone { exit_code } => {
                        // 新 PTY 初始化提示符也可能先发一个 CommandDone。Runtime
                        // 提交 barrier 尚未冲刷时，它不能结束当前请求。
                        if self.ctx.nebula_state.runtime_submit_barrier.is_some() {
                            return;
                        }
                        // Take (not just clear) the program: the toast below
                        // names it, and reading the field after the reset used
                        // to hand the toast a permanent `None`.
                        let program = self.ctx.nebula_state.running_program.take();
                        // CLI 退回提示符，对话不再是这个 pane 的前台事实；
                        // 留着它，快照会把一个已经退出的会话当活的接续。
                        self.ctx.nebula_state.ai_session = None;
                        self.ctx.nebula_state.agent_hook_seen = false;
                        self.ctx.nebula_state.agent_status = crate::ai_agents::AgentStatus::Unknown;
                        self.ctx.nebula_state.agent_status_source =
                            crate::ai_agents::AgentStatusSource::Unknown;
                        self.ctx.nebula_state.agent_status_rule = None;
                        let pending_ssh = self.ctx.nebula_state.pending_ssh_host.take();
                        self.ctx.nebula_state.awaiting_input = false;
                        if let Some(run) = self.ctx.nebula_state.active_run.take() {
                            self.ctx.nebula_state.last_run = Some(
                                crate::runtime_api::RuntimeRunOutcome::command_done(run, exit_code),
                            );
                        }
                        // 助手错误恢复（spec 001）：Nebula 集成上报的退出码
                        // 走触发判定；裸 133;D（第三方集成）码为 None，静默。
                        if let Some(code) = exit_code {
                            self.maybe_request_ai_fix(code, program.as_deref());
                        }
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
                                // 成败分流：非零码走警示三角，零码走"刚完成"
                                // 的对勾闪现。裸 133;D 没带码（第三方集成），
                                // 那种情况只当作完成，不敢报错。
                                match exit_code {
                                    Some(code) if code != 0 => {
                                        self.ctx.nebula_state.failed_unseen = true;
                                    },
                                    _ => {
                                        self.ctx.nebula_state.finished_at =
                                            Some(std::time::Instant::now());
                                    },
                                }
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
                    TerminalEvent::UserVar { name, value } => {
                        // `nebula_ai_query`（`#` 自然语言转命令）是阶段二的
                        // 消费者；通道先贯通，其余变量目前无人认领。
                        if name == "nebula_ai_query" {
                            info!(
                                "assistant: query channel received ({} chars)",
                                value.chars().count()
                            );
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

                        // Audible bell. AI CLIs ring BEL when a turn ends or
                        // they need input, so this is the "needs you" sound
                        // even with Nebula focused on another tab — the toast
                        // above only fires when the window is unfocused, and
                        // the visual bell is invisible from another tab.
                        // Plays regardless of focus; `platform::beep` throttles
                        // so a looping BEL cannot machine-gun it.
                        if self.ctx.config.bell.audible {
                            crate::platform::beep();
                        }

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
                    TerminalEvent::PtyFailure(reason) => {
                        // 宿主/管道异常死(shell 未退出)。三层裁定:用户有待办
                        // 动作(重开会话)→ 消息栏;toast 不承载唯一副本,必落 log。
                        // 随后到来的 `Exit` 走既有的 tab 关闭路径。
                        crate::display::nebula_debug_log(format!("pty failure: {reason}"));
                        self.ctx.message_buffer.push(Message::new(
                            format!("终端会话异常终止(宿主或管道故障):{reason}"),
                            MessageType::Error,
                        ));
                        self.ctx.display.pending_update.dirty = true;
                    },
                    TerminalEvent::Exit | TerminalEvent::ChildExit(_) | TerminalEvent::Wakeup => (),
                },
                #[cfg(unix)]
                EventType::IpcConfig(_) | EventType::IpcGetConfig(..) | EventType::Shutdown => (),
                EventType::Message(_)
                | EventType::ConfigReload(_)
                | EventType::ConfigReloadReady
                | EventType::TerminalProfilesChanged
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
                        if self.ctx.window().native_live_move() {
                            // During a mixed-DPI drag, Windows can emit several
                            // transient factors before the window settles. Keep
                            // only the newest one and defer glyph/UI work.
                            crate::display::nebula_debug_log(format!(
                                "winmove scale_factor_changed {scale_factor} deferred"
                            ));
                            self.ctx.window().defer_scale_factor(scale_factor);
                        } else {
                            let start = std::time::Instant::now();
                            self.ctx
                                .display
                                .apply_scale_factor_change(scale_factor, self.ctx.config);
                            crate::display::nebula_debug_log(format!(
                                "winmove scale_factor_changed {scale_factor} applied in {:?}",
                                start.elapsed()
                            ));
                        }
                    },
                    WindowEvent::Resized(size) => {
                        // Ignore unreasonably small resizes. A borderless window on
                        // Windows reports a tiny size (~237x39) when minimized instead
                        // of 0x0; honoring it would collapse the terminal grid to a
                        // single row and lose the visible content on restore.
                        if size.width < 100 || size.height < 100 {
                            return;
                        }

                        let defer_native_resize = {
                            let window = self.ctx.window();
                            window.native_live_move() && window.has_pending_scale_factor()
                        };
                        crate::display::nebula_debug_log(format!(
                            "winmove resized {}x{} defer={defer_native_resize}",
                            size.width, size.height
                        ));
                        if defer_native_resize {
                            // A DPI transition is followed by a synthetic
                            // resize on Windows. Keep its physical size until
                            // the native move exits, while ordinary edge
                            // resizing remains fully live.
                            self.ctx.window().defer_inner_size(size);
                            return;
                        }

                        if self.ctx.display.window.allows_drag_resize() {
                            *self.ctx.windowed_size =
                                size.to_logical(self.ctx.display.window.scale_factor);
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
                        // 焦点切换会让输入法宿主重置窗口状态；IME 位置缓存
                        // 必须跟着作废，否则回焦后第一次组合可能拿到陈旧的
                        // 候选窗位置（见 window.rs push_ime_cursor_area）。
                        self.ctx.display.window.reset_ime_cursor_area_cache();

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
                        // Windows 的遮挡事件不可靠：启动早期 / DWM 合成切换
                        // 会误发 `Occluded(true)`，而配对的 `false` 可能永远
                        // 不来。标志被误置后整条 draw 路径熄火——窗口"点什
                        // 么都没反应"，直到最小化再复原靠 Focused(true) 的
                        // 补丁解锁（issue #21）。窗口明明没最小化就发来的
                        // true 一律不信；false 永远接受。
                        let minimized = self.ctx.display.window.is_minimized().unwrap_or(false);
                        if !occluded || minimized {
                            *self.ctx.occluded = occluded;
                        }

                        // Force a full redraw when the window becomes visible again.
                        if !occluded {
                            *self.ctx.dirty = true;
                        }
                    },
                    WindowEvent::DroppedFile(path) => {
                        let over_sftp = if self.ctx.display().nebula_sftp_panel.is_some() {
                            let (x, y, width, height) =
                                self.ctx.display().side_panel_layout().panel;
                            let px = self.ctx.mouse.x as f32;
                            let py = self.ctx.mouse.y as f32;
                            px >= x && px < x + width && py >= y && py < y + height
                        } else {
                            false
                        };
                        if over_sftp {
                            self.ctx.display().sftp_upload_dropped_paths(vec![path]);
                        } else {
                            let path: String = path.to_string_lossy().into();
                            self.ctx.paste(&(path + " "), true);
                        }
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
                                // 设置页的自绘输入框也必须在 IME 提交阶段消费文字。
                                // Windows 中文输入法不会经过 `KeyboardInput` 的字符分支；
                                // 若这里漏掉某个字段，拼音确认后就会穿透到终端。
                                if self.ctx.display().settings_open()
                                    && self.ctx.display().nebula_settings_dropdown
                                        == Some(crate::display::SettingsDropdown::Font)
                                {
                                    self.ctx.display().font_query_edit(Some(&text));
                                } else if self.ctx.display().settings_open()
                                    && self.ctx.display().keymap_search_active()
                                {
                                    self.ctx.display().keymap_search_edit(&text);
                                } else if self.ctx.display().settings_open()
                                    && self.ctx.display().nebula_ssh_proxy_focus.is_some()
                                {
                                    self.ctx.display().ssh_proxy_field_paste(&text);
                                } else if self.ctx.display().settings_open()
                                    && self.ctx.display().nebula_sync_focus.is_some()
                                {
                                    self.ctx.display().sync_field_paste(&text);
                                } else if self.ctx.display().nebula_tab_rename.is_some() {
                                    // Tab rename owns committed text while editing: on
                                    // Windows (and any IME), printable characters are
                                    // delivered here, NOT through key_input — so the
                                    // rename buffer must consume them here or typing
                                    // silently pastes into the shell behind the box.
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
                                self.ctx.display().update_settings_ime_cursor();
                                self.ctx.update_cursor_blinking();
                            },
                            Ime::Preedit(text, cursor_offset) => {
                                let preedit =
                                    (!text.is_empty()).then(|| Preedit::new(text, cursor_offset));

                                if self.ctx.display.ime.preedit() != preedit.as_ref() {
                                    self.ctx.display.ime.set_preedit(preedit);
                                    self.ctx.display.update_settings_ime_cursor();
                                    self.ctx.update_cursor_blinking();
                                    *self.ctx.dirty = true;
                                }
                            },
                            Ime::Enabled => {
                                self.ctx.display.ime.set_enabled(true);
                                // 输入法启用/切换：位置状态从零开始，下一帧
                                // 必须重推（值相同也要推）。
                                self.ctx.display.window.reset_ime_cursor_area_cache();
                                self.ctx.display.update_settings_ime_cursor();
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
