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

mod ai_hook;
mod cli;
mod clipboard;
mod config;
mod daemon;
mod display;
mod event;
#[cfg(windows)]
mod file_uri;
mod input;
mod logging;
#[cfg(target_os = "macos")]
mod macos;
mod markdown;
mod message_bar;
mod migrate;
#[cfg(windows)]
mod mux;
mod nebula_history;
mod notify;
#[cfg(windows)]
mod panic;
#[cfg(unix)]
mod polling;
mod process_tree;
mod renderer;
mod scheduler;
mod session;
mod shell_detect;
#[cfg(windows)]
mod ssh;
#[cfg(windows)]
mod ssh_credentials;
#[cfg(windows)]
mod ssh_profiles;
#[cfg(windows)]
mod ssh_session;
#[cfg(windows)]
mod ssh_sftp;
mod string;
mod window_context;

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

    match options.subcommands {
        #[cfg(unix)]
        Some(Subcommands::Msg(options)) => msg(options)?,
        Some(Subcommands::Migrate(options)) => migrate::migrate(options),
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
    // Mux hand-over: a plain re-launch of Nebula does not start a second
    // terminal — the resident instance re-attaches its detached tabs (their
    // PTYs never stopped) or focuses its window. Explicit intent (-e,
    // --working-directory, --daemon) always starts a real instance.
    #[cfg(windows)]
    {
        let plain_launch = !options.daemon
            && options.window_options.terminal_options.working_directory.is_none()
            && options.window_options.terminal_options.command().is_none();
        if plain_launch && mux::try_attach_existing() {
            return Ok(());
        }
    }
    boot_trace("mux probe done");

    // Setup winit event loop.
    let window_event_loop = EventLoop::<Event>::with_user_event().build()?;
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
    #[cfg(windows)]
    {
        ai_hook::spawn_server(window_event_loop.create_proxy());
        ai_hook::spawn_config_guard();
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

    // Event processor.
    let mut processor = Processor::new(config, options, &window_event_loop);

    // Serve mux attach requests (window re-attach / single instance) for the
    // lifetime of the event loop; dropping it removes the port file.
    #[cfg(windows)]
    let _mux_server = mux::MuxServer::spawn(window_event_loop.create_proxy());

    // Start event loop and block until shutdown.
    let result = processor.run(window_event_loop);

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
