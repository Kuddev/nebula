//! The display subsystem including window management, font rasterization, and
//! GPU drawing.

use std::cmp;
use std::collections::HashSet;
use std::fmt::{self, Formatter};
use std::mem::{self, ManuallyDrop};
use std::num::NonZeroU32;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glutin::config::GetGlConfig;
use glutin::context::{NotCurrentContext, PossiblyCurrentContext};
use glutin::display::GetGlDisplay;
use glutin::error::ErrorKind;
use glutin::prelude::*;
use glutin::surface::{Surface, SwapInterval, WindowSurface};

use log::{debug, info};
use parking_lot::MutexGuard;
use serde::{Deserialize, Serialize};
use winit::dpi::PhysicalSize;
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::RawWindowHandle;
use winit::window::CursorIcon;

use crossfont::{Rasterize, Rasterizer, Size as FontSize};
use unicode_width::UnicodeWidthChar;

use nebula_terminal::event::{EventListener, OnResize, WindowSize};
use nebula_terminal::grid::Dimensions as TermDimensions;
use nebula_terminal::index::{Column, Direction, Line, Point};
use nebula_terminal::selection::Selection;
#[cfg(windows)]
use nebula_terminal::term::cell::Cell;
use nebula_terminal::term::cell::Flags;
use nebula_terminal::term::{
    self, LineDamageBounds, MIN_COLUMNS, MIN_SCREEN_LINES, Term, TermDamage, TermMode,
};
use nebula_terminal::vte::ansi::{CursorShape, NamedColor};

use nebula_completions::file::complete_item;
use nebula_completions::{CompletionOptions, Span};

use crate::config::UiConfig;
use crate::config::debug::RendererPreference;
use crate::config::font::Font;
use crate::config::window::Dimensions;
#[cfg(not(windows))]
use crate::config::window::StartupMode;
use crate::display::bell::VisualBell;
use crate::display::color::{List, Rgb};
use crate::display::content::{RenderableContent, RenderableCursor};
use crate::display::cursor::IntoRects;
use crate::display::damage::{DamageTracker, damage_y_to_viewport_y};
use crate::display::hint::{HintMatch, HintState};
use crate::display::meter::Meter;
use crate::display::window::Window;
use crate::event::{Event, EventType, Mouse, SearchState};
use crate::message_bar::{MessageBuffer, MessageType};
use crate::renderer::rects::{RenderLine, RenderLines, RenderRect};
use crate::renderer::ui::{Gradient, Rgba, UiQuad};
use crate::renderer::{self, GlyphCache, Renderer, platform};
use crate::scheduler::{Scheduler, TimerId, Topic};
use crate::string::{ShortenDirection, StrShortener};

pub mod color;
pub mod content;
pub mod cursor;
pub mod hint;
pub mod window;

pub mod command_palette;
pub mod side_panel;
mod chrome;

pub use chrome::{ChromeHit, TabDropAction, in_chrome_bar, resize_edge};
pub(crate) use chrome::chrome_settings_button_rect;
use chrome::{
    ChromeTabLayout, TabDrag, chrome_hit_with_tabs, chrome_tab_layout, contains_rect,
    truncate_tab_label,
};

mod settings;
mod theme;

pub use theme::NebulaTheme;
pub(crate) use theme::write_nebula_prompt_theme;

/// Shared caret blink phase for the chrome text editors (rename / filter /
/// commit boxes): 500ms on / 500ms off wall-clock, same time source as the
/// sidebar spinner. The fast tick (armed while an editor is focused) keeps
/// frames coming so the phase is actually visible instead of looking frozen.
pub(crate) fn caret_blink_on() -> bool {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (millis / 500) % 2 == 0
}
pub use settings::{NebulaSettingsSection, SettingsHit, settings_hit};

mod bell;
mod damage;
mod meter;

/// Label for the forward terminal search bar.
const FORWARD_SEARCH_LABEL: &str = "Search: ";

/// Label for the backward terminal search bar.
const BACKWARD_SEARCH_LABEL: &str = "Backward Search: ";

/// The character used to shorten the visible text like uri preview or search regex.
const SHORTENER: char = '…';

/// Private-use placeholders emitted by Nebula's injected prompt. They are
/// replaced with spaces before text rendering; the real icons are vector UI
/// quads, so no Nerd Font or bundled font is required.
const NEBULA_FOLDER_ICON_MARKER: char = '\u{E100}';
const NEBULA_GIT_BRANCH_ICON_MARKER: char = '\u{E101}';

/// Color which is used to highlight damaged rects when debugging.
const DAMAGE_RECT_COLOR: Rgb = Rgb::new(255, 0, 255);

/// Cap on ghost-text length so a deeply-nested path can never spill across the
/// whole row and clobber the chrome.
const NEBULA_GHOST_MAX: usize = 96;

/// Prompt arrow injected by the Windows PowerShell profile (`U+276F`, `NebPromptArrow`).
/// The active input line is rendered as `❯ <input>`, so on Windows the real,
/// echoed input can be read straight off the grid instead of being guessed from
/// keystrokes — that is the only source that never desyncs from the shell's own
/// line editor (PSReadLine).
#[cfg(windows)]
const NEBULA_PROMPT_ARROW: char = '\u{276F}';

/// Visible split divider gap. The drag hit target is intentionally wider.
pub(crate) const NEBULA_SPLIT_DIVIDER_GAP: f32 = 2.0;
pub(crate) const NEBULA_SPLIT_HIT_SLOP: f32 = 8.0;

/// How far the unfocused split is dimmed. Focus is conveyed by brightness, not
/// a border: the inactive pane is pushed back under a translucent veil so the
/// focused pane visually "lifts" without any outline.
/// `unfocused-split-opacity = 0.7` (i.e. a 0.3 dim veil).
pub(crate) const NEBULA_UNFOCUSED_SPLIT_DIM: f32 = 0.30;

/// Max remembered commands for the history hint.

/// Top chrome reserve, in logical pixels at scale factor 1.0. The bottom bar
/// was removed; this now reserves only enough breathing room for the title
/// bar while the symmetric `SizeInfo` padding model is still in use.
pub const CHROME_BAR_LOGICAL: f32 = 56.0;

/// Shared chrome/control corner radius. Used for the small in-shell affordances
/// (window-control hover pills, tab pills, the "+" square) — kept modest so the
/// controls stay crisp.
pub(super) const UI_CORNER_RADIUS_LOGICAL: f32 = 8.0;

/// Outer radius of the connected chrome shell (the L-frame formed by the top
/// bar + left sidebar). Larger than the control radius so the whole window
/// chrome reads as one soft-cornered card while the affordances inside keep
/// their tighter [`UI_CORNER_RADIUS_LOGICAL`] curve.
pub(super) const UI_SHELL_RADIUS_LOGICAL: f32 = 14.0;

/// Shared quiet outline thickness.
pub(super) const UI_HAIRLINE_LOGICAL: f32 = 1.0;

/// Horizontal breathing space for terminal content, in logical pixels.
/// Kept modest so the grid stays wide — this is *added on top of* the user's
/// configured `window.padding`, on both sides, so large values noticeably
/// narrow the usable area.
pub const CONTENT_PAD_X_LOGICAL: f32 = 20.0;

/// Reserved chrome height per side, in physical pixels for `scale_factor`.
#[inline]
pub fn chrome_reserve(scale_factor: f32) -> f32 {
    (CHROME_BAR_LOGICAL * scale_factor).round()
}

/// Horizontal content padding, in physical pixels for `scale_factor`.
#[inline]
pub fn content_pad_x(scale_factor: f32) -> f32 {
    (CONTENT_PAD_X_LOGICAL * scale_factor).round()
}

/// Width of the left tab sidebar when expanded, in logical pixels. Chosen to
/// match the reference design — wide enough for a directory-ish label plus a
/// close affordance, narrow enough to leave the grid roomy.
pub const SIDEBAR_W_LOGICAL: f32 = 230.0;

/// Sidebar width in physical pixels for `scale_factor`, honouring the collapsed
/// state. Collapsed folds the panel away entirely (0) so the grid reclaims the
/// full width; the reveal affordance then lives in the top bar.
#[inline]
pub fn sidebar_width(scale_factor: f32, collapsed: bool) -> f32 {
    if collapsed {
        0.0
    } else {
        (SIDEBAR_W_LOGICAL * scale_factor).round()
    }
}

#[derive(Debug, Clone, Copy)]
struct UiAnim {
    value: f32,
    target: f32,
    last_step: Option<Instant>,
}

impl UiAnim {
    fn new(value: f32) -> Self {
        let value = value.clamp(0.0, 1.0);
        Self { value, target: value, last_step: None }
    }

    fn value(self) -> f32 {
        self.value
    }

    fn visible(self, target_open: bool) -> bool {
        target_open || self.value > 0.004
    }

    fn animating_to(self, target: f32) -> bool {
        (self.value - target.clamp(0.0, 1.0)).abs() > 0.004
    }

    fn step(&mut self, target: f32) {
        let target = target.clamp(0.0, 1.0);
        if (self.target - target).abs() > f32::EPSILON {
            self.target = target;
            self.last_step = None;
        }
        if !self.animating_to(target) {
            self.value = target;
            self.last_step = None;
            return;
        }

        let now = Instant::now();
        let dt = self.last_step.replace(now).map_or(1.0 / 60.0, |t| (now - t).as_secs_f32().min(0.1));
        self.value += (target - self.value) * (1.0 - (-16.0 * dt).exp());
        if (self.value - target).abs() < 0.004 {
            self.value = target;
            self.last_step = None;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NebulaUiAnims {
    left_sidebar: UiAnim,
    right_drawer: UiAnim,
}

impl NebulaUiAnims {
    fn new() -> Self {
        Self { left_sidebar: UiAnim::new(1.0), right_drawer: UiAnim::new(0.0) }
    }

    fn step(&mut self, left_open: bool, right_open: bool) {
        self.left_sidebar.step(if left_open { 1.0 } else { 0.0 });
        self.right_drawer.step(if right_open { 1.0 } else { 0.0 });
    }

    fn animating(&self, left_open: bool, right_open: bool) -> bool {
        self.left_sidebar.animating_to(if left_open { 1.0 } else { 0.0 })
            || self.right_drawer.animating_to(if right_open { 1.0 } else { 0.0 })
    }
}

#[derive(Debug, Clone, Copy)]
enum NebulaPowerlineIconKind {
    Folder,
    GitBranch,
}

#[derive(Debug, Clone, Copy)]
struct NebulaPowerlineIcon {
    kind: NebulaPowerlineIconKind,
    point: Point<usize>,
}

/// Open the native "open file" dialog so the user can pick a background image.
///
/// Returns the chosen path, or `None` if the dialog was cancelled. `owner` is
/// the host window handle so the dialog is modal to Nebula; a non-Win32 handle
/// falls back to an unowned dialog. `GetOpenFileNameW` runs its own modal
/// message pump, so this blocks the caller until the user picks or cancels.
#[cfg(windows)]
fn nebula_pick_image_file(owner: RawWindowHandle) -> Option<String> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let hwnd: HWND = match owner {
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    };

    // Filter is a flat list of NUL-terminated "label\0pattern\0" pairs ending in
    // an extra NUL. Keep the patterns in sync with the image loader.
    let filter: Vec<u16> = "Images (*.png;*.jpg;*.jpeg;*.webp;*.bmp)\0\
        *.png;*.jpg;*.jpeg;*.webp;*.bmp\0\
        All files (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "Choose background image\0".encode_utf16().collect();

    // The selected path is written back into this buffer in place.
    let mut file_buf = vec![0u16; 1024];

    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = hwnd;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file_buf.as_mut_ptr();
    ofn.nMaxFile = file_buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    // NOCHANGEDIR: the dialog must not move our process's working directory.
    ofn.Flags = OFN_EXPLORER
        | OFN_FILEMUSTEXIST
        | OFN_PATHMUSTEXIST
        | OFN_NOCHANGEDIR
        | OFN_HIDEREADONLY;

    let picked = unsafe { GetOpenFileNameW(&mut ofn) };
    if picked == 0 {
        return None;
    }

    let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&file_buf[..len]))
}

/// Which key accepts an inline ghost suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcceptKey {
    Right,
    Tab,
    #[default]
    Both,
}

impl AcceptKey {
    fn label(self) -> &'static str {
        match self {
            Self::Right => "\u{2192}",
            Self::Tab => "Tab",
            Self::Both => "\u{2192} / Tab",
        }
    }
    fn cycle(self) -> Self {
        match self {
            Self::Right => Self::Tab,
            Self::Tab => Self::Both,
            Self::Both => Self::Right,
        }
    }
    pub fn accepts_right(self) -> bool {
        matches!(self, Self::Right | Self::Both)
    }
    pub fn accepts_tab(self) -> bool {
        matches!(self, Self::Tab | Self::Both)
    }
}

/// Runtime-selected default executor for new Nebula terminal sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NebulaShell {
    #[default]
    PowerShell,
    Bash,
}

impl NebulaShell {
    fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::Bash => "Bash",
        }
    }

    fn settings_value(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Bash => "bash",
        }
    }

    fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "powershell" | "pwsh" | "ps" => Some(Self::PowerShell),
            "bash" | "git-bash" | "gitbash" | "wsl" => Some(Self::Bash),
            _ => None,
        }
    }

    fn cycle(self) -> Self {
        match self {
            Self::PowerShell => Self::Bash,
            Self::Bash => Self::PowerShell,
        }
    }
}

/// A destructive action awaiting user confirmation (Enter 确认 / Esc 取消).
/// Drawn as a centered modal; the keyboard is owned by the modal while open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NebulaConfirm {
    /// Close one pane whose shell still has a busy child process.
    ClosePane { pane_id: u64, process: String },
    /// Close a whole tab (by displayed index) with a busy process inside.
    CloseTab { index: usize, process: String },
    /// Close the window while some pane still runs `process`.
    CloseWindow { process: String },
    /// Paste text that contains newlines (would execute in most shells).
    Paste { text: String, bracketed: bool, lines: usize },
}

/// One OSC 1337 inline image, anchored to an absolute grid row (see
/// `Grid::scrolled_out`). Pixels are shared via `Arc` so pane-state clones
/// stay cheap; the GPU texture is cached per `id` inside the renderer.
#[derive(Debug, Clone)]
pub struct NebulaInlineImage {
    /// Unique id keying the renderer's texture cache.
    pub id: u64,
    /// Top row in absolute grid line numbering.
    pub abs_line: usize,
    /// Display size in window pixels (already scaled to the terminal width).
    pub width: f32,
    pub height: f32,
    /// Decoded RGBA8 pixels.
    pub rgba: std::sync::Arc<Vec<u8>>,
    /// Pixel dimensions of `rgba`.
    pub px_w: u32,
    pub px_h: u32,
}

/// Per-pane Nebula prompt metadata and inline suggestion state.
/// 这些状态必须跟随具体 PTY/pane，而不能挂在 `Display` 全局上；否则分屏后右侧
/// shell 上报的 cwd、用户正在输入的行、ghost suggestion 会污染左侧 pane。
#[derive(Debug, Default, Clone)]
pub struct NebulaPaneState {
    /// Current working directory captured from `NEBULA|cwd|branch`.
    pub cwd: String,
    /// Current git branch captured from `NEBULA|cwd|branch`.
    pub branch: String,
    /// Inline ghost-text autosuggestion remainder.
    pub suggestion: String,
    /// Cache key (`cwd\0token`) for the current suggestion.
    suggestion_key: String,
    /// User-typed prompt line tracked from key events.
    pub line_buf: String,
    /// Last prompt line read from the raw terminal grid. This mirrors Nu's
    /// editor buffer more closely after shell-side edits such as Tab completion
    /// or history recall, where key-event reconstruction has been invalidated.
    pub(crate) screen_line: String,
    /// Whether the user has typed anything into this pane. A pristine pane is
    /// still showing only the welcome intro, so it may be safely re-printed
    /// (e.g. after a split resize reflows the two-column layout).
    pub touched: bool,
    /// OSC 1337 inline images anchored to absolute grid rows, newest last.
    pub inline_images: Vec<NebulaInlineImage>,
    /// When the running command started (OSC 133;C), for the finished-command
    /// notification (133;D). `None` when no command is being tracked.
    pub command_started: Option<std::time::Instant>,
    /// Program identity of the running command ("claude", "cargo", …), taken
    /// from the line committed at Enter. Drives the sidebar tab icon.
    pub running_program: Option<String>,
    /// The command line most recently committed with Enter. Captured because
    /// OSC 133;C arrives from the PTY *after* the prompt buffers were cleared.
    pub last_committed: String,
    /// The running program rang BEL and is now waiting for input (AI CLIs
    /// ring when a turn completes): the spinner pauses until the user types.
    pub awaiting_input: bool,
    /// A tracked command finished while its tab was in the background; shows
    /// the sidebar dot until the tab is selected.
    pub finished_unseen: bool,
}

/// First word of a committed command line, normalized to a program identity
/// for the sidebar icon: lowercased, path prefix and Windows launcher
/// extensions stripped. `D:\tools\Claude.EXE --resume` → `claude`.
pub(crate) fn extract_program(line: &str) -> Option<String> {
    let first = line.trim().split_whitespace().next()?;
    let base = first.trim_matches('"').rsplit(['/', '\\']).next().unwrap_or(first);
    let mut name = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1", ".com"] {
        if let Some(stripped) = name.strip_suffix(ext) {
            name = stripped.to_owned();
            break;
        }
    }
    (!name.is_empty()).then_some(name)
}

/// Sidebar icon for a running program — Nerd Font glyphs (the chrome text
/// layer renders with Maple NF). AI CLIs get distinct marks; everything else
/// shares a generic "running" play sign.
/// AI clients whose REAL brand mark is drawn in the sidebar as a textured
/// quad (embedded PNG), instead of a lookalike Nerd Font glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiLogo {
    /// Anthropic's coral starburst.
    Claude,
    /// OpenAI's blossom knot (codex).
    OpenAi,
    /// opencode's terminal-frame mark (sst). Two-tone: bright frame + dimmer
    /// inner screen block, encoded as luma and multiplied by the theme ink.
    OpenCode,
}

/// Official logo assets (Wikimedia SVG renders, 64 px, alpha-transparent).
const AI_LOGO_CLAUDE_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_claude.png");
const AI_LOGO_OPENAI_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_openai.png");
/// opencode's mark, rasterized from their `favicon.svg`. RGB carries luma
/// (frame=255, block=90), alpha the shape; tinted `ink × luma/255` at runtime.
const AI_LOGO_OPENCODE_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_opencode.png");

/// Texture ids for chrome logos live far above the inline-image counter
/// (which starts at 1), so the two id spaces can share the renderer cache.
const AI_LOGO_ID_BASE: u64 = 1 << 62;

/// Real-logo mapping for AI clients; everything else falls back to the
/// [`program_icon`] glyph. Gated on PNG support: without it there is no
/// texture to draw and the glyph must stay.
pub(crate) fn ai_logo(program: &str) -> Option<AiLogo> {
    if cfg!(any(not(feature = "png"), target_os = "macos")) {
        return None;
    }
    match program {
        "claude" => Some(AiLogo::Claude),
        "codex" => Some(AiLogo::OpenAi),
        "opencode" => Some(AiLogo::OpenCode),
        _ => None,
    }
}

/// Drop a `file://` / `file:///` scheme so a local link reads as a plain
/// path in the hover tooltip. On Windows `file:///D:/x` → `D:/x` (the slash
/// before the drive letter goes too); non-`file:` URIs pass through.
fn strip_file_scheme(uri: &str) -> String {
    let rest = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    // `file:///D:/x` yields `D:/x`; a UNC-ish `file://host/x` keeps `host/x`.
    rest.to_owned()
}

/// Truncate `s` to at most `budget` display columns (CJK counts as 2), keeping
/// the TAIL and prefixing `…` when cut — the filename end is what a hover
/// tooltip most needs. Returns a string whose display width is `<= budget`.
fn fit_tail(s: &str, budget: usize) -> String {
    let width = |c: char| c.width().unwrap_or(0);
    let total: usize = s.chars().map(width).sum();
    if total <= budget {
        return s.to_owned();
    }
    if budget == 0 {
        return String::new();
    }
    // Reserve one column for the ellipsis, then take chars from the end until
    // the reserved room fills up.
    let room = budget.saturating_sub(1);
    let mut kept = std::collections::VecDeque::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = width(c);
        if used + w > room {
            break;
        }
        used += w;
        kept.push_front(c);
    }
    let mut out = String::with_capacity(kept.len() + 1);
    out.push('…');
    out.extend(kept);
    out
}

pub(crate) fn program_icon(program: &str) -> &'static str {
    match program {
        "claude" => "\u{f0ce5}",                      // md-star-four-points (Claude spark)
        "codex" => "\u{f02d8}",                       // md-hexagon (OpenAI mark)
        "gemini" => "\u{f0ce6}",                      // md-star-four-points-outline
        "copilot" => "\u{f4b8}",                      // oct-copilot
        "cursor" | "cursor-agent" => "\u{f0ec3}",     // md-cursor-default-outline
        "aider" | "goose" | "crush" | "ollama" => "\u{f06a9}", // md-robot
        "opencode" => "\u{f489}",                     // oct-terminal
        "git" | "gh" | "lazygit" => "\u{f418}",       // oct-git-branch
        "vim" | "nvim" | "vi" | "hx" | "nano" => "\u{e62b}", // custom-vim
        "ssh" | "mosh" => "\u{f489}",                 // oct-terminal (remote)
        "cargo" | "rustc" => "\u{e7a8}",              // dev-rust
        "node" | "npm" | "pnpm" | "yarn" | "bun" | "deno" => "\u{e718}", // dev-nodejs
        "python" | "python3" | "pip" | "uv" => "\u{e73c}", // dev-python
        "docker" | "podman" => "\u{e7b0}",            // dev-docker
        _ => "\u{f04b}",                              // fa-play (generic busy)
    }
}

/// Orientation of a split between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Panes side by side, divided by a vertical line.
    LeftRight,
    /// Panes stacked, divided by a horizontal line.
    TopBottom,
}

/// Cardinal direction for moving keyboard focus between split panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitNav {
    Left,
    Right,
    Up,
    Down,
}

fn nebula_debug_log(message: impl AsRef<str>) {
    use std::io::Write as _;

    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        std::env::var("NEBULA_DEBUG_LOG").is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    }) {
        return;
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    let path = nebula_data_dir().join("nebula_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{ts}] {}", message.as_ref());
    }
}

/// Unconditional variant of [`nebula_debug_log`] for the link-click diagnosis:
/// clicks are rare (no perf concern), and requiring a relaunch with
/// NEBULA_DEBUG_LOG=1 would double every remote-debug round-trip. Remove or
/// downgrade to the gated logger once the link path is verified.
pub(crate) fn nebula_link_log(message: impl AsRef<str>) {
    use std::io::Write as _;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    let path = nebula_data_dir().join("nebula_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{ts}] {}", message.as_ref());
    }
}

/// Directory holding Nebula's persistent state (`%APPDATA%\Nebula` or
/// `~/.config/Nebula`), created on demand. Settings live here next to the
/// history file managed by [`crate::nebula_history`] and the session
/// snapshot managed by [`crate::session`].
pub(crate) fn nebula_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Nebula");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(windows)]
fn nebula_pathexts() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.PS1".to_owned())
        .split(';')
        .filter_map(|ext| {
            let ext = ext.trim();
            if ext.is_empty() {
                None
            } else if ext.starts_with('.') {
                Some(ext.to_ascii_lowercase())
            } else {
                Some(format!(".{}", ext.to_ascii_lowercase()))
            }
        })
        .collect()
}

#[cfg(windows)]
fn nebula_command_name(path: &Path, pathexts: &[String]) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let ext = format!(".{}", ext).to_ascii_lowercase();
    if !pathexts.iter().any(|known| known == &ext) {
        return None;
    }
    path.file_stem()?.to_str().filter(|name| !name.is_empty()).map(ToOwned::to_owned)
}

#[cfg(not(windows))]
fn nebula_command_name(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata().ok()?.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    path.file_name()?.to_str().filter(|name| !name.is_empty()).map(ToOwned::to_owned)
}

fn nebula_path_commands() -> Vec<String> {
    let Some(path_env) = std::env::var_os("PATH") else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    let mut seen = HashSet::new();
    #[cfg(windows)]
    let pathexts = nebula_pathexts();

    for dir in std::env::split_paths(&path_env).filter(|dir| !dir.as_os_str().is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }

            #[cfg(windows)]
            let command = nebula_command_name(&entry.path(), &pathexts);
            #[cfg(not(windows))]
            let command = nebula_command_name(&entry.path());

            if let Some(command) = command {
                #[cfg(windows)]
                let key = command.to_ascii_lowercase();
                #[cfg(not(windows))]
                let key = command.clone();

                // PATH 里同名 shim/真实可执行文件经常重复；这里只保留第一个，
                // 避免每次输入首 token 时 ghost 在等价候选间跳动。
                if seen.insert(key) {
                    commands.push(command);
                }
            }
        }
    }

    commands.sort_by(|a, b| {
        #[cfg(windows)]
        {
            a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then(a.cmp(b))
        }
        #[cfg(not(windows))]
        {
            a.cmp(b)
        }
    });
    commands
}

/// Collect external-shell command names: PATH executables plus, on Windows, the
/// running shell's cmdlets / functions / aliases (which never appear on PATH).
/// Merged and de-duplicated (case-insensitively on Windows).
fn nebula_collect_commands() -> Vec<String> {
    let mut commands = nebula_path_commands();

    #[cfg(windows)]
    {
        let mut seen: HashSet<String> =
            commands.iter().map(|c| c.to_ascii_lowercase()).collect();
        for command in nebula_powershell_commands() {
            if seen.insert(command.to_ascii_lowercase()) {
                commands.push(command);
            }
        }
        commands.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then(a.cmp(b)));
    }

    commands
}

/// PowerShell cmdlets / functions / aliases via a one-shot `Get-Command`, run
/// with `-NoProfile` to avoid the user's profile cost. Best-effort: any failure
/// (no PowerShell, error, parse problem) yields an empty list.
#[cfg(windows)]
fn nebula_powershell_commands() -> Vec<String> {
    // CREATE_NO_WINDOW: Nebula is a GUI-subsystem process, so a console child
    // would otherwise pop up (and instantly vanish) a visible PowerShell
    // window at startup.
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Command -CommandType Cmdlet,Function,Alias -ErrorAction SilentlyContinue \
             | Select-Object -ExpandProperty Name",
        ])
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Process-wide handle to the command list, populated once on a background
/// thread so the (PowerShell-invoking) collection never blocks window startup.
/// Readers see an empty list until collection finishes, then the merged set.
fn nebula_commands_handle() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    static COMMANDS: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<String>>>> =
        std::sync::OnceLock::new();
    COMMANDS
        .get_or_init(|| {
            let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let bg = shared.clone();
            std::thread::spawn(move || {
                let commands = nebula_collect_commands();
                if let Ok(mut guard) = bg.lock() {
                    *guard = commands;
                }
            });
            shared
        })
        .clone()
}

fn nebula_command_hint<'a>(commands: &'a [String], prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return None;
    }

    commands.iter().find_map(|command| {
        if command.len() <= prefix.len() || !command.is_char_boundary(prefix.len()) {
            return None;
        }
        let (head, rem) = command.split_at(prefix.len());
        #[cfg(windows)]
        let matches = head.eq_ignore_ascii_case(prefix);
        #[cfg(not(windows))]
        let matches = head == prefix;

        matches.then_some(rem)
    })
}

fn nebula_is_command_position(line: &str) -> bool {
    !line.contains([' ', '\t'])
        && !line.contains(['/', '\\'])
        && line.as_bytes().get(1) != Some(&b':')
}

fn nebula_path_wants_directory(line: &str) -> bool {
    let command = line.split([' ', '\t']).next().unwrap_or("");
    matches!(
        command.to_ascii_lowercase().as_str(),
        "cd" | "chdir" | "pushd" | "sl" | "set-location"
    )
}

#[derive(Debug)]
pub enum Error {
    /// Error with window management.
    Window(window::Error),

    /// Error dealing with fonts.
    Font(crossfont::Error),

    /// Error in renderer.
    Render(renderer::Error),

    /// Error during context operations.
    Context(glutin::error::Error),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Window(err) => err.source(),
            Error::Font(err) => err.source(),
            Error::Render(err) => err.source(),
            Error::Context(err) => err.source(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Window(err) => err.fmt(f),
            Error::Font(err) => err.fmt(f),
            Error::Render(err) => err.fmt(f),
            Error::Context(err) => err.fmt(f),
        }
    }
}

impl From<window::Error> for Error {
    fn from(val: window::Error) -> Self {
        Error::Window(val)
    }
}

impl From<crossfont::Error> for Error {
    fn from(val: crossfont::Error) -> Self {
        Error::Font(val)
    }
}

impl From<renderer::Error> for Error {
    fn from(val: renderer::Error) -> Self {
        Error::Render(val)
    }
}

impl From<glutin::error::Error> for Error {
    fn from(val: glutin::error::Error) -> Self {
        Error::Context(val)
    }
}

/// Terminal size info.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct SizeInfo<T = f32> {
    /// Terminal window width.
    width: T,

    /// Terminal window height.
    height: T,

    /// Width of individual cell.
    cell_width: T,

    /// Height of individual cell.
    cell_height: T,

    /// Horizontal window padding on the left. Doubles as the grid's left
    /// origin everywhere (cursor/IME/mouse/damage), so the Nebula sidebar makes
    /// itself room simply by inflating this.
    padding_x: T,

    /// Horizontal window padding on the right. Equal to `padding_x` in the
    /// classic symmetric layout; smaller when a left sidebar is present so the
    /// grid only pays the sidebar cost on one side.
    padding_right: T,

    /// Vertical window padding.
    padding_y: T,

    /// Number of lines in the viewport.
    screen_lines: usize,

    /// Number of columns in the viewport.
    columns: usize,
}

impl From<SizeInfo<f32>> for SizeInfo<u32> {
    fn from(size_info: SizeInfo<f32>) -> Self {
        Self {
            width: size_info.width as u32,
            height: size_info.height as u32,
            cell_width: size_info.cell_width as u32,
            cell_height: size_info.cell_height as u32,
            padding_x: size_info.padding_x as u32,
            padding_right: size_info.padding_right as u32,
            padding_y: size_info.padding_y as u32,
            screen_lines: size_info.screen_lines,
            columns: size_info.columns,
        }
    }
}

impl From<SizeInfo<f32>> for WindowSize {
    fn from(size_info: SizeInfo<f32>) -> Self {
        Self {
            num_cols: size_info.columns() as u16,
            num_lines: size_info.screen_lines() as u16,
            cell_width: size_info.cell_width() as u16,
            cell_height: size_info.cell_height() as u16,
        }
    }
}

impl<T: Clone + Copy> SizeInfo<T> {
    #[inline]
    pub fn width(&self) -> T {
        self.width
    }

    #[inline]
    pub fn height(&self) -> T {
        self.height
    }

    #[inline]
    pub fn cell_width(&self) -> T {
        self.cell_width
    }

    #[inline]
    pub fn cell_height(&self) -> T {
        self.cell_height
    }

    #[inline]
    pub fn padding_x(&self) -> T {
        self.padding_x
    }

    #[inline]
    pub fn padding_right(&self) -> T {
        self.padding_right
    }

    #[inline]
    pub fn padding_y(&self) -> T {
        self.padding_y
    }
}

impl SizeInfo<f32> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        mut padding_x: f32,
        mut padding_y: f32,
        dynamic_padding: bool,
    ) -> SizeInfo {
        // Symmetric entry point: right padding mirrors the left. Callers that
        // want an asymmetric layout (the sidebar) use `new_asymmetric`.
        let padding_right = padding_x;
        if dynamic_padding {
            padding_x = Self::dynamic_padding(padding_x.floor(), width, cell_width);
            padding_y = Self::dynamic_padding(padding_y.floor(), height, cell_height);
        }

        Self::assemble(width, height, cell_width, cell_height, padding_x, padding_right, padding_y)
    }

    /// Build a grid whose left and right paddings differ. The left padding is
    /// the grid origin (and absorbs the sidebar width); the right padding stays
    /// the ordinary content margin. Dynamic padding is intentionally skipped —
    /// an asymmetric layout is driven by explicit chrome geometry, not centring.
    #[allow(clippy::too_many_arguments)]
    pub fn new_asymmetric(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        padding_x: f32,
        padding_right: f32,
        padding_y: f32,
    ) -> SizeInfo {
        Self::assemble(width, height, cell_width, cell_height, padding_x, padding_right, padding_y)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        padding_x: f32,
        padding_right: f32,
        padding_y: f32,
    ) -> SizeInfo {
        let lines = (height - 2. * padding_y) / cell_height;
        let screen_lines = cmp::max(lines as usize, MIN_SCREEN_LINES);

        let columns = (width - padding_x - padding_right) / cell_width;
        let columns = cmp::max(columns as usize, MIN_COLUMNS);

        SizeInfo {
            width,
            height,
            cell_width,
            cell_height,
            padding_x: padding_x.floor(),
            padding_right: padding_right.floor(),
            padding_y: padding_y.floor(),
            screen_lines,
            columns,
        }
    }

    #[inline]
    pub fn reserve_lines(&mut self, count: usize) {
        self.screen_lines = cmp::max(self.screen_lines.saturating_sub(count), MIN_SCREEN_LINES);
    }

    /// Check if coordinates are inside the terminal grid.
    ///
    /// The padding, message bar or search are not counted as part of the grid.
    #[inline]
    pub fn contains_point(&self, x: usize, y: usize) -> bool {
        x <= (self.padding_x + self.columns as f32 * self.cell_width) as usize
            && x > self.padding_x as usize
            // right edge derives from columns, so asymmetric padding is implicit

            && y <= (self.padding_y + self.screen_lines as f32 * self.cell_height) as usize
            && y > self.padding_y as usize
    }

    /// Calculate padding to spread it evenly around the terminal content.
    #[inline]
    fn dynamic_padding(padding: f32, dimension: f32, cell_dimension: f32) -> f32 {
        padding + ((dimension - 2. * padding) % cell_dimension) / 2.
    }
}

impl TermDimensions for SizeInfo {
    #[inline]
    fn columns(&self) -> usize {
        self.columns
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
}

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct DisplayUpdate {
    pub dirty: bool,

    dimensions: Option<PhysicalSize<u32>>,
    cursor_dirty: bool,
    font: Option<Font>,
}

impl DisplayUpdate {
    pub fn dimensions(&self) -> Option<PhysicalSize<u32>> {
        self.dimensions
    }

    pub fn font(&self) -> Option<&Font> {
        self.font.as_ref()
    }

    pub fn cursor_dirty(&self) -> bool {
        self.cursor_dirty
    }

    pub fn set_dimensions(&mut self, dimensions: PhysicalSize<u32>) {
        self.dimensions = Some(dimensions);
        self.dirty = true;
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = Some(font);
        self.dirty = true;
    }

    pub fn set_cursor_dirty(&mut self) {
        self.cursor_dirty = true;
        self.dirty = true;
    }
}

/// The display wraps a window, font rasterizer, and GPU renderer.
pub struct Display {
    pub window: Window,

    pub size_info: SizeInfo,

    /// Hint highlighted by the mouse.
    pub highlighted_hint: Option<HintMatch>,
    /// Frames since hint highlight was created.
    highlighted_hint_age: usize,

    /// Hint highlighted by the vi mode cursor.
    pub vi_highlighted_hint: Option<HintMatch>,
    /// Frames since hint highlight was created.
    vi_highlighted_hint_age: usize,

    pub raw_window_handle: RawWindowHandle,

    /// UI cursor visibility for blinking.
    pub cursor_hidden: bool,

    /// When a split is active, the focused pane's geometry. Input and hint
    /// hit-testing use this (via `pane_view()`) so mouse coordinates map into
    /// the focused half-width grid rather than the full window, which would
    /// otherwise index past the grid and panic.
    pub nebula_pane_view: Option<SizeInfo>,

    /// Transient "cols × rows" HUD shown briefly after a window resize; it fades
    /// out over ~0.9s. `None` when nothing is showing.
    nebula_resize_hud: Option<(usize, usize, Instant)>,
    /// Skip the first resize (window creation) so no HUD flashes at startup.
    nebula_resize_hud_armed: bool,

    /// Indexed, persistent command history used to hint a whole previous
    /// command from its prefix.
    nebula_history: crate::nebula_history::NebulaHistory,
    /// Executable commands for first-token completion: PATH executables plus, on
    /// Windows, the shell's cmdlets/functions/aliases. Filled on a background
    /// thread so the PowerShell probe never blocks startup.
    nebula_commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Per-displayed-tab animated draw-x, eased toward the laid-out / drag
    /// target each frame so tab reorder "make way" slides instead of snapping.
    nebula_tab_anim: Vec<f32>,
    /// Active scrollbar drag: the pointer's y-offset inside the thumb captured
    /// at press time, so the thumb tracks the pointer without jumping.
    pub nebula_scrollbar_drag: Option<f32>,
    /// Slide-in reveal for a freshly created split pane: its final rect, the
    /// split direction and the animation start time. Drawn as a shrinking
    /// bg-coloured cover in `draw_split_overlays`; cleared when done.
    pub nebula_split_reveal:
        Option<(f32, f32, f32, f32, SplitDirection, std::time::Instant)>,
    /// Pending destructive-action confirmation (close with busy children /
    /// multi-line paste), drawn as a centered modal that owns the keyboard.
    pub nebula_confirm: Option<NebulaConfirm>,
    /// Screen rects of the confirm modal's (primary, cancel) buttons, written
    /// by `draw_confirm_modal` each frame so the mouse hit-test can never
    /// drift from what was actually drawn. `None` while no modal shows.
    pub nebula_confirm_buttons: Option<((f32, f32, f32, f32), (f32, f32, f32, f32))>,
    /// Inline images visible this frame, collected per pane during
    /// `draw_pane` (grid lock + pane viewport at hand) and drawn in one
    /// full-window pass in `present_frame` — mid-pane GL viewport swaps are
    /// fragile, one batched pass is not.
    nebula_frame_images: Vec<(u64, std::sync::Arc<Vec<u8>>, (u32, u32), (f32, f32, f32, f32))>,

    /// Runtime-only chrome theme selected from the top-left settings panel.
    pub nebula_theme: NebulaTheme,
    pub nebula_settings_open: bool,
    /// Settings content scroll offset in scaled px (0 = top of the section).
    nebula_settings_scroll: f32,
    /// Command palette (Ctrl+Shift+P): fuzzy launcher model + UI state.
    nebula_palette: command_palette::CommandPalette,
    /// Right-side drawer: directory tree / git status of the focused cwd.
    pub nebula_side_panel: side_panel::SidePanel,
    /// Shared chrome animation state. All sidebar/drawer transitions step here
    /// so easing/timing does not get scattered across render code.
    nebula_ui_anims: NebulaUiAnims,
    /// Active sidebar section inside the settings panel.
    nebula_settings_section: NebulaSettingsSection,
    nebula_chrome_hover: ChromeHit,
    nebula_settings_hover: SettingsHit,
    nebula_tab_labels: Vec<String>,
    nebula_tab_bells: Vec<bool>,
    /// Per-tab "command is running" flags driving the sidebar spinners.
    nebula_tab_running: Vec<bool>,
    /// Per-tab real AI brand logo (claude/codex), textured over the icon slot.
    nebula_tab_logos: Vec<Option<AiLogo>>,
    /// Decoded (+theme-tinted) logo pixels with stable renderer texture ids,
    /// keyed by (logo, ink). Decode and tint run once per key.
    nebula_ai_logo_cache:
        std::collections::HashMap<(AiLogo, [u8; 3]), (u64, std::sync::Arc<Vec<u8>>, (u32, u32))>,
    /// Brand logos staged by the chrome pass, drawn AFTER all chrome text.
    /// draw_inline_image flips viewport/blend around its draw; interleaving
    /// it with chrome text kills every glyph batch after it, so the textured
    /// icons get their own pass at the very end of the frame.
    nebula_chrome_logo_draws: Vec<(u64, std::sync::Arc<Vec<u8>>, (u32, u32), (f32, f32, f32, f32))>,
    nebula_active_tab: usize,
    /// In-progress tab reorder drag, if the pointer is grabbing a tab.
    nebula_tab_drag: Option<TabDrag>,
    /// Whether the tab bar may be reordered right now (false during a split,
    /// where the bar hides a pane and reordering is ambiguous).
    nebula_tabs_reorderable: bool,
    /// Whether the tab sidebar is folded away. When collapsed the grid
    /// reclaims the full width and only a reveal button remains in the top bar.
    nebula_sidebar_collapsed: bool,
    /// A grid resize happened whose PTY notification is deferred until the
    /// interactive resize settles (see `Topic::NebulaResizeSettle`): the
    /// in-box ConPTY repaints the whole viewport per resize, so notifying it
    /// on every drag tick floods the scrollback with shredded repaints.
    pub nebula_pty_resize_pending: bool,
    /// Whether inline ghost-text suggestions are shown at all.
    pub nebula_ghost_enabled: bool,
    /// Which key accepts a ghost suggestion.
    pub nebula_accept: AcceptKey,
    /// Default executor used by new sessions when no explicit shell is configured.
    pub nebula_shell: NebulaShell,
    /// Whether new sessions print the Nebula welcome/fetch screen.
    pub nebula_fetch_enabled: bool,
    /// Whether the injected prompt uses Nebula's powerline segments.
    pub nebula_powerline_enabled: bool,
    /// Closing a window detaches its panes into the resident process for
    /// re-attach (multiplexer restore). Off = close kills the shells.
    pub nebula_keep_session: bool,
    /// Runtime window opacity controlled from Nebula settings.
    pub nebula_window_opacity: f32,
    /// Optional runtime clear/background color controlled from settings.
    pub nebula_background: Option<Rgb>,
    /// Optional background image path drawn as a full-window wallpaper.
    pub nebula_background_image: Option<String>,
    /// Wallpaper alpha, separate from the window opacity to preserve text contrast.
    pub nebula_background_image_opacity: f32,
    nebula_settings_mtime: Option<std::time::SystemTime>,
    nebula_bg_palette_index: usize,

    /// Tab rename state: when `Some(index, text)`, a text input is shown over
    /// tab `index` with the current edit buffer `text`. The user types to edit,
    /// Enter commits, Esc cancels (Windows Terminal double-click rename).
    pub nebula_tab_rename: Option<(usize, String)>,
    /// True for the instant after a rename begins: the whole existing name
    /// reads as "selected" (nushell-style blue fill) and the first typed
    /// character replaces it wholesale. Cleared on the first edit.
    pub nebula_tab_rename_select_all: bool,
    /// Insertion caret inside the rename buffer, as a CHAR index (0..=chars).
    /// Click-to-place, arrow keys, and mid-string insert/delete all go
    /// through this — a rename is a real text field, not append-only.
    pub nebula_tab_rename_caret: usize,
    /// Left pixel of the rename buffer's first glyph, stashed by `draw_chrome`
    /// each frame the box shows. Click-to-place-caret maps pointer X through
    /// this — recomputing the draw-side layout in the input path would just
    /// let the two drift.
    pub nebula_tab_rename_text_x: f32,

    pub visual_bell: VisualBell,

    /// Mapped RGB values for each terminal color.
    pub colors: List,
    /// The user's configured color scheme, untouched by theme restyling —
    /// the base every `apply_term_colors` starts from.
    nebula_default_colors: List,

    /// State of the keyboard hints.
    pub hint_state: HintState,

    /// Unprocessed display updates.
    pub pending_update: DisplayUpdate,

    /// The renderer update that takes place only once before the actual rendering.
    pub pending_renderer_update: Option<RendererUpdate>,

    /// The ime on the given display.
    pub ime: Ime,

    /// The state of the timer for frame scheduling.
    pub frame_timer: FrameTimer,

    /// Damage tracker for the given display.
    pub damage_tracker: DamageTracker,

    /// Font size used by the window.
    pub font_size: FontSize,

    // Mouse point position when highlighting hints.
    hint_mouse_point: Option<Point>,

    renderer: ManuallyDrop<Renderer>,
    renderer_preference: Option<RendererPreference>,

    surface: ManuallyDrop<Surface<WindowSurface>>,

    context: ManuallyDrop<PossiblyCurrentContext>,

    glyph_cache: GlyphCache,
    meter: Meter,
}

impl Display {
    pub fn new(
        window: Window,
        gl_context: NotCurrentContext,
        config: &UiConfig,
        _tabbed: bool,
    ) -> Result<Display, Error> {
        let raw_window_handle = window.raw_window_handle();

        let scale_factor = window.scale_factor as f32;
        let rasterizer = Rasterizer::new()?;
        crate::boot_trace("rasterizer ready");

        let font_size = config.font.size().scale(scale_factor);
        debug!("Loading \"{}\" font", &config.font.normal().family);
        let font = config.font.clone().with_size(font_size);
        let mut glyph_cache = GlyphCache::new(rasterizer, &font)?;
        crate::boot_trace("glyph cache (font faces loaded)");

        let metrics = glyph_cache.font_metrics();
        let (cell_width, cell_height) = compute_cell_size(config, &metrics);

        // Resize the window to the user-configured size, or a Windows
        // Terminal-like default when unset. 120 columns keeps the default
        // window compact (user flagged 140 as too wide); 20 lines so the
        // welcome intro (17-row galaxy + prompt) still fits without
        // scrolling.
        let dimensions = config
            .window
            .dimensions()
            .unwrap_or(crate::config::window::Dimensions { columns: 120, lines: 20 });
        let size = window_size(config, dimensions, cell_width, cell_height, scale_factor);
        window.request_inner_size(size);

        // Create the GL surface to draw into.
        let surface = platform::create_gl_surface(
            &gl_context,
            window.inner_size(),
            window.raw_window_handle(),
        )?;

        // Make the context current.
        let context = gl_context.make_current(&surface)?;
        crate::boot_trace("surface + context current");

        // Create renderer.
        let mut renderer = Renderer::new(&context, config.debug.renderer)?;
        crate::boot_trace("renderer (shaders compiled)");

        // Load font common glyphs to accelerate rendering.
        debug!("Filling glyph cache with common glyphs");
        renderer.with_loader(|mut api| {
            glyph_cache.reset_glyph_cache(&mut api);
        });
        crate::boot_trace("glyph cache warmed");

        let padding = config.window.padding(window.scale_factor as f32);
        let chrome = chrome_reserve(window.scale_factor as f32);
        let viewport_size = window.inner_size();

        // Create new size with at least one column and row.
        // Asymmetric from the start: the sidebar is expanded on launch, so the
        // left padding carries it while the right keeps the plain content
        // margin. Dynamic padding is dropped — the sidebar fixes the left edge.
        let scale = window.scale_factor as f32;
        let content_pad = content_pad_x(scale);
        let size_info = SizeInfo::new_asymmetric(
            viewport_size.width as f32,
            viewport_size.height as f32,
            cell_width,
            cell_height,
            padding.0 + content_pad + sidebar_width(scale, false),
            padding.0 + content_pad,
            padding.1 + chrome,
        );

        info!("Cell size: {cell_width} x {cell_height}");
        info!("Padding: {} x {}", size_info.padding_x(), size_info.padding_y());
        info!("Width: {}, Height: {}", size_info.width(), size_info.height());

        // Update OpenGL projection.
        renderer.resize(&size_info);

        // Clear screen.
        let settings_init = settings::nebula_settings_load(config);
        let background_color = settings_init.background.unwrap_or(config.colors.primary.background);
        renderer.clear(background_color, settings_init.opacity);
        window.set_transparent(settings_init.opacity < 1.0);

        // Disable shadows for transparent windows on macOS.
        #[cfg(target_os = "macos")]
        window.set_has_shadow(settings_init.opacity >= 1.0);

        let is_wayland = matches!(raw_window_handle, RawWindowHandle::Wayland(_));

        // On Wayland we can safely ignore this call, since the window isn't visible until you
        // actually draw something into it and commit those changes.
        if !is_wayland {
            surface.swap_buffers(&context).expect("failed to swap buffers.");
            renderer.finish();
        }
        crate::boot_trace("first swap done");

        // Set resize increments for the newly created window.
        if config.window.resize_increments {
            window.set_resize_increments(PhysicalSize::new(cell_width, cell_height));
        }

        window.set_visible(true);
        crate::boot_trace("window visible");

        // Always focus new windows, even if no Nebula window is currently focused.
        #[cfg(target_os = "macos")]
        window.focus_window();

        #[allow(clippy::single_match)]
        #[cfg(not(windows))]
        if !_tabbed {
            match config.window.startup_mode {
                #[cfg(target_os = "macos")]
                StartupMode::SimpleFullscreen => window.set_simple_fullscreen(true),
                StartupMode::Maximized if !is_wayland => window.set_maximized(true),
                _ => (),
            }
        }

        let hint_state = HintState::new(config.hints.alphabet());
        // Publish the RESTORED theme to the prompt bridge (writing the default
        // here used to reset the powerline colors on every launch).
        write_nebula_prompt_theme(settings_init.theme);

        let mut damage_tracker = DamageTracker::new(size_info.screen_lines(), size_info.columns());
        damage_tracker.debug = config.debug.highlight_damage;

        // Disable vsync.
        if let Err(err) = surface.set_swap_interval(&context, SwapInterval::DontWait) {
            info!("Failed to disable vsync: {err}");
        }
        crate::boot_trace("swap interval set");

        // Terminal color table: the user's configured scheme, restyled by the
        // restored theme (light themes swap in a readable light ANSI set, and
        // the background OSC 11 reports must match the theme from frame one).
        let nebula_default_colors = List::from(&config.colors);
        let mut initial_colors = nebula_default_colors;
        settings_init.theme.apply_term_colors(&mut initial_colors, &nebula_default_colors);

        Ok(Self {
            context: ManuallyDrop::new(context),
            visual_bell: VisualBell::from(&config.bell),
            renderer: ManuallyDrop::new(renderer),
            renderer_preference: config.debug.renderer,
            surface: ManuallyDrop::new(surface),
            colors: initial_colors,
            nebula_default_colors,
            frame_timer: FrameTimer::new(),
            raw_window_handle,
            damage_tracker,
            glyph_cache,
            hint_state,
            size_info,
            font_size,
            window,
            pending_renderer_update: Default::default(),
            vi_highlighted_hint_age: Default::default(),
            highlighted_hint_age: Default::default(),
            vi_highlighted_hint: Default::default(),
            highlighted_hint: Default::default(),
            hint_mouse_point: Default::default(),
            pending_update: Default::default(),
            cursor_hidden: Default::default(),
            nebula_pane_view: None,
            nebula_resize_hud: None,
            nebula_resize_hud_armed: false,
            nebula_history: {
                let history = crate::nebula_history::NebulaHistory::load();
                crate::boot_trace("history loaded");
                history
            },
            nebula_commands: nebula_commands_handle(),
            nebula_tab_anim: Vec::new(),
            nebula_scrollbar_drag: None,
            nebula_split_reveal: None,
            nebula_confirm: None,
            nebula_confirm_buttons: None,
            nebula_frame_images: Vec::new(),
            nebula_theme: settings_init.theme,
            nebula_settings_open: false,
            nebula_settings_scroll: 0.0,
            nebula_palette: command_palette::CommandPalette::new(),
            nebula_side_panel: side_panel::SidePanel::new(),
            nebula_ui_anims: NebulaUiAnims::new(),
            nebula_settings_section: NebulaSettingsSection::default(),
            nebula_chrome_hover: ChromeHit::None,
            nebula_settings_hover: SettingsHit::None,
            nebula_tab_labels: vec![".".to_owned()],
            nebula_tab_bells: vec![false],
            nebula_tab_running: vec![false],
            nebula_tab_logos: vec![None],
            nebula_ai_logo_cache: Default::default(),
            nebula_chrome_logo_draws: Vec::new(),
            nebula_active_tab: 0,
            nebula_tab_drag: None,
            nebula_tabs_reorderable: true,
            nebula_sidebar_collapsed: false,
            nebula_tab_rename: None,
            nebula_tab_rename_select_all: false,
            nebula_tab_rename_caret: 0,
            nebula_tab_rename_text_x: 0.0,
            nebula_pty_resize_pending: false,
            nebula_ghost_enabled: settings_init.ghost,
            nebula_accept: settings_init.accept,
            nebula_shell: settings_init.shell,
            nebula_fetch_enabled: settings_init.fetch,
            nebula_powerline_enabled: settings_init.powerline,
            nebula_keep_session: settings_init.keep_session,
            nebula_window_opacity: settings_init.opacity,
            nebula_background: settings_init.background,
            nebula_background_image: settings_init.background_image,
            nebula_background_image_opacity: settings_init.background_image_opacity,
            nebula_settings_mtime: settings::nebula_settings_mtime(),
            nebula_bg_palette_index: 0,
            meter: Default::default(),
            ime: Default::default(),
        })
    }

    pub fn settings_open(&self) -> bool {
        self.nebula_settings_open
    }

    pub fn settings_section(&self) -> NebulaSettingsSection {
        self.nebula_settings_section
    }

    pub fn select_settings_section(&mut self, section: NebulaSettingsSection) {
        if self.nebula_settings_section != section {
            self.nebula_settings_section = section;
            // Each section starts reading from its top.
            self.nebula_settings_scroll = 0.0;
            self.pending_update.dirty = true;
        }
    }

    /// Scroll the settings content by `delta` px (positive = content moves
    /// up). Clamped against the active section's overflow; no-op while the
    /// panel is closed.
    pub fn settings_scroll_by(&mut self, delta: f32) {
        if !self.nebula_settings_open {
            return;
        }
        let max = settings::settings_max_scroll(
            &self.size_info,
            self.window.scale_factor as f32,
            self.nebula_settings_section,
        );
        let next = (self.nebula_settings_scroll + delta).clamp(0.0, max);
        if (next - self.nebula_settings_scroll).abs() > f32::EPSILON {
            self.nebula_settings_scroll = next;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn settings_scroll(&self) -> f32 {
        self.nebula_settings_scroll
    }

    pub fn set_chrome_tabs(
        &mut self,
        labels: Vec<String>,
        mut dots: Vec<bool>,
        mut running: Vec<bool>,
        mut logos: Vec<Option<AiLogo>>,
        active: usize,
        reorderable: bool,
    ) {
        self.nebula_tab_labels = if labels.is_empty() { vec![".".to_owned()] } else { labels };
        dots.truncate(self.nebula_tab_labels.len());
        dots.resize(self.nebula_tab_labels.len(), false);
        self.nebula_tab_bells = dots;
        running.truncate(self.nebula_tab_labels.len());
        running.resize(self.nebula_tab_labels.len(), false);
        self.nebula_tab_running = running;
        logos.truncate(self.nebula_tab_labels.len());
        logos.resize(self.nebula_tab_labels.len(), None);
        self.nebula_tab_logos = logos;
        self.nebula_active_tab = active.min(self.nebula_tab_labels.len().saturating_sub(1));
        self.nebula_tabs_reorderable = reorderable;
        // A tab count change (close/open) mid-drag invalidates the grabbed slot.
        if self.nebula_tab_drag.map_or(false, |d| d.source >= self.nebula_tab_labels.len()) {
            self.nebula_tab_drag = None;
        }
    }

    /// Whether any sidebar tab currently shows a running spinner — drives
    /// the fast (8 fps) chrome animation clock instead of the 1 Hz one.
    pub fn any_tab_running(&self) -> bool {
        self.nebula_tab_running.iter().any(|running| *running)
    }

    /// A chrome text editor (tab rename / drawer filter / commit message) has
    /// keyboard focus — the window context bumps the redraw tick to the fast
    /// cadence so the insertion caret visibly blinks.
    pub fn chrome_editor_active(&self) -> bool {
        self.nebula_tab_rename.is_some()
            || self.nebula_side_panel.search_focus
            || self.nebula_side_panel.commit_focus
    }

    /// Decoded (and theme-tinted) pixels for an AI brand logo, plus a stable
    /// texture id for the renderer's inline cache. Decode + tint run once per
    /// (logo, ink); the GPU upload happens lazily inside the renderer.
    fn ai_logo_pixels(
        &mut self,
        logo: AiLogo,
        ink: Rgb,
    ) -> Option<(u64, std::sync::Arc<Vec<u8>>, (u32, u32))> {
        // Claude's mark ships in Anthropic coral and is used as-is (only its
        // alpha matters), so its cache key ignores the ink. The OpenAI mark
        // is black-on-alpha and follows the chrome ink to stay visible on
        // every theme.
        let key = match logo {
            AiLogo::Claude => (logo, [0, 0, 0]),
            AiLogo::OpenAi | AiLogo::OpenCode => (logo, [ink.r, ink.g, ink.b]),
        };
        if let Some(cached) = self.nebula_ai_logo_cache.get(&key) {
            return Some(cached.clone());
        }
        let bytes: &[u8] = match logo {
            AiLogo::Claude => AI_LOGO_CLAUDE_PNG,
            AiLogo::OpenAi => AI_LOGO_OPENAI_PNG,
            AiLogo::OpenCode => AI_LOGO_OPENCODE_PNG,
        };
        let (width, height, mut rgba) = match crate::renderer::image::decode_png_bytes(bytes) {
            Ok(decoded) => decoded,
            // Unreachable for a valid embedded asset; degrade to no icon.
            Err(err) => {
                log::warn!("failed to decode embedded AI logo: {err}");
                return None;
            },
        };
        match logo {
            AiLogo::OpenAi => {
                for px in rgba.chunks_exact_mut(4) {
                    (px[0], px[1], px[2]) = (ink.r, ink.g, ink.b);
                }
            },
            AiLogo::OpenCode => {
                // Stored grayscale = luma map (frame 255, screen-block 90).
                // Tint to theme ink scaled by luma: frame → full ink, inner
                // block → ~35% ink, keeping the two-tone mark on every theme.
                for px in rgba.chunks_exact_mut(4) {
                    let luma = px[0] as u16; // R==G==B in the asset
                    px[0] = (ink.r as u16 * luma / 255) as u8;
                    px[1] = (ink.g as u16 * luma / 255) as u8;
                    px[2] = (ink.b as u16 * luma / 255) as u8;
                }
            },
            AiLogo::Claude => {},
        }
        let id = AI_LOGO_ID_BASE + self.nebula_ai_logo_cache.len() as u64;
        let entry = (id, std::sync::Arc::new(rgba), (width, height));
        self.nebula_ai_logo_cache.insert(key, entry.clone());
        Some(entry)
    }

    /// Arm a potential tab drag from a press on displayed tab `source`. Always
    /// arms (even single-tab), because the release decides between click /
    /// reorder / dock — selection itself is deferred to the release.
    pub fn arm_tab_drag(&mut self, source: usize, x: f32, y: f32) {
        self.nebula_tab_drag = Some(TabDrag {
            source,
            origin_x: x,
            origin: y,
            current: y,
            active: false,
            dock: None,
        });
    }

    /// Whether a tab drag is currently armed (pressed, possibly not yet moved).
    pub fn tab_drag_armed(&self) -> bool {
        self.nebula_tab_drag.is_some()
    }

    /// Feed the pointer into an armed drag. Y drives the in-sidebar reorder;
    /// crossing into the terminal area computes the dock side. Returns `true`
    /// once the drag is active (past threshold on either axis), signalling the
    /// caller to show the grab cursor and repaint.
    pub fn update_tab_drag(&mut self, x: f32, y: f32) -> bool {
        let threshold = 6.0 * self.window.scale_factor as f32;
        // Compute before the mutable borrow below.
        let dock = self.dock_nav_at(x, y);
        match self.nebula_tab_drag.as_mut() {
            Some(drag) => {
                drag.current = y;
                if !drag.active
                    && ((y - drag.origin).abs() > threshold
                        || (x - drag.origin_x).abs() > threshold)
                {
                    drag.active = true;
                }
                if drag.active {
                    drag.dock = dock;
                }
                drag.active
            },
            None => false,
        }
    }

    /// Dock side for a pointer inside the terminal area, `None` outside it.
    /// The area is quartered along its diagonals: the nearest edge wins, which
    /// gives the natural triangular dock zones.
    fn dock_nav_at(&self, x: f32, y: f32) -> Option<SplitNav> {
        let gx = self.size_info.padding_x();
        let gy = self.size_info.padding_y();
        let gw = self.size_info.width() - gx - self.size_info.padding_right();
        let gh = self.size_info.height() - 2.0 * gy;
        if gw <= 0.0 || gh <= 0.0 || x < gx || y < gy || x > gx + gw || y > gy + gh {
            return None;
        }
        let nx = (x - gx) / gw;
        let ny = (y - gy) / gh;
        let (dl, dr, dt, db) = (nx, 1.0 - nx, ny, 1.0 - ny);
        let min = dl.min(dr).min(dt).min(db);
        Some(if min == dl {
            SplitNav::Left
        } else if min == dr {
            SplitNav::Right
        } else if min == dt {
            SplitNav::Up
        } else {
            SplitNav::Down
        })
    }

    /// Finish a tab drag, deciding what the release means.
    pub fn end_tab_drag(&mut self) -> Option<TabDropAction> {
        let drag = self.nebula_tab_drag.take()?;
        if !drag.active {
            // Never moved: a plain click — select on release.
            return Some(TabDropAction::Click(drag.source));
        }
        if let Some(nav) = drag.dock {
            return Some(TabDropAction::Dock { source: drag.source, nav });
        }
        if !self.nebula_tabs_reorderable || self.nebula_tab_labels.len() < 2 {
            return Some(TabDropAction::Click(drag.source));
        }
        let target = self.tab_drop_index(drag.source, drag.current);
        if target != drag.source
            && drag.source < self.nebula_tab_anim.len()
            && target < self.nebula_tab_anim.len()
        {
            // Reorder the animated draw-y values alongside the tabs so each
            // pill keeps its on-screen position and *eases* into its new slot
            // instead of snapping when the drop commits.
            let v = self.nebula_tab_anim.remove(drag.source);
            self.nebula_tab_anim.insert(target, v);
        }
        if target != drag.source {
            Some(TabDropAction::Reorder { from: drag.source, to: target })
        } else {
            Some(TabDropAction::Click(drag.source))
        }
    }

    /// Displayed slot the grabbed tab would drop into for pointer X: the number
    /// of *other* tabs whose centre the pointer has passed. This yields the
    /// correct remove-then-insert target index for a single-tab move.
    fn tab_drop_index(&self, source: usize, y: f32) -> usize {
        let scale = self.window.scale_factor as f32;
        let sidebar_expand = self.left_sidebar_progress();
        let layout = chrome_tab_layout(
            &self.size_info,
            scale,
            self.nebula_tab_labels.len(),
            sidebar_expand,
        );
        // Tabs stack vertically now: count rows whose vertical centre the
        // pointer has passed to get the remove-then-insert target slot.
        let passed = layout
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, rect)| *i != source && y > rect.1 + rect.3 * 0.5)
            .count();
        passed.min(self.nebula_tab_labels.len().saturating_sub(1))
    }

    /// Draw-X for a tab's pill/label during a reorder drag. The grabbed pill
    /// follows the pointer (clamped to the strip); every other tab between the
    /// grabbed slot and the current drop target slides one slot toward the
    /// vacated source, opening a gap for the drop ("让位"). No shift when idle.
    fn tab_drag_draw_y(&self, index: usize, tab_y: f32, layout: &ChromeTabLayout) -> f32 {
        let Some(d) = self.nebula_tab_drag.filter(|d| d.active) else { return tab_y };

        // The grabbed pill tracks the pointer, clamped to the tab column.
        if d.source == index {
            let lo = layout.tabs.first().map_or(tab_y, |t| t.1);
            let hi = layout.tabs.last().map_or(tab_y, |t| t.1);
            return (tab_y + d.current - d.origin).clamp(lo, hi);
        }

        // Other tabs make way. Slot pitch = distance between adjacent rows
        // (uniform height + gap); needs at least two tabs, which a drag implies.
        let Some(second) = layout.tabs.get(1) else { return tab_y };
        let slot = second.1 - layout.tabs[0].1;
        let target = self.tab_drop_index(d.source, d.current);
        if d.source < target && index > d.source && index <= target {
            tab_y - slot // dragging down: rows in (source, target] slide up
        } else if d.source > target && index >= target && index < d.source {
            tab_y + slot // dragging up: rows in [target, source) slide down
        } else {
            tab_y
        }
    }

    pub fn set_chrome_hover(&mut self, chrome: ChromeHit, settings: SettingsHit) {
        if self.nebula_chrome_hover != chrome || self.nebula_settings_hover != settings {
            self.nebula_chrome_hover = chrome;
            self.nebula_settings_hover = settings;
            self.pending_update.dirty = true;
        }
    }

    pub fn chrome_hit(&self, x: f32, y: f32) -> ChromeHit {
        chrome_hit_with_tabs(
            &self.size_info,
            self.window.scale_factor as f32,
            self.nebula_tab_labels.len().max(1),
            self.nebula_sidebar_collapsed,
            x,
            y,
        )
    }

    /// Fold the tab sidebar in or out. Toggling changes the grid's usable width,
    /// so it re-runs the resize/reflow path by re-feeding the current window
    /// size — `handle_update` then recomputes the asymmetric padding split.
    pub fn toggle_sidebar(&mut self) {
        self.nebula_sidebar_collapsed = !self.nebula_sidebar_collapsed;
        let size = PhysicalSize::new(self.size_info.width() as u32, self.size_info.height() as u32);
        self.pending_update.set_dimensions(size);
        self.window.request_redraw();
        self.pending_update.dirty = true;
    }

    /// Snapshot of the state the settings render reads, owning the wallpaper
    /// path so `draw_chrome` can still borrow `&mut renderer` afterwards.
    fn settings_view(&self) -> settings::SettingsView {
        settings::SettingsView {
            section: self.nebula_settings_section,
            hover: self.nebula_settings_hover,
            theme: self.nebula_theme,
            ghost: self.nebula_ghost_enabled,
            accept: self.nebula_accept,
            shell: self.nebula_shell,
            fetch: self.nebula_fetch_enabled,
            powerline: self.nebula_powerline_enabled,
            keep_session: self.nebula_keep_session,
            opacity: self.nebula_window_opacity,
            background: self.nebula_background,
            background_image: self.nebula_background_image.clone(),
            background_image_opacity: self.nebula_background_image_opacity,
            scroll: self.nebula_settings_scroll,
        }
    }

    pub fn toggle_settings(&mut self) {
        self.nebula_settings_open = !self.nebula_settings_open;
        // Fresh open always starts at the top.
        self.nebula_settings_scroll = 0.0;
        self.pending_update.dirty = true;
    }

    pub fn dismiss_settings(&mut self) {
        if self.nebula_settings_open {
            self.nebula_settings_open = false;
            self.pending_update.dirty = true;
        }
    }

    pub fn select_nebula_theme(&mut self, theme: NebulaTheme) {
        self.nebula_theme = theme;
        // A theme carries its terminal background (the light themes are
        // unusable without it). Switching theme IS choosing the look, so it
        // overwrites a previous custom color by design.
        self.nebula_background = Some(theme.palette().term_bg);
        // Restyle the terminal color table: OSC 11 must report the new
        // background (TUIs key light/dark off it) and light themes need the
        // light ANSI set to stay readable.
        let defaults = self.nebula_default_colors;
        theme.apply_term_colors(&mut self.colors, &defaults);
        write_nebula_prompt_theme(theme);
        self.persist_nebula_settings();
        // Panel stays open so users can adjust several settings at once.
        self.pending_update.dirty = true;
    }

    pub fn toggle_ghost(&mut self) {
        self.nebula_ghost_enabled = !self.nebula_ghost_enabled;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn cycle_accept(&mut self) {
        self.nebula_accept = self.nebula_accept.cycle();
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn cycle_shell(&mut self) {
        self.nebula_shell = self.nebula_shell.cycle();
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn toggle_fetch(&mut self) {
        self.nebula_fetch_enabled = !self.nebula_fetch_enabled;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn toggle_powerline(&mut self) {
        self.nebula_powerline_enabled = !self.nebula_powerline_enabled;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// 高级→会话: whether closing a window keeps its shells in the resident
    /// process (detach / re-attach restore) or kills them outright.
    pub fn toggle_keep_session(&mut self) {
        self.nebula_keep_session = !self.nebula_keep_session;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn adjust_window_opacity(&mut self, delta: f32) {
        self.nebula_window_opacity = (self.nebula_window_opacity + delta).clamp(0.35, 1.0);
        self.window.set_transparent(self.nebula_window_opacity < 1.0);
        #[cfg(target_os = "macos")]
        self.window.set_has_shadow(self.nebula_window_opacity >= 1.0);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn cycle_background_color(&mut self) {
        const BACKGROUNDS: [Rgb; 6] = [
            Rgb::new(8, 10, 24),
            Rgb::new(0, 43, 54),
            Rgb::new(24, 24, 37),
            Rgb::new(12, 16, 28),
            Rgb::new(18, 14, 32),
            Rgb::new(6, 26, 28),
        ];
        self.nebula_bg_palette_index = (self.nebula_bg_palette_index + 1) % BACKGROUNDS.len();
        self.nebula_background = Some(BACKGROUNDS[self.nebula_bg_palette_index]);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// Pick a background image through the OS file dialog, then persist it and
    /// refresh the renderer's cached wallpaper. On non-Windows platforms the
    /// native dialog isn't wired up, so we fall back to opening the settings
    /// file for the path to be entered by hand.
    pub fn pick_background_image(&mut self) {
        #[cfg(windows)]
        {
            if let Some(path) = nebula_pick_image_file(self.raw_window_handle) {
                self.nebula_background_image = Some(path);
                self.persist_nebula_settings();
                self.renderer.invalidate_background_image();
                self.pending_update.dirty = true;
            }
        }
        #[cfg(not(windows))]
        {
            self.open_settings_file();
        }
    }

    pub fn open_settings_file(&mut self) {
        self.persist_nebula_settings();
        let path = nebula_data_dir().join("nebula_settings.txt");
        #[cfg(windows)]
        let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&path).spawn();
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
        self.pending_update.dirty = true;
    }

    pub fn reset_appearance_settings(&mut self) {
        self.nebula_theme = NebulaTheme::default();
        let defaults = self.nebula_default_colors;
        self.nebula_theme.apply_term_colors(&mut self.colors, &defaults);
        write_nebula_prompt_theme(self.nebula_theme);
        self.nebula_window_opacity = 1.0;
        self.nebula_background = None;
        self.nebula_background_image = None;
        self.nebula_background_image_opacity = 0.38;
        self.window.set_transparent(false);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// Toggle the command palette (Ctrl+Shift+P). `profiles` are the config's
    /// quick-launch profile names, refreshed on every open so live config
    /// reloads are reflected.
    pub fn toggle_command_palette(&mut self, profiles: &[String]) {
        self.nebula_palette.set_profiles(profiles);
        self.nebula_palette.toggle();
        self.pending_update.dirty = true;
    }

    /// Open the palette restricted to quick-launch profiles — the context menu
    /// behind right-clicking the sidebar "+" button.
    pub fn open_profile_menu(&mut self, profiles: &[String]) {
        self.nebula_palette.set_profiles(profiles);
        self.nebula_palette.open_profiles();
        self.pending_update.dirty = true;
    }

    pub fn command_palette_open(&self) -> bool {
        self.nebula_palette.is_open()
    }

    pub fn close_command_palette(&mut self) {
        self.nebula_palette.close();
        self.pending_update.dirty = true;
    }

    pub fn palette_input_char(&mut self, c: char) {
        self.nebula_palette.input_char(c);
        self.pending_update.dirty = true;
    }

    pub fn palette_backspace(&mut self) {
        self.nebula_palette.backspace();
        self.pending_update.dirty = true;
    }

    pub fn palette_move(&mut self, delta: i32) {
        self.nebula_palette.move_selection(delta);
        self.pending_update.dirty = true;
    }

    /// Confirm the palette selection; returns the action for the input layer to
    /// dispatch (only it can reach both the display and the window context).
    pub fn palette_confirm(&mut self) -> Option<command_palette::PaletteAction> {
        let action = self.nebula_palette.confirm();
        self.pending_update.dirty = true;
        action
    }

    /// Mouse click on the palette's visible row `row` (0 = topmost visible):
    /// select and confirm it, returning the action to dispatch.
    pub fn palette_click(
        &mut self,
        row: usize,
        max_rows: usize,
    ) -> Option<command_palette::PaletteAction> {
        let action = self.nebula_palette.click(row, max_rows);
        self.pending_update.dirty = true;
        action
    }

    /// Toggle the right-side drawer (directory tree / git status).
    pub fn toggle_side_panel(&mut self, view: side_panel::PanelView) {
        self.nebula_side_panel.toggle(view);
        self.window.request_redraw();
        self.pending_update.dirty = true;
    }

    // ---- tab rename caret editing (the rename box is a real text field) ----

    /// Insert `text` at the caret. A pending select-all is replaced wholesale
    /// (type-to-overwrite), matching every native text field.
    pub fn tab_rename_insert(&mut self, text: &str) {
        let select_all = self.nebula_tab_rename_select_all;
        let caret = self.nebula_tab_rename_caret;
        let Some((_, buf)) = self.nebula_tab_rename.as_mut() else { return };
        if select_all {
            buf.clear();
            self.nebula_tab_rename_select_all = false;
            self.nebula_tab_rename_caret = 0;
        }
        let caret = if select_all { 0 } else { caret.min(buf.chars().count()) };
        let byte = buf.char_indices().nth(caret).map(|(b, _)| b).unwrap_or(buf.len());
        buf.insert_str(byte, text);
        self.nebula_tab_rename_caret = caret + text.chars().count();
        self.pending_update.dirty = true;
    }

    /// Backspace at the caret; a pending select-all clears the whole name.
    pub fn tab_rename_backspace(&mut self) {
        let select_all = self.nebula_tab_rename_select_all;
        let caret = self.nebula_tab_rename_caret;
        let Some((_, buf)) = self.nebula_tab_rename.as_mut() else { return };
        if select_all {
            buf.clear();
            self.nebula_tab_rename_select_all = false;
            self.nebula_tab_rename_caret = 0;
        } else if caret > 0 {
            let caret = caret.min(buf.chars().count());
            if let Some((byte, _)) = buf.char_indices().nth(caret - 1) {
                buf.remove(byte);
                self.nebula_tab_rename_caret = caret - 1;
            }
        }
        self.pending_update.dirty = true;
    }

    /// Move the caret by `delta` chars. A select-all collapses to the matching
    /// end first (left → start, right → end) without moving further.
    pub fn tab_rename_move_caret(&mut self, delta: i32) {
        let Some((_, buf)) = self.nebula_tab_rename.as_ref() else { return };
        let len = buf.chars().count();
        if self.nebula_tab_rename_select_all {
            self.nebula_tab_rename_select_all = false;
            self.nebula_tab_rename_caret = if delta < 0 { 0 } else { len };
        } else {
            let caret = self.nebula_tab_rename_caret.min(len) as i64 + delta as i64;
            self.nebula_tab_rename_caret = caret.clamp(0, len as i64) as usize;
        }
        self.pending_update.dirty = true;
    }

    /// Jump the caret to the start/end (Home/End).
    pub fn tab_rename_caret_edge(&mut self, end: bool) {
        let Some((_, buf)) = self.nebula_tab_rename.as_ref() else { return };
        self.nebula_tab_rename_select_all = false;
        self.nebula_tab_rename_caret = if end { buf.chars().count() } else { 0 };
        self.pending_update.dirty = true;
    }

    /// Place the caret from a pointer press at window-space `x`: map the
    /// pixel offset from the buffer's first glyph (stashed by `draw_chrome`)
    /// into a char index, honoring CJK double-width glyphs. This is what lets
    /// users click where they want to edit instead of retyping the name.
    pub fn tab_rename_click(&mut self, x: f32) {
        let text_x = self.nebula_tab_rename_text_x;
        let cell_w = self.size_info.cell_width();
        let Some((_, buf)) = self.nebula_tab_rename.as_ref() else { return };
        let mut col = ((x - text_x) / cell_w).round().max(0.0) as usize;
        let mut caret = 0usize;
        for c in buf.chars() {
            let w = c.width().unwrap_or(0).max(1);
            if col < w {
                break;
            }
            col -= w;
            caret += 1;
        }
        self.nebula_tab_rename_select_all = false;
        self.nebula_tab_rename_caret = caret;
        self.pending_update.dirty = true;
    }

    /// Adopt the focused pane's cwd into the drawer (per drawn frame; cheap
    /// no-op unless the drawer is open and something changed).
    pub fn side_panel_sync(&mut self, cwd: Option<std::path::PathBuf>) {
        if self.nebula_side_panel.sync(cwd) {
            self.pending_update.dirty = true;
        }
    }

    /// Geometry of the drawer for the current window size.
    pub fn side_panel_layout(&self) -> side_panel::PanelLayout {
        let size = self.size_info;
        let scale = self.window.scale_factor as f32;
        let reserve = chrome_reserve(scale);
        side_panel::panel_layout(
            size.width(),
            size.height(),
            reserve,
            reserve,
            scale,
            self.nebula_ui_anims.right_drawer.value(),
        )
    }

    pub fn step_chrome_anims(&mut self) {
        self.nebula_ui_anims
            .step(!self.nebula_sidebar_collapsed, self.nebula_side_panel.open);
    }

    pub fn chrome_animating(&self) -> bool {
        self.nebula_ui_anims
            .animating(!self.nebula_sidebar_collapsed, self.nebula_side_panel.open)
    }

    pub fn left_sidebar_progress(&self) -> f32 {
        self.nebula_ui_anims.left_sidebar.value()
    }

    pub fn left_sidebar_visible(&self) -> bool {
        self.nebula_ui_anims
            .left_sidebar
            .visible(!self.nebula_sidebar_collapsed)
    }

    pub fn side_panel_visible(&self) -> bool {
        self.nebula_ui_anims.right_drawer.visible(self.nebula_side_panel.open)
    }

    fn persist_nebula_settings(&mut self) {
        settings::nebula_settings_write(&settings::NebulaRuntimeSettings {
            ghost: self.nebula_ghost_enabled,
            accept: self.nebula_accept,
            shell: self.nebula_shell,
            fetch: self.nebula_fetch_enabled,
            powerline: self.nebula_powerline_enabled,
            keep_session: self.nebula_keep_session,
            opacity: self.nebula_window_opacity,
            background: self.nebula_background,
            background_image: self.nebula_background_image.clone(),
            background_image_opacity: self.nebula_background_image_opacity,
            theme: self.nebula_theme,
        });
        self.nebula_settings_mtime = settings::nebula_settings_mtime();
    }

    fn reload_nebula_settings_if_changed(&mut self, config: &UiConfig) {
        let mtime = settings::nebula_settings_mtime();
        if mtime == self.nebula_settings_mtime {
            return;
        }

        let settings = settings::nebula_settings_load(config);
        let image_changed = settings.background_image != self.nebula_background_image;
        if settings.theme != self.nebula_theme {
            // Hand-edited theme in the config file: apply and re-publish the
            // prompt bridge just like an in-panel selection would.
            self.nebula_theme = settings.theme;
            let defaults = self.nebula_default_colors;
            settings.theme.apply_term_colors(&mut self.colors, &defaults);
            write_nebula_prompt_theme(settings.theme);
        }
        self.nebula_ghost_enabled = settings.ghost;
        self.nebula_accept = settings.accept;
        self.nebula_shell = settings.shell;
        self.nebula_fetch_enabled = settings.fetch;
        self.nebula_powerline_enabled = settings.powerline;
        self.nebula_keep_session = settings.keep_session;
        self.nebula_window_opacity = settings.opacity;
        self.nebula_background = settings.background;
        self.nebula_background_image = settings.background_image;
        self.nebula_background_image_opacity = settings.background_image_opacity;
        if image_changed {
            self.renderer.invalidate_background_image();
        }
        self.nebula_settings_mtime = mtime;
        self.window.set_transparent(self.nebula_window_opacity < 1.0);
        #[cfg(target_os = "macos")]
        self.window.set_has_shadow(self.nebula_window_opacity >= 1.0);
        self.pending_update.dirty = true;
    }

    fn draw_background_image(&mut self) {
        let Some(path) = self.nebula_background_image.as_deref() else {
            return;
        };
        let path = path.trim().trim_matches('"');
        if path.is_empty() {
            return;
        }

        // Keep PNG wallpaper loading in the renderer cache. The setting stores a
        // user path verbatim (usually `D:\...` on Windows); `cover` scaling and
        // alpha are handled by the image renderer.
        let opacity = self.nebula_background_image_opacity * self.nebula_window_opacity;
        self.renderer.draw_background_image(&self.size_info, Path::new(path), opacity);
    }

    #[inline]
    pub fn gl_context(&self) -> &PossiblyCurrentContext {
        &self.context
    }

    pub fn make_not_current(&mut self) {
        if self.context.is_current() {
            self.context.make_not_current_in_place().expect("failed to disable context");
        }
    }

    pub fn make_current(&mut self) {
        let is_current = self.context.is_current();

        // Attempt to make the context current if it's not.
        let context_loss = if is_current {
            self.renderer.was_context_reset()
        } else {
            match self.context.make_current(&self.surface) {
                Err(err) if err.error_kind() == ErrorKind::ContextLost => {
                    info!("Context lost for window {:?}", self.window.id());
                    true
                },
                _ => false,
            }
        };

        if !context_loss {
            return;
        }

        let gl_display = self.context.display();
        let gl_config = self.context.config();
        let raw_window_handle = Some(self.window.raw_window_handle());
        let context = platform::create_gl_context(&gl_display, &gl_config, raw_window_handle)
            .expect("failed to recreate context.");

        // Drop the old context and renderer.
        unsafe {
            ManuallyDrop::drop(&mut self.renderer);
            ManuallyDrop::drop(&mut self.context);
        }

        // Activate new context.
        let context = context.treat_as_possibly_current();
        self.context = ManuallyDrop::new(context);
        self.context.make_current(&self.surface).expect("failed to reativate context after reset.");

        // Recreate renderer.
        let renderer = Renderer::new(&self.context, self.renderer_preference)
            .expect("failed to recreate renderer after reset");
        self.renderer = ManuallyDrop::new(renderer);

        // Resize the renderer.
        self.renderer.resize(&self.size_info);

        self.reset_glyph_cache();
        self.damage_tracker.frame().mark_fully_damaged();

        debug!("Recovered window {:?} from gpu reset", self.window.id());
    }

    fn swap_buffers(&self) {
        #[allow(clippy::single_match)]
        let res = match (self.surface.deref(), &self.context.deref()) {
            #[cfg(not(any(target_os = "macos", windows)))]
            (Surface::Egl(surface), PossiblyCurrentContext::Egl(context))
                if matches!(self.raw_window_handle, RawWindowHandle::Wayland(_))
                    && !self.damage_tracker.debug =>
            {
                let damage = self.damage_tracker.shape_frame_damage(self.size_info.into());
                surface.swap_buffers_with_damage(context, &damage)
            },
            (surface, context) => surface.swap_buffers(context),
        };
        if let Err(err) = res {
            debug!("error calling swap_buffers: {err}");
        }
    }

    /// Update font size and cell dimensions.
    ///
    /// This will return a tuple of the cell width and height.
    fn update_font_size(
        glyph_cache: &mut GlyphCache,
        config: &UiConfig,
        font: &Font,
    ) -> (f32, f32) {
        let _ = glyph_cache.update_font_size(font);

        // Compute new cell sizes.
        compute_cell_size(config, &glyph_cache.font_metrics())
    }

    /// Reset glyph cache.
    fn reset_glyph_cache(&mut self) {
        let cache = &mut self.glyph_cache;
        self.renderer.with_loader(|mut api| {
            cache.reset_glyph_cache(&mut api);
        });
    }

    // XXX: this function must not call to any `OpenGL` related tasks. Renderer updates are
    // performed in [`Self::process_renderer_update`] right before drawing.
    //
    /// Process update events.
    pub fn handle_update<T>(
        &mut self,
        terminal: &mut Term<T>,
        // PTY resizes are deferred to the settle timer (see
        // `nebula_pty_resize_pending`); the handle stays in the signature so
        // the call sites don't churn if an immediate path returns.
        _pty_resize_handle: &mut dyn OnResize,
        message_buffer: &MessageBuffer,
        search_state: &mut SearchState,
        config: &UiConfig,
    ) where
        T: EventListener,
    {
        let pending_update = mem::take(&mut self.pending_update);

        let (mut cell_width, mut cell_height) =
            (self.size_info.cell_width(), self.size_info.cell_height());

        if pending_update.font().is_some() || pending_update.cursor_dirty() {
            let renderer_update = self.pending_renderer_update.get_or_insert(Default::default());
            renderer_update.clear_font_cache = true
        }

        // Update font size and cell dimensions.
        if let Some(font) = pending_update.font() {
            let cell_dimensions = Self::update_font_size(&mut self.glyph_cache, config, font);
            cell_width = cell_dimensions.0;
            cell_height = cell_dimensions.1;

            info!("Cell size: {cell_width} x {cell_height}");

            // Mark entire terminal as damaged since glyph size could change without cell size
            // changes.
            self.damage_tracker.frame().mark_fully_damaged();
        }

        let (mut width, mut height) = (self.size_info.width(), self.size_info.height());
        if let Some(dimensions) = pending_update.dimensions() {
            width = dimensions.width as f32;
            height = dimensions.height as f32;
        }

        let padding = config.window.padding(self.window.scale_factor as f32);
        let chrome = chrome_reserve(self.window.scale_factor as f32);

        let scale = self.window.scale_factor as f32;
        let content_pad = content_pad_x(scale);
        let sidebar = sidebar_width(scale, self.nebula_sidebar_collapsed);
        let mut new_size = SizeInfo::new_asymmetric(
            width,
            height,
            cell_width,
            cell_height,
            padding.0 + content_pad + sidebar,
            padding.0 + content_pad,
            padding.1 + chrome,
        );

        // Update number of column/lines in the viewport.
        let search_active = search_state.history_index.is_some();
        let message_bar_lines = message_buffer.message().map_or(0, |m| m.text(&new_size).len());
        let search_lines = usize::from(search_active);
        new_size.reserve_lines(message_bar_lines + search_lines);

        // Update resize increments.
        if config.window.resize_increments {
            self.window.set_resize_increments(PhysicalSize::new(cell_width, cell_height));
        }

        // Resize when terminal when its dimensions have changed.
        if self.size_info.screen_lines() != new_size.screen_lines
            || self.size_info.columns() != new_size.columns()
        {
            // Defer the PTY resize to the settle timer instead of notifying
            // per tick: the in-box ConPTY repaints its entire viewport on
            // every resize, so drag-resizing would flood the scrollback with
            // dozens of shredded repaints (and TUIs like Claude Code redraw
            // storms). The grid resizes live below, so rendering stays exact;
            // only the child's SIGWINCH-equivalent waits for the drag to end.
            self.nebula_pty_resize_pending = true;

            // Resize terminal.
            terminal.resize(new_size);

            // Resize damage tracking.
            self.damage_tracker.resize(new_size.screen_lines(), new_size.columns());

            // Flash a transient "cols × rows" HUD, skipping the first (startup)
            // resize so nothing flashes when the window is first created.
            if self.nebula_resize_hud_armed {
                self.nebula_resize_hud =
                    Some((new_size.columns(), new_size.screen_lines(), Instant::now()));
            }
            self.nebula_resize_hud_armed = true;
        }

        // Check if dimensions have changed.
        if new_size != self.size_info {
            // Queue renderer update.
            let renderer_update = self.pending_renderer_update.get_or_insert(Default::default());
            renderer_update.resize = true;

            // Clear focused search match.
            search_state.clear_focused_match();
        }
        self.size_info = new_size;
    }

    // NOTE: Renderer updates are split off, since platforms like Wayland require resize and other
    // OpenGL operations to be performed right before rendering. Otherwise they could lock the
    // back buffer and render with the previous state. This also solves flickering during resizes.
    //
    /// Update the state of the renderer.
    pub fn process_renderer_update(&mut self) {
        let renderer_update = match self.pending_renderer_update.take() {
            Some(renderer_update) => renderer_update,
            _ => return,
        };

        // Resize renderer.
        if renderer_update.resize {
            let width = NonZeroU32::new(self.size_info.width() as u32).unwrap();
            let height = NonZeroU32::new(self.size_info.height() as u32).unwrap();
            self.surface.resize(&self.context, width, height);
        }

        // Ensure we're modifying the correct OpenGL context.
        self.make_current();

        if renderer_update.clear_font_cache {
            self.reset_glyph_cache();
        }

        self.renderer.resize(&self.size_info);

        info!("Padding: {} x {}", self.size_info.padding_x(), self.size_info.padding_y());
        info!("Width: {}, Height: {}", self.size_info.width(), self.size_info.height());
    }

    /// Draw the screen.
    ///
    /// A reference to Term whose state is being drawn must be provided.
    ///
    /// This call may block if vsync is enabled.
    /// Render a single terminal into the region described by `view`.
    ///
    /// This paints grid cells, cursor, overlays (search/IME/message bar) and the
    /// inline ghost suggestion, but it does NOT clear (unless `clear_first`),
    /// draw the window chrome, or present — those are the caller's job so that
    /// multiple panes can share one frame. `force_focus` overrides the terminal's
    /// own focus state for split panes (`None` keeps the real window focus).
    #[allow(clippy::too_many_arguments)]
    fn draw_pane<T: EventListener>(
        &mut self,
        mut terminal: MutexGuard<'_, Term<T>>,
        message_buffer: &MessageBuffer,
        config: &UiConfig,
        search_state: &mut SearchState,
        pane_state: &mut NebulaPaneState,
        view: SizeInfo,
        force_focus: Option<bool>,
        clear_first: bool,
    ) {
        // Override focus for split panes so the unfocused side shows a hollow
        // cursor; in single-pane mode keep the real window focus state.
        if let Some(focused) = force_focus {
            terminal.is_focused = focused;
        }

        // Tell the renderer the full window height so pane viewports flip
        // correctly into OpenGL's bottom-left origin — matters for top/bottom
        // splits, where panes occupy different vertical bands of the window.
        self.renderer.set_window_height(self.size_info.height());

        // Collect renderable content before the terminal is dropped.
        let custom_background = self.nebula_background;
        let mut content = RenderableContent::new(config, self, &terminal, search_state, &view);
        let mut grid_cells = Vec::new();
        let mut grid_pad_bg = None;
        for cell in &mut content {
            if grid_pad_bg.is_none() && cell.bg_alpha > 0.0 {
                grid_pad_bg = Some(cell.bg);
            }
            grid_cells.push(cell);
        }
        let selection_range = content.selection_range();
        let foreground_color = content.color(NamedColor::Foreground as usize);
        let background_color =
            custom_background.unwrap_or_else(|| content.color(NamedColor::Background as usize));
        let display_offset = content.display_offset();
        let cursor = content.cursor();

        let cursor_point = terminal.grid().cursor.point;
        // Anchors for OSC 1337 inline images (absolute-line bookkeeping).
        let grid_scrolled_out = terminal.grid().scrolled_out();
        let image_anchor = grid_scrolled_out + terminal.grid().history_size();
        // Ghost text is suppressed on the alt screen (vim/less/etc.).
        let alt_screen = terminal.mode().contains(TermMode::ALT_SCREEN);
        let total_lines = terminal.grid().total_lines();
        let metrics = self.glyph_cache.font_metrics();
        let size_info = view;

        let vi_mode = terminal.mode().contains(TermMode::VI);
        let vi_cursor_point = if vi_mode { Some(terminal.vi_mode_cursor.point) } else { None };
        #[cfg(windows)]
        let line_override = if alt_screen || vi_mode || search_state.regex().is_some() {
            None
        } else {
            Self::nebula_input_from_raw_grid(&terminal, cursor_point)
        };
        #[cfg(windows)]
        let row_preview = if alt_screen || vi_mode || search_state.regex().is_some() {
            None
        } else {
            Some(Self::nebula_raw_grid_row_preview(&terminal, cursor_point))
        };

        // Add damage from the terminal.
        match terminal.damage() {
            TermDamage::Full => self.damage_tracker.frame().mark_fully_damaged(),
            TermDamage::Partial(damaged_lines) => {
                for damage in damaged_lines {
                    self.damage_tracker.frame().damage_line(damage);
                }
            },
        }
        terminal.reset_damage();

        // Drop terminal as early as possible to free lock.
        drop(terminal);

        // Invalidate highlighted hints if grid has changed.
        self.validate_hint_highlights(display_offset);

        // OSC 1337 inline images: prune rows that scrolled out of history for
        // good, then collect the ones visible in this pane's viewport for the
        // single full-window draw pass in `present_frame`.
        if !pane_state.inline_images.is_empty() {
            let cell_h = view.cell_height();
            pane_state.inline_images.retain(|img| {
                let rows = (img.height / cell_h).ceil().max(1.0) as usize;
                img.abs_line + rows >= grid_scrolled_out
            });
            let top_abs = (image_anchor - display_offset) as f32;
            for img in &pane_state.inline_images {
                let y = view.padding_y() + (img.abs_line as f32 - top_abs) * cell_h;
                // Cull images entirely outside this pane's band.
                if y + img.height <= view.padding_y() - cell_h
                    || y >= view.padding_y() + view.height()
                {
                    continue;
                }
                self.nebula_frame_images.push((
                    img.id,
                    img.rgba.clone(),
                    (img.px_w, img.px_h),
                    (view.padding_x(), y, img.width, img.height),
                ));
            }
        }

        // Refresh the inline ghost-text suggestion. On Windows the input is read
        // off the grid (screen truth, never desyncs); elsewhere the tracked
        // `line_buf` is used. Only on the primary screen, never during vi/search
        // overlays.
        if alt_screen || vi_mode || search_state.regex().is_some() {
            pane_state.suggestion.clear();
            pane_state.suggestion_key.clear();
        } else {
            #[cfg(windows)]
            {
                // No prompt arrow before the cursor (or a mid-line edit) means we
                // cannot trust a hint here — clear it rather than guess.
                if !pane_state.line_buf.is_empty()
                    || line_override.as_ref().is_some_and(|s| !s.is_empty())
                {
                    nebula_debug_log(format!(
                        "grid_input cwd={:?} line_buf={:?} raw={:?} cursor=line:{} col:{} row={:?}",
                        pane_state.cwd,
                        pane_state.line_buf,
                        line_override,
                        cursor_point.line.0,
                        cursor_point.column.0,
                        row_preview
                    ));
                }
                match line_override {
                    Some(line) => {
                        pane_state.screen_line = line.clone();
                        self.nebula_update_suggestion(pane_state, Some(line));
                    },
                    None => {
                        pane_state.screen_line.clear();
                        pane_state.suggestion.clear();
                        pane_state.suggestion_key.clear();
                    },
                }
            }
            #[cfg(not(windows))]
            self.nebula_update_suggestion(pane_state, None);
        }

        // Add damage from nebula's UI elements overlapping terminal.

        // Nebula always redraws and presents the full window: the chrome
        // (clock, ambient glow, gradient border) is painted every frame, and
        // partial damage would leave terminal content (prompt, scrollback)
        // stale after the window is occluded or sent to the background.
        let _ = (self.visual_bell.intensity(), self.hint_state.active(), search_state.regex());
        self.damage_tracker.frame().mark_fully_damaged();
        self.damage_tracker.next_frame().mark_fully_damaged();

        let vi_cursor_viewport_point =
            vi_cursor_point.and_then(|cursor| term::point_to_viewport(display_offset, cursor));
        self.damage_tracker.damage_vi_cursor(vi_cursor_viewport_point);
        self.damage_tracker.damage_selection(selection_range, display_offset);

        // Make sure this window's OpenGL context is active. The caller is
        // expected to have already activated it; calling again is cheap and
        // keeps `draw_pane` safe to invoke standalone.
        self.make_current();

        // Only the first pane of a frame clears the whole window; subsequent
        // panes paint on top of the shared, already-cleared backdrop.
        if clear_first {
            self.renderer.clear(background_color, self.nebula_window_opacity);
            self.draw_background_image();
        }

        // 分屏渲染时每个 pane 都有独立的 viewport/projection；否则右侧内容会沿用上一帧
        // 或左侧 pane 的坐标系，最终叠到左边而不是显示在右边。
        self.renderer.resize(&size_info);

        let mut lines = RenderLines::new();

        // Optimize loop hint comparator.
        let has_highlighted_hint =
            self.highlighted_hint.is_some() || self.vi_highlighted_hint.is_some();

        // Draw grid.
        let mut powerline_icons = Vec::new();
        {
            let _sampler = self.meter.sampler();

            // Ensure macOS hasn't reset our viewport.
            #[cfg(target_os = "macos")]
            self.renderer.set_viewport(&size_info);

            let glyph_cache = &mut self.glyph_cache;
            let highlighted_hint = &self.highlighted_hint;
            let vi_highlighted_hint = &self.vi_highlighted_hint;
            let damage_tracker = &mut self.damage_tracker;

            let cells = grid_cells.into_iter().map(|mut cell| {
                match cell.character {
                    NEBULA_FOLDER_ICON_MARKER => {
                        powerline_icons.push(NebulaPowerlineIcon {
                            kind: NebulaPowerlineIconKind::Folder,
                            point: cell.point,
                        });
                        cell.character = ' ';
                    },
                    NEBULA_GIT_BRANCH_ICON_MARKER => {
                        powerline_icons.push(NebulaPowerlineIcon {
                            kind: NebulaPowerlineIconKind::GitBranch,
                            point: cell.point,
                        });
                        cell.character = ' ';
                    },
                    _ => (),
                }

                // Underline hints hovered by mouse or vi mode cursor.
                if has_highlighted_hint {
                    let point = term::viewport_to_point(display_offset, cell.point);
                    let hyperlink = cell.extra.as_ref().and_then(|extra| extra.hyperlink.as_ref());

                    let should_highlight = |hint: &Option<HintMatch>| {
                        hint.as_ref().is_some_and(|hint| hint.should_highlight(point, hyperlink))
                    };
                    if should_highlight(highlighted_hint) || should_highlight(vi_highlighted_hint) {
                        damage_tracker.frame().damage_point(cell.point);
                        cell.flags.insert(Flags::UNDERLINE);
                    }
                }

                // Update underline/strikeout.
                lines.update(&cell);

                cell
            });
            self.renderer.draw_cells(&size_info, glyph_cache, cells);
        }

        let mut rects = lines.rects(&metrics, &size_info);

        if alt_screen {
            if let Some(pad_bg) = grid_pad_bg {
                let x = size_info.padding_x();
                let w = size_info.width() - size_info.padding_x() - size_info.padding_right();
                let top_h = size_info.padding_y();
                let bottom_y = size_info.padding_y() + size_info.screen_lines() as f32 * size_info.cell_height();
                let bottom_h = (size_info.height() - bottom_y).max(0.0);
                if top_h > 0.0 {
                    rects.push(RenderRect::new(x, 0.0, w, top_h, pad_bg, 1.0));
                }
                if bottom_h > 0.0 {
                    rects.push(RenderRect::new(x, bottom_y, w, bottom_h, pad_bg, 1.0));
                }
            }
        }

        if let Some(vi_cursor_point) = vi_cursor_point {
            // Indicate vi mode by showing the cursor's position in the top right corner.
            let line = (-vi_cursor_point.line.0 + size_info.bottommost_line().0) as usize;
            let obstructed_column = Some(vi_cursor_point)
                .filter(|point| point.line == -(display_offset as i32))
                .map(|point| point.column);
            self.draw_line_indicator(config, total_lines, obstructed_column, line);
        } else if search_state.regex().is_some() {
            // Show current display offset in vi-less search to indicate match position.
            self.draw_line_indicator(config, total_lines, None, display_offset);
        };

        // Draw cursor.
        rects.extend(cursor.rects(&size_info, config.cursor.thickness()));

        // Push visual bell after url/underline/strikeout rects.
        let visual_bell_intensity = self.visual_bell.intensity();
        if visual_bell_intensity != 0. {
            let visual_bell_rect = RenderRect::new(
                0.,
                0.,
                size_info.width(),
                size_info.height(),
                config.bell.color,
                visual_bell_intensity as f32,
            );
            rects.push(visual_bell_rect);
        }

        // Handle IME positioning and search bar rendering.
        let ime_position = match search_state.regex() {
            Some(regex) => {
                let search_label = match search_state.direction() {
                    Direction::Right => FORWARD_SEARCH_LABEL,
                    Direction::Left => BACKWARD_SEARCH_LABEL,
                };

                let search_text = Self::format_search(regex, search_label, size_info.columns());

                // Render the search bar.
                self.draw_search(config, &search_text);

                // Draw search bar cursor.
                let line = size_info.screen_lines();
                let column = Column(search_text.chars().count() - 1);

                // Add cursor to search bar if IME is not active.
                if self.ime.preedit().is_none() {
                    let fg = config.colors.footer_bar_foreground();
                    let shape = CursorShape::Underline;
                    let cursor_width = NonZeroU32::new(1).unwrap();
                    let cursor =
                        RenderableCursor::new(Point::new(line, column), shape, fg, cursor_width);
                    rects.extend(cursor.rects(&size_info, config.cursor.thickness()));
                }

                Some(Point::new(line, column))
            },
            None => {
                let num_lines = size_info.screen_lines();
                match vi_cursor_viewport_point {
                    None => term::point_to_viewport(display_offset, cursor_point)
                        .filter(|point| point.line < num_lines),
                    point => point,
                }
            },
        };

        // Handle IME.
        if self.ime.is_enabled() {
            if let Some(point) = ime_position {
                let (fg, bg) = if search_state.regex().is_some() {
                    (config.colors.footer_bar_foreground(), config.colors.footer_bar_background())
                } else {
                    (foreground_color, background_color)
                };

                self.draw_ime_preview(point, fg, bg, &mut rects, config);
            }
        }

        if let Some(message) = message_buffer.message() {
            let search_offset = usize::from(search_state.regex().is_some());
            let text = message.text(&size_info);

            // Create a new rectangle for the background.
            let start_line = size_info.screen_lines() + search_offset;
            let y = size_info.cell_height().mul_add(start_line as f32, size_info.padding_y());

            let bg = match message.ty() {
                MessageType::Error => config.colors.normal.red,
                MessageType::Warning => config.colors.normal.yellow,
            };

            let x = 0;
            let width = size_info.width() as i32;
            let height = (size_info.height() - y) as i32;
            let message_bar_rect =
                RenderRect::new(x as f32, y, width as f32, height as f32, bg, 1.);

            // Push message_bar in the end, so it'll be above all other content.
            rects.push(message_bar_rect);

            // Always damage message bar, since it could have messages of the same size in it.
            self.damage_tracker.frame().add_viewport_rect(&size_info, x, y as i32, width, height);

            // Draw rectangles.
            self.renderer.draw_rects(&size_info, &metrics, rects);

            // Relay messages to the user.
            let glyph_cache = &mut self.glyph_cache;
            let fg = config.colors.primary.background;
            for (i, message_text) in text.iter().enumerate() {
                let point = Point::new(start_line + i, Column(0));
                self.renderer.draw_string(
                    point,
                    fg,
                    bg,
                    message_text.chars(),
                    &size_info,
                    glyph_cache,
                );
            }
        } else {
            // Draw rectangles.
            self.renderer.draw_rects(&size_info, &metrics, rects);
        }

        self.draw_powerline_icons(&powerline_icons, size_info);
        // `draw_powerline_icons` uses the full-window UI renderer and restores
        // a full-window viewport; bind the pane projection again before drawing
        // the inline ghost suggestion.
        self.renderer.resize(&size_info);

        // Draw inline ghost-text autosuggestion directly after the cursor,
        // once everything else for the cell row is on screen. The color is
        // the theme's faintest ink (not a fixed gray), so on light themes it
        // stays clearly weaker than the near-black real input instead of
        // colliding with it.
        if !pane_state.suggestion.is_empty() && self.ime.preedit().is_none() {
            if let Some(point) = term::point_to_viewport(display_offset, cursor_point)
                .filter(|p| p.line < size_info.screen_lines() && p.column.0 < size_info.columns())
            {
                let avail = size_info.columns() - point.column.0;
                let ghost: String = pane_state.suggestion.chars().take(avail).collect();
                let ghost_fg = self.nebula_theme.skin().ink_faint;
                let glyph_cache = &mut self.glyph_cache;
                self.renderer.draw_string(
                    point,
                    ghost_fg,
                    background_color,
                    ghost.chars(),
                    &size_info,
                    glyph_cache,
                );
            }
        }

        self.draw_render_timer(config);

        // Draw hyperlink uri preview.
        if has_highlighted_hint {
            let cursor_point = vi_cursor_point.or(Some(cursor_point));
            self.draw_hyperlink_preview(config, cursor_point, display_offset);
        }

        // Overlay scrollbar on the right edge while scrolled into history.
        self.draw_scrollbar(&size_info, display_offset, total_lines);
    }

    /// Draw the screen for a single, full-window terminal.
    ///
    /// A reference to the Term whose state is being drawn must be provided.
    /// This call may block if vsync is enabled.
    pub fn draw<T: EventListener>(
        &mut self,
        terminal: MutexGuard<'_, Term<T>>,
        scheduler: &mut Scheduler,
        message_buffer: &MessageBuffer,
        config: &UiConfig,
        search_state: &mut SearchState,
        pane_state: &mut NebulaPaneState,
    ) {
        let view = self.size_info;
        self.make_current();
        self.reload_nebula_settings_if_changed(config);
        // `None` focus → keep the terminal's real window-focus state.
        self.draw_pane(
            terminal,
            message_buffer,
            config,
            search_state,
            pane_state,
            view,
            None,
            true,
        );
        self.present_frame(scheduler);
    }

    /// Begin a multi-pane frame: bind the GL context and refresh themed
    /// settings before the per-pane draws.
    pub fn begin_pane_frame(&mut self, config: &UiConfig) {
        self.reload_nebula_settings_if_changed(config);
        self.make_current();
    }

    /// Draw one pane of a multi-pane layout into `view`. `clear_first` clears
    /// the whole window before the first pane; later panes paint on top.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_pane_view<T: EventListener>(
        &mut self,
        terminal: MutexGuard<'_, Term<T>>,
        message_buffer: &MessageBuffer,
        config: &UiConfig,
        search_state: &mut SearchState,
        pane_state: &mut NebulaPaneState,
        view: SizeInfo,
        focused: bool,
        clear_first: bool,
    ) {
        self.draw_pane(
            terminal,
            message_buffer,
            config,
            search_state,
            pane_state,
            view,
            Some(focused),
            clear_first,
        );
    }

    /// Overlay split chrome over the drawn panes: dim every unfocused pane and
    /// paint divider hairlines. Rectangles are screen-space `(x, y, w, h)` with
    /// a top-left origin. Focus reads as a brightness difference 
    /// `unfocused-split-opacity`) rather than an outline.
    pub fn draw_split_overlays(
        &mut self,
        dim_rects: &[(f32, f32, f32, f32)],
        divider_rects: &[(f32, f32, f32, f32)],
    ) {
        let palette = self.nebula_theme.palette();
        let veil = Rgba::new(0, 0, 0, 0).with_alpha(NEBULA_UNFOCUSED_SPLIT_DIM);
        let line_color = palette.edge_l.with_alpha(0.35);

        let mut quads: Vec<UiQuad> = Vec::with_capacity(dim_rects.len() + divider_rects.len() + 1);
        for &(x, y, w, h) in dim_rects {
            if w > 0.0 && h > 0.0 {
                quads.push(UiQuad::solid(x, y, w, h, 0.0, veil));
            }
        }
        for &(x, y, w, h) in divider_rects {
            if w > 0.0 && h > 0.0 {
                quads.push(UiQuad::solid(x, y, w, h, 0.0, line_color));
            }
        }

        // Freshly split pane slides in: a bg-coloured cover anchored at the
        // pane's far edge shrinks away over ~160ms (ease-out), so the new pane
        // wipes in from the divider instead of popping. Timestamp-derived, no
        // per-frame allocation (same discipline as the quick-terminal slide).
        if let Some((x, y, w, h, direction, start)) = self.nebula_split_reveal {
            const DUR: f32 = 0.16;
            let t = (start.elapsed().as_secs_f32() / DUR).min(1.0);
            if t >= 1.0 {
                self.nebula_split_reveal = None;
            } else {
                let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
                let bg = self
                    .nebula_background
                    .unwrap_or(Rgb::new(15, 17, 26));
                let cover = Rgba::new(bg.r, bg.g, bg.b, 255);
                let (cx, cy, cw, chh) = match direction {
                    SplitDirection::LeftRight => (x + w * e, y, w * (1.0 - e), h),
                    SplitDirection::TopBottom => (x, y + h * e, w, h * (1.0 - e)),
                };
                if cw > 0.5 && chh > 0.5 {
                    quads.push(UiQuad::solid(cx, cy, cw, chh, 0.0, cover));
                }
                self.window.request_redraw();
            }
        }

        self.renderer.draw_ui(&self.size_info, &quads);
    }

    /// Finish a multi-pane frame: draw window chrome and present.
    pub fn finish_pane_frame(&mut self, scheduler: &mut Scheduler) {
        self.present_frame(scheduler);
    }

    /// Paint the divider between two split panes and dim the unfocused one.
    /// (Removed: superseded by `draw_split_overlays` + the layout tree in
    /// `window_context/split.rs`.)
    #[cfg(any())]
    fn _removed_split_helpers() {}


    /// Overlay scrollbar on the right edge of a pane, shown only while scrolled
    /// up into the scrollback (auto-hides at the bottom).
    /// overlay-style `scrollbar`: a thin, semi-transparent thumb floating over
    /// the grid's right edge, sized to the visible fraction of total content.
    fn draw_scrollbar(&mut self, view: &SizeInfo, display_offset: usize, total_lines: usize) {
        let Some(geo) = self.scrollbar_geometry(view, display_offset, total_lines) else { return };
        let (thumb_x, thumb_y, thumb_w, thumb_h) = geo;

        // Skinned so it reads as chrome on both light and dark themes; a bit
        // more opaque while grabbed so the drag has visible feedback.
        let alpha = if self.nebula_scrollbar_drag.is_some() { 0.62 } else { 0.40 };
        let thumb_color = self.nebula_theme.skin().scrollbar_thumb.with_alpha(alpha);
        let quad = UiQuad::solid(thumb_x, thumb_y, thumb_w, thumb_h, thumb_w * 0.5, thumb_color);
        self.renderer.draw_ui(&self.size_info, &[quad]);
    }

    /// Scrollbar thumb rect `(x, y, w, h)` for a pane `view` — the single
    /// source of truth shared by rendering and input hit-testing. `None` while
    /// the bar is hidden (at the bottom, or no history).
    fn scrollbar_geometry(
        &self,
        view: &SizeInfo,
        display_offset: usize,
        total_lines: usize,
    ) -> Option<(f32, f32, f32, f32)> {
        let screen_lines = view.screen_lines();
        // Nothing to show when sitting at the bottom or when there's no history.
        if display_offset == 0 || total_lines <= screen_lines {
            return None;
        }

        let scale = self.window.scale_factor as f32;
        let total = total_lines as f32;
        let track_top = view.padding_y();
        let track_h = screen_lines as f32 * view.cell_height();
        if track_h <= 1.0 {
            return None;
        }

        // Thumb height = visible fraction of total content, with a sane minimum.
        let min_thumb = (24.0 * scale).min(track_h);
        let thumb_h = (track_h * (screen_lines as f32 / total)).clamp(min_thumb, track_h);

        // Lines of history above the current viewport top (0 = top, history = bottom).
        let history = total_lines - screen_lines;
        let above = (history - display_offset) as f32;
        let max_y = (track_h - thumb_h).max(0.0);
        let thumb_y = track_top + (track_h * (above / total)).clamp(0.0, max_y);

        // Float over the grid's right edge (overlay style, like macOS scrollbars).
        let thumb_w = (4.0 * scale).max(2.0);
        let grid_right = view.padding_x() + view.columns() as f32 * view.cell_width();
        let thumb_x = grid_right - thumb_w;

        Some((thumb_x, thumb_y, thumb_w, thumb_h))
    }

    /// Hit-test a press against the scrollbar. The 4px thumb gets a widened
    /// grab zone; a hit returns the pointer's y-offset inside the thumb so the
    /// drag doesn't jump. A press on the track (above/below the thumb) recenters
    /// the thumb there (`grab = thumb_h / 2`).
    pub fn scrollbar_grab(
        &self,
        view: &SizeInfo,
        display_offset: usize,
        total_lines: usize,
        x: f32,
        y: f32,
    ) -> Option<f32> {
        let (thumb_x, thumb_y, thumb_w, thumb_h) =
            self.scrollbar_geometry(view, display_offset, total_lines)?;
        let scale = self.window.scale_factor as f32;
        let slop = 8.0 * scale;
        // Horizontal band around the thumb column.
        if x < thumb_x - slop || x > thumb_x + thumb_w + slop {
            return None;
        }
        // Vertical: inside the track at all?
        let track_top = view.padding_y();
        let track_h = view.screen_lines() as f32 * view.cell_height();
        if y < track_top || y > track_top + track_h {
            return None;
        }
        if y >= thumb_y && y <= thumb_y + thumb_h {
            Some(y - thumb_y) // grab inside the thumb
        } else {
            Some(thumb_h / 2.0) // track press: jump so the thumb centers on it
        }
    }

    /// Map a dragged pointer `y` back to a scrollback `display_offset`,
    /// inverting the thumb-position math (`grab` = offset captured at press).
    pub fn scrollbar_target_offset(
        &self,
        view: &SizeInfo,
        total_lines: usize,
        y: f32,
        grab: f32,
    ) -> usize {
        let screen_lines = view.screen_lines();
        let history = total_lines.saturating_sub(screen_lines);
        if history == 0 {
            return 0;
        }
        let track_top = view.padding_y();
        let track_h = (screen_lines as f32 * view.cell_height()).max(1.0);
        let above = ((y - grab - track_top) / track_h * total_lines as f32).round();
        let above = above.clamp(0.0, history as f32) as usize;
        history - above
    }

    /// Centered confirmation modal for destructive actions (busy-process close,
    /// multi-line paste). A dim veil + a rounded box with the question and the
    /// `Enter 确认 · Esc 取消` hint; the keyboard router owns dismissal.
    fn draw_confirm_modal(&mut self) {
        let Some(confirm) = self.nebula_confirm.clone() else {
            self.nebula_confirm_buttons = None;
            return;
        };
        let size = self.size_info;
        let scale = self.window.scale_factor as f32;
        let s = |v: f32| v * scale;
        let cell_w = size.cell_width();
        let cell_h = size.cell_height();

        // Same tokens as the settings shell (design discipline: one flat
        // surface, hairline stroke, semantic color only on the primary
        // action). Danger red for destructive closes, theme accent for paste.
        // All from the theme skin, so light themes get a pale card + dark ink.
        let sk = self.nebula_theme.skin();
        let accent = Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255);
        let txt = sk.ink;
        let dim = sk.ink_dim;

        let (title, body, primary_label, danger) = match &confirm {
            NebulaConfirm::ClosePane { process, .. } => (
                "关闭此分栏？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                "关闭 Enter",
                true,
            ),
            NebulaConfirm::CloseTab { process, .. } => (
                "关闭此标签页？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                "关闭 Enter",
                true,
            ),
            NebulaConfirm::CloseWindow { process } => (
                "关闭整个窗口？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                "关闭 Enter",
                true,
            ),
            NebulaConfirm::Paste { lines, .. } => (
                format!("粘贴 {lines} 行文本？"),
                "多行粘贴会被 shell 逐行执行，请确认来源可信。".to_owned(),
                "粘贴 Enter",
                false,
            ),
        };
        let cancel_label = "取消 Esc";

        let text_w = |t: &str| -> f32 {
            let cols: usize = t.chars().map(|c| c.width().unwrap_or(1)).sum();
            cols as f32 * cell_w
        };

        // Buttons: right-aligned row, primary rightmost (Windows order).
        let btn_h = s(34.0);
        let btn_pad = s(18.0);
        let primary_w = text_w(primary_label) + 2.0 * btn_pad;
        let cancel_w = text_w(cancel_label) + 2.0 * btn_pad;

        // Card sized to content, clamped into the window.
        let pad = s(26.0);
        let content_w = text_w(&title)
            .max(text_w(&body))
            .max(primary_w + s(12.0) + cancel_w);
        let box_w = (content_w + 2.0 * pad).max(s(380.0)).min(size.width() - s(32.0));
        let box_h = pad + cell_h + s(10.0) + cell_h + s(24.0) + btn_h + pad * 0.75;
        let bx = ((size.width() - box_w) * 0.5).max(s(16.0));
        let by = ((size.height() - box_h) * 0.5).max(s(16.0));

        // Veil dims the whole window so the modal reads as, well, modal.
        let veil =
            UiQuad::solid(0.0, 0.0, size.width(), size.height(), 0.0, Rgba::new(0, 0, 0, 118));
        // Hairline edge + flat themed card (no glow, no gradient).
        let edge = UiQuad::solid(
            bx - s(1.0),
            by - s(1.0),
            box_w + s(2.0),
            box_h + s(2.0),
            s(13.0),
            sk.hairline,
        );
        let card = UiQuad::solid(bx, by, box_w, box_h, s(12.0), sk.panel);

        // Button geometry (kept for the mouse hit-test).
        let btn_y = by + box_h - pad * 0.75 - btn_h;
        let primary_x = bx + box_w - pad + s(2.0) - primary_w;
        let cancel_x = primary_x - s(12.0) - cancel_w;
        let primary_rect = (primary_x, btn_y, primary_w, btn_h);
        let cancel_rect = (cancel_x, btn_y, cancel_w, btn_h);
        self.nebula_confirm_buttons = Some((primary_rect, cancel_rect));

        let primary_fill = if danger { sk.danger } else { accent };
        let mut quads = vec![veil, edge, card];
        // Cancel: quiet ghost button (hairline + faint fill).
        quads.push(UiQuad::solid(
            cancel_x - s(1.0),
            btn_y - s(1.0),
            cancel_w + s(2.0),
            btn_h + s(2.0),
            s(9.0),
            sk.hairline,
        ));
        quads.push(UiQuad::solid(cancel_x, btn_y, cancel_w, btn_h, s(8.0), sk.panel));
        quads.push(UiQuad::solid(cancel_x, btn_y, cancel_w, btn_h, s(8.0), sk.surface));
        // Primary: the single loud element on the card.
        quads.push(UiQuad::solid(primary_x, btn_y, primary_w, btn_h, s(8.0), primary_fill));
        self.renderer.draw_ui(&size, &quads);

        // Text: free-pixel chrome text (no opaque cell backgrounds), left
        // aligned like a native Windows dialog.
        let glyph_cache = &mut self.glyph_cache;
        let tx = bx + pad;
        self.renderer.draw_chrome_text(&size, tx, by + pad, txt, &title, glyph_cache);
        self.renderer.draw_chrome_text(
            &size,
            tx,
            by + pad + cell_h + s(10.0),
            dim,
            &body,
            glyph_cache,
        );
        let btn_text_y = btn_y + (btn_h - cell_h) / 2.0;
        self.renderer.draw_chrome_text(
            &size,
            cancel_x + btn_pad,
            btn_text_y,
            txt,
            cancel_label,
            glyph_cache,
        );
        // Danger keeps pale ink (red is dark in both modes); the accent
        // button contrast flips with the theme.
        let on_primary = if danger { Rgb::new(255, 244, 246) } else { sk.ink_on_accent };
        self.renderer.draw_chrome_text(
            &size,
            primary_x + btn_pad,
            btn_text_y,
            on_primary,
            primary_label,
            glyph_cache,
        );
    }

    /// Draw the window chrome and present the accumulated frame.
    /// Overlay a transient, fading "cols × rows" HUD centered in the window,
    /// shown briefly after a resize (a resize overlay HUD). Keeps requesting
    /// redraws until it fades out, then clears itself.
    fn draw_resize_hud(&mut self) {
        let Some((cols, rows, start)) = self.nebula_resize_hud else { return };
        const HUD_DUR: f32 = 0.9;
        let elapsed = start.elapsed().as_secs_f32();
        if elapsed >= HUD_DUR {
            self.nebula_resize_hud = None;
            return;
        }
        let fade = (1.0 - elapsed / HUD_DUR).clamp(0.0, 1.0);

        let size = self.size_info;
        let scale = self.window.scale_factor as f32;
        let cw = size.cell_width();
        let ch = size.cell_height();

        let text = format!("{cols} × {rows}");
        let text_cols = text.chars().count();

        // Centered translucent rounded box (fades out), skinned by the theme
        // so it reads as chrome on light panels too.
        let sk = self.nebula_theme.skin();
        let hud_rgb = Rgb::new(sk.panel.r, sk.panel.g, sk.panel.b);
        let pad = 12.0 * scale;
        let box_w = text_cols as f32 * cw + 2.0 * pad;
        let box_h = ch + 2.0 * pad;
        let box_x = ((size.width() - box_w) * 0.5).max(0.0);
        let box_y = ((size.height() - box_h) * 0.5).max(0.0);
        let bg = Rgba::new(hud_rgb.r, hud_rgb.g, hud_rgb.b, 0).with_alpha(0.85 * fade);
        let quad = UiQuad::solid(box_x, box_y, box_w, box_h, 8.0 * scale, bg);
        self.renderer.draw_ui(&size, &[quad]);

        // Centered text. `draw_string` paints opaque cell backgrounds, so give it
        // the box's core color to blend into the rounded quad.
        let col = size.columns().saturating_sub(text_cols) / 2;
        let line = size.screen_lines() / 2;
        let glyph_cache = &mut self.glyph_cache;
        self.renderer.draw_string(
            Point::new(line, Column(col)),
            sk.ink_strong,
            hud_rgb,
            text.chars(),
            &size,
            glyph_cache,
        );

        // Keep the frame loop alive so the HUD animates out.
        self.window.request_redraw();
    }

    fn present_frame(&mut self, scheduler: &mut Scheduler) {
        // OSC 1337 inline images collected by the pane passes: draw above the
        // cells, below the chrome/modals.
        if !self.nebula_frame_images.is_empty() {
            let size = self.size_info;
            let images = std::mem::take(&mut self.nebula_frame_images);
            for (id, rgba, px, rect) in &images {
                self.renderer.draw_inline_image(&size, *id, rgba, *px, *rect);
            }
        }

        // Draw Nebula window chrome (title bar and tab sidebar).
        chrome::draw_chrome(self);

        // AI brand logos staged by the chrome pass: drawn only now, after the
        // last chrome text flush, because draw_inline_image's viewport/blend
        // round-trip poisons any glyph batch that follows it.
        if !self.nebula_chrome_logo_draws.is_empty() {
            let size = self.size_info;
            let logos = std::mem::take(&mut self.nebula_chrome_logo_draws);
            for (id, rgba, px, rect) in &logos {
                self.renderer.draw_inline_image(&size, *id, rgba, *px, *rect);
            }
        }

        // Transient resize HUD painted on top of the chrome.
        self.draw_resize_hud();
        self.draw_confirm_modal();

        // Notify winit that we're about to present.
        self.window.pre_present_notify();

        // Highlight damage for debugging.
        if self.damage_tracker.debug {
            let metrics = self.glyph_cache.font_metrics();
            let damage = self.damage_tracker.shape_frame_damage(self.size_info.into());
            let mut rects = Vec::with_capacity(damage.len());
            self.highlight_damage(&mut rects);
            self.renderer.draw_rects(&self.size_info, &metrics, rects);
        }

        // Clearing debug highlights from the previous frame requires full redraw.
        self.swap_buffers();

        if matches!(self.raw_window_handle, RawWindowHandle::Xcb(_) | RawWindowHandle::Xlib(_)) {
            // On X11 `swap_buffers` does not block for vsync. However the next OpenGl command
            // will block to synchronize (this is `glClear` in Nebula), which causes a
            // permanent one frame delay.
            self.renderer.finish();
        }

        // XXX: Request the new frame after swapping buffers, so the
        // time to finish OpenGL operations is accounted for in the timeout.
        if !matches!(self.raw_window_handle, RawWindowHandle::Wayland(_)) {
            self.request_frame(scheduler);
        }

        self.damage_tracker.swap_damage();
    }

    /// Geometry that input and hint hit-testing should use: the focused pane's
    /// half-width view when a split is active, otherwise the full window.
    #[inline]
    pub fn pane_view(&self) -> SizeInfo {
        self.nebula_pane_view.unwrap_or(self.size_info)
    }

    /// Update to a new configuration.
    pub fn update_config(&mut self, config: &UiConfig) {
        self.damage_tracker.debug = config.debug.highlight_damage;
        self.visual_bell.update_config(&config.bell);
        // Refresh the base scheme, then re-apply the active theme's restyle.
        self.nebula_default_colors = List::from(&config.colors);
        let defaults = self.nebula_default_colors;
        self.nebula_theme.apply_term_colors(&mut self.colors, &defaults);
    }

    /// Update the mouse/vi mode cursor hint highlighting.
    ///
    /// This will return whether the highlighted hints changed.
    pub fn update_highlighted_hints<T>(
        &mut self,
        term: &Term<T>,
        config: &UiConfig,
        mouse: &Mouse,
        modifiers: ModifiersState,
    ) -> bool {
        // Update vi mode cursor hint.
        let vi_highlighted_hint = if term.mode().contains(TermMode::VI) {
            let mods = ModifiersState::all();
            let point = term.vi_mode_cursor.point;
            hint::highlighted_at(term, config, point, mods)
        } else {
            None
        };
        let mut dirty = vi_highlighted_hint != self.vi_highlighted_hint;
        self.vi_highlighted_hint = vi_highlighted_hint;
        self.vi_highlighted_hint_age = 0;

        // Force full redraw if the vi mode highlight was cleared.
        if dirty {
            self.damage_tracker.frame().mark_fully_damaged();
        }

        // Abort if mouse highlighting conditions are not met.
        if !self.window.mouse_visible()
            || !mouse.inside_text_area
            || !term.selection.as_ref().is_none_or(Selection::is_empty)
        {
            if self.highlighted_hint.take().is_some() {
                self.damage_tracker.frame().mark_fully_damaged();
                dirty = true;
            }
            return dirty;
        }

        // Find highlighted hint at mouse position.
        let point = mouse.point(&self.pane_view(), term.grid().display_offset());
        let highlighted_hint = hint::highlighted_at(term, config, point, modifiers);

        // Update cursor shape.
        if highlighted_hint.is_some() {
            // If mouse changed the line, we should update the hyperlink preview, since the
            // highlighted hint could be disrupted by the old preview.
            dirty = self.hint_mouse_point.is_some_and(|p| p.line != point.line);
            self.hint_mouse_point = Some(point);
            self.window.set_mouse_cursor(CursorIcon::Pointer);
        } else if self.highlighted_hint.is_some() {
            self.hint_mouse_point = None;
            if term.mode().intersects(TermMode::MOUSE_MODE) && !term.mode().contains(TermMode::VI) {
                self.window.set_mouse_cursor(CursorIcon::Default);
            } else {
                // Nebula: normal arrow over the terminal area (no I-beam).
                self.window.set_mouse_cursor(CursorIcon::Default);
            }
        }

        let mouse_highlight_dirty = self.highlighted_hint != highlighted_hint;
        dirty |= mouse_highlight_dirty;
        self.highlighted_hint = highlighted_hint;
        self.highlighted_hint_age = 0;

        // Force full redraw if the mouse cursor highlight was changed.
        if mouse_highlight_dirty {
            self.damage_tracker.frame().mark_fully_damaged();
        }

        dirty
    }

    #[inline(never)]
    /// Append a typed character to the prompt-line buffer that backs the
    /// history hint.
    pub fn nebula_input_char(state: &mut NebulaPaneState, c: char) {
        state.line_buf.push(c);
        state.touched = true;
        state.suggestion.clear();
        state.suggestion_key.clear();
        nebula_debug_log(format!("input_char c={c:?} line_buf={:?}", state.line_buf));
    }

    /// Drop the last character (Backspace) from the prompt-line buffer.
    pub fn nebula_input_backspace(state: &mut NebulaPaneState) {
        state.line_buf.pop();
        state.touched = true;
        state.suggestion.clear();
        state.suggestion_key.clear();
        nebula_debug_log(format!("input_backspace line_buf={:?}", state.line_buf));
    }

    /// Drop the previous whitespace-delimited token, matching the common
    /// Ctrl+W/Ctrl+Backspace shell behavior closely enough for prompt hints.
    pub fn nebula_input_delete_word(state: &mut NebulaPaneState) {
        state.touched = true;
        while state.line_buf.ends_with(char::is_whitespace) {
            state.line_buf.pop();
        }
        while state.line_buf.chars().last().is_some_and(|c| !c.is_whitespace()) {
            state.line_buf.pop();
        }
        state.suggestion.clear();
        state.suggestion_key.clear();
        nebula_debug_log(format!("input_delete_word line_buf={:?}", state.line_buf));
    }

    /// Merge pasted literal text into the prompt-line buffer. Multi-line or
    /// escape-bearing paste can execute commands or move the cursor, so we
    /// invalidate instead of guessing; Nushell avoids this class of bug by
    /// owning the editor buffer directly via Reedline.
    pub fn nebula_input_text(state: &mut NebulaPaneState, text: &str) {
        if text.contains(['\r', '\n']) || text.chars().any(|c| c.is_control() && c != '\t') {
            nebula_debug_log(format!("input_text_clear text={text:?}"));
            Self::nebula_clear_line(state);
            return;
        }

        state.line_buf.push_str(text);
        state.touched = true;
        state.suggestion.clear();
        state.suggestion_key.clear();
        nebula_debug_log(format!("input_text text={text:?} line_buf={:?}", state.line_buf));
    }

    /// Commit the current line to history (on Enter) and reset the buffer.
    ///
    /// `screen_line` (the input read off the grid, i.e. what the shell's own
    /// editor really contained) wins over the keystroke-reconstructed
    /// `line_buf`: the latter desyncs on cursor motion / completion / history
    /// recall and used to commit spliced garbage like "laudeclaude", which the
    /// hint would then resurface as a command the user never typed.
    pub fn nebula_commit_line(&mut self, state: &mut NebulaPaneState) {
        // On Windows the grid read is the only source that sees tab
        // completions; when it failed (no prompt arrow — cmd/ssh/REPL — or a
        // mid-line edit) the keystroke buffer likely holds spliced garbage,
        // and recording that would resurface it forever as a bogus ghost hint
        // (truncated CJK paths were the visible symptom). Better no history
        // entry than a corrupted one.
        #[cfg(windows)]
        let line = state.screen_line.trim().to_owned();
        #[cfg(not(windows))]
        let line = if state.screen_line.trim().is_empty() {
            state.line_buf.trim()
        } else {
            state.screen_line.trim()
        }
        .to_owned();
        nebula_debug_log(format!(
            "input_commit cwd={:?} line={line:?} line_buf={:?} screen_line={:?}",
            state.cwd, state.line_buf, state.screen_line
        ));
        self.nebula_history.record(&line, &state.cwd);
        // Kept for CommandStart (OSC 133;C): by the time it arrives from the
        // PTY these buffers are already cleared, so the program identity for
        // the tab icon has to be captured here. Fall back to the keystroke
        // buffer so the icon still resolves when the grid read failed — its
        // first token is good enough for program identity.
        state.last_committed =
            if line.is_empty() { state.line_buf.trim().to_owned() } else { line };
        Self::nebula_clear_line(state);
    }

    /// Reset the prompt-line buffer (Enter, Ctrl-C, or any non-text key).
    pub fn nebula_clear_line(state: &mut NebulaPaneState) {
        if !state.line_buf.is_empty() || !state.suggestion.is_empty() {
            nebula_debug_log(format!(
                "input_clear line_buf={:?} suggestion={:?}",
                state.line_buf, state.suggestion
            ));
        }
        state.line_buf.clear();
        state.screen_line.clear();
        state.suggestion.clear();
        state.suggestion_key.clear();
    }

    /// Read the real, echoed input off the cursor's grid row on Windows.
    ///
    /// The PowerShell profile renders the active input line as `❯ <input>`, so
    /// the user's current input is exactly the run of cells from just past the
    /// last prompt arrow up to the cursor column. Reading the grid (the screen
    /// truth PSReadLine itself produced) sidesteps the keystroke-reconstructed
    /// `line_buf`, which desyncs the instant the cursor moves, Tab-completes or
    /// recalls history — the "hint flickers in and out" bug.
    ///
    /// Returns `None` (suppressing the hint) when no prompt arrow precedes the
    /// cursor, or when non-space cells sit after the cursor on the same row —
    /// i.e. a mid-line edit, where a trailing ghost would be misplaced (fish and
    /// PSReadLine suppress the hint there too).
    #[cfg(windows)]
    fn nebula_raw_grid_row_preview<T: EventListener>(terminal: &Term<T>, cursor: Point) -> String {
        let grid = terminal.grid();
        let columns = grid.columns();
        let mut text = String::with_capacity(columns);
        let mut arrow_cols = Vec::new();

        for col in 0..columns {
            let cell: &Cell = &grid[cursor.line][Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            if cell.c == NEBULA_PROMPT_ARROW {
                arrow_cols.push(col);
            }
            text.push(cell.c);
        }

        while text.ends_with(' ') {
            text.pop();
        }

        format!(
            "line={} col={} arrows={arrow_cols:?} text={text:?}",
            cursor.line.0, cursor.column.0
        )
    }

    #[cfg(windows)]
    pub(crate) fn nebula_input_from_raw_grid<T: EventListener>(
        terminal: &Term<T>,
        cursor: Point,
    ) -> Option<String> {
        let grid = terminal.grid();
        let columns = grid.columns();
        let cursor_col = cursor.column.0.min(columns);

        // A cursor mid-wrap means the input continues on the row below (a
        // mid-line edit); a hint anchored here would be wrong.
        if grid[cursor.line][Column(columns - 1)].flags.contains(Flags::WRAPLINE) {
            return None;
        }

        // Suppress when anything non-space follows the cursor on this row.
        for col in cursor_col..columns {
            let cell: &Cell = &grid[cursor.line][Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            if !cell.c.is_whitespace() {
                return None;
            }
        }

        // A long input soft-wraps — CJK paths hit the margin fast at two
        // columns per char — leaving the prompt arrow rows ABOVE the cursor.
        // Walk up while the row above ends in WRAPLINE to the logical line's
        // first row (bounded, so a pathological grid can't spin every frame).
        let topmost = grid.topmost_line().0;
        let mut first_row = cursor.line.0;
        while first_row > topmost
            && cursor.line.0 - first_row < 64
            && grid[Line(first_row - 1)][Column(columns - 1)].flags.contains(Flags::WRAPLINE)
        {
            first_row -= 1;
        }

        // Rebuild the logical line from its first row down to the cursor,
        // remembering the last prompt arrow before it. This deliberately reads
        // the raw terminal grid, not RenderableCell: renderables omit
        // default-background spaces, and that was collapsing `cd D:\te` into
        // `cdD:\te`, making directory-history hints impossible to parse.
        let mut text = String::with_capacity(columns);
        let mut arrow_pos = None;
        for row in first_row..=cursor.line.0 {
            let row_end = if row == cursor.line.0 { cursor_col } else { columns };
            for col in 0..row_end {
                let cell: &Cell = &grid[Line(row)][Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                if cell.c == NEBULA_PROMPT_ARROW {
                    arrow_pos = Some(text.len());
                }
                text.push(cell.c);
            }
        }

        let input = &text[arrow_pos? + NEBULA_PROMPT_ARROW.len_utf8()..];

        // Drop the single space that follows the arrow.
        Some(input.strip_prefix(' ').unwrap_or(input).to_owned())
    }

    /// Recompute the inline ghost-text suggestion. `line_override` carries the
    /// grid-read input on Windows (the authoritative screen truth); when `None`
    /// the keystroke-tracked `line_buf` is used (other platforms). A whole
    /// previous command sharing the prefix wins (fish-style history hint);
    /// otherwise the final token gets path completion against the shell-reported
    /// cwd. Cached on `cwd\0buffer` so disk is only touched when the line
    /// changes — not every frame.
    fn nebula_update_suggestion(
        &mut self,
        state: &mut NebulaPaneState,
        line_override: Option<String>,
    ) {
        let line = line_override.unwrap_or_else(|| state.line_buf.clone());
        if !self.nebula_ghost_enabled || line.is_empty() {
            state.suggestion.clear();
            state.suggestion_key.clear();
            nebula_debug_log(format!(
                "suggest_skip enabled={} cwd={:?} line={:?} line_buf={:?}",
                self.nebula_ghost_enabled, state.cwd, line, state.line_buf
            ));
            return;
        }

        let key = format!("{}\u{0}{line}", state.cwd);
        if key == state.suggestion_key {
            return;
        }
        state.suggestion_key = key;
        state.suggestion.clear();

        nebula_debug_log(format!(
            "suggest_begin cwd={:?} line={:?} line_buf={:?}",
            state.cwd, line, state.line_buf
        ));

        // History first: newest command that extends the whole line (indexed
        // prefix lookup — scales with matches, not history size).
        if let Some(rem) = self.nebula_history.hint(&line) {
            state.suggestion = Self::nebula_clamp_ghost(rem);
            nebula_debug_log(format!("suggest_result kind=history rem={:?}", state.suggestion));
            return;
        }

        // Directory-history hint for cd-like commands. This normalizes Windows
        // slash style/case, so `cd D:\te` can pick a previous
        // `cd D:/temp_build/wuwei` before generic filesystem completion falls
        // back to alphabetic candidates like `D:\Telegram\`.
        if let Some(rem) = self.nebula_history.dir_hint(&line) {
            state.suggestion = Self::nebula_clamp_ghost(&rem);
            nebula_debug_log(format!("suggest_result kind=dir rem={:?}", state.suggestion));
            return;
        }

        // First token with no path separators is a command position. Reuse the
        // process PATH inherited by the shell so typing `ca` can ghost `rgo`
        // even before that command has appeared in Nebula/Nushell history.
        if nebula_is_command_position(&line) {
            if let Ok(commands) = self.nebula_commands.lock() {
                if let Some(rem) = nebula_command_hint(commands.as_slice(), &line) {
                    state.suggestion = Self::nebula_clamp_ghost(rem);
                    nebula_debug_log(format!("suggest_result kind=command rem={:?}", state.suggestion));
                    return;
                }
            }
        }

        // Otherwise complete the final path token. Absolute tokens (drive,
        // root or `~`) resolve without a cwd; relative ones need the tracked
        // cwd, so bail if it is unknown.
        let token = line.rsplit([' ', '\t']).next().unwrap_or("");
        if token.is_empty() {
            return;
        }
        let absolute =
            token.starts_with(['/', '\\', '~']) || token.as_bytes().get(1) == Some(&b':'); // Windows drive, e.g. `D:`
        if !absolute && state.cwd.is_empty() {
            return;
        }

        // Case-insensitive so `mor` completes `MoRealm` on Windows; prefer
        // directories for the common directory-changing commands.
        let options = CompletionOptions { case_sensitive: false, ..CompletionOptions::default() };
        let want_dir = nebula_path_wants_directory(&line);
        let span = Span::new(0, token.len());
        let cwd = state.cwd.clone();
        let cwd_slot = [cwd.as_str()];
        let cwds: &[&str] = if cwd.is_empty() { &[] } else { &cwd_slot };
        let matches = complete_item(want_dir, span, token, cwds, &options, false, None);
        let candidates: Vec<_> = matches
            .iter()
            .take(6)
            .map(|s| s.display_override.as_deref().unwrap_or(&s.path).to_owned())
            .collect();
        let remainder = matches.into_iter().find_map(|s| {
            let path = s.display_override.as_deref().unwrap_or(&s.path);
            // The match was case-insensitive, so the suggestion is the slice of
            // `path` past what the user typed. Compare the head ignoring ASCII
            // case (so `mor` matches `MoRealm`) and guard the byte split against
            // multibyte boundaries.
            if path.len() <= token.len() || !path.is_char_boundary(token.len()) {
                return None;
            }
            let (head, rem) = path.split_at(token.len());
            if !head.eq_ignore_ascii_case(token) {
                return None;
            }
            // Stop at the first separator so a single deep match doesn't drill
            // the whole tree into the ghost; suggest one segment.
            Some(match rem.find(['/', '\\']) {
                Some(i) => rem[..=i].to_owned(),
                None => rem.to_owned(),
            })
        });
        if let Some(rem) = remainder {
            state.suggestion = Self::nebula_clamp_ghost(&rem);
            nebula_debug_log(format!(
                "suggest_result kind=path token={:?} candidates={:?} rem={:?}",
                token, candidates, state.suggestion
            ));
        } else {
            nebula_debug_log(format!(
                "suggest_result kind=none token={:?} candidates={:?}",
                token, candidates
            ));
        }
    }

    /// Cap ghost length so a long path/command can't spill into the chrome.
    fn nebula_clamp_ghost(rem: &str) -> String {
        rem.chars().take(NEBULA_GHOST_MAX).collect()
    }

    fn draw_powerline_icons(&mut self, icons: &[NebulaPowerlineIcon], view: SizeInfo) {
        if icons.is_empty() {
            return;
        }

        let size = view;
        let cell_w = size.cell_width();
        let cell_h = size.cell_height();
        let pad_x = size.padding_x();
        let pad_y = size.padding_y();
        let palette = self.nebula_theme.palette();
        let folder_color = Rgb::new(palette.edge_r.r, palette.edge_r.g, palette.edge_r.b);
        let branch_color = Rgb::new(palette.edge_l.r, palette.edge_l.g, palette.edge_l.b);

        let mut quads = Vec::with_capacity(icons.len() * 8);
        for icon in icons {
            if icon.point.line >= size.screen_lines() {
                continue;
            }

            let x = pad_x + icon.point.column.0 as f32 * cell_w;
            let y = pad_y + icon.point.line as f32 * cell_h;

            match icon.kind {
                NebulaPowerlineIconKind::Folder => {
                    Self::push_folder_icon(&mut quads, x, y, cell_w, cell_h, folder_color);
                },
                NebulaPowerlineIconKind::GitBranch => {
                    Self::push_git_branch_icon(&mut quads, x, y, cell_w, cell_h, branch_color);
                },
            }
        }

        self.renderer.draw_ui(&self.size_info, &quads);
    }

    fn push_folder_icon(
        quads: &mut Vec<UiQuad>,
        cell_x: f32,
        cell_y: f32,
        cell_w: f32,
        cell_h: f32,
        color: Rgb,
    ) {
        let icon_w = (cell_w * 1.18).clamp(8.0, cell_h * 0.72);
        let icon_h = (icon_w * 0.74).clamp(6.0, cell_h * 0.58);
        let x = cell_x + (cell_w - icon_w) * 0.5;
        let y = cell_y + (cell_h - icon_h) * 0.5 + cell_h * 0.02;
        let radius = (icon_h * 0.16).max(1.4);

        let glow = Self::rgba_from_rgb(color, 46);
        let main = Self::rgba_towards_white(color, 0.16, 236);
        let light = Self::rgba_towards_white(color, 0.34, 246);
        let shade = Self::rgba_towards_black(color, 0.16, 230);
        let shine = Rgba::new(255, 255, 255, 82);

        quads.push(UiQuad::glow(
            x - icon_w * 0.20,
            y - icon_h * 0.22,
            icon_w * 1.40,
            icon_h * 1.45,
            glow,
        ));
        quads.push(UiQuad::gradient(
            x + icon_w * 0.03,
            y + icon_h * 0.08,
            icon_w * 0.48,
            icon_h * 0.30,
            radius * 0.70,
            light,
            main,
            Gradient::Axis([0.9, 0.35]),
        ));
        quads.push(UiQuad::gradient(
            x,
            y + icon_h * 0.25,
            icon_w,
            icon_h * 0.68,
            radius,
            main,
            shade,
            Gradient::Axis([0.85, 0.45]),
        ));
        quads.push(UiQuad::solid(
            x + icon_w * 0.14,
            y + icon_h * 0.48,
            icon_w * 0.72,
            (cell_h * 0.035).max(1.0),
            0.8,
            shine,
        ));
    }

    fn push_git_branch_icon(
        quads: &mut Vec<UiQuad>,
        cell_x: f32,
        cell_y: f32,
        cell_w: f32,
        cell_h: f32,
        color: Rgb,
    ) {
        let icon = (cell_w * 1.12).clamp(7.0, cell_h * 0.68);
        let x = cell_x + (cell_w - icon) * 0.5;
        let y = cell_y + (cell_h - icon) * 0.5;
        let stroke = (icon * 0.13).clamp(1.15, 2.4);
        let node = (icon * 0.27).clamp(2.8, 5.0);
        let radius = node * 0.5;

        let main = Self::rgba_towards_white(color, 0.12, 240);
        let glow = Self::rgba_from_rgb(color, 42);
        let line = Self::rgba_towards_black(color, 0.08, 218);

        let trunk_x = x + icon * 0.34;
        let top_y = y + icon * 0.23;
        let mid_y = y + icon * 0.43;
        let bottom_y = y + icon * 0.78;
        let branch_x = x + icon * 0.70;

        quads.push(UiQuad::glow(x - icon * 0.20, y - icon * 0.18, icon * 1.42, icon * 1.40, glow));
        Self::push_icon_line(quads, trunk_x, top_y, trunk_x, bottom_y, stroke, line);
        Self::push_icon_line(quads, trunk_x, mid_y, branch_x, top_y, stroke, line);

        for (cx, cy) in [(trunk_x, top_y), (branch_x, top_y), (trunk_x, bottom_y)] {
            quads.push(UiQuad::solid(cx - node * 0.5, cy - node * 0.5, node, node, radius, main));
        }
    }

    fn push_icon_line(
        quads: &mut Vec<UiQuad>,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
        color: Rgba,
    ) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= f32::EPSILON {
            return;
        }

        let nx = -dy / len * width * 0.5;
        let ny = dx / len * width * 0.5;
        quads.push(UiQuad::poly(
            [[x0 + nx, y0 + ny], [x0 - nx, y0 - ny], [x1 + nx, y1 + ny], [x1 - nx, y1 - ny]],
            color,
            color,
            Gradient::None,
        ));
    }

    fn rgba_from_rgb(color: Rgb, alpha: u8) -> Rgba {
        Rgba::new(color.r, color.g, color.b, alpha)
    }

    fn rgba_towards_white(color: Rgb, amount: f32, alpha: u8) -> Rgba {
        Self::rgba_mix(color, Rgb::new(255, 255, 255), amount, alpha)
    }

    fn rgba_towards_black(color: Rgb, amount: f32, alpha: u8) -> Rgba {
        Self::rgba_mix(color, Rgb::new(0, 0, 0), amount, alpha)
    }

    fn rgba_mix(from: Rgb, to: Rgb, amount: f32, alpha: u8) -> Rgba {
        let t = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgba::new(mix(from.r, to.r), mix(from.g, to.g), mix(from.b, to.b), alpha)
    }

    #[inline(never)]
    fn draw_ime_preview(
        &mut self,
        point: Point<usize>,
        fg: Rgb,
        bg: Rgb,
        rects: &mut Vec<RenderRect>,
        config: &UiConfig,
    ) {
        let preedit = match self.ime.preedit() {
            Some(preedit) => preedit,
            None => {
                // In case we don't have preedit, just set the popup point.
                self.window.update_ime_position(point, &self.size_info);
                return;
            },
        };

        let num_cols = self.size_info.columns();

        // Get the visible preedit.
        let visible_text: String = match (preedit.cursor_byte_offset, preedit.cursor_end_offset) {
            (Some(byte_offset), Some(end_offset)) if end_offset.0 > num_cols => StrShortener::new(
                &preedit.text[byte_offset.0..],
                num_cols,
                ShortenDirection::Right,
                Some(SHORTENER),
            ),
            _ => {
                StrShortener::new(&preedit.text, num_cols, ShortenDirection::Left, Some(SHORTENER))
            },
        }
        .collect();

        let visible_len = visible_text.chars().count();

        let end = cmp::min(point.column.0 + visible_len, num_cols);
        let start = end.saturating_sub(visible_len);

        let start = Point::new(point.line, Column(start));
        let end = Point::new(point.line, Column(end - 1));

        let glyph_cache = &mut self.glyph_cache;
        let metrics = glyph_cache.font_metrics();

        self.renderer.draw_string(
            start,
            fg,
            bg,
            visible_text.chars(),
            &self.size_info,
            glyph_cache,
        );

        // Damage preedit inside the terminal viewport.
        if point.line < self.size_info.screen_lines() {
            let damage = LineDamageBounds::new(start.line, 0, num_cols);
            self.damage_tracker.frame().damage_line(damage);
            self.damage_tracker.next_frame().damage_line(damage);
        }

        // Add underline for preedit text.
        let underline = RenderLine { start, end, color: fg };
        rects.extend(underline.rects(Flags::UNDERLINE, &metrics, &self.size_info));

        let ime_popup_point = match preedit.cursor_end_offset {
            Some(cursor_end_offset) => {
                // Use hollow block when multiple characters are changed at once.
                let (shape, width) = if let Some(width) =
                    NonZeroU32::new((cursor_end_offset.0 - cursor_end_offset.1) as u32)
                {
                    (CursorShape::HollowBlock, width)
                } else {
                    (CursorShape::Beam, NonZeroU32::new(1).unwrap())
                };

                let cursor_column = Column(
                    (end.column.0 as isize - cursor_end_offset.0 as isize + 1).max(0) as usize,
                );
                let cursor_point = Point::new(point.line, cursor_column);
                let cursor = RenderableCursor::new(cursor_point, shape, fg, width);
                rects.extend(cursor.rects(&self.size_info, config.cursor.thickness()));
                cursor_point
            },
            _ => end,
        };

        self.window.update_ime_position(ime_popup_point, &self.size_info);
    }

    /// Format search regex to account for the cursor and fullwidth characters.
    fn format_search(search_regex: &str, search_label: &str, max_width: usize) -> String {
        let label_len = search_label.len();

        // Skip `search_regex` formatting if only label is visible.
        if label_len > max_width {
            return search_label[..max_width].to_owned();
        }

        // The search string consists of `search_label` + `search_regex` + `cursor`.
        let mut bar_text = String::from(search_label);
        bar_text.extend(StrShortener::new(
            search_regex,
            max_width.wrapping_sub(label_len + 1),
            ShortenDirection::Left,
            Some(SHORTENER),
        ));

        // Add place for cursor.
        bar_text.push(' ');

        bar_text
    }

    /// Draw preview for the currently highlighted `Hyperlink`.
    #[inline(never)]
    /// Draw a compact "open this link" tooltip next to the mouse pointer.
    ///
    /// Replaces the old full-width `file:///…` bar pinned to the bottom row.
    /// Shows the destination (with the noisy `file://` scheme stripped) and
    /// the gesture that opens it — `Ctrl+点击 打开` — anchored one row below
    /// the hovered cell so it reads where the user is actually looking.
    fn draw_hyperlink_preview(
        &mut self,
        config: &UiConfig,
        _cursor_point: Option<Point>,
        display_offset: usize,
    ) {
        let num_cols = self.size_info.columns();

        // The destination under the mouse (first highlighted hint with a URI).
        let Some(uri) = self
            .highlighted_hint
            .iter()
            .chain(&self.vi_highlighted_hint)
            .find_map(|hint| hint.hyperlink().map(|h| h.uri().to_owned()))
        else {
            return;
        };
        // Anchor at the hovered cell; without one there is nothing to point at.
        let Some(anchor) =
            self.hint_mouse_point.and_then(|p| term::point_to_viewport(display_offset, p))
        else {
            return;
        };

        // Strip the `file://` scheme (and its leading slash before a Windows
        // drive) so a local path reads as a path, not a URL — the user asked
        // specifically not to surface the raw `file:` form.
        let target = strip_file_scheme(&uri);
        const HINT: &str = " · Ctrl+点击打开";

        // Fit the target (keep the tail — the filename matters most) into the
        // room left after the hint, computed in DISPLAY columns (CJK = 2) so
        // the tooltip can never overrun the row and clip on the right.
        let width = |s: &str| -> usize { s.chars().map(|c| c.width().unwrap_or(0)).sum() };
        let hint_w = width(HINT);
        let target_budget = num_cols.saturating_sub(hint_w + 1);
        let target = fit_tail(&target, target_budget);
        let label = format!("{target}{HINT}");
        let label_w = width(&label);

        // Position: one row below the pointer, or above when on the last row.
        let line = if anchor.line + 1 < self.size_info.screen_lines() {
            anchor.line + 1
        } else {
            anchor.line.saturating_sub(1)
        };
        // Start near the pointer column, shifted left so the whole label stays
        // on screen (label_w is guaranteed <= num_cols by fit_tail above).
        let column = anchor.column.0.min(num_cols.saturating_sub(label_w));
        let point = Point::new(line, Column(column));

        // Damage the tooltip row this frame and next (it moves with the mouse).
        let damage = LineDamageBounds::new(point.line, point.column.0, num_cols);
        self.damage_tracker.frame().damage_line(damage);
        self.damage_tracker.next_frame().damage_line(damage);

        let fg = config.colors.footer_bar_foreground();
        let bg = config.colors.footer_bar_background();
        self.renderer.draw_string(
            point,
            fg,
            bg,
            label.chars(),
            &self.size_info,
            &mut self.glyph_cache,
        );
    }

    /// Draw current search regex.
    #[inline(never)]
    fn draw_search(&mut self, config: &UiConfig, text: &str) {
        // Assure text length is at least num_cols.
        let num_cols = self.size_info.columns();
        let text = format!("{text:<num_cols$}");

        let point = Point::new(self.size_info.screen_lines(), Column(0));

        let fg = config.colors.footer_bar_foreground();
        let bg = config.colors.footer_bar_background();

        self.renderer.draw_string(
            point,
            fg,
            bg,
            text.chars(),
            &self.size_info,
            &mut self.glyph_cache,
        );
    }


    /// Draw render timer.
    #[inline(never)]
    fn draw_render_timer(&mut self, config: &UiConfig) {
        if !config.debug.render_timer {
            return;
        }

        let timing = format!("{:.3} usec", self.meter.average());
        let point = Point::new(self.size_info.screen_lines().saturating_sub(2), Column(0));
        let fg = config.colors.primary.background;
        let bg = config.colors.normal.red;

        // Damage render timer for current and next frame.
        let damage = LineDamageBounds::new(point.line, point.column.0, timing.len());
        self.damage_tracker.frame().damage_line(damage);
        self.damage_tracker.next_frame().damage_line(damage);

        let glyph_cache = &mut self.glyph_cache;
        self.renderer.draw_string(point, fg, bg, timing.chars(), &self.size_info, glyph_cache);
    }

    /// Draw an indicator for the position of a line in history.
    #[inline(never)]
    fn draw_line_indicator(
        &mut self,
        config: &UiConfig,
        total_lines: usize,
        obstructed_column: Option<Column>,
        line: usize,
    ) {
        let columns = self.size_info.columns();
        let text = format!("[{}/{}]", line, total_lines - 1);
        let column = Column(self.size_info.columns().saturating_sub(text.len()));
        let point = Point::new(0, column);

        // Damage the line indicator for current and next frame.
        let damage = LineDamageBounds::new(point.line, point.column.0, columns - 1);
        self.damage_tracker.frame().damage_line(damage);
        self.damage_tracker.next_frame().damage_line(damage);

        let colors = &config.colors;
        let fg = colors.line_indicator.foreground.unwrap_or(colors.primary.background);
        let bg = colors.line_indicator.background.unwrap_or(colors.primary.foreground);

        // Do not render anything if it would obscure the vi mode cursor.
        if obstructed_column.is_none_or(|obstructed_column| obstructed_column < column) {
            let glyph_cache = &mut self.glyph_cache;
            self.renderer.draw_string(point, fg, bg, text.chars(), &self.size_info, glyph_cache);
        }
    }

    /// Highlight damaged rects.
    ///
    /// This function is for debug purposes only.
    fn highlight_damage(&self, render_rects: &mut Vec<RenderRect>) {
        for damage_rect in &self.damage_tracker.shape_frame_damage(self.size_info.into()) {
            let x = damage_rect.x as f32;
            let height = damage_rect.height as f32;
            let width = damage_rect.width as f32;
            let y = damage_y_to_viewport_y(&self.size_info, damage_rect) as f32;
            let render_rect = RenderRect::new(x, y, width, height, DAMAGE_RECT_COLOR, 0.5);

            render_rects.push(render_rect);
        }
    }

    /// Check whether a hint highlight needs to be cleared.
    fn validate_hint_highlights(&mut self, display_offset: usize) {
        let frame = self.damage_tracker.frame();
        let hints = [
            (&mut self.highlighted_hint, &mut self.highlighted_hint_age, true),
            (&mut self.vi_highlighted_hint, &mut self.vi_highlighted_hint_age, false),
        ];

        let num_lines = self.size_info.screen_lines();
        for (hint, hint_age, reset_mouse) in hints {
            let (start, end) = match hint {
                Some(hint) => (*hint.bounds().start(), *hint.bounds().end()),
                None => continue,
            };

            // Ignore hints that were created this frame.
            *hint_age += 1;
            if *hint_age == 1 {
                continue;
            }

            // Convert hint bounds to viewport coordinates.
            let start = term::point_to_viewport(display_offset, start)
                .filter(|point| point.line < num_lines)
                .unwrap_or_default();
            let end = term::point_to_viewport(display_offset, end)
                .filter(|point| point.line < num_lines)
                .unwrap_or_else(|| Point::new(num_lines - 1, self.size_info.last_column()));

            // Clear invalidated hints.
            if frame.intersects(start, end) {
                if reset_mouse {
                    self.window.set_mouse_cursor(CursorIcon::Default);
                }
                frame.mark_fully_damaged();
                *hint = None;
            }
        }
    }

    /// Request a new frame for a window on Wayland.
    fn request_frame(&mut self, scheduler: &mut Scheduler) {
        // Mark that we've used a frame.
        self.window.has_frame = false;

        // Get the display vblank interval.
        let monitor_vblank_interval = 1_000_000.
            / self
                .window
                .current_monitor()
                .and_then(|monitor| monitor.refresh_rate_millihertz())
                .unwrap_or(60_000) as f64;

        // Now convert it to micro seconds.
        let monitor_vblank_interval =
            Duration::from_micros((1000. * monitor_vblank_interval) as u64);

        let swap_timeout = self.frame_timer.compute_timeout(monitor_vblank_interval);

        let window_id = self.window.id();
        let timer_id = TimerId::new(Topic::Frame, window_id);
        let event = Event::new(EventType::Frame, window_id);

        scheduler.schedule(event, swap_timeout, false, timer_id);
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // Switch OpenGL context before dropping, otherwise objects (like programs) from other
        // contexts might be deleted when dropping renderer.
        self.make_current();
        unsafe {
            ManuallyDrop::drop(&mut self.renderer);
            ManuallyDrop::drop(&mut self.context);
            ManuallyDrop::drop(&mut self.surface);
        }
    }
}

/// Input method state.
#[derive(Debug, Default)]
pub struct Ime {
    /// Whether the IME is enabled.
    enabled: bool,

    /// Current IME preedit.
    preedit: Option<Preedit>,
}

impl Ime {
    #[inline]
    pub fn set_enabled(&mut self, is_enabled: bool) {
        if is_enabled {
            self.enabled = is_enabled
        } else {
            // Clear state when disabling IME.
            *self = Default::default();
        }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[inline]
    pub fn set_preedit(&mut self, preedit: Option<Preedit>) {
        self.preedit = preedit;
    }

    #[inline]
    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    /// The preedit text.
    text: String,

    /// Byte offset for cursor start into the preedit text.
    ///
    /// `None` means that the cursor is invisible.
    cursor_byte_offset: Option<(usize, usize)>,

    /// The cursor offset from the end of the start of the preedit in char width.
    cursor_end_offset: Option<(usize, usize)>,
}

impl Preedit {
    pub fn new(text: String, cursor_byte_offset: Option<(usize, usize)>) -> Self {
        let cursor_end_offset = if let Some(byte_offset) = cursor_byte_offset {
            // Convert byte offset into char offset.
            let start_to_end_offset =
                text[byte_offset.0..].chars().fold(0, |acc, ch| acc + ch.width().unwrap_or(1));
            let end_to_end_offset =
                text[byte_offset.1..].chars().fold(0, |acc, ch| acc + ch.width().unwrap_or(1));

            Some((start_to_end_offset, end_to_end_offset))
        } else {
            None
        };

        Self { text, cursor_byte_offset, cursor_end_offset }
    }
}

/// Pending renderer updates.
///
/// All renderer updates are cached to be applied just before rendering, to avoid platform-specific
/// rendering issues.
#[derive(Debug, Default, Copy, Clone)]
pub struct RendererUpdate {
    /// Should resize the window.
    resize: bool,

    /// Clear font caches.
    clear_font_cache: bool,
}

/// The frame timer state.
pub struct FrameTimer {
    /// Base timestamp used to compute sync points.
    base: Instant,

    /// The last timestamp we synced to.
    last_synced_timestamp: Instant,

    /// The refresh rate we've used to compute sync timestamps.
    refresh_interval: Duration,
}

impl FrameTimer {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { base: now, last_synced_timestamp: now, refresh_interval: Duration::ZERO }
    }

    /// Compute the delay that we should use to achieve the target frame
    /// rate.
    pub fn compute_timeout(&mut self, refresh_interval: Duration) -> Duration {
        let now = Instant::now();

        // Handle refresh rate change.
        if self.refresh_interval != refresh_interval {
            self.base = now;
            self.last_synced_timestamp = now;
            self.refresh_interval = refresh_interval;
            return refresh_interval;
        }

        let next_frame = self.last_synced_timestamp + self.refresh_interval;

        if next_frame < now {
            // Redraw immediately if we haven't drawn in over `refresh_interval` microseconds.
            let elapsed_micros = (now - self.base).as_micros() as u64;
            let refresh_micros = self.refresh_interval.as_micros() as u64;
            self.last_synced_timestamp =
                now - Duration::from_micros(elapsed_micros % refresh_micros);
            Duration::ZERO
        } else {
            // Redraw on the next `refresh_interval` clock tick.
            self.last_synced_timestamp = next_frame;
            next_frame - now
        }
    }
}

/// Calculate the cell dimensions based on font metrics.
///
/// This will return a tuple of the cell width and height.
#[inline]
fn compute_cell_size(config: &UiConfig, metrics: &crossfont::Metrics) -> (f32, f32) {
    let offset_x = f64::from(config.font.offset.x);
    let offset_y = f64::from(config.font.offset.y);
    (
        (metrics.average_advance + offset_x).floor().max(1.) as f32,
        (metrics.line_height + offset_y).floor().max(1.) as f32,
    )
}

/// Calculate the size of the window given padding, terminal dimensions and cell size.
fn window_size(
    config: &UiConfig,
    dimensions: Dimensions,
    cell_width: f32,
    cell_height: f32,
    scale_factor: f32,
) -> PhysicalSize<u32> {
    let padding = config.window.padding(scale_factor);
    let chrome = chrome_reserve(scale_factor);

    let grid_width = cell_width * dimensions.columns.max(MIN_COLUMNS) as f32;
    let grid_height = cell_height * dimensions.lines.max(MIN_SCREEN_LINES) as f32;

    // Left absorbs the sidebar (expanded by default), right is the plain
    // content margin, matching the asymmetric grid the sidebar produces.
    let pad_left = padding.0 + content_pad_x(scale_factor) + sidebar_width(scale_factor, false);
    let pad_right = padding.0 + content_pad_x(scale_factor);
    let width = (grid_width + pad_left + pad_right).floor();
    let height = (padding.1 + chrome).mul_add(2., grid_height).floor();

    PhysicalSize::new(width as u32, height as u32)
}
