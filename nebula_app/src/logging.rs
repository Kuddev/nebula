//! Logging for Nebula.
//!
//! The main executable is supposed to call `initialize()` exactly once during
//! startup. All logging messages are written to stdout, given that their
//! log-level is sufficient for the level configured in `cli::Options`.

use std::fs::{File, OpenOptions};
use std::io::{self, LineWriter, Stdout, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use std::{env, process};

use log::{Level, LevelFilter};
#[cfg(feature = "legacy-shell")]
use winit::event_loop::EventLoopProxy;

use crate::cli::Options;
#[cfg(feature = "legacy-shell")]
use crate::event::{Event, EventType};
#[cfg(feature = "legacy-shell")]
use crate::message_bar::{Message, MessageType};

/// Logging target for IPC config error messages.
pub const LOG_TARGET_IPC_CONFIG: &str = "nebula_log_window_config";

/// Name for the environment variable containing the log file's path.
const NEBULA_LOG_ENV: &str = "NEBULA_LOG";

/// Logging target for config error messages.
pub const LOG_TARGET_CONFIG: &str = "nebula_config_derive";

/// Logging target for winit events.
pub const LOG_TARGET_WINIT: &str = "nebula_winit_event";

/// Name for the environment variable containing extra logging targets.
///
/// The targets are semicolon separated.
const NEBULA_EXTRA_LOG_TARGETS_ENV: &str = "NEBULA_EXTRA_LOG_TARGETS";

pub(crate) fn debug_log(message: impl AsRef<str>) {
    use std::io::Write as _;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        env::var("NEBULA_DEBUG_LOG").is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    }) {
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    let path = crate::platform::dirs::data_dir().join("nebula_debug.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{timestamp} pid={}] {}", process::id(), message.as_ref());
    }
}

/// User configurable extra log targets to include.
fn extra_log_targets() -> &'static [String] {
    static EXTRA_LOG_TARGETS: OnceLock<Vec<String>> = OnceLock::new();

    EXTRA_LOG_TARGETS.get_or_init(|| {
        env::var(NEBULA_EXTRA_LOG_TARGETS_ENV)
            .map_or(Vec::new(), |targets| targets.split(';').map(ToString::to_string).collect())
    })
}

/// List of targets which will be logged by Nebula.
const ALLOWED_TARGETS: &[&str] = &[
    LOG_TARGET_IPC_CONFIG,
    LOG_TARGET_CONFIG,
    LOG_TARGET_WINIT,
    "nebula_config_derive",
    "nebula_terminal",
    "nebula",
    "crossfont",
];

/// Initialize the logger to its defaults.
pub fn initialize(options: &Options) -> Result<Option<PathBuf>, log::SetLoggerError> {
    initialize_logger(options, Logger::new())
}

/// Initialize the logger with the legacy shell's message-bar event sink.
#[cfg(feature = "legacy-shell")]
pub fn initialize_legacy(
    options: &Options,
    event_proxy: EventLoopProxy<Event>,
) -> Result<Option<PathBuf>, log::SetLoggerError> {
    initialize_logger(options, Logger::new_legacy(event_proxy))
}

fn initialize_logger(
    options: &Options,
    logger: Logger,
) -> Result<Option<PathBuf>, log::SetLoggerError> {
    log::set_max_level(options.log_level());

    let path = logger.file_path();
    log::set_boxed_logger(Box::new(logger))?;

    Ok(path)
}

pub struct Logger {
    logfile: Mutex<OnDemandLogFile>,
    stdout: Mutex<LineWriter<Stdout>>,
    #[cfg(feature = "legacy-shell")]
    event_proxy: Option<Mutex<EventLoopProxy<Event>>>,
    start: Instant,
}

impl Logger {
    fn new() -> Self {
        let logfile = Mutex::new(OnDemandLogFile::new());
        let stdout = Mutex::new(LineWriter::new(io::stdout()));

        Logger {
            logfile,
            stdout,
            #[cfg(feature = "legacy-shell")]
            event_proxy: None,
            start: Instant::now(),
        }
    }

    #[cfg(feature = "legacy-shell")]
    fn new_legacy(event_proxy: EventLoopProxy<Event>) -> Self {
        let mut logger = Self::new();
        logger.event_proxy = Some(Mutex::new(event_proxy));
        logger
    }

    fn file_path(&self) -> Option<PathBuf> {
        let logfile_lock = self.logfile.lock().ok()?;
        Some(logfile_lock.path().clone())
    }

    /// Log a record to the message bar.
    #[cfg(feature = "legacy-shell")]
    fn message_bar_log(&self, record: &log::Record<'_>, logfile_path: &str) {
        let message_type = match record.level() {
            Level::Error => MessageType::Error,
            Level::Warn => MessageType::Warning,
            _ => return,
        };

        let Some(event_proxy) = self.event_proxy.as_ref() else { return };
        let event_proxy = match event_proxy.lock() {
            Ok(event_proxy) => event_proxy,
            Err(_) => return,
        };

        // Show the variable in the reader's own shell syntax. On Windows the
        // default shell is PowerShell, where cmd-style `%NEBULA_LOG%` does not
        // expand (issue #36); use `$env:NEBULA_LOG` so a copy-paste resolves.
        #[cfg(not(windows))]
        let env_var = format!("${NEBULA_LOG_ENV}");
        #[cfg(windows)]
        let env_var = format!("$env:{NEBULA_LOG_ENV}");

        let message = format!(
            "[{}] {}\nSee log at {} ({})",
            record.level(),
            record.args(),
            logfile_path,
            env_var,
        );

        let mut message = Message::new(message, message_type);
        message.set_target(record.target().to_owned());

        let _ = event_proxy.send_event(Event::new(EventType::Message(message), None));
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        // Get target crate.
        let index = record.target().find(':').unwrap_or_else(|| record.target().len());
        let target = &record.target()[..index];

        // Only log our own crates, except when logging at Level::Trace.
        if !self.enabled(record.metadata()) || !is_allowed_target(record.level(), target) {
            return;
        }

        // Create log message for the given `record` and `target`.
        let message = create_log_message(record, target, self.start);

        if let Ok(mut logfile) = self.logfile.lock() {
            // Write to logfile.
            let _ = logfile.write_all(message.as_ref());

            // Log relevant entries to message bar.
            #[cfg(feature = "legacy-shell")]
            self.message_bar_log(record, &logfile.path.to_string_lossy());
        }

        // Write to stdout.
        if let Ok(mut stdout) = self.stdout.lock() {
            let _ = stdout.write_all(message.as_ref());
        }
    }

    fn flush(&self) {}
}

fn create_log_message(record: &log::Record<'_>, target: &str, start: Instant) -> String {
    let runtime = start.elapsed();
    let secs = runtime.as_secs();
    let nanos = runtime.subsec_nanos();
    let mut message = format!("[{}.{:0>9}s] [{:<5}] [{}] ", secs, nanos, record.level(), target);

    // Alignment for the lines after the first new line character in the payload. We don't deal
    // with fullwidth/unicode chars here, so just `message.len()` is sufficient.
    let alignment = message.len();

    // Push lines with added extra padding on the next line, which is trimmed later.
    let lines = record.args().to_string();
    for line in lines.split('\n') {
        let line = format!("{}\n{:width$}", line, "", width = alignment);
        message.push_str(&line);
    }

    // Drop extra trailing alignment.
    message.truncate(message.len() - alignment);
    message
}

/// Check if log messages from a crate should be logged.
fn is_allowed_target(level: Level, target: &str) -> bool {
    match (level, log::max_level()) {
        (Level::Error, LevelFilter::Trace) | (Level::Warn, LevelFilter::Trace) => true,
        _ => ALLOWED_TARGETS.contains(&target) || extra_log_targets().iter().any(|t| t == target),
    }
}

struct OnDemandLogFile {
    file: Option<LineWriter<File>>,
    created: Arc<AtomicBool>,
    path: PathBuf,
}

impl OnDemandLogFile {
    fn new() -> Self {
        let mut path = env::temp_dir();
        path.push(format!("Nebula-{}.log", process::id()));

        // Set log path as an environment variable.
        unsafe { env::set_var(NEBULA_LOG_ENV, path.as_os_str()) };

        OnDemandLogFile { path, file: None, created: Arc::new(AtomicBool::new(false)) }
    }

    fn file(&mut self) -> Result<&mut LineWriter<File>, io::Error> {
        // Allow to recreate the file if it has been deleted at runtime.
        if self.file.is_some() && !self.path.as_path().exists() {
            self.file = None;
        }

        // Create the file if it doesn't exist yet.
        if self.file.is_none() {
            let file = OpenOptions::new().append(true).create_new(true).open(&self.path);

            match file {
                Ok(file) => {
                    self.file = Some(io::LineWriter::new(file));
                    self.created.store(true, Ordering::Relaxed);
                    let _ =
                        writeln!(io::stdout(), "Created log file at \"{}\"", self.path.display());
                },
                Err(e) => {
                    let _ = writeln!(io::stdout(), "Unable to create log file: {e}");
                    return Err(e);
                },
            }
        }

        Ok(self.file.as_mut().unwrap())
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Write for OnDemandLogFile {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.file()?.write(buf)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        self.file()?.flush()
    }
}
