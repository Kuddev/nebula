use std::cmp::max;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::rc::Rc;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, ValueHint};
use log::{LevelFilter, error};
use nebula_config::SerdeReplace;
use serde::{Deserialize, Serialize};
use toml::Value;

use nebula_terminal::tty::Options as PtyOptions;

use crate::config::UiConfig;
use crate::config::ui_config::Program;
use crate::config::window::{Class, Identity};
use crate::logging::LOG_TARGET_IPC_CONFIG;

/// CLI options for the main Nebula executable.
#[derive(Parser, Default, Debug)]
#[clap(author, about, version = env!("VERSION"))]
pub struct Options {
    /// Print all events to STDOUT.
    #[clap(long)]
    pub print_events: bool,

    /// Generates ref test.
    #[clap(long, conflicts_with("daemon"))]
    pub ref_test: bool,

    /// X11 window ID to embed Nebula within (decimal or hexadecimal with "0x" prefix).
    #[clap(long)]
    pub embed: Option<String>,

    /// Launch the GPUI UI shell as the main window.
    ///
    /// Optional on 1.1.0+ `gpui-shell` builds: the GUI already defaults to
    /// GPUI. Kept so existing launchers and scripts keep working.
    #[cfg(feature = "gpui-shell")]
    #[clap(long, conflicts_with = "legacy_shell")]
    pub gpui: bool,

    /// Launch the legacy winit shell instead of GPUI.
    #[cfg(feature = "gpui-shell")]
    #[clap(long = "legacy-shell")]
    pub legacy_shell: bool,

    /// Specify alternative configuration file [default:
    /// $XDG_CONFIG_HOME/nebula/nebula.toml].
    #[cfg(not(any(target_os = "macos", windows)))]
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Specify alternative configuration file [default: %APPDATA%\nebula\nebula.toml].
    #[cfg(windows)]
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Specify alternative configuration file [default: $HOME/.config/nebula/nebula.toml].
    #[cfg(target_os = "macos")]
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Path for IPC socket creation.
    #[cfg(unix)]
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub socket: Option<PathBuf>,

    /// Reduces the level of verbosity (the min level is -qq).
    #[clap(short, conflicts_with("verbose"), action = ArgAction::Count)]
    quiet: u8,

    /// Increases the level of verbosity (the max level is -vvv).
    #[clap(short, conflicts_with("quiet"), action = ArgAction::Count)]
    verbose: u8,

    /// Do not spawn an initial window.
    #[clap(long)]
    pub daemon: bool,

    /// CLI options for config overrides.
    #[clap(skip)]
    pub config_options: ParsedOptions,

    /// Options which can be passed via IPC.
    #[clap(flatten)]
    pub window_options: WindowOptions,

    /// Subcommand passed to the CLI.
    #[clap(subcommand)]
    pub subcommands: Option<Subcommands>,
}

impl Options {
    pub fn new() -> Self {
        let mut options = Self::parse();

        // Parse CLI config overrides.
        options.config_options = options.window_options.config_overrides();

        options
    }

    /// Override configuration file with options from the CLI.
    pub fn override_config(&mut self, config: &mut UiConfig) {
        #[cfg(unix)]
        if self.socket.is_some() {
            config.ipc_socket = Some(true);
        }

        config.window.embed = self.embed.as_ref().and_then(|embed| parse_hex_or_decimal(embed));
        config.debug.print_events |= self.print_events;
        config.debug.log_level = max(config.debug.log_level, self.log_level());
        config.debug.ref_test |= self.ref_test;

        if config.debug.print_events {
            config.debug.log_level = max(config.debug.log_level, LevelFilter::Info);
        }

        // Replace CLI options.
        self.config_options.override_config(config);
    }

    /// Logging filter level.
    pub fn log_level(&self) -> LevelFilter {
        match (self.quiet, self.verbose) {
            // Force at least `Info` level for `--print-events`.
            (_, 0) if self.print_events => LevelFilter::Info,

            // Default.
            (0, 0) => LevelFilter::Warn,

            // Verbose.
            (_, 1) => LevelFilter::Info,
            (_, 2) => LevelFilter::Debug,
            (0, _) => LevelFilter::Trace,

            // Quiet.
            (1, _) => LevelFilter::Error,
            (..) => LevelFilter::Off,
        }
    }
}

/// Parse the class CLI parameter.
fn parse_class(input: &str) -> Result<Class, String> {
    let (general, instance) = match input.split_once(',') {
        // Warn the user if they've passed too many values.
        Some((_, instance)) if instance.contains(',') => {
            return Err(String::from("Too many parameters"));
        },
        Some((general, instance)) => (general, instance),
        None => (input, input),
    };

    Ok(Class::new(general, instance))
}

/// Convert to hex if possible, else decimal
fn parse_hex_or_decimal(input: &str) -> Option<u32> {
    input
        .strip_prefix("0x")
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .or_else(|| input.parse().ok())
}

/// Terminal specific cli options which can be passed to new windows via IPC.
#[derive(Serialize, Deserialize, Args, Default, Debug, Clone, PartialEq, Eq)]
pub struct TerminalOptions {
    /// Start the shell in the specified working directory.
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub working_directory: Option<PathBuf>,

    /// Remain open after child process exit.
    #[clap(long)]
    pub hold: bool,

    /// Command and args to execute (must be last argument).
    #[clap(short = 'e', long, allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

/// Undo the trailing-backslash mangling Explorer's context-menu quoting
/// inflicts on drive-root working directories.
///
/// The installed verb passes `--working-directory "%V"`; for a drive root
/// `%V` is `D:\`, so the command line ends in `\"` and `CommandLineToArgvW`
/// reads that trailing backslash as an escaped quote — Nebula receives `D:"`
/// and rejects it as an invalid directory (issue #36). A double quote can
/// never appear in a Windows path, so a trailing one is unambiguously that
/// swallowed separator: restore it. Any other path is returned unchanged, and
/// off Windows the input is untouched (a quote is a legal Unix filename byte).
pub(crate) fn repair_context_menu_dir(dir: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(stripped) = dir.to_string_lossy().strip_suffix('"') {
            return PathBuf::from(format!("{stripped}\\"));
        }
    }
    dir
}

impl TerminalOptions {
    /// Shell override passed through the CLI.
    pub fn command(&self) -> Option<Program> {
        let (program, args) = self.command.split_first()?;
        Some(Program::WithArgs { program: program.clone(), args: args.to_vec() })
    }

    /// Working directory with Explorer's context-menu quote-mangling undone.
    /// Every shell-launching path resolves the directory through here so the
    /// repair applies whether the options came from the CLI or over IPC.
    pub fn resolved_working_directory(&self) -> Option<PathBuf> {
        self.working_directory.clone().map(repair_context_menu_dir)
    }

    /// Override the [`PtyOptions`]'s fields with the [`TerminalOptions`].
    pub fn override_pty_config(&self, pty_config: &mut PtyOptions) {
        if let Some(working_directory) = self.resolved_working_directory() {
            if working_directory.is_dir() {
                pty_config.working_directory = Some(working_directory);
            } else {
                error!("Invalid working directory: {working_directory:?}");
            }
        }

        if let Some(command) = self.command() {
            pty_config.shell = Some(command.into());
        }

        pty_config.drain_on_exit |= self.hold;
    }
}

impl From<TerminalOptions> for PtyOptions {
    fn from(mut options: TerminalOptions) -> Self {
        let working_directory = options.resolved_working_directory();
        options.working_directory = None;
        PtyOptions {
            working_directory,
            shell: options.command().map(Into::into),
            drain_on_exit: options.hold,
            env: HashMap::new(),
            #[cfg(target_os = "windows")]
            escape_args: false,
        }
    }
}

/// Window specific cli options which can be passed to new windows via IPC.
#[derive(Serialize, Deserialize, Args, Default, Debug, Clone, PartialEq, Eq)]
pub struct WindowIdentity {
    /// Defines the window title [default: Nebula Terminal].
    #[clap(short = 'T', short_alias('t'), long)]
    pub title: Option<String>,

    /// Defines window class/app_id on X11/Wayland [default: Nebula].
    #[clap(long, value_name = "general> | <general>,<instance", value_parser = parse_class)]
    pub class: Option<Class>,
}

impl WindowIdentity {
    /// Override the [`WindowIdentity`]'s fields with the [`WindowOptions`].
    pub fn override_identity_config(&self, identity: &mut Identity) {
        if let Some(title) = &self.title {
            identity.title.clone_from(title);
        }
        if let Some(class) = &self.class {
            identity.class.clone_from(class);
        }
    }
}

/// Available CLI subcommands.
#[derive(Subcommand, Debug)]
pub enum Subcommands {
    /// Agent-oriented terminal control: split panes, run commands, start Codex/Claude,
    /// send prompts, wait for state changes, and read verified terminal output.
    Ctl(ControlOptions),
    /// Report this pane's terminal identity, the control-plane path, and every
    /// command available to it. Answers even with no runtime reachable, so an
    /// agent can always discover what it has instead of guessing.
    Env(EnvOptions),
    /// Control terminal windows.
    Window(WindowResourceOptions),
    /// Control tabs within one terminal window.
    Tab(TabResourceOptions),
    /// Inspect and drive terminal panes: list, read, send, paste, wait, and change layout.
    Pane(PaneOptions),
    /// Inspect and drive the AI agents running in panes: list, send, read, wait.
    Agent(AgentOptions),
    #[cfg(unix)]
    Msg(MessageOptions),
    Migrate(MigrateOptions),
    /// Validate or create the Nebula configuration.
    Config(ConfigOptions),
    /// Test system notification (toast) delivery.
    #[cfg(windows)]
    NotifyTest,
    /// Install (or --remove) AI hooks plus the Nebula Runtime Skill for
    /// Codex and Claude Code.
    #[cfg(windows)]
    SetupAi(SetupAiOptions),
    /// SSH with Nebula shell integration bootstrapped on the remote host, so
    /// tab icons / spinner / cwd track the program running over the connection
    /// (claude, vim, cargo…). All arguments are forwarded to the system `ssh`.
    #[cfg(windows)]
    Ssh(SshOptions),
}

/// Query timeout shared by the short commands. Long enough to ride out a busy
/// UI thread, short enough that a wedged runtime does not hang an agent.
const SHORT_TIMEOUT_MS: u64 = 30_000;

/// Default ceiling for `nebula pane wait` / `nebula agent wait`. Waiting on a
/// coding agent is measured in minutes, so reusing the query timeout would turn
/// a normal turn into a spurious `timeout` error.
const SHORT_WAIT_TIMEOUT_MS: u64 = 600_000;

/// Flags every short command accepts. Kept tiny on purpose: this surface exists
/// so an agent can act without first learning a flag vocabulary.
#[derive(Args, Debug)]
pub struct ShortOutput {
    /// Pretty-print the JSON response. Output is JSON either way.
    #[clap(long)]
    pub pretty: bool,
}

#[derive(Args, Debug)]
pub struct EnvOptions {
    #[clap(flatten)]
    pub output: ShortOutput,

    /// Maximum time to wait for the runtime probe. The environment half of the
    /// answer is reported regardless.
    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct WindowResourceOptions {
    #[clap(subcommand)]
    pub command: WindowCommand,
}

#[derive(Subcommand, Debug)]
pub enum WindowCommand {
    /// Close an idle window. Busy panes require confirmation in the GUI instead.
    Close(WindowCloseOptions),
}

#[derive(Args, Debug)]
pub struct WindowCloseOptions {
    /// Window id from `nebula pane list`.
    pub window: u64,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct TabResourceOptions {
    #[clap(subcommand)]
    pub command: TabCommand,
}

/// Tab indices are scoped to one window, so `--window` is always required.
#[derive(Subcommand, Debug)]
pub enum TabCommand {
    /// Close an idle tab.
    Close(TabCloseOptions),
    /// Set a tab's custom name. Pass an empty name to restore the generated title.
    Rename(TabRenameOptions),
    /// Move a tab to another index in the same window.
    Move(TabMoveOptions),
}

#[derive(Args, Debug)]
pub struct TabCloseOptions {
    /// Zero-based tab index from the runtime snapshot.
    pub tab: usize,

    /// Window containing the tab.
    #[clap(long)]
    pub window: u64,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct TabRenameOptions {
    /// Zero-based tab index from the runtime snapshot.
    pub tab: usize,

    /// New custom name. An empty string restores the generated title.
    pub name: String,

    /// Window containing the tab.
    #[clap(long)]
    pub window: u64,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct TabMoveOptions {
    /// Current zero-based tab index.
    pub tab: usize,

    /// Destination zero-based tab index in the same window.
    pub to: usize,

    /// Window containing the tab.
    #[clap(long)]
    pub window: u64,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneOptions {
    #[clap(subcommand)]
    pub command: PaneCommand,
}

/// Pane verbs. A pane is addressed by its numeric id from `nebula pane list`.
#[derive(Subcommand, Debug)]
pub enum PaneCommand {
    /// List every pane with its id, task state, cwd, and Git branch.
    List(ListOptions),
    /// Read the tail of a pane's real terminal buffer.
    Read(PaneReadOptions),
    /// Write one line into a pane, submitting it with Enter by default.
    Send(PaneSendOptions),
    /// Paste bounded UTF-8 text as one bracketed block.
    Paste(PanePasteOptions),
    /// Block until a pane reaches a semantic task state.
    Wait(PaneWaitOptions),
    /// Execute an argv vector as an independent non-TTY child and capture both streams.
    Exec(PaneExecOptions),
    /// Close an idle pane.
    Close(PaneCloseOptions),
    /// Explicitly enable or disable focused-pane zoom for the pane's tab.
    Zoom(PaneZoomOptions),
    /// Set this pane's share of its direct parent split.
    Resize(PaneResizeOptions),
}

#[derive(Args, Debug)]
pub struct AgentOptions {
    #[clap(subcommand)]
    pub command: AgentCommand,
}

/// Agent verbs. An agent is addressed by the name or stable id from
/// `nebula agent list` — not by pane, so a restarted session cannot silently
/// inherit work aimed at the one it replaced.
#[derive(Subcommand, Debug)]
pub enum AgentCommand {
    /// List the panes running an AI CLI, with session identity and generation.
    List(ListOptions),
    /// Hand one task to an agent and submit it.
    Send(AgentSendOptions),
    /// Paste bounded UTF-8 text into an agent as one bracketed block.
    Paste(AgentPasteOptions),
    /// Read the tail of what an agent printed.
    Read(AgentReadOptions),
    /// Block until an agent's turn ends.
    Wait(AgentWaitOptions),
}

#[derive(Args, Debug)]
pub struct ListOptions {
    #[clap(flatten)]
    pub output: ShortOutput,

    /// Restrict the listing to one window.
    #[clap(long)]
    pub window: Option<u64>,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneReadOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    /// Logical terminal rows to read from the buffer tail.
    #[clap(long, default_value_t = crate::runtime_api::DEFAULT_READ_LINES)]
    pub lines: usize,

    /// Disambiguate the pane when several windows are open.
    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneSendOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    /// The line to write. Several words are joined with single spaces, so both
    /// `send 17 "cargo test"` and `send 17 cargo test` work.
    #[clap(required = true, num_args = 1..)]
    pub text: Vec<String>,

    /// Write the text without pressing Enter.
    #[clap(long)]
    pub no_submit: bool,

    /// After submitting, block until the pane settles again. The baseline comes
    /// from the submission response, so a pane that was already idle cannot
    /// satisfy the wait immediately. Meaningless without submission, so it
    /// conflicts with `--no-submit` rather than silently doing nothing.
    #[clap(long, conflicts_with = "no_submit")]
    pub wait: bool,

    /// Ceiling for `--wait`.
    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub wait_timeout_ms: u64,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

/// A paste has exactly one local source. Runtime requests receive only the
/// validated UTF-8 payload; local paths and stdin handles never cross the
/// control-plane boundary.
#[derive(Args, Debug)]
pub struct PasteSourceOptions {
    /// Literal text. Multiple words are joined with single spaces.
    #[clap(num_args = 0.., conflicts_with_all = ["stdin", "from_file"])]
    pub text: Vec<String>,

    /// Read the paste payload from standard input.
    #[clap(long, conflicts_with = "from_file")]
    pub stdin: bool,

    /// Read the paste payload from a UTF-8 file.
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub from_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct PanePasteOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    #[clap(flatten)]
    pub source: PasteSourceOptions,

    /// Paste the block without pressing Enter.
    #[clap(long)]
    pub no_submit: bool,

    /// After submitting, block until the pane settles again.
    #[clap(long, conflicts_with = "no_submit")]
    pub wait: bool,

    /// Ceiling for `--wait`.
    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub wait_timeout_ms: u64,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneWaitOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    /// The state to wait for. `settled` covers finished, failed, and
    /// waiting-input, which is what "the work is over" usually means.
    #[clap(long, value_enum, default_value = "settled")]
    pub state: ControlWaitState,

    /// Require the pane's `state_change_seq` to advance past this value. Pass
    /// the counter observed *before* dispatching work, otherwise a pane that is
    /// already settled satisfies the wait immediately.
    #[clap(long)]
    pub after_seq: Option<u64>,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneExecOptions {
    /// Pane id from `nebula pane list`; its current cwd and local environment are reused.
    pub pane: u64,

    #[clap(long)]
    pub window: Option<u64>,

    /// Maximum bytes retained from each stream. Both streams are always fully drained.
    #[clap(long, default_value_t = crate::runtime_api::DEFAULT_EXEC_OUTPUT_BYTES)]
    pub max_output_bytes: usize,

    #[clap(flatten)]
    pub output: ShortOutput,

    /// Child-process timeout.
    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,

    /// Program and arguments. `--` is required so child flags cannot be parsed by Nebula.
    #[clap(last = true, required = true, num_args = 1.., value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[derive(Args, Debug)]
pub struct PaneCloseOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneZoomOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    /// Desired zoom state. Requiring the value keeps the command idempotent.
    #[clap(
        long,
        required = true,
        action = ArgAction::Set,
        value_parser = clap::value_parser!(bool)
    )]
    pub zoomed: bool,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct PaneResizeOptions {
    /// Pane id from `nebula pane list`.
    pub pane: u64,

    /// Desired share of the pane's direct parent split, from 0.05 through 0.95.
    pub ratio: f32,

    #[clap(long)]
    pub window: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentSendOptions {
    /// Agent name or stable id from `nebula agent list`.
    pub agent: String,

    /// The task itself. Several words are joined with single spaces, so both
    /// `send codex "fix login"` and `send codex fix login` work.
    #[clap(required = true, num_args = 1..)]
    pub text: Vec<String>,

    /// Write the task without submitting it.
    #[clap(long)]
    pub no_submit: bool,

    /// After submitting, block until the turn ends. The baseline comes from the
    /// submission response, so an idle agent cannot satisfy the wait
    /// immediately. Conflicts with `--no-submit`, which never starts a turn.
    #[clap(long, conflicts_with = "no_submit")]
    pub wait: bool,

    /// Ceiling for `--wait`.
    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub wait_timeout_ms: u64,

    /// Refuse the send unless the agent is still this generation.
    #[clap(long)]
    pub generation: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentPasteOptions {
    /// Agent name or stable id from `nebula agent list`.
    pub agent: String,

    #[clap(flatten)]
    pub source: PasteSourceOptions,

    /// Paste the block without submitting it.
    #[clap(long)]
    pub no_submit: bool,

    /// After submitting, block until the turn ends.
    #[clap(long, conflicts_with = "no_submit")]
    pub wait: bool,

    /// Ceiling for `--wait`.
    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub wait_timeout_ms: u64,

    /// Refuse the paste unless the agent is still this generation.
    #[clap(long)]
    pub generation: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentReadOptions {
    /// Agent name or stable id from `nebula agent list`.
    pub agent: String,

    /// Logical terminal rows to read from the buffer tail.
    #[clap(long, default_value_t = crate::runtime_api::DEFAULT_READ_LINES)]
    pub lines: usize,

    #[clap(long)]
    pub generation: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentWaitOptions {
    /// Agent name or stable id from `nebula agent list`.
    pub agent: String,

    /// The state to wait for.
    #[clap(long, value_enum, default_value = "settled")]
    pub state: ControlWaitState,

    /// Require the agent's `state_change_seq` to advance past this value. Pass
    /// the counter observed *before* dispatching work, otherwise an agent that
    /// is already idle satisfies the wait immediately.
    #[clap(long)]
    pub after_seq: Option<u64>,

    /// Pin the wait to one generation. Without it, the currently active
    /// generation is resolved first, so a session that was replaced in the
    /// meantime fails loudly instead of being waited on by mistake.
    #[clap(long)]
    pub generation: Option<u64>,

    #[clap(flatten)]
    pub output: ShortOutput,

    #[clap(long, default_value_t = SHORT_WAIT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct ConfigOptions {
    #[clap(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Validate a Lua, TOML, or YAML configuration without opening the GUI.
    Check(ConfigCheckOptions),
    /// Create an annotated Lua configuration template.
    Init(ConfigInitOptions),
}

#[derive(Args, Debug)]
pub struct ConfigCheckOptions {
    /// Configuration file to validate; otherwise use normal discovery.
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ConfigInitOptions {
    /// Lua configuration path; otherwise use the platform user config directory.
    #[clap(long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Comment language for the generated template.
    #[clap(long, value_enum, default_value = "system")]
    pub language: ConfigLanguage,

    /// Back up and replace an existing configuration.
    #[clap(long)]
    pub force: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLanguage {
    #[value(name = "system")]
    System,
    #[value(name = "zh-CN")]
    ZhCn,
    #[value(name = "en-US")]
    EnUs,
}

impl ConfigLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

/// Options for the `setup-ai` subcommand.
#[cfg(windows)]
#[derive(Args, Debug)]
pub struct SetupAiOptions {
    /// Remove Nebula's hooks from claude's settings.json instead of
    /// installing them.
    #[clap(long)]
    pub remove: bool,
}

/// Options for the `ssh` subcommand: every token after `ssh` is captured raw
/// and handed to the system `ssh` binary (host, `-p`, `-i`, `-L`, …), so
/// `nebula ssh -p 2222 user@host` behaves exactly like the real client.
#[cfg(windows)]
#[derive(Args, Debug)]
pub struct SshOptions {
    /// All arguments forwarded verbatim to `ssh` (destination and flags).
    #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// Options for the cross-platform runtime control API used by humans and coding agents.
///
/// Start with `nebula ctl describe --pretty` and `nebula ctl snapshot --pretty`.
/// A split returns the new focused pane id; pass that id to `prompt`, `run`, `wait`,
/// or `read` to build deterministic multi-pane workflows without GUI automation.
#[derive(Args, Debug)]
pub struct ControlOptions {
    /// Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines.
    #[clap(long, global = true)]
    pub pretty: bool,

    /// Maximum time to wait for a command response.
    #[clap(long, global = true, default_value_t = 30_000)]
    pub timeout_ms: u64,

    #[clap(subcommand)]
    pub command: ControlCommand,
}

/// Commands exposed by the versioned runtime control plane.
#[derive(Subcommand, Debug)]
pub enum ControlCommand {
    /// Describe the protocol version, runtime version, and available capabilities.
    Describe,
    /// Read the authoritative window, tab, pane, and task-state projection.
    Snapshot,
    /// Execute one typed multi-step terminal workflow in a single Runtime request.
    Orchestrate {
        /// Inline UTF-8 JSON object containing steps and on_error.
        #[clap(long, conflicts_with = "file", required_unless_present = "file")]
        spec: Option<String>,
        /// Read the UTF-8 workflow JSON object from a file.
        #[clap(long, value_hint = ValueHint::FilePath, conflicts_with = "spec")]
        file: Option<PathBuf>,
    },
    /// List only panes Nebula recognizes as AI agents, with semantic state and session identity.
    Agents {
        #[clap(long)]
        window: Option<u64>,
    },
    /// Start a verified AI CLI in a new terminal tab and assign a stable name.
    AgentStart {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        name: String,
        #[clap(long)]
        kind: String,
        #[clap(long)]
        cwd: Option<PathBuf>,
        #[clap(long)]
        resume_session_id: Option<String>,
    },
    /// Create an isolated Git worktree, then start a named AI CLI in that checkout.
    AgentFork {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        source_pane: Option<u64>,
        #[clap(long)]
        source_cwd: Option<PathBuf>,
        #[clap(long)]
        name: String,
        #[clap(long)]
        kind: String,
        #[clap(long)]
        resume_session_id: Option<String>,
        #[clap(long)]
        branch: Option<String>,
        #[clap(long)]
        base: Option<String>,
        #[clap(long)]
        path: Option<PathBuf>,
        #[clap(long)]
        allow_dirty_source: bool,
    },
    /// Resolve one managed agent by stable id or active name.
    AgentGet {
        #[clap(long)]
        agent: String,
        #[clap(long)]
        generation: Option<u64>,
    },
    /// Send one plain-text prompt to a managed agent.
    AgentPrompt {
        #[clap(long)]
        agent: String,
        #[clap(long)]
        generation: Option<u64>,
        #[clap(long)]
        text: String,
        #[clap(long)]
        no_submit: bool,
    },
    /// Paste one bounded UTF-8 block into a managed agent.
    AgentPaste {
        #[clap(long)]
        agent: String,
        #[clap(long)]
        generation: Option<u64>,
        #[clap(long)]
        text: String,
        #[clap(long)]
        no_submit: bool,
    },
    /// Read the terminal-buffer tail owned by a managed agent.
    AgentRead {
        #[clap(long)]
        agent: String,
        #[clap(long)]
        generation: Option<u64>,
        #[clap(long, default_value_t = crate::runtime_api::DEFAULT_READ_LINES)]
        lines: usize,
    },
    /// Wait for the same managed-agent generation to reach a semantic state.
    AgentWait {
        #[clap(long)]
        agent: String,
        #[clap(long)]
        generation: u64,
        #[clap(long, value_enum, default_value = "settled")]
        state: ControlWaitState,
        #[clap(long)]
        after_seq: Option<u64>,
    },
    /// Stream state snapshots whenever their semantic content changes.
    Subscribe {
        /// Resume after this revision; the current snapshot is sent when newer.
        #[clap(long)]
        since: Option<u64>,
    },
    /// Create and focus a new terminal window.
    NewWindow,
    /// Close an idle window. Omitting --window targets the uniquely resolved window.
    CloseWindow {
        #[clap(long)]
        window: Option<u64>,
    },
    /// Focus a window or one of its panes.
    Focus {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: Option<u64>,
    },
    /// Create a default-shell tab in the target window.
    NewTab {
        #[clap(long)]
        window: Option<u64>,
    },
    /// Close an idle tab by its zero-based index.
    CloseTab {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long = "tab")]
        tab_index: usize,
    },
    /// Set a tab's custom name. An empty value restores the generated title.
    RenameTab {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long = "tab")]
        tab_index: usize,
        #[clap(long)]
        name: String,
    },
    /// Move a tab to another index in the same window.
    MoveTab {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long = "tab")]
        tab_index: usize,
        #[clap(long = "to")]
        to_index: usize,
    },
    /// Split the focused pane in the target window.
    Split {
        #[clap(long)]
        window: Option<u64>,
        /// Split this pane instead of the currently focused pane.
        #[clap(long)]
        pane: Option<u64>,
        #[clap(long, value_enum, default_value = "right")]
        direction: ControlSplitDirection,
    },
    /// Close an idle pane.
    ClosePane {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
    },
    /// Explicitly enable or disable focused-pane zoom for the pane's tab.
    ZoomPane {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(
            long,
            required = true,
            action = ArgAction::Set,
            value_parser = clap::value_parser!(bool)
        )]
        zoomed: bool,
    },
    /// Set this pane's share of its direct parent split.
    ResizePane {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(long)]
        ratio: f32,
    },
    /// Send one plain-text prompt to a pane, optionally submitting it with Enter.
    Prompt {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(long)]
        text: String,
        /// Write the text without appending Enter.
        #[clap(long)]
        no_submit: bool,
        /// After sending, wait until the pane reaches this task state.
        #[clap(long, value_enum)]
        wait: Option<ControlWaitState>,
    },
    /// Paste one bounded UTF-8 block into a pane using bracketed-paste mode.
    Paste {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(long)]
        text: String,
        /// Paste the block without appending Enter.
        #[clap(long)]
        no_submit: bool,
        /// After submitting, wait until the pane reaches this task state.
        #[clap(long, value_enum)]
        wait: Option<ControlWaitState>,
    },
    /// Read the latest logical lines from a pane's real terminal buffer.
    Read {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        /// Number of logical terminal rows to read from the buffer tail.
        #[clap(long, default_value_t = crate::runtime_api::DEFAULT_READ_LINES)]
        lines: usize,
    },
    /// List the real local process tree rooted at a pane's PTY shell.
    Procs {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
    },
    /// Send a restricted named control key using the pane's active terminal mode.
    SendKey {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        /// Named key: escape, enter, arrows, navigation, f1-f12, or a-z with --control.
        #[clap(long)]
        key: String,
        #[clap(long)]
        shift: bool,
        #[clap(long)]
        alt: bool,
        #[clap(long)]
        control: bool,
        #[clap(long, default_value_t = 1)]
        repeat: u16,
    },
    /// Run one shell command and return its real OSC 133 exit status.
    Run {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(long)]
        command: String,
        /// Submit the command and return its run id without waiting for completion.
        #[clap(long)]
        no_wait: bool,
    },
    /// Execute argv as an independent non-TTY child and capture stdout/stderr separately.
    ExecPane {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        /// Maximum bytes retained from each stream. Both streams are always fully drained.
        #[clap(long, default_value_t = crate::runtime_api::DEFAULT_EXEC_OUTPUT_BYTES)]
        max_output_bytes: usize,
        /// Program and arguments. `--` is required so child flags remain untouched.
        #[clap(last = true, required = true, num_args = 1.., value_name = "ARGV")]
        argv: Vec<String>,
    },
    /// Wait until a pane reaches a semantic task state.
    Wait {
        #[clap(long)]
        window: Option<u64>,
        #[clap(long)]
        pane: u64,
        #[clap(long, value_enum, default_value = "settled")]
        state: ControlWaitState,
        /// Require the pane's state_change_seq to advance past this value.
        /// Pass the value observed before sending work so an already-settled
        /// pane does not satisfy the wait immediately.
        #[clap(long)]
        after_seq: Option<u64>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSplitDirection {
    /// Create the new pane to the right of the focused pane.
    Right,
    /// Create the new pane below the focused pane.
    Down,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlWaitState {
    Idle,
    Running,
    WaitingInput,
    Attention,
    Finished,
    Failed,
    /// Any non-running terminal state.
    Settled,
}

/// Send a message to the Nebula socket.
#[cfg(unix)]
#[derive(Args, Debug)]
pub struct MessageOptions {
    /// IPC socket connection path override.
    #[clap(short, long, value_hint = ValueHint::FilePath)]
    pub socket: Option<PathBuf>,

    /// Message which should be sent.
    #[clap(subcommand)]
    pub message: SocketMessage,
}

/// Available socket messages.
#[cfg(unix)]
#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum SocketMessage {
    /// Create a new window in the same Nebula process.
    CreateWindow(WindowOptions),

    /// Update the Nebula configuration.
    Config(IpcConfig),

    /// Read runtime Nebula configuration.
    GetConfig(IpcGetConfig),
}

/// Migrate the configuration file.
#[derive(Args, Clone, Debug)]
pub struct MigrateOptions {
    /// Path to the configuration file.
    #[clap(short, long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Only output TOML config to STDOUT.
    #[clap(short, long)]
    pub dry_run: bool,

    /// Do not recurse over imports.
    #[clap(short = 'i', long)]
    pub skip_imports: bool,

    /// Do not move renamed fields to their new location.
    #[clap(long)]
    pub skip_renames: bool,

    #[clap(short, long)]
    /// Do not output to STDOUT.
    pub silent: bool,
}

/// Subset of options that we pass to 'create-window' IPC subcommand.
#[derive(Serialize, Deserialize, Args, Default, Clone, Debug, PartialEq, Eq)]
pub struct WindowOptions {
    /// Terminal options which can be passed via IPC.
    #[clap(flatten)]
    pub terminal_options: TerminalOptions,

    #[clap(flatten)]
    /// Window options which could be passed via IPC.
    pub window_identity: WindowIdentity,

    #[clap(skip)]
    #[cfg(target_os = "macos")]
    /// The window tabbing identifier to use when building a window.
    pub window_tabbing_id: Option<String>,

    #[clap(skip)]
    #[cfg(not(any(target_os = "macos", windows)))]
    /// `ActivationToken` that we pass to winit.
    pub activation_token: Option<String>,

    /// Override configuration file options [example: 'cursor.style="Beam"'].
    #[clap(short = 'o', long, num_args = 1..)]
    option: Vec<String>,
}

impl WindowOptions {
    /// Get the parsed set of CLI config overrides.
    pub fn config_overrides(&self) -> ParsedOptions {
        ParsedOptions::from_options(&self.option)
    }
}

/// Parameters to the `config` IPC subcommand.
#[cfg(unix)]
#[derive(Args, Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct IpcConfig {
    /// Configuration file options [example: 'cursor.style="Beam"'].
    #[clap(required = true, value_name = "CONFIG_OPTIONS")]
    pub options: Vec<String>,

    /// Window ID for the new config.
    ///
    /// Use `-1` to apply this change to all windows.
    #[clap(short, long, allow_hyphen_values = true, env = "NEBULA_WINDOW_ID")]
    pub window_id: Option<i128>,

    /// Clear all runtime configuration changes.
    #[clap(short, long, conflicts_with = "options")]
    pub reset: bool,
}

/// Parameters to the `get-config` IPC subcommand.
#[cfg(unix)]
#[derive(Args, Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct IpcGetConfig {
    /// Window ID for the config request.
    ///
    /// Use `-1` to get the global config.
    #[clap(short, long, allow_hyphen_values = true, env = "NEBULA_WINDOW_ID")]
    pub window_id: Option<i128>,
}

/// Parsed CLI config overrides.
#[derive(Debug, Default)]
pub struct ParsedOptions {
    config_options: Vec<(String, Value)>,
}

impl ParsedOptions {
    /// Parse CLI config overrides.
    pub fn from_options(options: &[String]) -> Self {
        let mut config_options = Vec::new();

        for option in options {
            let parsed = match toml::from_str(option) {
                Ok(parsed) => parsed,
                Err(err) => {
                    eprintln!("Ignoring invalid CLI option '{option}': {err}");
                    continue;
                },
            };
            config_options.push((option.clone(), parsed));
        }

        Self { config_options }
    }

    /// Apply CLI config overrides, removing broken ones.
    pub fn override_config(&mut self, config: &mut UiConfig) {
        let mut i = 0;
        while i < self.config_options.len() {
            let (option, parsed) = &self.config_options[i];
            match config.replace(parsed.clone()) {
                Err(err) => {
                    error!(
                        target: LOG_TARGET_IPC_CONFIG,
                        "Unable to override option '{option}': {err}"
                    );
                    self.config_options.swap_remove(i);
                },
                Ok(_) => i += 1,
            }
        }
    }

    /// Apply CLI config overrides to a CoW config.
    pub fn override_config_rc(&mut self, config: Rc<UiConfig>) -> Rc<UiConfig> {
        // Skip clone without write requirement.
        if self.config_options.is_empty() {
            return config;
        }

        // Override cloned config.
        let mut config = (*config).clone();
        self.override_config(&mut config);

        Rc::new(config)
    }
}

impl Deref for ParsedOptions {
    type Target = Vec<(String, Value)>;

    fn deref(&self) -> &Self::Target {
        &self.config_options
    }
}

impl DerefMut for ParsedOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config_options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::File;
    use std::io::{Read, Write};

    use clap::CommandFactory;
    use clap_complete::Shell;
    use toml::Table;

    #[test]
    fn dynamic_title_ignoring_options_by_default() {
        let mut config = UiConfig::default();
        let old_dynamic_title = config.window.dynamic_title;

        Options::default().override_config(&mut config);

        assert_eq!(old_dynamic_title, config.window.dynamic_title);
    }

    #[test]
    fn dynamic_title_not_overridden_by_config() {
        let mut config = UiConfig::default();

        config.window.identity.title = "foo".to_owned();
        Options::default().override_config(&mut config);

        assert!(config.window.dynamic_title);
    }

    #[test]
    fn valid_option_as_value() {
        // Test with a single field.
        let value: Value = toml::from_str("field=true").unwrap();

        let mut table = Table::new();
        table.insert(String::from("field"), Value::Boolean(true));

        assert_eq!(value, Value::Table(table));

        // Test with nested fields
        let value: Value = toml::from_str("parent.field=true").unwrap();

        let mut parent_table = Table::new();
        parent_table.insert(String::from("field"), Value::Boolean(true));
        let mut table = Table::new();
        table.insert(String::from("parent"), Value::Table(parent_table));

        assert_eq!(value, Value::Table(table));
    }

    #[test]
    fn invalid_option_as_value() {
        let value = toml::from_str::<Value>("}");
        assert!(value.is_err());
    }

    #[test]
    fn parses_config_check_and_init_subcommands() {
        let check =
            Options::try_parse_from(["nebula", "config", "check", "--config-file", "sample.lua"])
                .unwrap();
        assert!(matches!(
            check.subcommands,
            Some(Subcommands::Config(ConfigOptions { command: ConfigCommand::Check(_) }))
        ));

        let init =
            Options::try_parse_from(["nebula", "config", "init", "--language", "zh-CN"]).unwrap();
        assert!(matches!(
            init.subcommands,
            Some(Subcommands::Config(ConfigOptions {
                command: ConfigCommand::Init(ConfigInitOptions {
                    language: ConfigLanguage::ZhCn,
                    ..
                })
            }))
        ));
    }

    #[test]
    fn send_options_after_text_are_not_swallowed_by_the_positional() {
        let agent = Options::try_parse_from([
            "nebula",
            "agent",
            "send",
            "codex",
            "fix",
            "login",
            "--no-submit",
            "--pretty",
        ])
        .unwrap();
        let Some(Subcommands::Agent(AgentOptions { command: AgentCommand::Send(agent) })) =
            agent.subcommands
        else {
            panic!("expected agent send");
        };
        assert_eq!(agent.text, ["fix", "login"]);
        assert!(agent.no_submit);
        assert!(agent.output.pretty);

        let pane = Options::try_parse_from([
            "nebula",
            "pane",
            "send",
            "17",
            "cargo",
            "test",
            "--no-submit",
            "--pretty",
        ])
        .unwrap();
        let Some(Subcommands::Pane(PaneOptions { command: PaneCommand::Send(pane) })) =
            pane.subcommands
        else {
            panic!("expected pane send");
        };
        assert_eq!(pane.text, ["cargo", "test"]);
        assert!(pane.no_submit);
        assert!(pane.output.pretty);
    }

    #[test]
    fn send_double_dash_preserves_hyphen_prefixed_text() {
        let parsed = Options::try_parse_from([
            "nebula",
            "agent",
            "send",
            "codex",
            "--no-submit",
            "--",
            "--fix",
            "--pretty",
        ])
        .unwrap();
        let Some(Subcommands::Agent(AgentOptions { command: AgentCommand::Send(options) })) =
            parsed.subcommands
        else {
            panic!("expected agent send");
        };
        assert_eq!(options.text, ["--fix", "--pretty"]);
        assert!(options.no_submit);
        assert!(!options.output.pretty);
    }

    #[test]
    fn paste_sources_are_explicit_and_mutually_exclusive() {
        let parsed = Options::try_parse_from([
            "nebula",
            "pane",
            "paste",
            "17",
            "first",
            "line",
            "--no-submit",
        ])
        .unwrap();
        let Some(Subcommands::Pane(PaneOptions { command: PaneCommand::Paste(options) })) =
            parsed.subcommands
        else {
            panic!("expected pane paste");
        };
        assert_eq!(options.source.text, ["first", "line"]);
        assert!(!options.source.stdin);
        assert_eq!(options.source.from_file, None);
        assert!(options.no_submit);

        let parsed =
            Options::try_parse_from(["nebula", "agent", "paste", "codex", "--stdin", "--wait"])
                .unwrap();
        let Some(Subcommands::Agent(AgentOptions { command: AgentCommand::Paste(options) })) =
            parsed.subcommands
        else {
            panic!("expected agent paste");
        };
        assert!(options.source.stdin);
        assert!(options.source.text.is_empty());
        assert!(options.wait);

        assert!(
            Options::try_parse_from([
                "nebula",
                "pane",
                "paste",
                "17",
                "literal",
                "--from-file",
                "task.txt",
            ])
            .is_err()
        );
        assert!(
            Options::try_parse_from([
                "nebula",
                "agent",
                "paste",
                "codex",
                "--stdin",
                "--from-file",
                "task.txt",
            ])
            .is_err()
        );
    }

    #[test]
    fn layout_resource_commands_keep_ids_and_explicit_zoom_state() {
        let parsed = Options::try_parse_from([
            "nebula", "tab", "move", "2", "0", "--window", "7", "--pretty",
        ])
        .unwrap();
        assert!(matches!(
            parsed.subcommands,
            Some(Subcommands::Tab(TabResourceOptions {
                command: TabCommand::Move(TabMoveOptions {
                    tab: 2,
                    to: 0,
                    window: 7,
                    output: ShortOutput { pretty: true },
                    ..
                })
            }))
        ));

        let parsed = Options::try_parse_from([
            "nebula", "pane", "zoom", "17", "--zoomed", "false", "--window", "7",
        ])
        .unwrap();
        assert!(matches!(
            parsed.subcommands,
            Some(Subcommands::Pane(PaneOptions {
                command: PaneCommand::Zoom(PaneZoomOptions {
                    pane: 17,
                    window: Some(7),
                    zoomed: false,
                    ..
                })
            }))
        ));

        // 省略状态不是“默认关闭”：调用方必须明确表达期望状态，才能安全重试。
        assert!(Options::try_parse_from(["nebula", "pane", "zoom", "17"]).is_err());
    }

    #[test]
    fn float_option_as_value() {
        let value: Value = toml::from_str("float=3.4").unwrap();

        let mut expected = Table::new();
        expected.insert(String::from("float"), Value::Float(3.4));

        assert_eq!(value, Value::Table(expected));
    }

    #[test]
    fn parse_instance_class() {
        let class = parse_class("one").unwrap();
        assert_eq!(class.general, "one");
        assert_eq!(class.instance, "one");
    }

    #[test]
    fn parse_general_class() {
        let class = parse_class("one,two").unwrap();
        assert_eq!(class.general, "one");
        assert_eq!(class.instance, "two");
    }

    #[test]
    fn parse_invalid_class() {
        let class = parse_class("one,two,three");
        assert!(class.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn repairs_drive_root_mangled_by_explorer_quoting() {
        // `--working-directory "D:\"` reaches us as `D:"` because the trailing
        // backslash escaped the closing quote (issue #36); restore the root.
        assert_eq!(repair_context_menu_dir(PathBuf::from("D:\"")), PathBuf::from("D:\\"));
        assert_eq!(
            repair_context_menu_dir(PathBuf::from("\\\\server\\share\"")),
            PathBuf::from("\\\\server\\share\\")
        );
        // A normal folder (no trailing backslash, so no mangling) is untouched.
        assert_eq!(
            repair_context_menu_dir(PathBuf::from("D:\\temp_build")),
            PathBuf::from("D:\\temp_build")
        );
    }

    #[test]
    fn valid_decimal() {
        let value = parse_hex_or_decimal("10485773");
        assert_eq!(value, Some(10485773));
    }

    #[test]
    fn valid_hex_to_decimal() {
        let value = parse_hex_or_decimal("0xa0000d");
        assert_eq!(value, Some(10485773));
    }

    #[test]
    fn invalid_hex_to_decimal() {
        let value = parse_hex_or_decimal("0xa0xx0d");
        assert_eq!(value, None);
    }

    #[test]
    fn completions() {
        let mut clap = Options::command();

        for (shell, file) in
            &[(Shell::Bash, "nebula.bash"), (Shell::Fish, "nebula.fish"), (Shell::Zsh, "_nebula")]
        {
            let mut generated = Vec::new();
            clap_complete::generate(*shell, &mut clap, "nebula", &mut generated);
            let generated = String::from_utf8_lossy(&generated);

            let mut completion = String::new();
            let mut file = File::open(format!("../extra/completions/{file}")).unwrap();
            file.read_to_string(&mut completion).unwrap();

            assert_eq!(generated, completion);
        }
    }

    #[test]
    #[ignore = "maintenance command: rewrites checked-in shell completions"]
    fn regenerate_completions() {
        let mut clap = Options::command();
        for (shell, file) in
            &[(Shell::Bash, "nebula.bash"), (Shell::Fish, "nebula.fish"), (Shell::Zsh, "_nebula")]
        {
            let mut generated = Vec::new();
            clap_complete::generate(*shell, &mut clap, "nebula", &mut generated);
            File::create(format!("../extra/completions/{file}"))
                .and_then(|mut file| file.write_all(&generated))
                .expect("write generated completion");
        }
    }
}
