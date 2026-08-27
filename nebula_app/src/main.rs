//! Nebula - The GPU Enhanced Terminal.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
#![cfg_attr(clippy, deny(warnings))]
// With the default subsystem, 'console', windows creates an additional console
// window for the program.
// This is silently ignored on non-windows systems.
// See https://msdn.microsoft.com/en-us/library/4cc7ya5b.aspx for more details.
#![windows_subsystem = "windows"]

#[cfg(not(any(feature = "x11", feature = "wayland", target_os = "macos", windows)))]
compile_error!(r#"at least one of the "x11"/"wayland" features must be enabled"#);

use std::error::Error;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::PathBuf;
use std::{env, fs};

use log::info;
#[cfg(windows)]
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole};
use winit::event_loop::EventLoop;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

use nebula_terminal::tty;

mod agent_env;
mod ai_agents;
mod ai_assistant;
mod ai_hook;
mod ai_providers;
mod ai_sessions;
mod atomic_file;
mod backup_remote;
mod cli;
mod clipboard;
mod codex_config;
mod config;
mod config_cli;
mod daemon;
mod directory_history;
mod display;
mod encrypted_backup;
mod event;
#[cfg(windows)]
mod file_uri;
#[cfg(windows)]
mod font_install;
mod git_worktree;
#[cfg(feature = "gpui-shell")]
mod gpui_shell;
mod input;
mod logging;
#[cfg(target_os = "macos")]
mod macos;
mod markdown;
mod math;
mod message_bar;
mod migrate;
mod motion;
mod mux;
mod nebula_history;
mod notify;
#[cfg(windows)]
mod panic;
mod platform;
#[cfg(unix)]
mod polling;
mod process_tree;
mod remote_dirs;
mod renderer;
mod runtime_api;
mod runtime_exec;
mod scheduler;
mod session;
mod shell_detect;
#[cfg(windows)]
mod ssh;
#[cfg(windows)]
mod ssh_credentials;
#[cfg(windows)]
mod ssh_profiles;
mod ssh_proxy;
#[cfg(windows)]
mod ssh_session;
#[cfg(windows)]
mod ssh_sftp;
mod string;
mod svn_status;
mod taskbar;
mod sync;
mod terminal_profiles;
mod tray;
mod update_check;
#[cfg(feature = "gpui-shell")]
mod update_download;
mod ux;
// pub(crate)：GPUI 壳复用 welcome 的 fastfetch 欢迎屏命令生成。
pub(crate) mod window_context;
mod window_transition;

mod gl {
    #![allow(clippy::all, unsafe_op_in_unsafe_fn)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

#[cfg(unix)]
use crate::cli::MessageOptions;
#[cfg(not(any(target_os = "macos", windows)))]
use crate::cli::SocketMessage;
use crate::cli::{Options, Subcommands};
use crate::config::UiConfig;
use crate::config::monitor::ConfigMonitor;
use crate::event::{Event, Processor};
#[cfg(target_os = "macos")]
use crate::macos::locale;
#[cfg(unix)]
use crate::polling::{IoListener, ipc};

fn main() -> Result<(), Box<dyn Error>> {
    // OpenSSH AskPass reuses the GUI executable as a credential helper. It
    // must exit before CLI parsing or window initialization because ssh passes
    // the human-readable prompt as an argument, not as a Nebula subcommand.
    #[cfg(windows)]
    if let Some(code) = ssh_credentials::run_askpass_from_env() {
        std::process::exit(code);
    }

    boot_trace("main enter");
    #[cfg(windows)]
    panic::attach_handler();

    // When linked with the windows subsystem windows won't automatically attach
    // to the console of the parent process, so we do it explicitly. This fails
    // silently if the parent has no console.
    #[cfg(windows)]
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    // Load command line options.
    let options = Options::new();

    // Portable builds are not necessarily on PATH. Export the exact executable
    // before any PTY is created so Codex/Claude can call the supported control
    // plane directly instead of scanning processes, port files, or source code.
    //
    // This is the fallback layer: every terminal pane gets the full identity
    // contract (`TERM_PROGRAM`, pane id, bin dir, `PATH`) from `agent_env`.
    // Setting it on the process too covers children spawned outside a PTY,
    // which never see `tty::Options::env`.
    #[cfg(windows)]
    if options.subcommands.is_none()
        && let Ok(executable) = env::current_exe()
    {
        // SAFETY: startup is still single-threaded here; all child PTYs are
        // created later and inherit this stable value.
        unsafe { env::set_var(agent_env::CLI_ENV, executable) };
    }

    #[cfg(windows)]
    if options.subcommands.is_none() && env::var_os("NEBULA_DETACHED_LAUNCH").is_some() {
        // 必须在进任何消息循环之前脱离启动控制台。GPUI 以前在这条
        // FreeConsole 之前就 `return`，启动器（agent 作业对象）一退出
        // 窗口就被带走。旧壳注释同一合同。
        unsafe {
            FreeConsole();
        }
    }

    // 产品主窗：GPUI 作为 nebula.exe 的 UI 层，从主线程直接进 GPUI
    // 消息循环，winit 旧壳完全不启动。1.1.0 安装包 / 双击 / 资源管理器
    // 右键都走这里；`--legacy-shell` 才回旧壳。
    //
    // mux probe 必须在 `run_shell` 之前：第二份 GUI 进程交给驻留实例
    // （ATTACH），不能再拉一套 PTY。
    #[cfg(feature = "gpui-shell")]
    if wants_gpui_shell(&options) {
        #[cfg(windows)]
        if try_hand_over_to_resident(&options) {
            return Ok(());
        }
        let initial_cwd = options
            .window_options
            .terminal_options
            .resolved_working_directory()
            .filter(|path| path.is_dir());
        gpui_shell::run_shell(initial_cwd);
        return Ok(());
    }

    // C 路线 spike：GPUI UI 层跑在专用线程，拥有自己的消息循环与窗口；
    // 与主线程的 winit 循环互不接管。仅在显式设置环境变量时启动，
    // 用于验证双 UI 运行时共存（焦点/IME/DPI）；P3 主窗接管完成后移除。
    #[cfg(feature = "gpui-shell")]
    if std::env::var_os("NEBULA_GPUI_SHELL").is_some() {
        std::thread::Builder::new()
            .name("gpui-shell".into())
            .spawn(|| gpui_shell::run_shell(None))
            .expect("spawn gpui-shell thread");
    }

    match options.subcommands {
        Some(Subcommands::Ctl(options)) => runtime_api::run_cli(options)?,
        // Resource-verb commands are thin aliases over `ctl`, speaking the same
        // serialized protocol (see `runtime_api::shortcuts`).
        Some(Subcommands::Env(options)) => runtime_api::shortcuts::env(options)?,
        Some(Subcommands::Window(options)) => runtime_api::shortcuts::window(options)?,
        Some(Subcommands::Tab(options)) => runtime_api::shortcuts::tab(options)?,
        Some(Subcommands::Pane(options)) => runtime_api::shortcuts::pane(options)?,
        Some(Subcommands::Agent(options)) => runtime_api::shortcuts::agent(options)?,
        #[cfg(unix)]
        Some(Subcommands::Msg(options)) => msg(options)?,
        Some(Subcommands::Migrate(options)) => migrate::migrate(options),
        Some(Subcommands::Config(options)) => std::process::exit(config_cli::run(options)),
        #[cfg(windows)]
        Some(Subcommands::NotifyTest) => std::process::exit(crate::notify::notify_test()),
        #[cfg(windows)]
        Some(Subcommands::SetupAi(options)) => {
            std::process::exit(crate::ai_hook::setup_ai_cli(options.remove))
        },
        #[cfg(windows)]
        Some(Subcommands::Ssh(options)) => std::process::exit(crate::ssh::run(options.args)),
        None => nebula(options)?,
    }

    Ok(())
}

/// `msg` subcommand entrypoint.
#[cfg(unix)]
#[allow(unused_mut)]
fn msg(mut options: MessageOptions) -> Result<(), Box<dyn Error>> {
    #[cfg(not(any(target_os = "macos", windows)))]
    if let SocketMessage::CreateWindow(window_options) = &mut options.message {
        window_options.activation_token =
            env::var("XDG_ACTIVATION_TOKEN").or_else(|_| env::var("DESKTOP_STARTUP_ID")).ok();
    }
    ipc::send_message(options.socket, options.message).map_err(|err| err.into())
}

/// Temporary files stored for Nebula.
///
/// This stores temporary files to automate their destruction through its `Drop` implementation.
struct TemporaryFiles {
    #[cfg(unix)]
    socket_path: Option<PathBuf>,
    log_file: Option<PathBuf>,
}

impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        // Clean up the IPC socket file.
        #[cfg(unix)]
        if let Some(socket_path) = self.socket_path.as_deref() {
            let _ = fs::remove_file(socket_path);
        }

        // Clean up logfile.
        if let Some(log_file) = &self.log_file {
            if fs::remove_file(log_file).is_ok() {
                let _ = writeln!(io::stdout(), "Deleted log file at \"{}\"", log_file.display());
            }
        }
    }
}

/// Startup profiling: `NEBULA_BOOT_TRACE=1 nebula` prints a per-stage
/// timeline to stderr, timed from process entry. First call sets t=0, so it
/// must be the first statement in `main`.
pub(crate) fn boot_trace(label: &str) {
    use std::sync::OnceLock;
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    static ON: OnceLock<bool> = OnceLock::new();
    let t0 = *T0.get_or_init(Instant::now);
    if *ON.get_or_init(|| std::env::var_os("NEBULA_BOOT_TRACE").is_some()) {
        eprintln!("[boot +{:>7.1}ms] {label}", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Run main Nebula entrypoint.
///
/// Creates a window, the terminal state, PTY, I/O event loop, input processor,
/// config change monitor, and runs the main display loop.
fn nebula(mut options: Options) -> Result<(), Box<dyn Error>> {
    // WebDAV 启动自动拉取（spec 003）：守门函数一次文件读判断是否配置；
    // 未配置的用户到此为止，加密/网络代码一行不跑。结果静默（warn log），
    // settings 变化由 mtime 监视生效，避免刚启动就弹消息条。
    if sync::auto_pull_enabled() {
        std::thread::spawn(|| {
            let result = sync::pull();
            sync::warn_result(&result);
        });
    }

    // Mux hand-over: a plain re-launch of Nebula does not start a second
    // terminal — the resident instance re-attaches its detached tabs (their
    // PTYs never stopped) or focuses its window. Explicit intent (-e,
    // --daemon) always starts a real instance.
    #[cfg(windows)]
    if try_hand_over_to_resident(&options) {
        return Ok(());
    }
    boot_trace("mux probe done");

    // Setup winit event loop. Windows observes native size/move stages via a
    // WinEvent hook installed on this thread (see window_transition); winit's
    // own message dispatch is untouched.
    let native_window_stages = window_transition::NativeWindowStageTracker::new();
    let mut event_loop_builder = EventLoop::<Event>::with_user_event();
    let window_event_loop = event_loop_builder.build()?;
    boot_trace("event loop built");

    // Initialize the logger as soon as possible as to capture output from other subsystems.
    let log_file = logging::initialize(&options, window_event_loop.create_proxy())
        .expect("Unable to initialize logger");

    info!("Welcome to Nebula");
    info!("Version {}", env!("VERSION"));

    // Real-time AI-CLI turn notifications: the named-pipe server must exist
    // before the first PTY spawns (children inherit NEBULA_NOTIFY_PIPE), and
    // the settings guard installs claude's hooks / codex's notify now and
    // re-installs them whenever another tool (ccswitch…) rewrites the config.
    // The notify proxy powers toast click-to-focus. See ai_hook / notify.
    notify::init_proxy(window_event_loop.create_proxy());
    update_check::spawn_once(window_event_loop.create_proxy());
    #[cfg(windows)]
    {
        ai_hook::spawn_server(window_event_loop.create_proxy());
        ai_hook::spawn_config_guard();
        // 托盘 attention（T1-3）：常驻图标 + agent 状态菜单。开关读原始
        // 设置存储——事件循环还没有任何窗口/Display 可问。
        tray::init(window_event_loop.create_proxy());
        tray::set_enabled(display::tray_enabled());
    }

    #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
    info!(
        "Running on {}",
        if matches!(
            window_event_loop.display_handle().unwrap().as_raw(),
            RawDisplayHandle::Wayland(_)
        ) {
            "Wayland"
        } else {
            "X11"
        }
    );
    #[cfg(not(any(feature = "x11", target_os = "macos", windows)))]
    info!("Running on Wayland");

    // Load configuration file.
    let config = config::load(&mut options);
    log_config_path(&config);
    boot_trace("config loaded");

    // Update the log level from config.
    log::set_max_level(config.debug.log_level);

    // Set tty environment variables.
    tty::setup_env();

    // Set env vars from config.
    for (key, value) in config.env.iter() {
        unsafe { env::set_var(key, value) };
    }

    // Switch to home directory.
    #[cfg(target_os = "macos")]
    env::set_current_dir(home::home_dir().unwrap()).unwrap();

    // Set macOS locale.
    #[cfg(target_os = "macos")]
    locale::set_locale_environment();

    #[cfg(target_os = "macos")]
    macos::disable_autofill();

    // Spawn the Unix I/O event polling thread.
    #[cfg(unix)]
    let socket_path = match IoListener::spawn(&config, &options, window_event_loop.create_proxy()) {
        Ok(handle) => handle.ipc_socket_path,
        Err(err) if options.daemon => return Err(err.into()),
        Err(err) => {
            log::warn!("Unable to create socket: {err:?}");
            None
        },
    };

    // Setup automatic RAII cleanup for our files.
    let log_cleanup = log_file.filter(|_| !config.debug.persistent_logging);
    let _files = TemporaryFiles {
        #[cfg(unix)]
        socket_path,
        log_file: log_cleanup,
    };

    // The server and event processor share only an immutable snapshot hub;
    // every runtime mutation still crosses the typed winit event protocol.
    let runtime_hub = runtime_api::RuntimeHub::new();
    let mut processor = Processor::new(
        config,
        options,
        &window_event_loop,
        native_window_stages,
        runtime_hub.clone(),
    );

    // Serve both the legacy attach verb and the versioned runtime API for the
    // lifetime of the event loop; dropping it removes the discovery file.
    let _runtime_server =
        runtime_api::RuntimeServer::spawn(window_event_loop.create_proxy(), runtime_hub);

    // Start event loop and block until shutdown.
    let result = processor.run(window_event_loop);

    // 摘掉托盘图标：进程退出后残影会留在通知区直到用户悬停。
    #[cfg(windows)]
    tray::shutdown();

    // `Processor` must be dropped before calling `FreeConsole`.
    //
    // This is needed for ConPTY backend. Otherwise a deadlock can occur.
    // The cause:
    //   - Drop for ConPTY will deadlock if the conout pipe has already been dropped
    //   - ConPTY is dropped when the last of processor and window context are dropped, because both
    //     of them own an Arc<ConPTY>
    //
    // The fix is to ensure that processor is dropped first. That way, when window context (i.e.
    // PTY) is dropped, it can ensure ConPTY is dropped before the conout pipe in the PTY drop
    // order.
    //
    // FIXME: Change PTY API to enforce the correct drop order with the typesystem.

    // Terminate the config monitor.
    if let Some(config_monitor) = processor.config_monitor.take() {
        config_monitor.shutdown();
    }

    // Without explicitly detaching the console cmd won't redraw it's prompt.
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }

    info!("Goodbye");

    result
}

/// `gpui-shell` 构建的 GUI 默认进 GPUI。子命令、daemon、ref-test 和
/// `--legacy-shell` 仍走旧路径。`--gpui` 只是显式同义开关。
#[cfg(feature = "gpui-shell")]
fn wants_gpui_shell(options: &Options) -> bool {
    if options.legacy_shell || options.subcommands.is_some() || options.daemon || options.ref_test {
        return false;
    }
    true
}

/// Windows 单实例交接：普通启动或带工作目录、无 `-e` 时，先 ATTACH 到
/// 驻留进程，再 `tab.new`。GPUI 与 winit 共用，避免第二份进程无声退出。
#[cfg(windows)]
fn try_hand_over_to_resident(options: &Options) -> bool {
    let has_command = options.window_options.terminal_options.command().is_some();
    let launch_dir = options
        .window_options
        .terminal_options
        .resolved_working_directory()
        .filter(|path| path.is_dir());
    if !options.daemon
        && !has_command
        && nebula_settings::RuntimeSettings::load().windowing_behavior
            == nebula_settings::WindowingBehaviorName::UseNew
    {
        return runtime_api::try_open_window_existing(launch_dir.as_deref());
    }
    let plain_launch = !options.daemon
        && options.window_options.terminal_options.working_directory.is_none()
        && !has_command;
    if plain_launch && runtime_api::try_open_default_tab_existing() {
        return true;
    }
    // Explorer 右键「在 Nebula 中打开」带着 --working-directory 走到这里。
    // 带目录、无 -e 命令的启动优先并入驻留实例——ATTACH 恢复窗口，再在
    // 其中打开定目录标签；没有驻留实例时照旧独立启动。
    let dir_launch = !options.daemon && !has_command;
    if dir_launch
        && let Some(dir) = launch_dir
        && runtime_api::try_open_directory_existing(&dir)
    {
        return true;
    }
    false
}

fn log_config_path(config: &UiConfig) {
    if config.config_paths.is_empty() {
        return;
    }

    let mut msg = String::from("Configuration files loaded from:");
    for path in &config.config_paths {
        let _ = write!(msg, "\n  {:?}", path.display());
    }

    info!("{msg}");
}
