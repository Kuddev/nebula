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

use log::{debug, info, warn};
use parking_lot::MutexGuard;
use winit::dpi::PhysicalSize;
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::RawWindowHandle;
use winit::window::{CursorIcon, Theme as WinitTheme};

use crossfont::{Rasterize, Size as FontSize};
use unicode_width::UnicodeWidthChar;

use nebula_terminal::event::{EventListener, OnResize};
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
use crate::message_bar::{self, MessageBuffer, MessageType};
use crate::renderer::Rasterizer;
use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};
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

mod chrome;
pub mod command_palette;
mod context_menu;
pub mod design_tokens;
mod i18n;
pub mod markdown_view;
mod message_queue_entry;
pub mod sftp_panel;
pub mod side_panel;
mod icons;
mod size_info;
mod state;
mod surface_opacity;
mod terminal_color;
mod terminal_math;
mod widgets;

pub(crate) use chrome::chrome_settings_button_rect;
pub use chrome::{ChromeHit, TabDropAction, in_chrome_bar, resize_edge};
use chrome::{
    ChromeTabLayout, TabDrag, chrome_hit_with_tabs, chrome_tab_layout, contains_rect,
    truncate_tab_label,
};
pub use context_menu::{ContextMenuAction, ContextMenuHit, ContextMenuTarget};
pub use i18n::{LanguagePreference, UiLanguage};
pub use size_info::SizeInfo;
pub use state::{
    AcceptKey, NebulaConfirm, NebulaInlineImage, NebulaPaneState, NebulaShell, SplitDirection,
    SplitNav,
};

mod file_dialog;
pub(crate) mod keymap;
mod settings;
mod ssh_editor_render;
mod ssh_ui;
mod text_input;
mod theme;

use ssh_ui::SshDeleteUndo;
pub use ssh_ui::{
    SSH_DELETE_UNDO_DURATION, SshEditorField, SshEditorHit, SshEditorRects, SshHostEditor,
};
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
pub use settings::{NebulaSettingsSection, SettingsDropdown, SettingsHit, settings_hit};
pub(crate) use settings::SettingsOpacityTarget;

/// 按显示列宽贪心断行（确认框正文等 UI 段落用）：CJK 逐字可断，行首空
/// 格吞掉；零宽字符跟随前一个字。不做拉丁连词回退——正文以中文为主，
/// 偶发的英文单词被折断可接受。
fn wrap_display_cols(text: &str, max_cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cols = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            line.push(ch);
            continue;
        }
        if cols + w > max_cols && cols > 0 {
            lines.push(std::mem::take(&mut line));
            cols = 0;
            if ch == ' ' {
                continue;
            }
        }
        line.push(ch);
        cols += w;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

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

/// Top chrome reserve, in logical pixels at scale factor 1.0. Sized as: top
/// bar (8 margin + 40 bar) + card seam (8) + 8px of breathing room inside the
/// terminal card, so the first grid row doesn't touch the card's top edge.
pub const CHROME_BAR_LOGICAL: f32 = 64.0;

/// Shared chrome/control corner radius. Used for the small in-shell affordances
/// (window-control hover pills, tab pills, the "+" square) — kept modest so the
/// controls stay crisp.
pub(super) const UI_CORNER_RADIUS_LOGICAL: f32 = 8.0;

/// Outer radius of the connected chrome shell (the L-frame formed by the top
/// bar + left sidebar). Larger than the control radius so the whole window
/// chrome reads as one soft-cornered card while the affordances inside keep
/// their tighter [`UI_CORNER_RADIUS_LOGICAL`] curve.
pub(super) const UI_SHELL_RADIUS_LOGICAL: f32 = 14.0;

/// Gap between the terminal card and the window's right/bottom edges, in
/// logical pixels — the visible "seam" of shell color that makes the terminal
/// read as a rounded card floating on the shell backdrop. Top and left carry
/// no seam of their own: the card tucks up under the top bar and sidebar.
pub(super) const UI_CARD_SEAM_LOGICAL: f32 = 8.0;

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

/// Bottom grid reserve: card seam plus the same 8px inner breathing room used
/// above the first row. Unlike [`chrome_reserve`], there is no title bar below
/// the terminal, so mirroring the 64px top reserve creates a large dead band.
#[inline]
pub fn bottom_content_reserve(scale_factor: f32) -> f32 {
    ((UI_CARD_SEAM_LOGICAL + 8.0) * scale_factor).round()
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
    if collapsed { 0.0 } else { (SIDEBAR_W_LOGICAL * scale_factor).round() }
}

#[derive(Debug, Clone, Copy)]
struct UiAnim {
    spring: crate::motion::Spring,
}

impl UiAnim {
    fn new(value: f32) -> Self {
        Self { spring: crate::motion::Spring::new(value.clamp(0.0, 1.0)).with_response(0.14) }
    }

    fn value(self) -> f32 {
        self.spring.value().clamp(0.0, 1.0)
    }

    fn visible(self, target_open: bool) -> bool {
        target_open || self.value() > 0.004
    }

    fn animating_to(self, target: f32) -> bool {
        (self.value() - target.clamp(0.0, 1.0)).abs() > 0.004 || self.spring.is_active()
    }

    fn step(&mut self, frame: crate::motion::Frame, target: f32) {
        self.spring.set_target(target.clamp(0.0, 1.0), crate::motion::MotionPolicy::Full);
        self.spring.step(frame);
    }
}

#[derive(Debug, Clone)]
struct NebulaUiAnims {
    clock: crate::motion::MotionClock,
    frame: Option<crate::motion::Frame>,
    /// Continuous sidebar-spinner phase in turns (`0.0..1.0`). Advancing it
    /// from the shared monotonic frame delta avoids wall-clock jumps and needs
    /// only four bytes per window.
    spinner_phase: f32,
    left_sidebar: UiAnim,
    right_drawer: UiAnim,
    ssh_editor: UiAnim,
}

impl NebulaUiAnims {
    fn new() -> Self {
        Self {
            clock: crate::motion::MotionClock::default(),
            frame: None,
            spinner_phase: 0.0,
            left_sidebar: UiAnim::new(1.0),
            right_drawer: UiAnim::new(0.0),
            ssh_editor: UiAnim::new(0.0),
        }
    }

    fn step(&mut self, left_open: bool, right_open: bool, ssh_open: bool) {
        let frame = self.clock.tick();
        self.frame = Some(frame);
        self.left_sidebar.step(frame, if left_open { 1.0 } else { 0.0 });
        self.right_drawer.step(frame, if right_open { 1.0 } else { 0.0 });
        self.ssh_editor.step(frame, if ssh_open { 1.0 } else { 0.0 });
    }

    fn frame(&mut self) -> crate::motion::Frame {
        if let Some(frame) = self.frame {
            frame
        } else {
            let frame = self.clock.tick();
            self.frame = Some(frame);
            frame
        }
    }

    fn animating(&self, left_open: bool, right_open: bool) -> bool {
        self.left_sidebar.animating_to(if left_open { 1.0 } else { 0.0 })
            || self.right_drawer.animating_to(if right_open { 1.0 } else { 0.0 })
    }
}

#[derive(Debug, Clone, Copy)]
struct ResizeHud {
    columns: usize,
    rows: usize,
    opacity: crate::motion::Tween,
}

impl ResizeHud {
    fn new(columns: usize, rows: usize) -> Self {
        let mut opacity = crate::motion::Tween::new(1.0);
        opacity.animate_to(
            0.0,
            Duration::from_millis(900),
            crate::motion::Easing::Linear,
            crate::motion::MotionPolicy::Full,
        );
        Self { columns, rows, opacity }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SplitReveal {
    rect: (f32, f32, f32, f32),
    direction: SplitDirection,
    motion: crate::motion::Tween,
}

impl SplitReveal {
    pub fn new(rect: (f32, f32, f32, f32), direction: SplitDirection) -> Self {
        let mut motion = crate::motion::Tween::new(0.0);
        motion.animate_role(
            1.0,
            crate::motion::MotionRole::Enter,
            crate::motion::MotionPolicy::Full,
        );
        Self { rect, direction, motion }
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

/// Sidebar SSH HOSTS content: auto-saved destinations (most recent first) +
/// `~/.ssh/config` aliases (file order), deduped, pinned entries floated to
/// the top in pin order. One function so startup and settings hot-reload
/// build the exact same list.
fn merge_ssh_hosts(saved: &[String], pinned: &[String], hidden: &[String]) -> Vec<String> {
    let mut hosts: Vec<_> = saved.iter().filter(|host| !hidden.contains(host)).cloned().collect();
    for host in crate::ssh::ssh_config_hosts() {
        if !hidden.contains(&host) && !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    // Stable sort: pinned first in pin order, the rest keep saved→config order.
    hosts.sort_by_key(|h| pinned.iter().position(|p| p == h).unwrap_or(usize::MAX));
    hosts
}

/// Remove one destination while recording exactly enough list state for Undo.
/// Kept independent from rendering and Credential Manager so the destructive
/// state transition can be regression-tested without touching real secrets.
fn remove_ssh_host_from_lists(
    host: &str,
    saved: &mut Vec<String>,
    pinned: &mut Vec<String>,
    hidden: &mut Vec<String>,
) -> (Option<usize>, Option<usize>, bool) {
    let saved_index = saved.iter().position(|entry| entry == host);
    let pinned_index = pinned.iter().position(|entry| entry == host);
    let was_hidden = hidden.iter().any(|entry| entry == host);
    saved.retain(|entry| entry != host);
    pinned.retain(|entry| entry != host);
    if !was_hidden {
        hidden.push(host.to_owned());
    }
    (saved_index, pinned_index, was_hidden)
}

fn restore_ssh_host_to_lists(
    host: &str,
    saved_index: Option<usize>,
    pinned_index: Option<usize>,
    was_hidden: bool,
    saved: &mut Vec<String>,
    pinned: &mut Vec<String>,
    hidden: &mut Vec<String>,
) {
    saved.retain(|entry| entry != host);
    if let Some(index) = saved_index {
        saved.insert(index.min(saved.len()), host.to_owned());
    }
    pinned.retain(|entry| entry != host);
    if let Some(index) = pinned_index {
        pinned.insert(index.min(pinned.len()), host.to_owned());
    }
    if !was_hidden {
        hidden.retain(|entry| entry != host);
    }
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

/// Log replay commands can contain terminal query sequences captured from a
/// different process. Replying writes those answers into the shell's stdin,
/// where they become the next command after the replay process exits.
pub(crate) fn replays_untrusted_terminal_output(line: &str) -> bool {
    let words: Vec<String> = line
        .split_whitespace()
        .take(4)
        .map(|word| word.trim_matches(['"', '\'']).to_ascii_lowercase())
        .collect();
    matches!(
        words.as_slice(),
        [docker, logs, ..] if docker == "docker" && logs == "logs"
    ) || matches!(
        words.as_slice(),
        [docker, compose, logs, ..]
            if docker == "docker" && compose == "compose" && logs == "logs"
    ) || matches!(
        words.as_slice(),
        [podman, logs, ..] if podman == "podman" && logs == "logs"
    ) || matches!(
        words.as_slice(),
        [kubectl, logs, ..] if kubectl == "kubectl" && logs == "logs"
    ) || matches!(words.as_slice(), [journalctl, ..] if journalctl == "journalctl")
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
    /// Pi's block-built `pi` mark from pi.dev.
    Pi,
}

/// Official logo assets (Wikimedia SVG renders, 64 px, alpha-transparent).
const AI_LOGO_CLAUDE_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_claude.png");
const AI_LOGO_OPENAI_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_openai.png");
/// opencode's mark, rasterized from their `favicon.svg`. RGB carries luma
/// (frame=255, block=90), alpha the shape; tinted `ink × luma/255` at runtime.
const AI_LOGO_OPENCODE_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_opencode.png");
const AI_LOGO_PI_PNG: &[u8] = include_bytes!("../../../extra/logo/ai_pi.png");

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
        "pi" => Some(AiLogo::Pi),
        _ => None,
    }
}

/// Drop a `file://` / `file:///` scheme so a local link reads as a plain
/// path in the hover tooltip. On Windows `file:///D:/x` → `D:/x` (the slash
/// before the drive letter goes too); non-`file:` URIs pass through.
///
/// 2026-07-26 用户反馈：`ls --hyperlink` 会把非 ASCII 文件名 percent-encode
/// （一个汉字 → `%E6%98%9F` 九列），48 列预算被编码噪声吃光后再截尾，提示
/// "显示不全"。file 路径解码后再展示；其他 scheme 保持原样（URL 的编码
/// 属于其身份，不替它翻译）。
fn strip_file_scheme(uri: &str) -> String {
    match uri.strip_prefix("file:///").or_else(|| uri.strip_prefix("file://")) {
        // `file:///D:/x` yields `D:/x`; a UNC-ish `file://host/x` keeps `host/x`.
        Some(rest) => percent_decode_lossy(rest),
        None => uri.to_owned(),
    }
}

/// Decode `%XX` escapes as UTF-8 bytes for DISPLAY. Malformed escapes pass
/// through verbatim; if the decoded bytes are not valid UTF-8 the original
/// string is returned unchanged — a tooltip must never invent mojibake.
fn percent_decode_lossy(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| {
                let hi = (bytes[i + 1] as char).to_digit(16)?;
                let lo = (bytes[i + 2] as char).to_digit(16)?;
                Some((hi * 16 + lo) as u8)
            })
            .flatten();
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            },
            None => {
                out.push(bytes[i]);
                i += 1;
            },
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
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
        "claude" => "\u{f0ce5}", // md-star-four-points (Claude spark)
        "codex" => "\u{f02d8}",  // md-hexagon (OpenAI mark)
        "gemini" => "\u{f0ce6}", // md-star-four-points-outline
        "copilot" => "\u{f4b8}", // oct-copilot
        "cursor" | "cursor-agent" => "\u{f0ec3}", // md-cursor-default-outline
        "aider" | "goose" | "crush" | "ollama" => "\u{f06a9}", // md-robot
        "opencode" => "\u{f489}", // oct-terminal
        "pi" => "\u{f135}",      // fa-code
        "git" | "gh" | "lazygit" => "\u{f418}", // oct-git-branch
        "vim" | "nvim" | "vi" | "hx" | "nano" => "\u{e62b}", // custom-vim
        "ssh" | "mosh" => "\u{f489}", // oct-terminal (remote)
        "cargo" | "rustc" => "\u{e7a8}", // dev-rust
        "node" | "npm" | "pnpm" | "yarn" | "bun" | "deno" => "\u{e718}", // dev-nodejs
        "python" | "python3" | "pip" | "uv" => "\u{e73c}", // dev-python
        "docker" | "podman" => "\u{e7b0}", // dev-docker
        _ => "\u{f04b}",         // fa-play (generic busy)
    }
}

/// The UI font role (wezterm's `window_frame` pattern): the size chrome
/// typography rasterizes at and the cell chrome layout steps by. Anchored to
/// the config font at the window's DPI — never to the terminal zoom. Stage 3
/// exposes family/size as user config.
#[derive(Clone, Copy, Debug)]
struct NebulaUiFont {
    /// Role size in physical px (config size × DPI scale).
    px: f32,
    /// Cell the chrome layout steps by, from the role's REAL rasterized
    /// metrics (`compute_cell_size` over `GlyphCache::set_ui_font_size`).
    cell: (f32, f32),
}

pub(crate) fn nebula_debug_log(message: impl AsRef<str>) {
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
        let _ = writeln!(file, "[{ts} pid={}] {}", std::process::id(), message.as_ref());
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

/// Read one raw `key=value` from `nebula_settings.txt` (case-insensitive key).
/// The typed loader is `settings::nebula_settings_load`; this is for the few
/// callers (e.g. the default-shell id) that want the raw string verbatim.
pub(crate) fn nebula_settings_value(key: &str) -> Option<String> {
    let data = std::fs::read_to_string(nebula_data_dir().join("nebula_settings.txt")).ok()?;
    data.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_owned())
    })
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
        let mut seen: HashSet<String> = commands.iter().map(|c| c.to_ascii_lowercase()).collect();
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

    // 完整命令已经可执行时必须停止补全；否则 `claude` 会跳过自身，继续
    // 命中 `claude-agent-acp` 这类更长的 PATH 邻居。
    #[cfg(windows)]
    let exact = commands.iter().any(|command| command.eq_ignore_ascii_case(prefix));
    #[cfg(not(windows))]
    let exact = commands.iter().any(|command| command == prefix);
    if exact {
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

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct DisplayUpdate {
    pub dirty: bool,

    dimensions: Option<PhysicalSize<u32>>,
    cursor_dirty: bool,
    font: Option<Font>,
    terminal_colors_dirty: bool,
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

    pub fn terminal_colors_dirty(&self) -> bool {
        self.terminal_colors_dirty
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

    fn set_terminal_colors_dirty(&mut self) {
        self.terminal_colors_dirty = true;
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
    nebula_resize_hud: Option<ResizeHud>,
    /// Skip the first resize (window creation) so no HUD flashes at startup.
    nebula_resize_hud_armed: bool,

    /// Indexed, persistent command history used to hint a whole previous
    /// command from its prefix.
    nebula_history: crate::nebula_history::NebulaHistory,
    /// Process-wide frecency model fed only by successful shell cwd reports.
    directory_history: crate::directory_history::DirectoryHistory,
    /// Executable commands for first-token completion: PATH executables plus, on
    /// Windows, the shell's cmdlets/functions/aliases. Filled on a background
    /// thread so the PowerShell probe never blocks startup.
    nebula_commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Per-displayed-tab animated draw-x, eased toward the laid-out / drag
    /// target each frame so tab reorder "make way" slides instead of snapping.
    nebula_tab_anim: Vec<crate::motion::Spring>,
    /// Active scrollbar drag: the pointer's y-offset inside the thumb captured
    /// at press time, so the thumb tracks the pointer without jumping.
    pub nebula_scrollbar_drag: Option<f32>,
    /// Slide-in reveal for a freshly created split pane: its final rect, the
    /// split direction and the animation start time. Drawn as a shrinking
    /// bg-coloured cover in `draw_split_overlays`; cleared when done.
    pub nebula_split_reveal: Option<SplitReveal>,
    /// Pending destructive-action confirmation (close with busy children /
    /// multi-line paste), drawn as a centered modal that owns the keyboard.
    pub nebula_confirm: Option<NebulaConfirm>,
    /// Screen rects of the confirm modal's (primary, cancel) buttons, written
    /// by `draw_confirm_modal` each frame so the mouse hit-test can never
    /// drift from what was actually drawn. `None` while no modal shows.
    pub nebula_confirm_buttons: Option<((f32, f32, f32, f32), (f32, f32, f32, f32))>,
    /// Most recently deleted SSH host while its action is still reversible.
    nebula_ssh_delete_undo: Option<SshDeleteUndo>,
    /// 焦点 pane 的助手建议条快照（spec 001）：每帧由 WindowContext 从
    /// `NebulaPaneState::ai_fix` 同步，绘制层只认自己的字段（撤销条同款）。
    pub nebula_ai_fix_bar: Option<crate::ai_assistant::AiFixState>,
    /// Undo button geometry published by the draw pass for exact hit-testing.
    nebula_ssh_delete_undo_rect: Option<(f32, f32, f32, f32)>,
    nebula_ssh_delete_undo_hover: bool,
    pub nebula_ssh_editor: Option<SshHostEditor>,
    pub nebula_ssh_editor_rects: Option<SshEditorRects>,
    nebula_ssh_editor_open: bool,
    nebula_ssh_editor_hover: SshEditorHit,
    /// 「测试连接」点击时暂存的请求；input 层随后取走并交给 SSH runtime。
    /// display 不持有事件代理，这一格就是点击→网络之间的交接台。
    nebula_ssh_test_request: Option<crate::ssh_session::SshTestRequest>,
    /// Inline images visible this frame, collected per pane during
    /// `draw_pane` (grid lock + pane viewport at hand) and drawn in one
    /// full-window pass in `present_frame` — mid-pane GL viewport swaps are
    /// fragile, one batched pass is not.
    nebula_frame_images: Vec<(u64, std::sync::Arc<Vec<u8>>, (u32, u32), (f32, f32, f32, f32))>,

    /// Theme currently painted. In automatic mode this is the light/dark
    /// member resolved from `nebula_theme_preference` and the system state.
    pub nebula_theme: NebulaTheme,
    /// Theme family explicitly selected by the user and written to settings.
    /// Kept separate from the painted theme so an automatic light switch does
    /// not forget which dark theme to restore later.
    nebula_theme_preference: NebulaTheme,
    pub nebula_follow_system_theme: bool,
    nebula_system_theme: Option<WinitTheme>,
    /// User-configured winit decoration override. Automatic Nebula theming
    /// temporarily clears it because winit only emits `ThemeChanged` while a
    /// window is following the operating system.
    nebula_window_theme_override: Option<WinitTheme>,
    pub nebula_settings_open: bool,
    pub nebula_special_tab_active: bool,
    nebula_language_preference: LanguagePreference,
    nebula_language: UiLanguage,
    /// Paths from the last successful app configuration generation.
    nebula_config_paths: Vec<PathBuf>,
    /// Settings content scroll offset in scaled px (0 = top of the section).
    nebula_settings_scroll: f32,
    /// Command palette (Ctrl+Shift+P): fuzzy launcher model + UI state.
    nebula_palette: command_palette::CommandPalette,
    /// Installed shells, detected once (registry + filesystem scan) and cached
    /// for the new-tab dropdown. `None` until the first menu open.
    nebula_detected_shells: Option<Vec<crate::shell_detect::DetectedShell>>,
    /// Right-side drawer: directory tree / git status of the focused cwd.
    pub nebula_side_panel: side_panel::SidePanel,
    /// Remote file drawer opened from an SSH destination context menu.
    pub nebula_sftp_panel: Option<sftp_panel::SftpPanel>,
    /// Shared chrome animation state. All sidebar/drawer transitions step here
    /// so easing/timing does not get scattered across render code.
    nebula_ui_anims: NebulaUiAnims,
    /// Active sidebar section inside the settings panel.
    nebula_settings_section: NebulaSettingsSection,
    nebula_chrome_hover: ChromeHit,
    /// Bottom-docked queue affordance. The entry state lives separately from
    /// Tabs/SSH so real Agent events can be connected without changing chrome
    /// geometry or input contracts again.
    nebula_message_queue_entry: message_queue_entry::MessageQueueEntry,
    nebula_settings_hover: SettingsHit,
    /// Active settings opacity drag: target plus the screen-space track used
    /// for pointer-to-value mapping. Values persist only when the drag ends.
    pub nebula_settings_opacity_drag: Option<(settings::SettingsOpacityTarget, f32, f32)>,
    /// Unified native right-click menu shared by tab and SSH rows. The menu
    /// owns its short open/close animation so no input path needs timers.
    nebula_context_menu: Option<context_menu::ContextMenu>,
    nebula_tab_labels: Vec<String>,
    /// Per-tab custom accent. `None` follows the live theme accent.
    nebula_tab_colors: Vec<Option<Rgb>>,
    nebula_tab_bells: Vec<bool>,
    /// Per-tab "command is running" flags driving the sidebar spinners.
    nebula_tab_running: Vec<bool>,
    /// Per-tab real AI brand logo (claude/codex), textured over the icon slot.
    nebula_tab_logos: Vec<Option<AiLogo>>,
    /// Decoded (+theme-tinted) logo pixels with stable renderer texture ids,
    /// keyed by (logo, ink). Decode and tint run once per key.
    nebula_ai_logo_cache:
        std::collections::HashMap<(AiLogo, [u8; 3]), (u64, std::sync::Arc<Vec<u8>>, (u32, u32))>,
    /// Decoded shell icons (full-color PNGs) with stable texture ids, keyed by
    /// shell id (pwsh/cmd/nu/wsl:Ubuntu). Decode runs once per id.
    nebula_shell_icon_cache:
        std::collections::HashMap<String, (u64, std::sync::Arc<Vec<u8>>, (u32, u32))>,
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
    /// SSH host aliases from `~/.ssh/config` for the sidebar's "SSH HOSTS"
    /// section, pinned entries first (see `nebula_pinned_hosts`).
    pub nebula_ssh_hosts: Vec<String>,
    /// Host names the user pinned to the top (right-click), persisted in the
    /// runtime settings file so the order survives restarts.
    nebula_pinned_hosts: Vec<String>,
    /// Destinations auto-saved from typed `ssh` commands once the connection
    /// confirmed (see `NebulaPaneState::pending_ssh_host`), most recent
    /// first, persisted. Merged into `nebula_ssh_hosts` after the pinned
    /// block, before the `~/.ssh/config` aliases.
    nebula_saved_hosts: Vec<String>,
    /// User-deleted SSH config aliases. Config files remain untouched; hiding
    /// them here makes Delete stable instead of letting the next merge revive
    /// the row immediately.
    nebula_hidden_hosts: Vec<String>,
    /// Accordion fold state of the two sidebar sections.
    nebula_tabs_section_open: bool,
    nebula_hosts_section_open: bool,
    /// Per-section scroll offsets, in whole rows (clamped by the layout).
    nebula_tabs_scroll: usize,
    nebula_hosts_scroll: usize,
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
    /// Raw default-shell id when the user picked a detected shell the 2-value
    /// `nebula_shell` enum can't represent (cmd/pwsh/nu/wsl:X). Drives the
    /// settings row label and is persisted verbatim.
    pub nebula_shell_id: Option<String>,
    /// User-selected working directory for newly created terminal tabs.
    pub nebula_startup_directory: Option<PathBuf>,
    /// Whether new sessions print the Nebula welcome/fetch screen.
    pub nebula_fetch_enabled: bool,
    /// Whether the injected prompt uses Nebula's powerline segments.
    pub nebula_powerline_enabled: bool,
    /// Closing a window detaches its panes into the resident process for
    /// re-attach (multiplexer restore). Off = close kills the shells.
    pub nebula_keep_session: bool,
    /// Runtime window opacity controlled from Nebula settings.
    pub nebula_window_opacity: f32,
    /// Which settings combobox (floating option list) is expanded, if any.
    /// One field for every dropdown: shell, font, wallpaper fit/alignment,
    /// language, accept key and cursor shape all share the widget.
    pub nebula_settings_dropdown: Option<settings::SettingsDropdown>,
    /// Default cursor shape/blink from settings. Programs may still override
    /// the shape with DECSCUSR escapes (vim's mode cursor keeps working).
    pub nebula_cursor_shape: CursorShape,
    pub nebula_cursor_blink: bool,
    /// 交互: WT-style copyOnSelect. Off = right-click copies / pastes instead.
    pub nebula_copy_on_select: bool,
    /// 全宽字形（CJK 等）bold run 用 Regular 字形（粗体提亮不加粗，#4）。
    pub nebula_cjk_bold_regular: bool,
    /// User keybinding overrides, raw `(combo, action)` from
    /// `nebula_settings.txt` in file order (persisted verbatim).
    pub(crate) nebula_keybinds: Vec<(String, String)>,
    /// Parsed override table (newest-first); consulted by
    /// `process_key_bindings` BEFORE the config table (spec 002).
    pub nebula_keymap: Vec<crate::config::KeyBinding>,
    /// When `Some(row)`, the Keymap settings page is capturing a new combo
    /// for `keymap::EDITABLE_ACTIONS[row]` and the keyboard is owned by it.
    pub nebula_keymap_capture: Option<usize>,
    /// 捕获态实时回显：当前按住的修饰键前缀（"Ctrl+Shift+"）。松开清空。
    pub nebula_keymap_capture_preview: String,
    pub nebula_font_family: String,
    nebula_font_families: Vec<String>,
    nebula_font_notice: Option<String>,
    /// Optional runtime clear/background color controlled from settings.
    pub nebula_background: Option<Rgb>,
    /// Optional background image path drawn as a full-window wallpaper.
    pub nebula_background_image: Option<String>,
    /// Wallpaper alpha, separate from the window opacity to preserve text contrast.
    pub nebula_background_image_opacity: f32,
    /// Windows Terminal-compatible wallpaper sizing and anchor settings.
    pub nebula_background_image_fit: BackgroundImageFit,
    pub nebula_background_image_alignment: BackgroundImageAlignment,
    /// Off by default: wallpapers stay inside terminal content. Enabling this
    /// requires an explicit warning confirmation because it reduces chrome contrast.
    pub nebula_background_image_cover_chrome: bool,
    nebula_settings_mtime: Option<std::time::SystemTime>,
    nebula_bg_palette_index: usize,
    /// 背景色浮层的 16 进制草稿与聚焦态（浮层关闭时归零）。
    nebula_bg_hex_input: String,
    pub(crate) nebula_bg_hex_active: bool,
    /// 设置→高级→同步（WebDAV）的四个输入草稿：url、用户名、WebDAV
    /// 密码、E2E 口令。密码/口令只是「待保存」缓冲——提交即入凭据
    /// 管理器并清空，明文从不驻留。
    nebula_sync_inputs: [String; 4],
    /// 聚焦的同步输入框（0..4，对应 [`nebula_sync_inputs`] 下标）。
    pub(crate) nebula_sync_focus: Option<usize>,
    nebula_sync_auto_pull: bool,
    /// 凭据管理器里已有 [密码, 口令]（只存在性，绝不回读明文进 UI）。
    nebula_sync_secret_set: [bool; 2],
    /// 最近一次同步动作的结果 `(message, is_error)`，画在按钮行下方。
    pub(crate) nebula_sync_status: Option<(String, bool)>,
    nebula_sync_busy: bool,

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
    /// Draw-time adaptation for application-owned RGB colors. The terminal
    /// grid retains the original values so protocol state and copying are exact.
    terminal_color_resolver: terminal_color::TerminalColorResolver,

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

    /// UI 字体角色（wezterm `window_frame` 范式）：chrome 排版锚定在这个
    /// 状态上，永不跟随终端缩放。Ctrl+滚轮 / 设置 spinner 只改
    /// `font_size`（终端网格与跟随它的文档查看器）。阶段 3 将把
    /// family/size 暴露为独立配置。
    nebula_ui_font: NebulaUiFont,

    /// 抽屉视图是否路由到 SFTP（聚焦 pane 的 SSH 身份与面板连接匹配）。
    /// 见 [`Self::route_side_panel`]。
    nebula_sftp_routed: bool,

    // Mouse point position when highlighting hints.
    hint_mouse_point: Option<Point>,

    renderer: ManuallyDrop<Renderer>,
    renderer_preference: Option<RendererPreference>,

    surface: ManuallyDrop<Surface<WindowSurface>>,

    context: ManuallyDrop<PossiblyCurrentContext>,

    glyph_cache: GlyphCache,
    meter: Meter,
}

/// 计算全屏 TUI 在网格之外需要补齐的垂直背景带。内部边缘必须停在 Pane 边界，
/// 只有接触终端外沿的 Pane 才能继续延伸到圆角卡片边缘。
fn alt_screen_vertical_padding_bands(
    window: &SizeInfo,
    pane: &SizeInfo,
    card_y: f32,
    card_height: f32,
) -> [Option<(f32, f32)>; 2] {
    const EDGE_EPSILON: f32 = 0.5;

    let window_grid_top = window.padding_y();
    let window_grid_bottom = window.height() - window.padding_bottom();
    let pane_top = pane.padding_y();
    let pane_bottom = pane.height() - pane.padding_bottom();
    let grid_bottom = pane_top + pane.screen_lines() as f32 * pane.cell_height();

    let band = |start: f32, end: f32| {
        let height = (end - start).max(0.0);
        (height > f32::EPSILON).then_some((start, height))
    };

    let top = if (pane_top - window_grid_top).abs() <= EDGE_EPSILON {
        band(card_y, pane_top)
    } else {
        None
    };
    let bottom_limit = if (pane_bottom - window_grid_bottom).abs() <= EDGE_EPSILON {
        card_y + card_height
    } else {
        pane_bottom
    };

    [top, band(grid_bottom, bottom_limit)]
}

/// 本地文件/目录送回收站（Windows：`SHFileOperationW` + `FOF_ALLOWUNDO`）。
/// 不弹系统确认（Nebula 自己的确认弹窗已经问过了），不弹系统错误框——
/// 失败以 Err 返回给面板提示条。
#[cfg(windows)]
fn send_to_recycle_bin(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };

    if !path.exists() {
        return Err("路径已不存在".to_owned());
    }
    // pFrom 是双 NUL 结尾的路径列表。
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE as u32,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code == 0 && op.fAnyOperationsAborted == 0 {
        Ok(())
    } else {
        Err(format!("系统拒绝（代码 {code}）"))
    }
}

#[cfg(not(windows))]
fn send_to_recycle_bin(path: &std::path::Path) -> Result<(), String> {
    // 非 Windows 暂无回收站集成：递归删除前置确认已由弹窗承担。
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|err| err.to_string())
}

/// Prefer the event loop's system-wide appearance over the window theme.
///
/// On Windows, `Window::theme()` is a cached per-window value and can still
/// contain the previous manual override immediately after `set_theme(None)`.
fn system_theme_snapshot(
    event_loop_theme: Option<WinitTheme>,
    window_theme: Option<WinitTheme>,
) -> Option<WinitTheme> {
    event_loop_theme.or(window_theme)
}

impl Display {
    pub fn new(
        window: Window,
        gl_context: NotCurrentContext,
        config: &UiConfig,
        system_theme: Option<WinitTheme>,
        _tabbed: bool,
    ) -> Result<Display, Error> {
        let raw_window_handle = window.raw_window_handle();

        let scale_factor = window.scale_factor as f32;
        let settings_init = settings::nebula_settings_load(config);
        let rasterizer = Rasterizer::new()?;
        crate::boot_trace("rasterizer ready");

        // 设置里保存过字号则优先生效（逻辑 px × 缩放），否则跟随配置文件；
        // Ctrl+滚轮 / 设置 spinner 改过的字号因此在重启后保持。
        let font_size = settings_init
            .font_size
            .map(|px| FontSize::from_px(px * scale_factor))
            .unwrap_or_else(|| config.font.size().scale(scale_factor));
        // UI 锚定字号始终取配置字号（0.7 默认）：终端字号的持久化缩放不
        // 影响 chrome。2026-07-28 曾试过锚定 settings 保存字号，实测 16.3px
        // 让 chrome 明显过大，用户裁定回到 0.7 默认——当时「图标变小」的
        // 真凶是 ambiguous 宽度缩放误伤 PUA 图标，已在 glyph_cache 排除。
        let ui_font_px = config.font.size().scale(scale_factor).as_px();
        #[cfg(windows)]
        let (rasterizer, required_font_install) = {
            let mut rasterizer = rasterizer;
            let installed = GlyphCache::font_family_available(
                &mut rasterizer,
                crate::font_install::REQUIRED_FONT_FAMILY,
                font_size,
            );
            let required = (!installed).then(|| NebulaConfirm::InstallRequiredFont {
                directory: crate::font_install::bundled_font_directory(),
            });
            (rasterizer, required)
        };
        #[cfg(not(windows))]
        let required_font_install = None;

        debug!("Loading \"{}\" font", &settings_init.font_family);
        let font =
            config.font.clone().with_family(settings_init.font_family.clone()).with_size(font_size);
        let mut glyph_cache = GlyphCache::new(rasterizer, &font)?;
        glyph_cache.wide_bold_use_regular = settings_init.cjk_bold_regular;
        #[cfg(windows)]
        let mut nebula_font_families = glyph_cache.private_font_families();
        #[cfg(not(windows))]
        let mut nebula_font_families = vec![settings_init.font_family.clone()];
        nebula_font_families.retain(|family| family != crate::font_install::REQUIRED_FONT_FAMILY);
        nebula_font_families.insert(0, crate::font_install::REQUIRED_FONT_FAMILY.to_owned());
        if !nebula_font_families.iter().any(|family| family == &settings_init.font_family) {
            nebula_font_families.push(settings_init.font_family.clone());
        }
        crate::boot_trace("glyph cache (font faces loaded)");

        let metrics = glyph_cache.font_metrics();
        let (cell_width, cell_height) = compute_cell_size(config, &metrics);

        // Resize the window to the user-configured size, or a Windows
        // Terminal-like default when unset. A 116-column by 30-row canvas is
        // the standard startup size. Two inputs are deliberately excluded:
        // the session file's saved window size (stale/wrong-domain values
        // kept resurfacing as near-fullscreen launches), and the persisted
        // terminal zoom — the startup grid is priced at the CONFIG base font
        // size, because 116 columns of a Ctrl+wheel-enlarged cell is itself
        // a near-fullscreen window. The zoomed font still renders; it just
        // shows fewer columns in the standard-sized window.
        let dimensions = config
            .window
            .dimensions()
            .unwrap_or(crate::config::window::Dimensions { columns: 116, lines: 30 });
        let base_font_size = config.font.size().scale(scale_factor);
        let (base_cell_width, base_cell_height) = glyph_cache
            .metrics_at(base_font_size)
            .map(|base_metrics| compute_cell_size(config, &base_metrics))
            .unwrap_or((cell_width, cell_height));
        let size =
            window_size(config, dimensions, base_cell_width, base_cell_height, scale_factor);
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
        let size_info = SizeInfo::new_fully_asymmetric(
            viewport_size.width as f32,
            viewport_size.height as f32,
            cell_width,
            cell_height,
            padding.0 + content_pad + sidebar_width(scale, false),
            padding.0 + content_pad,
            padding.1 + chrome,
            padding.1 + bottom_content_reserve(scale),
        );

        info!("Cell size: {cell_width} x {cell_height}");
        info!("Padding: {} x {}", size_info.padding_x(), size_info.padding_y());
        info!("Width: {}, Height: {}", size_info.width(), size_info.height());

        // Update OpenGL projection.
        renderer.resize(&size_info);

        // Clear screen.
        let nebula_window_theme_override = config.window.theme();
        if settings_init.follow_system_theme {
            window.set_theme(None);
        }
        let nebula_system_theme = system_theme_snapshot(system_theme, window.theme());
        let nebula_theme = if settings_init.follow_system_theme {
            nebula_system_theme
                .map(|theme| {
                    settings_init.theme.for_system_appearance(matches!(theme, WinitTheme::Light))
                })
                .unwrap_or(settings_init.theme)
        } else {
            settings_init.theme
        };
        let background_color = if settings_init.follow_system_theme {
            nebula_theme.palette().term_bg
        } else {
            settings_init.background.unwrap_or(config.colors.primary.background)
        };
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
            window.set_resize_increments(Some(PhysicalSize::new(cell_width, cell_height)));
        }

        window.set_visible(true);
        crate::boot_trace("window visible");

        // Always focus new windows, even if no Nebula window is currently focused.
        #[cfg(target_os = "macos")]
        window.focus_window();

        if !_tabbed {
            match config.window.startup_mode {
                #[cfg(target_os = "macos")]
                StartupMode::SimpleFullscreen => window.set_simple_fullscreen(true),
                StartupMode::Maximized if !is_wayland => window.set_maximized(true),
                #[cfg(windows)]
                StartupMode::Fullscreen => window.set_fullscreen(true),
                _ => (),
            }
        }

        let hint_state = HintState::new(config.hints.alphabet());
        // Publish the RESTORED theme to the prompt bridge (writing the default
        // here used to reset the powerline colors on every launch).
        write_nebula_prompt_theme(nebula_theme);

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
        nebula_theme.apply_term_colors(&mut initial_colors, &nebula_default_colors);

        let mut display = Self {
            context: ManuallyDrop::new(context),
            visual_bell: VisualBell::from(&config.bell),
            renderer: ManuallyDrop::new(renderer),
            renderer_preference: config.debug.renderer,
            surface: ManuallyDrop::new(surface),
            colors: initial_colors,
            nebula_default_colors,
            terminal_color_resolver: Default::default(),
            frame_timer: FrameTimer::new(),
            raw_window_handle,
            damage_tracker,
            glyph_cache,
            hint_state,
            size_info,
            font_size,
            nebula_ui_font: NebulaUiFont { px: ui_font_px, cell: (0.0, 0.0) },
            nebula_sftp_routed: true,
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
            directory_history: crate::directory_history::global(),
            nebula_commands: nebula_commands_handle(),
            nebula_tab_anim: Vec::new(),
            nebula_scrollbar_drag: None,
            nebula_split_reveal: None,
            nebula_confirm: required_font_install,
            nebula_confirm_buttons: None,
            nebula_ssh_delete_undo: None,
            nebula_ai_fix_bar: None,
            nebula_ssh_delete_undo_rect: None,
            nebula_ssh_delete_undo_hover: false,
            nebula_ssh_editor: None,
            nebula_ssh_editor_rects: None,
            nebula_ssh_editor_open: false,
            nebula_ssh_editor_hover: SshEditorHit::None,
            nebula_ssh_test_request: None,
            nebula_frame_images: Vec::new(),
            nebula_theme,
            nebula_theme_preference: settings_init.theme,
            nebula_follow_system_theme: settings_init.follow_system_theme,
            nebula_system_theme,
            nebula_window_theme_override,
            nebula_settings_open: false,
            nebula_special_tab_active: false,
            nebula_language_preference: settings_init.language,
            nebula_language: settings_init.language.resolved(),
            nebula_config_paths: config.config_paths.clone(),
            nebula_settings_scroll: 0.0,
            nebula_palette: {
                let mut palette = command_palette::CommandPalette::new();
                palette.set_language(settings_init.language.resolved());
                palette
            },
            nebula_detected_shells: None,
            nebula_side_panel: side_panel::SidePanel::new(),
            nebula_sftp_panel: None,
            nebula_ui_anims: NebulaUiAnims::new(),
            nebula_settings_section: NebulaSettingsSection::default(),
            nebula_chrome_hover: ChromeHit::None,
            nebula_message_queue_entry: message_queue_entry::MessageQueueEntry::default(),
            nebula_settings_hover: SettingsHit::None,
            nebula_settings_opacity_drag: None,
            nebula_context_menu: None,
            nebula_settings_dropdown: None,
            nebula_cursor_shape: settings_init.cursor_shape,
            nebula_cursor_blink: settings_init.cursor_blink,
            nebula_copy_on_select: settings_init.copy_on_select,
            nebula_cjk_bold_regular: settings_init.cjk_bold_regular,
            nebula_keymap: keymap::build_bindings(&settings_init.keybinds),
            nebula_keybinds: settings_init.keybinds,
            nebula_keymap_capture: None,
            nebula_keymap_capture_preview: String::new(),
            nebula_font_family: settings_init.font_family,
            nebula_font_families,
            nebula_font_notice: None,
            nebula_tab_labels: vec![".".to_owned()],
            nebula_tab_colors: vec![None],
            nebula_tab_bells: vec![false],
            nebula_tab_running: vec![false],
            nebula_tab_logos: vec![None],
            nebula_ai_logo_cache: Default::default(),
            nebula_shell_icon_cache: Default::default(),
            nebula_chrome_logo_draws: Vec::new(),
            nebula_active_tab: 0,
            nebula_tab_drag: None,
            nebula_tabs_reorderable: true,
            nebula_sidebar_collapsed: false,
            nebula_ssh_hosts: merge_ssh_hosts(
                &settings_init.saved_hosts,
                &settings_init.pinned_hosts,
                &settings_init.hidden_hosts,
            ),
            nebula_pinned_hosts: settings_init.pinned_hosts.clone(),
            nebula_saved_hosts: settings_init.saved_hosts.clone(),
            nebula_hidden_hosts: settings_init.hidden_hosts.clone(),
            nebula_tabs_section_open: true,
            nebula_hosts_section_open: true,
            nebula_tabs_scroll: 0,
            nebula_hosts_scroll: 0,
            nebula_tab_rename: None,
            nebula_tab_rename_select_all: false,
            nebula_tab_rename_caret: 0,
            nebula_tab_rename_text_x: 0.0,
            nebula_pty_resize_pending: false,
            nebula_ghost_enabled: settings_init.ghost,
            nebula_accept: settings_init.accept,
            nebula_shell: settings_init.shell,
            nebula_shell_id: settings_init.shell_id.clone(),
            nebula_startup_directory: settings_init.startup_directory,
            nebula_fetch_enabled: settings_init.fetch,
            nebula_powerline_enabled: settings_init.powerline,
            nebula_keep_session: settings_init.keep_session,
            nebula_window_opacity: settings_init.opacity,
            nebula_background: if settings_init.follow_system_theme {
                Some(nebula_theme.palette().term_bg)
            } else {
                settings_init.background
            },
            nebula_background_image: settings_init.background_image,
            nebula_background_image_opacity: settings_init.background_image_opacity,
            nebula_background_image_fit: settings_init.background_image_fit,
            nebula_background_image_alignment: settings_init.background_image_alignment,
            nebula_background_image_cover_chrome: settings_init.background_image_cover_chrome,
            nebula_settings_mtime: settings::nebula_settings_mtime(),
            nebula_bg_palette_index: 0,
            nebula_bg_hex_input: String::new(),
            nebula_bg_hex_active: false,
            nebula_sync_inputs: Default::default(),
            nebula_sync_focus: None,
            nebula_sync_auto_pull: false,
            nebula_sync_secret_set: [false; 2],
            nebula_sync_status: None,
            nebula_sync_busy: false,
            meter: Default::default(),
            ime: Default::default(),
        };
        // A persisted zoom means the very first frame already runs off the UI
        // base size — the font role must be pinned NOW, not after the first
        // font change funnels through handle_update.
        display.refresh_ui_font(config);
        Ok(display)
    }

    pub fn settings_open(&self) -> bool {
        self.nebula_settings_open
    }

    pub fn ui_language(&self) -> UiLanguage {
        self.nebula_language
    }

    pub fn settings_section(&self) -> NebulaSettingsSection {
        self.nebula_settings_section
    }

    pub fn select_settings_section(&mut self, section: NebulaSettingsSection) {
        self.close_shell_picker();
        self.close_font_picker();
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
        let area = self.terminal_card_rect();
        let max = settings::settings_max_scroll(
            &self.size_info,
            self.window.scale_factor as f32,
            area,
            self.nebula_settings_section,
            self.nebula_hidden_hosts.len(),
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

    pub fn shell_picker_count(&self) -> usize {
        self.nebula_detected_shells.as_ref().map_or(0, Vec::len)
    }

    pub fn font_picker_count(&self) -> usize {
        self.nebula_font_families.len() + 1
    }

    pub fn hidden_ssh_host_count(&self) -> usize {
        self.nebula_hidden_hosts.len()
    }

    pub fn open_ssh_editor(&mut self) {
        self.nebula_ssh_editor = Some(SshHostEditor {
            original_destination: None,
            error: None,
            destination_selection: Default::default(),
            password_selection: Default::default(),
            destination: String::new(),
            password: String::new(),
            save_password: true,
            show_password: false,
            auth: crate::ssh_profiles::SshAuthMode::Auto,
            private_keys: Vec::new(),
            field: SshEditorField::Destination,
            focus: crate::ux::FocusIndex::default(),
            test: Default::default(),
        });
        self.nebula_ssh_editor_rects = None;
        self.nebula_ssh_editor_open = true;
        self.nebula_ssh_editor_hover = SshEditorHit::None;
        // 每次打开都从零开始，避免上一次退出动画的残余进度造成闪跳。
        self.nebula_ui_anims.ssh_editor = UiAnim::new(0.0);
        self.pending_update.dirty = true;
    }

    pub fn edit_ssh_host(&mut self, index: usize) {
        let Some(destination) = self.nebula_ssh_hosts.get(index).cloned() else { return };
        let profile =
            crate::ssh_profiles::SshProfiles::load(&nebula_data_dir().join("ssh_profiles.json"))
                .unwrap_or_else(|err| {
                    warn!("加载 SSH Profile 失败，编辑器使用自动认证: {err}");
                    crate::ssh_profiles::SshProfiles::default()
                })
                .for_destination(&destination);
        self.nebula_ssh_editor = Some(SshHostEditor {
            original_destination: Some(destination.clone()),
            error: None,
            destination_selection: Default::default(),
            password_selection: Default::default(),
            destination,
            // Never pull a stored secret back into a text field. Leaving this
            // blank preserves the existing credential when the address stays
            // unchanged; typing a new value explicitly replaces it.
            password: String::new(),
            save_password: true,
            show_password: false,
            auth: profile.auth,
            private_keys: profile.private_keys,
            field: SshEditorField::Destination,
            focus: crate::ux::FocusIndex::default(),
            test: Default::default(),
        });
        self.nebula_ssh_editor_rects = None;
        self.nebula_ssh_editor_open = true;
        self.nebula_ssh_editor_hover = SshEditorHit::None;
        self.nebula_ui_anims.ssh_editor = UiAnim::new(0.0);
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_editor_active(&self) -> bool {
        self.nebula_ssh_editor_open && self.nebula_ssh_editor.is_some()
    }

    pub fn close_ssh_editor(&mut self) {
        if self.nebula_ssh_editor.is_some() {
            self.nebula_ssh_editor_open = false;
            self.nebula_ssh_editor_hover = SshEditorHit::None;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn ssh_editor_hit(&self, x: f32, y: f32) -> SshEditorHit {
        let Some(rects) = self.nebula_ssh_editor_rects.as_ref() else {
            return SshEditorHit::None;
        };
        let hit = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        if hit(rects.close) {
            SshEditorHit::Close
        } else if hit(rects.destination) {
            SshEditorHit::Destination
        } else if let Some((mode, _)) = rects.auth.iter().find(|(_, rect)| hit(*rect)) {
            SshEditorHit::Auth(*mode)
        } else if hit(rects.add_private_key) {
            SshEditorHit::AddPrivateKey
        } else if let Some((index, _)) = rects.private_key_rows.iter().find(|(_, rect)| hit(*rect))
        {
            SshEditorHit::RemovePrivateKey(*index)
        } else if hit(rects.password_toggle) {
            SshEditorHit::PasswordToggle
        } else if hit(rects.password) {
            SshEditorHit::Password
        } else if hit(rects.save_checkbox) {
            SshEditorHit::SaveToggleBox
        } else if hit(rects.save_toggle) {
            SshEditorHit::SaveToggleLabel
        } else if hit(rects.test) {
            SshEditorHit::Test
        } else if hit(rects.cancel) {
            SshEditorHit::Cancel
        } else if hit(rects.primary) {
            SshEditorHit::Primary
        } else {
            SshEditorHit::None
        }
    }

    pub fn set_ssh_editor_hover(&mut self, hover: SshEditorHit) {
        if self.nebula_ssh_editor_hover != hover {
            self.nebula_ssh_editor_hover = hover;
            self.pending_update.dirty = true;
        }
    }

    pub fn ssh_editor_insert(&mut self, text: &str) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.error = None;
            editor.test = Default::default();
            match editor.field {
                SshEditorField::Destination => {
                    editor.destination_selection.insert(&mut editor.destination, text)
                },
                SshEditorField::Password => {
                    editor.password_selection.insert(&mut editor.password, text)
                },
            }
        }
    }

    pub fn ssh_editor_backspace(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.test = Default::default();
            match editor.field {
                SshEditorField::Destination => {
                    editor.destination_selection.backspace(&mut editor.destination)
                },
                SshEditorField::Password => {
                    editor.password_selection.backspace(&mut editor.password)
                },
            }
        }
    }

    pub fn ssh_editor_select_all(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            match editor.field {
                SshEditorField::Destination => {
                    editor.destination_selection.select(&editor.destination)
                },
                SshEditorField::Password => editor.password_selection.select(&editor.password),
            }
        }
    }

    /// Copying an invisible password would persist a secret in the system
    /// clipboard without visible intent, so it is enabled only after Reveal.
    pub fn ssh_editor_selected_text(&self) -> Option<String> {
        let editor = self.nebula_ssh_editor.as_ref()?;
        match editor.field {
            SshEditorField::Destination => {
                editor.destination_selection.selected_text(&editor.destination)
            },
            SshEditorField::Password if editor.show_password => {
                editor.password_selection.selected_text(&editor.password)
            },
            SshEditorField::Password => None,
        }
    }

    pub fn ssh_editor_next_field(&mut self, reverse: bool) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.destination_selection.clear();
            editor.password_selection.clear();
            let shows_password = ssh_ui::auth_sections(editor.auth).0;
            let count = if shows_password { 4 } else { 3 };
            editor.focus.advance(count, reverse);
            editor.field = match (shows_password, editor.focus.current()) {
                (_, 0) => SshEditorField::Destination,
                (true, 1) => SshEditorField::Password,
                _ => editor.field,
            };
        }
    }

    pub fn ssh_editor_activate_focus(&mut self) {
        let Some(editor) = self.nebula_ssh_editor.as_ref() else { return };
        let shows_password = ssh_ui::auth_sections(editor.auth).0;
        match (shows_password, editor.focus.current()) {
            (true, 2) | (false, 1) => self.close_ssh_editor(),
            (true, 3) | (false, 2) => self.save_ssh_editor(),
            _ => {},
        }
    }

    pub fn ssh_editor_toggle_save(&mut self) {
        if let Some(editor) = self.nebula_ssh_editor.as_mut() {
            editor.save_password = !editor.save_password;
        }
    }

    /// input 层在处理完编辑器点击后调用：取走「测试连接」暂存的请求。
    pub fn take_ssh_test_request(&mut self) -> Option<crate::ssh_session::SshTestRequest> {
        self.nebula_ssh_test_request.take()
    }

    /// 后台测试完成回报。结果只属于发起时的草稿：地址已改或状态已非
    /// Running（用户改过字段被清位）时直接丢弃，不让旧结果背书新配置。
    pub fn ssh_test_done(&mut self, destination: &str, ok: bool, message: &str, elapsed_ms: u64) {
        let Some(editor) = self.nebula_ssh_editor.as_mut() else { return };
        if editor.destination.trim() != destination
            || editor.test != ssh_ui::SshTestState::Running
        {
            return;
        }
        editor.test = if ok {
            ssh_ui::SshTestState::Ok { elapsed_ms }
        } else {
            ssh_ui::SshTestState::Failed { summary: message.to_owned() }
        };
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_editor_click(&mut self, x: f32, y: f32) -> bool {
        match self.ssh_editor_hit(x, y) {
            SshEditorHit::Destination => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.focus.set(0, 4);
                    editor.destination_selection.clear();
                    editor.password_selection.clear();
                    editor.field = SshEditorField::Destination;
                }
            },
            SshEditorHit::PasswordToggle => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.show_password = !editor.show_password;
                }
            },
            SshEditorHit::Password => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.focus.set(1, 4);
                    editor.destination_selection.clear();
                    editor.password_selection.clear();
                    editor.field = SshEditorField::Password;
                }
            },
            SshEditorHit::Auth(mode) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    editor.auth = mode;
                    editor.error = None;
                    editor.test = Default::default();
                    if !ssh_ui::auth_sections(mode).0 {
                        editor.field = SshEditorField::Destination;
                    }
                }
            },
            SshEditorHit::AddPrivateKey => {
                if let Some(result) = file_dialog::pick_private_key_file(&self.window) {
                    if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                        match result {
                            Ok(path) => {
                                ssh_ui::push_private_key(&mut editor.private_keys, path);
                                editor.error = None;
                                editor.test = Default::default();
                            },
                            Err(err) => editor.error = Some(err),
                        }
                    }
                }
            },
            SshEditorHit::RemovePrivateKey(index) => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    if index < editor.private_keys.len() {
                        editor.private_keys.remove(index);
                        editor.test = Default::default();
                    }
                }
            },
            SshEditorHit::SaveToggleBox | SshEditorHit::SaveToggleLabel => {
                self.ssh_editor_toggle_save();
            },
            SshEditorHit::Test => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let destination = editor.destination.trim().to_owned();
                    if destination.is_empty() {
                        editor.error = Some("请输入 SSH 地址，例如 user@example.com".to_owned());
                    } else if editor.test != ssh_ui::SshTestState::Running {
                        editor.error = None;
                        editor.test = ssh_ui::SshTestState::Running;
                        self.nebula_ssh_test_request =
                            Some(crate::ssh_session::SshTestRequest {
                                destination,
                                auth: editor.auth,
                                private_keys: editor.private_keys.clone(),
                                password: (!editor.password.is_empty())
                                    .then(|| editor.password.clone()),
                            });
                    }
                }
            },
            SshEditorHit::Close => {
                self.close_ssh_editor();
            },
            SshEditorHit::Cancel => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let shows_password = ssh_ui::auth_sections(editor.auth).0;
                    editor.focus.set(
                        if shows_password { 2 } else { 1 },
                        if shows_password { 4 } else { 3 },
                    );
                }
                self.close_ssh_editor();
            },
            SshEditorHit::Primary => {
                if let Some(editor) = self.nebula_ssh_editor.as_mut() {
                    let shows_password = ssh_ui::auth_sections(editor.auth).0;
                    editor.focus.set(
                        if shows_password { 3 } else { 2 },
                        if shows_password { 4 } else { 3 },
                    );
                }
                self.save_ssh_editor();
            },
            SshEditorHit::None => return false,
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    pub fn save_ssh_editor(&mut self) {
        let Some(mut editor) = self.nebula_ssh_editor.take() else { return };
        let destination = editor.destination.trim().to_owned();
        let valid = !destination.is_empty()
            && !destination
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || ";&|<>\"'`".contains(c));
        if !valid {
            editor.error = Some(if destination.is_empty() {
                "请输入 SSH 地址，例如 user@example.com".to_owned()
            } else {
                "地址不能包含空白、控制字符或 shell 分隔符".to_owned()
            });
            editor.field = SshEditorField::Destination;
            self.nebula_ssh_editor = Some(editor);
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return;
        }
        if let Some(original) = editor.original_destination.as_deref() {
            if original != destination {
                self.nebula_saved_hosts.retain(|host| host != original);
                self.nebula_pinned_hosts.retain(|host| host != original);
                if !self.nebula_hidden_hosts.iter().any(|host| host == original) {
                    self.nebula_hidden_hosts.push(original.to_owned());
                }
                #[cfg(windows)]
                {
                    let _ = crate::ssh_credentials::forget_password(original);
                }
            }
        }
        // Saving/editing is an explicit request to surface this destination,
        // so it also undoes a previous Delete of the same address.
        self.nebula_hidden_hosts.retain(|host| host != &destination);
        self.nebula_saved_hosts.retain(|host| host != &destination);
        self.nebula_saved_hosts.insert(0, destination.clone());
        self.nebula_saved_hosts.truncate(20);
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        let profile_path = nebula_data_dir().join("ssh_profiles.json");
        let mut profiles =
            crate::ssh_profiles::SshProfiles::load(&profile_path).unwrap_or_else(|err| {
                warn!("加载 SSH Profile 失败，将创建新文件: {err}");
                crate::ssh_profiles::SshProfiles::default()
            });
        if let Some(original) = editor.original_destination.as_deref() {
            if original != destination {
                profiles.rename(original, &destination);
            }
        }
        profiles.upsert(crate::ssh_profiles::SshProfileAuth {
            destination: destination.clone(),
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
        });
        if let Err(err) = profiles.save(&profile_path) {
            editor.error = Some(format!("保存 SSH Profile 失败: {err}"));
            self.nebula_ssh_editor = Some(editor);
            self.pending_update.dirty = true;
            self.window.request_redraw();
            return;
        }
        if ssh_ui::auth_sections(editor.auth).0
            && editor.save_password
            && !editor.password.is_empty()
        {
            #[cfg(windows)]
            {
                let _ = crate::ssh_credentials::store_password(
                    &destination,
                    editor.password.as_bytes(),
                );
            }
        }
        // 凭据落盘后立即清除内存中的明文，但保留其余内容完成短退出动画。
        editor.password.clear();
        self.persist_nebula_settings();
        self.nebula_ssh_editor = Some(editor);
        self.close_ssh_editor();
    }

    /// Ask before removing a saved destination. Config aliases use different
    /// wording because Delete hides them inside Nebula and never edits
    /// `~/.ssh/config` itself.
    pub fn request_delete_ssh_host(&mut self, index: usize) {
        let Some(host) = self.nebula_ssh_hosts.get(index).cloned() else { return };
        let from_config = crate::ssh::ssh_config_hosts().contains(&host);
        self.nebula_confirm = Some(NebulaConfirm::DeleteSsh { host, from_config });
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// Apply a confirmed deletion and arm a complete, credential-safe Undo.
    /// Replacing an older Undo finalizes that older credential deletion first.
    pub fn confirm_delete_ssh_host(&mut self, host: &str) -> bool {
        if !self.nebula_ssh_hosts.iter().any(|entry| entry == host) {
            return false;
        }

        let from_config = crate::ssh::ssh_config_hosts().iter().any(|entry| entry == host);

        // Taking the previous record commits its pending Credential Manager
        // deletion through Drop. Only the most recent destructive action is
        // reversible, matching standard snackbar Undo behavior.
        self.nebula_ssh_delete_undo.take();

        let (saved_index, pinned_index, was_hidden) = remove_ssh_host_from_lists(
            host,
            &mut self.nebula_saved_hosts,
            &mut self.nebula_pinned_hosts,
            &mut self.nebula_hidden_hosts,
        );
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        self.nebula_ssh_delete_undo = Some(SshDeleteUndo {
            host: host.to_owned(),
            saved_index,
            pinned_index,
            was_hidden,
            from_config,
            started_at: std::time::Instant::now(),
            delete_credential_on_drop: true,
        });
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    /// Reverse the complete host-list mutation. The credential was intentionally
    /// kept alive during the grace period, so disarming Drop restores it without
    /// ever copying secret bytes into the UI process.
    pub fn undo_delete_ssh_host(&mut self) -> bool {
        let Some(mut undo) = self.nebula_ssh_delete_undo.take() else { return false };
        if undo.started_at.elapsed() >= SSH_DELETE_UNDO_DURATION {
            // Drop commits the pending credential deletion.
            return false;
        }

        undo.delete_credential_on_drop = false;
        restore_ssh_host_to_lists(
            &undo.host,
            undo.saved_index,
            undo.pinned_index,
            undo.was_hidden,
            &mut self.nebula_saved_hosts,
            &mut self.nebula_pinned_hosts,
            &mut self.nebula_hidden_hosts,
        );
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        self.nebula_ssh_delete_undo_rect = None;
        self.nebula_ssh_delete_undo_hover = false;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    /// Commit the pending Credential Manager deletion when the Undo timer ends.
    pub fn expire_ssh_delete_undo(&mut self) {
        self.nebula_ssh_delete_undo.take();
        self.nebula_ssh_delete_undo_rect = None;
        self.nebula_ssh_delete_undo_hover = false;
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn ssh_delete_undo_available(&self) -> bool {
        self.nebula_ssh_delete_undo
            .as_ref()
            .is_some_and(|undo| undo.started_at.elapsed() < SSH_DELETE_UNDO_DURATION)
    }

    pub fn ssh_delete_undo_hit(&self, x: f32, y: f32) -> bool {
        self.ssh_delete_undo_available()
            && self.nebula_ssh_delete_undo_rect.is_some_and(|rect| {
                x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
            })
    }

    pub fn set_ssh_delete_undo_hover(&mut self, hovered: bool) {
        if self.nebula_ssh_delete_undo_hover != hovered {
            self.nebula_ssh_delete_undo_hover = hovered;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn set_chrome_tabs(
        &mut self,
        labels: Vec<String>,
        mut colors: Vec<Option<Rgb>>,
        mut dots: Vec<bool>,
        mut running: Vec<bool>,
        mut logos: Vec<Option<AiLogo>>,
        active: usize,
        reorderable: bool,
    ) {
        self.nebula_tab_labels = if labels.is_empty() { vec![".".to_owned()] } else { labels };
        colors.truncate(self.nebula_tab_labels.len());
        colors.resize(self.nebula_tab_labels.len(), None);
        self.nebula_tab_colors = colors;
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

    /// Whether any sidebar tab currently shows a running spinner. Only this
    /// state raises the chrome clock to display-rate frames.
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
            || self.nebula_sftp_panel.as_ref().is_some_and(sftp_panel::SftpPanel::editor_active)
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
            AiLogo::OpenAi | AiLogo::OpenCode | AiLogo::Pi => (logo, [ink.r, ink.g, ink.b]),
        };
        if let Some(cached) = self.nebula_ai_logo_cache.get(&key) {
            return Some(cached.clone());
        }
        let bytes: &[u8] = match logo {
            AiLogo::Claude => AI_LOGO_CLAUDE_PNG,
            AiLogo::OpenAi => AI_LOGO_OPENAI_PNG,
            AiLogo::OpenCode => AI_LOGO_OPENCODE_PNG,
            AiLogo::Pi => AI_LOGO_PI_PNG,
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
            AiLogo::OpenAi | AiLogo::Pi => {
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

    /// Decoded pixels for a full-color shell icon (128×128 PNG embedded from
    /// extra/shell-icons), plus a stable texture id for the renderer's inline
    /// cache. Decode runs once per shell id; the GPU upload happens lazily
    /// inside the renderer. Returns `None` when the id has no brand asset.
    fn shell_icon_pixels(
        &mut self,
        shell_id: &str,
    ) -> Option<(u64, std::sync::Arc<Vec<u8>>, (u32, u32))> {
        if let Some(cached) = self.nebula_shell_icon_cache.get(shell_id) {
            return Some(cached.clone());
        }
        let bytes = crate::shell_detect::color_icon_png(shell_id)?;
        let (width, height, rgba) = match crate::renderer::image::decode_png_bytes(bytes) {
            Ok(decoded) => decoded,
            Err(err) => {
                log::warn!("failed to decode shell icon for {shell_id}: {err}");
                return None;
            },
        };
        // Shell icons ship in brand colors and are used as-is (no tint).
        let id = AI_LOGO_ID_BASE + 1000 + self.nebula_shell_icon_cache.len() as u64;
        let entry = (id, std::sync::Arc::new(rgba), (width, height));
        self.nebula_shell_icon_cache.insert(shell_id.to_owned(), entry.clone());
        Some(entry)
    }

    /// Arm a potential tab drag from a press on displayed tab `source`. Always
    /// arms (even single-tab), because the release decides between click /
    /// reorder / dock — selection itself is deferred to the release.
    pub fn arm_tab_drag(&mut self, source: usize, x: f32, y: f32) {
        self.nebula_tab_drag =
            Some(TabDrag { source, origin_x: x, origin: y, current: y, active: false, dock: None });
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
        let gh = self.size_info.height() - gy - self.size_info.padding_bottom();
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
        let layout =
            chrome_tab_layout(&self.ui_size_info(), scale, self.sidebar_model(), sidebar_expand);
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

    pub fn context_menu_interactive(&self) -> bool {
        self.nebula_context_menu.as_ref().is_some_and(context_menu::ContextMenu::interactive)
    }

    /// 右键菜单是否已在同一目标上开着（开着就别重开——反复右键不该让菜单
    /// 因指针位置或底边 clamp 而漂移）。
    fn context_menu_open_for(&self, target: ContextMenuTarget) -> bool {
        self.nebula_context_menu
            .as_ref()
            .is_some_and(|menu| menu.interactive() && menu.target() == target)
    }

    /// 侧栏行右键菜单的锚点：贴行矩形右缘、与行顶对齐——菜单与被点的行
    /// 强相关，而不是跟着指针走。行不存在时回落指针位置。
    fn sidebar_row_anchor(&self, tab: bool, index: usize, fallback: (f32, f32)) -> (f32, f32) {
        let size = self.ui_size_info();
        let scale = self.window.scale_factor as f32;
        let expand = if self.nebula_sidebar_collapsed { 0.0 } else { 1.0 };
        let layout = chrome::chrome_tab_layout(&size, scale, self.sidebar_model(), expand);
        let rows = if tab { &layout.tabs } else { &layout.hosts };
        rows.get(index).map_or(fallback, |(rx, ry, rw, _)| (rx + rw + 4.0 * scale, *ry))
    }

    pub fn open_tab_context_menu(&mut self, index: usize, x: f32, y: f32) {
        if index >= self.nebula_tab_labels.len()
            || self.context_menu_open_for(ContextMenuTarget::Tab(index))
        {
            return;
        }
        let anchor = self.sidebar_row_anchor(true, index, (x, y));
        let color = self.nebula_tab_colors.get(index).copied().flatten();
        self.nebula_context_menu =
            Some(context_menu::ContextMenu::new(ContextMenuTarget::Tab(index), anchor, color));
        self.nebula_tab_drag = None;
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn open_ssh_context_menu(&mut self, index: usize, x: f32, y: f32) {
        if index >= self.nebula_ssh_hosts.len()
            || self.context_menu_open_for(ContextMenuTarget::Ssh(index))
        {
            return;
        }
        let anchor = self.sidebar_row_anchor(false, index, (x, y));
        self.nebula_context_menu =
            Some(context_menu::ContextMenu::new(ContextMenuTarget::Ssh(index), anchor, None));
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn open_sftp_context_menu(&mut self, index: usize, x: f32, y: f32) {
        let Some(panel) = self.nebula_sftp_panel.as_ref() else { return };
        if panel.visible_entry(index).is_none() {
            return;
        }
        self.nebula_context_menu =
            Some(context_menu::ContextMenu::new(ContextMenuTarget::Sftp(index), (x, y), None));
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// 本地文件树行的右键菜单。`..` 导航行不给菜单；打开时把该行设为
    /// 持久选中，菜单与行的关联在视觉上立得住。
    pub fn open_file_tree_context_menu(&mut self, row: usize, x: f32, y: f32) {
        let Some((path, is_dir, is_parent)) = self
            .nebula_side_panel
            .visible_row(row)
            .map(|r| (r.path.clone(), r.is_dir, r.is_parent))
        else {
            return;
        };
        if is_parent {
            return;
        }
        let target = ContextMenuTarget::FileTree { row, is_dir };
        if self.context_menu_open_for(target) {
            return;
        }
        self.nebula_side_panel.selected = Some(path);
        self.nebula_context_menu = Some(context_menu::ContextMenu::new(target, (x, y), None));
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// 菜单动作执行时按行索引回查路径（树可能在菜单打开期间被节流刷新，
    /// 拿不到就当无操作，绝不落在别的行上）。
    pub fn file_tree_row_path(&self, row: usize) -> Option<(std::path::PathBuf, bool)> {
        self.nebula_side_panel
            .visible_row(row)
            .filter(|r| !r.is_parent)
            .map(|r| (r.path.clone(), r.is_dir))
    }

    pub fn request_delete_file_tree(&mut self, row: usize) {
        let Some((path, is_dir)) = self.file_tree_row_path(row) else { return };
        self.nebula_confirm = Some(NebulaConfirm::DeleteFileTreePath { path, is_dir });
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// 确认后的本地删除：送回收站（`FOF_ALLOWUNDO`），不是永久删除——
    /// 树紧挨终端、误触成本高，回收站是最后一道保险。
    pub fn confirm_delete_file_tree(&mut self, path: &std::path::Path) {
        self.nebula_confirm = None;
        match send_to_recycle_bin(path) {
            Ok(()) => self.nebula_side_panel.request_refresh(),
            Err(err) => self.nebula_side_panel.set_notice(format!("删除失败：{err}")),
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn open_sftp_panel_context_menu(&mut self, x: f32, y: f32) {
        if self.nebula_sftp_panel.is_none() {
            return;
        }
        self.nebula_context_menu =
            Some(context_menu::ContextMenu::new(ContextMenuTarget::SftpPanel, (x, y), None));
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn context_menu_hit(&self, x: f32, y: f32) -> ContextMenuHit {
        self.nebula_context_menu.as_ref().map_or(ContextMenuHit::Outside, |menu| {
            context_menu::hit_test(menu, self.ui_size_info(), self.window.scale_factor as f32, x, y)
        })
    }

    pub fn context_menu_hover(&mut self, x: f32, y: f32) -> ContextMenuHit {
        let hit = self.context_menu_hit(x, y);
        let action = match hit {
            ContextMenuHit::Action(action) => Some(action),
            ContextMenuHit::Outside | ContextMenuHit::Panel => None,
        };
        if self.nebula_context_menu.as_mut().is_some_and(|menu| menu.set_hover(action)) {
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
        hit
    }

    /// Resolve one menu click and start the close animation. A click inside
    /// the panel but between targets is swallowed without dismissing it.
    pub fn context_menu_click(&mut self, x: f32, y: f32) -> ContextMenuHit {
        let hit = self.context_menu_hit(x, y);
        if matches!(hit, ContextMenuHit::Action(_) | ContextMenuHit::Outside) {
            self.close_context_menu();
        }
        hit
    }

    pub fn close_context_menu(&mut self) {
        if let Some(menu) = self.nebula_context_menu.as_mut() {
            menu.begin_close();
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn chrome_hit(&self, x: f32, y: f32) -> ChromeHit {
        // Hit-testing must read the SAME layout the chrome was drawn with —
        // the UI-anchored SizeInfo — or clicks drift off their targets as
        // soon as the terminal is zoomed away from the base font size.
        chrome_hit_with_tabs(
            &self.ui_size_info(),
            self.window.scale_factor as f32,
            self.sidebar_model(),
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
            area: self.terminal_card_rect(),
            language_preference: self.nebula_language_preference,
            language: self.nebula_language,
            section: self.nebula_settings_section,
            hover: self.nebula_settings_hover,
            theme: self.nebula_theme,
            follow_system_theme: self.nebula_follow_system_theme,
            ghost: self.nebula_ghost_enabled,
            accept: self.nebula_accept,
            shell_label: {
                // Rich picked id (cmd/pwsh/nu/wsl:X) wins; else the 2-value
                // enum label. Icon comes from the same table the dropdown
                // rows use, so the setting always mirrors the menu.
                let id = self.nebula_shell_id.as_deref();
                let name = id
                    .map(crate::shell_detect::display_name_for_id)
                    .unwrap_or_else(|| self.nebula_shell.label().to_owned());
                let icon = crate::shell_detect::icon_for_id(
                    id.unwrap_or_else(|| self.nebula_shell.settings_value()),
                );
                format!("{icon}  {name}")
            },
            dropdown: self.nebula_settings_dropdown,
            shells: self
                .nebula_detected_shells
                .as_ref()
                .map(|shells| {
                    shells
                        .iter()
                        .map(|s| (s.id.clone(), s.name.clone(), s.program.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            shell_id: self.nebula_shell_id.clone(),
            startup_directory: self
                .nebula_startup_directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            font_family: self.nebula_font_family.clone(),
            font_size_px: self.font_size.as_px() / self.window.scale_factor as f32,
            fonts: self.nebula_font_families.clone(),
            font_notice: self.nebula_font_notice.clone(),
            hidden_hosts: self.nebula_hidden_hosts.clone(),
            fetch: self.nebula_fetch_enabled,
            powerline: self.nebula_powerline_enabled,
            keep_session: self.nebula_keep_session,
            opacity: self.nebula_window_opacity,
            dragging_opacity: self.nebula_settings_opacity_drag.map(|(target, _, _)| target),
            cursor_shape: self.nebula_cursor_shape,
            cursor_blink: self.nebula_cursor_blink,
            copy_on_select: self.nebula_copy_on_select,
            cjk_bold_regular: self.nebula_cjk_bold_regular,
            preview_bg: self.preview_terminal_bg(),
            preview_fg: {
                let bg = self.preview_terminal_bg();
                // 亮底配深字、暗底配浅字：预览要在任何自定义背景色上可读。
                let luma = 0.299 * bg.r as f32 + 0.587 * bg.g as f32 + 0.114 * bg.b as f32;
                if luma > 140.0 { Rgb::new(40, 44, 52) } else { Rgb::new(225, 228, 240) }
            },
            background: self.nebula_background,
            bg_hex_input: self.nebula_bg_hex_input.clone(),
            bg_hex_active: self.nebula_bg_hex_active,
            background_image: self.nebula_background_image.clone(),
            background_image_opacity: self.nebula_background_image_opacity,
            background_image_fit: self.nebula_background_image_fit,
            background_image_alignment: self.nebula_background_image_alignment,
            background_image_cover_chrome: self.nebula_background_image_cover_chrome,
            scroll: self.nebula_settings_scroll,
            keymap: keymap::EDITABLE_ACTIONS
                .iter()
                .map(|(action, ..)| keymap::effective_combo(action, &self.nebula_keymap))
                .collect(),
            keymap_capture: self.nebula_keymap_capture,
            keymap_capture_preview: self.nebula_keymap_capture_preview.clone(),
            sync_inputs: self.nebula_sync_inputs.clone(),
            sync_focus: self.nebula_sync_focus,
            sync_auto_pull: self.nebula_sync_auto_pull,
            sync_secret_set: self.nebula_sync_secret_set,
            sync_status: self.nebula_sync_status.clone(),
            sync_busy: self.nebula_sync_busy,
        }
    }

    pub fn set_settings_tab_active(&mut self, active: bool) {
        if self.nebula_settings_open == active {
            return;
        }
        self.nebula_settings_open = active;
        self.nebula_special_tab_active = active;
        if !active {
            self.commit_sync_field();
            self.nebula_settings_dropdown = None;
            self.nebula_settings_hover = SettingsHit::None;
            self.nebula_keymap_capture = None;
        } else {
            self.load_sync_state();
            // Each explicit visit starts at a predictable page origin.
            self.nebula_settings_scroll = 0.0;
        }
        self.pending_update.dirty = true;
    }

    pub fn set_special_tab_active(&mut self, active: bool) {
        self.nebula_special_tab_active = active;
        if !active {
            self.nebula_settings_open = false;
        }
    }

    pub fn set_ui_language(&mut self, preference: LanguagePreference) {
        if self.nebula_language_preference == preference {
            return;
        }
        self.nebula_language_preference = preference;
        self.nebula_language = preference.resolved();
        self.nebula_palette.set_language(self.nebula_language);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    fn apply_nebula_theme(&mut self, theme: NebulaTheme) {
        let previous_theme = self.nebula_theme;
        let theme_changed = previous_theme != theme;
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
        if theme_changed {
            // 旧 pane 可能持有应用通过 OSC 写入的上一主题颜色；交给窗口层在
            // 未持有任何终端锁时统一清理，避免只刷新当前焦点 pane。
            self.terminal_color_resolver
                .theme_changed(previous_theme.palette().term_bg, theme.palette().term_bg);
            self.pending_update.set_terminal_colors_dirty();
        }
        write_nebula_prompt_theme(theme);
        self.pending_update.dirty = true;
    }

    pub fn select_nebula_theme(&mut self, theme: NebulaTheme) {
        self.nebula_theme_preference = theme;
        // Clicking a concrete theme is an explicit manual choice. Automatic
        // mode must step aside instead of changing it again on the next OS
        // appearance event.
        self.nebula_follow_system_theme = false;
        self.window.set_theme(self.nebula_window_theme_override);
        self.apply_nebula_theme(theme);
        self.persist_nebula_settings();
        // Panel stays open so users can adjust several settings at once.
    }

    pub fn toggle_system_theme_following(&mut self) {
        self.nebula_follow_system_theme = !self.nebula_follow_system_theme;
        if self.nebula_follow_system_theme {
            // winit explicitly suppresses ThemeChanged for overridden
            // windows, so automatic mode must let the OS own this value.
            self.window.set_theme(None);
            self.nebula_system_theme =
                system_theme_snapshot(self.nebula_system_theme, self.window.theme());
        } else {
            self.window.set_theme(self.nebula_window_theme_override);
        }
        let theme = if self.nebula_follow_system_theme {
            self.nebula_system_theme
                .map(|system| {
                    self.nebula_theme_preference
                        .for_system_appearance(matches!(system, WinitTheme::Light))
                })
                .unwrap_or(self.nebula_theme_preference)
        } else {
            self.nebula_theme_preference
        };
        self.apply_nebula_theme(theme);
        self.persist_nebula_settings();
    }

    /// Apply a live operating-system appearance change without rewriting the
    /// stored theme family. This is intentionally a no-op in manual mode.
    pub fn system_theme_changed(&mut self, system_theme: WinitTheme) {
        self.sync_system_theme(Some(system_theme));
    }

    /// Refresh the system appearance independently from the window's cached
    /// theme. This also keeps manual-mode windows ready to switch immediately
    /// when the user enables automatic following.
    pub fn sync_system_theme(&mut self, system_theme: Option<WinitTheme>) {
        let Some(system_theme) = system_theme else { return };
        if self.nebula_system_theme == Some(system_theme) {
            return;
        }

        self.nebula_system_theme = Some(system_theme);
        if self.nebula_follow_system_theme {
            let theme = self
                .nebula_theme_preference
                .for_system_appearance(matches!(system_theme, WinitTheme::Light));
            self.apply_nebula_theme(theme);
        }
    }

    /// Remember a reloaded window-decoration preference without allowing it
    /// to suppress OS theme notifications while automatic mode is enabled.
    pub fn update_window_theme_override(&mut self, theme: Option<WinitTheme>) {
        self.nebula_window_theme_override = theme;
        self.window.set_theme(if self.nebula_follow_system_theme { None } else { theme });
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

    /// Open the "默认 Shell" picker (the settings row click): the same
    /// Toggle the inline shell picker in settings (expand/collapse the list).
    /// Toggle a settings combobox. All dropdowns share one field so opening
    /// one always closes the others.
    pub fn toggle_settings_dropdown(&mut self, dropdown: settings::SettingsDropdown) {
        if self.nebula_settings_dropdown == Some(dropdown) {
            self.nebula_settings_dropdown = None;
        } else {
            if dropdown == settings::SettingsDropdown::Shell {
                // Ensure shells are detected before opening.
                let _ = self
                    .nebula_detected_shells
                    .get_or_insert_with(crate::shell_detect::detect_shells);
            }
            if dropdown == settings::SettingsDropdown::Font {
                self.nebula_font_notice = None;
            }
            self.nebula_settings_dropdown = Some(dropdown);
        }
        self.pending_update.dirty = true;
    }

    pub fn close_settings_dropdown(&mut self) -> bool {
        if self.nebula_settings_dropdown.take().is_none() {
            return false;
        }
        self.nebula_bg_hex_active = false;
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    pub fn set_background_image_fit_option(&mut self, index: usize) {
        if let Some(fit) = settings::BACKGROUND_FIT_OPTIONS.get(index) {
            self.nebula_background_image_fit = *fit;
            self.persist_nebula_settings();
        }
        self.nebula_settings_dropdown = None;
        self.pending_update.dirty = true;
    }

    pub fn set_background_image_alignment_option(&mut self, index: usize) {
        if let Some(alignment) = settings::BACKGROUND_ALIGNMENT_OPTIONS.get(index) {
            self.nebula_background_image_alignment = *alignment;
            self.persist_nebula_settings();
        }
        self.nebula_settings_dropdown = None;
        self.pending_update.dirty = true;
    }

    pub fn set_accept_option(&mut self, index: usize) {
        if let Some(accept) = settings::ACCEPT_OPTIONS.get(index) {
            self.nebula_accept = *accept;
            self.persist_nebula_settings();
        }
        self.nebula_settings_dropdown = None;
        self.pending_update.dirty = true;
    }

    /// Returns true when the default cursor style changed (the caller then
    /// pushes the new default into every live terminal).
    pub fn set_cursor_shape_option(&mut self, index: usize) -> bool {
        self.nebula_settings_dropdown = None;
        self.pending_update.dirty = true;
        let Some(shape) = settings::CURSOR_SHAPE_OPTIONS.get(index).copied() else {
            return false;
        };
        if self.nebula_cursor_shape == shape {
            return false;
        }
        self.nebula_cursor_shape = shape;
        self.persist_nebula_settings();
        true
    }

    pub fn toggle_cursor_blink(&mut self) {
        self.nebula_cursor_blink = !self.nebula_cursor_blink;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn toggle_copy_on_select(&mut self) {
        self.nebula_copy_on_select = !self.nebula_copy_on_select;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// 同步拉到新历史后热加载（spec 003）：ghost 补全的内存副本只在
    /// 启动时 load，这里手动重读一次。
    pub fn reload_nebula_history(&mut self) {
        self.nebula_history = crate::nebula_history::NebulaHistory::load();
        self.pending_update.dirty = true;
    }

    pub fn toggle_cjk_bold_regular(&mut self) {
        self.nebula_cjk_bold_regular = !self.nebula_cjk_bold_regular;
        self.glyph_cache.wide_bold_use_regular = self.nebula_cjk_bold_regular;
        // 已缓存的 bold CJK 位图立即作废：切换要当场可见，不能等重启。
        self.reset_glyph_cache();
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// The default cursor style (shape + blink) every terminal should fall
    /// back to when no escape has overridden it.
    pub fn nebula_default_cursor_style(&self) -> nebula_terminal::vte::ansi::CursorStyle {
        nebula_terminal::vte::ansi::CursorStyle {
            shape: self.nebula_cursor_shape,
            blinking: self.nebula_cursor_blink,
        }
    }

    /// Terminal background color the live settings preview should show: the
    /// custom background wins, else the active theme's terminal background.
    fn preview_terminal_bg(&self) -> Rgb {
        self.nebula_background.unwrap_or(self.nebula_theme.palette().term_bg)
    }

    pub fn toggle_shell_picker(&mut self) {
        self.toggle_settings_dropdown(settings::SettingsDropdown::Shell);
    }

    pub fn close_shell_picker(&mut self) {
        if self.nebula_settings_dropdown == Some(settings::SettingsDropdown::Shell) {
            self.close_settings_dropdown();
        }
    }

    pub fn pick_startup_directory(&mut self) {
        let Some(path) = file_dialog::pick_startup_directory(&self.window) else { return };
        if !path.is_dir() {
            return;
        }

        self.nebula_startup_directory = Some(path);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn clear_startup_directory(&mut self) {
        if self.nebula_startup_directory.take().is_none() {
            return;
        }

        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub(crate) fn startup_directory(&self) -> Option<PathBuf> {
        self.nebula_startup_directory.as_ref().filter(|path| path.is_dir()).cloned()
    }

    pub fn toggle_font_picker(&mut self) {
        self.toggle_settings_dropdown(settings::SettingsDropdown::Font);
    }

    pub fn close_font_picker(&mut self) {
        if self.nebula_settings_dropdown == Some(settings::SettingsDropdown::Font) {
            self.close_settings_dropdown();
        }
    }

    pub fn effective_font(&self, base: &Font) -> Font {
        base.clone().with_family(self.nebula_font_family.clone())
    }

    fn apply_font_family(&mut self, family: String, base: &Font) {
        self.nebula_font_family = family;
        self.nebula_font_notice = None;
        let font = self.effective_font(base).with_size(self.font_size);
        self.pending_update.set_font(font);
        self.persist_nebula_settings();
        self.window.request_redraw();
    }

    pub fn set_terminal_font_by_index(&mut self, index: usize, base: &Font) {
        if let Some(family) = self.nebula_font_families.get(index).cloned() {
            self.apply_font_family(family, base);
            self.nebula_settings_dropdown = None;
            return;
        }
        if index != self.nebula_font_families.len() {
            return;
        }

        #[cfg(windows)]
        {
            let Some(source) = file_dialog::pick_font_file(&self.window) else { return };
            let stored = match crate::font_install::store_imported_font(&source) {
                Ok(stored) => stored,
                Err(error) => {
                    self.nebula_font_notice = Some(error);
                    self.nebula_settings_dropdown = None;
                    self.pending_update.dirty = true;
                    return;
                },
            };
            match self.glyph_cache.add_private_font(&stored.path) {
                Ok(families) => {
                    for family in &families {
                        if !self.nebula_font_families.iter().any(|known| known == family) {
                            self.nebula_font_families.push(family.clone());
                        }
                    }
                    self.nebula_font_families[1..]
                        .sort_by_key(|family| family.to_ascii_lowercase());
                    if let Some(family) = families.into_iter().next() {
                        self.apply_font_family(family, base);
                    }
                },
                Err(error) => {
                    if stored.created {
                        let _ = std::fs::remove_file(&stored.path);
                    }
                    self.nebula_font_notice = Some(format!("字体无法加载：{error}"));
                    self.pending_update.dirty = true;
                },
            }
            self.nebula_settings_dropdown = None;
        }
        #[cfg(not(windows))]
        self.open_user_config_file();
    }

    /// WT-style default-shell picker (command palette mode). Kept for compatibility.
    /// detected-shell dropdown as the "+" chevron, but confirming SETS the
    /// default instead of launching a tab. Replaces the old 2-value cycle.
    pub fn open_default_shell_picker(&mut self) {
        let shells =
            self.nebula_detected_shells.get_or_insert_with(crate::shell_detect::detect_shells);
        self.nebula_palette.set_default_shell_menu(shells);
        self.nebula_palette.open_default_picker();
        self.pending_update.dirty = true;
    }

    /// Apply a picked default shell: keep the raw id for persistence and the
    /// spawn override, and track the PTY-integrated executor family in the
    /// enum so the prompt bootstrap picks the right base.
    pub fn set_default_shell(&mut self, shell: &crate::shell_detect::DetectedShell) {
        if let Some(family) = NebulaShell::from_settings(&shell.id) {
            self.nebula_shell = family;
        }
        self.nebula_shell_id = Some(shell.id.clone());
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn set_default_shell_by_index(&mut self, index: usize) {
        let shell =
            self.nebula_detected_shells.as_ref().and_then(|shells| shells.get(index)).cloned();
        if let Some(shell) = shell {
            self.set_default_shell(&shell);
        }
        self.nebula_settings_dropdown = None;
        self.pending_update.dirty = true;
    }

    /// Restore a destination after the short Undo period. Config aliases only
    /// need to leave `hidden_hosts`; manually saved addresses are re-added to
    /// the saved list. Expired credentials intentionally remain deleted.
    pub fn restore_hidden_ssh_host(&mut self, index: usize) {
        let Some(host) = self.nebula_hidden_hosts.get(index).cloned() else { return };
        let pending_same_host =
            self.nebula_ssh_delete_undo.as_ref().is_some_and(|undo| undo.host == host);
        if pending_same_host && self.undo_delete_ssh_host() {
            return;
        }

        self.nebula_hidden_hosts.retain(|entry| entry != &host);
        let from_config = crate::ssh::ssh_config_hosts().iter().any(|entry| entry == &host);
        if !from_config && !self.nebula_saved_hosts.iter().any(|entry| entry == &host) {
            self.nebula_saved_hosts.insert(0, host);
        }
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        self.window.request_redraw();
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

    pub fn begin_settings_opacity_drag(&mut self, target: SettingsOpacityTarget, pointer_x: f32) {
        let slider = settings::opacity_slider_rect(
            &self.ui_size_info(),
            self.window.scale_factor as f32,
            self.terminal_card_rect(),
            self.nebula_settings_scroll,
            target,
        );
        self.nebula_settings_opacity_drag = Some((target, slider.0, slider.2));
        self.update_settings_opacity_drag(pointer_x);
    }

    pub fn update_settings_opacity_drag(&mut self, pointer_x: f32) -> bool {
        let Some((target, track_x, track_width)) = self.nebula_settings_opacity_drag else {
            return false;
        };
        let value = settings::opacity_from_pointer(pointer_x, (track_x, 0.0, track_width, 0.0));
        match target {
            SettingsOpacityTarget::Terminal => {
                if (self.nebula_window_opacity - value).abs() <= f32::EPSILON {
                    return true;
                }
                self.nebula_window_opacity = value;
                self.update_window_transparency();
            },
            SettingsOpacityTarget::BackgroundImage => {
                if (self.nebula_background_image_opacity - value).abs() <= f32::EPSILON {
                    return true;
                }
                self.nebula_background_image_opacity = value;
                self.update_window_transparency();
            },
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    pub fn finish_settings_opacity_drag(&mut self) -> bool {
        if self.nebula_settings_opacity_drag.take().is_none() {
            return false;
        }
        // 拖动过程只刷新画面，松手后集中落盘，避免连续写设置文件。
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
        true
    }

    /// 键盘快捷键仍可循环预设背景色（与色盘同一色板）。
    pub fn cycle_background_color(&mut self) {
        self.nebula_bg_palette_index =
            (self.nebula_bg_palette_index + 1) % settings::BACKGROUND_SWATCHES.len();
        self.nebula_background = Some(settings::BACKGROUND_SWATCHES[self.nebula_bg_palette_index]);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    /// 打开/关闭背景色浮层（色板 + 16 进制输入），草稿预填当前生效色。
    pub fn open_background_color_picker(&mut self) {
        let current = self.nebula_background.unwrap_or(self.colors[NamedColor::Background]);
        self.nebula_bg_hex_input = format!("#{:02X}{:02X}{:02X}", current.r, current.g, current.b);
        self.nebula_bg_hex_active = false;
        self.toggle_settings_dropdown(settings::SettingsDropdown::BackgroundColor);
    }

    /// 点选色板某格：应用、落盘并收起浮层。
    pub fn set_background_color_option(&mut self, index: usize) {
        if let Some(color) = settings::BACKGROUND_SWATCHES.get(index) {
            self.nebula_bg_palette_index = index;
            self.nebula_background = Some(*color);
            self.persist_nebula_settings();
        }
        self.close_settings_dropdown();
        self.pending_update.dirty = true;
    }

    pub fn focus_bg_hex_input(&mut self) {
        self.nebula_bg_hex_active = true;
        self.pending_update.dirty = true;
    }

    /// 追加 hex 字符：只收 `#` 与 16 进制位，总长 ≤ 7（`#RRGGBB`）。
    pub fn bg_hex_push(&mut self, ch: char) {
        let ok = ch == '#' || ch.is_ascii_hexdigit();
        if ok && self.nebula_bg_hex_input.chars().count() < 7 {
            self.nebula_bg_hex_input.push(ch);
            self.pending_update.dirty = true;
        }
    }

    pub fn bg_hex_backspace(&mut self) {
        if self.nebula_bg_hex_input.pop().is_some() {
            self.pending_update.dirty = true;
        }
    }

    /// 回车应用 16 进制草稿；解析失败保持浮层与草稿原样。
    pub fn bg_hex_commit(&mut self) -> bool {
        let Some(color) = settings::parse_hex_rgb(self.nebula_bg_hex_input.trim()) else {
            return false;
        };
        self.nebula_background = Some(color);
        self.persist_nebula_settings();
        self.close_settings_dropdown();
        self.pending_update.dirty = true;
        true
    }

    // ---- 设置→高级→同步（WebDAV） ----

    /// 打开设置时装载同步状态：url/username/auto_pull 来自
    /// `nebula_sync.txt`，密码/口令只查存在性（明文不进 UI 状态）。
    pub fn load_sync_state(&mut self) {
        let cfg = crate::sync::SyncConfig::load();
        self.nebula_sync_inputs[0] = cfg.url;
        self.nebula_sync_inputs[1] = cfg.username;
        self.nebula_sync_inputs[2].clear();
        self.nebula_sync_inputs[3].clear();
        self.nebula_sync_auto_pull = cfg.auto_pull;
        self.nebula_sync_secret_set =
            [crate::sync::has_password(), crate::sync::has_passphrase()];
        self.nebula_sync_focus = None;
    }

    /// 聚焦某个同步输入框；先提交上一个（点击切换即失焦保存）。
    pub fn focus_sync_field(&mut self, index: usize) {
        if self.nebula_sync_focus == Some(index) {
            return;
        }
        self.commit_sync_field();
        self.nebula_sync_focus = Some(index.min(3));
        self.pending_update.dirty = true;
    }

    pub fn sync_field_push(&mut self, ch: char) {
        let Some(index) = self.nebula_sync_focus else { return };
        if ch.is_control() {
            return;
        }
        // url/username 拒绝空白；密码/口令允许内部空格（trim 在保存侧）。
        if index < 2 && ch.is_whitespace() {
            return;
        }
        if self.nebula_sync_inputs[index].chars().count() < 256 {
            self.nebula_sync_inputs[index].push(ch);
            self.pending_update.dirty = true;
        }
    }

    pub fn sync_field_paste(&mut self, text: &str) {
        for ch in text.chars() {
            self.sync_field_push(ch);
        }
    }

    pub fn sync_field_backspace(&mut self) {
        let Some(index) = self.nebula_sync_focus else { return };
        if self.nebula_sync_inputs[index].pop().is_some() {
            self.pending_update.dirty = true;
        }
    }

    /// 失焦提交：url/username 写 `nebula_sync.txt`；密码/口令若有输入则
    /// 存入凭据管理器并清空缓冲。口令被弱口令闸拒绝时留在状态行。
    pub fn commit_sync_field(&mut self) {
        let Some(index) = self.nebula_sync_focus.take() else { return };
        self.pending_update.dirty = true;
        match index {
            0 | 1 => {
                let mut cfg = crate::sync::SyncConfig::load();
                cfg.url = self.nebula_sync_inputs[0].trim().to_owned();
                cfg.username = self.nebula_sync_inputs[1].trim().to_owned();
                cfg.auto_pull = self.nebula_sync_auto_pull;
                if let Err(err) = cfg.save() {
                    self.nebula_sync_status = Some((err, true));
                }
            },
            2 | 3 => {
                let secret = std::mem::take(&mut self.nebula_sync_inputs[index]);
                if secret.trim().is_empty() {
                    return;
                }
                let username = self.nebula_sync_inputs[1].trim().to_owned();
                let result = if index == 2 {
                    crate::sync::store_password(&username, &secret)
                } else {
                    crate::sync::store_passphrase(&username, &secret)
                };
                match result {
                    Ok(()) => {
                        self.nebula_sync_secret_set[index - 2] = true;
                        self.nebula_sync_status = Some((
                            if index == 2 {
                                "WebDAV 密码已保存到凭据管理器".to_owned()
                            } else {
                                "同步口令已保存到凭据管理器".to_owned()
                            },
                            false,
                        ));
                    },
                    Err(err) => self.nebula_sync_status = Some((err, true)),
                }
            },
            _ => {},
        }
    }

    /// Esc：丢弃当前草稿并失焦（url/username 还原为文件值）。
    pub fn cancel_sync_field(&mut self) {
        let Some(index) = self.nebula_sync_focus.take() else { return };
        let cfg = crate::sync::SyncConfig::load();
        match index {
            0 => self.nebula_sync_inputs[0] = cfg.url,
            1 => self.nebula_sync_inputs[1] = cfg.username,
            _ => self.nebula_sync_inputs[index].clear(),
        }
        self.pending_update.dirty = true;
    }

    pub fn toggle_sync_auto_pull(&mut self) {
        self.nebula_sync_auto_pull = !self.nebula_sync_auto_pull;
        let mut cfg = crate::sync::SyncConfig::load();
        cfg.url = self.nebula_sync_inputs[0].trim().to_owned();
        cfg.username = self.nebula_sync_inputs[1].trim().to_owned();
        cfg.auto_pull = self.nebula_sync_auto_pull;
        if let Err(err) = cfg.save() {
            self.nebula_sync_status = Some((err, true));
        }
        self.pending_update.dirty = true;
    }

    /// 推/拉按钮按下：提交草稿、置忙。实际网络动作由调用侧发事件。
    pub fn begin_sync_action(&mut self) -> bool {
        if self.nebula_sync_busy {
            return false;
        }
        self.commit_sync_field();
        self.nebula_sync_busy = true;
        self.nebula_sync_status = Some(("同步中…".to_owned(), false));
        self.pending_update.dirty = true;
        true
    }

    /// 后台同步线程回报（`NebulaSyncDone`）。
    pub fn sync_action_done(&mut self, message: &str, error: bool) {
        self.nebula_sync_busy = false;
        self.nebula_sync_status = Some((message.to_owned(), error));
        // 拉取可能改写了设置文件的凭据外字段；存在性也可能被首存翻转。
        self.nebula_sync_secret_set =
            [crate::sync::has_password(), crate::sync::has_passphrase()];
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// Pick a background image through the OS file dialog, then persist it and
    /// refresh the renderer's cached wallpaper. On non-Windows platforms the
    /// native dialog isn't wired up, so we fall back to opening the settings
    /// file for the path to be entered by hand.
    /// Native save dialog for a workspace export, pre-filled with
    /// `default_name`. `None` when the user cancels.
    pub fn save_workspace_dialog(&self, default_name: &str) -> Option<std::path::PathBuf> {
        file_dialog::save_workspace_file(&self.window, default_name)
    }

    /// Native open dialog for a workspace import. `None` when cancelled.
    pub fn pick_workspace_dialog(&self) -> Option<std::path::PathBuf> {
        file_dialog::pick_workspace_file(&self.window)
    }

    pub fn pick_background_image(&mut self) {
        #[cfg(windows)]
        {
            if let Some(path) = file_dialog::pick_image_file(&self.window) {
                self.nebula_background_image = Some(path);
                self.persist_nebula_settings();
                self.renderer.invalidate_background_image();
                self.update_window_transparency();
                self.pending_update.dirty = true;
            }
        }
        #[cfg(not(windows))]
        {
            self.open_user_config_file();
        }
    }

    pub fn clear_background_image(&mut self) {
        if self.nebula_background_image.take().is_some() {
            self.persist_nebula_settings();
            self.renderer.invalidate_background_image();
            self.update_window_transparency();
            self.pending_update.dirty = true;
        }
    }

    pub fn request_toggle_background_image_cover_chrome(&mut self) {
        if self.nebula_background_image_cover_chrome {
            self.nebula_background_image_cover_chrome = false;
            self.persist_nebula_settings();
        } else {
            self.nebula_confirm = Some(NebulaConfirm::EnableBackgroundImageCoverChrome);
        }
        self.pending_update.dirty = true;
    }

    pub fn confirm_background_image_cover_chrome(&mut self) {
        self.nebula_confirm = None;
        self.nebula_background_image_cover_chrome = true;
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub fn open_user_config_file(&mut self) {
        self.persist_nebula_settings();
        let active_lua = self.nebula_config_paths.first().filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lua"))
        });
        let path = active_lua.cloned().or_else(|| crate::config::source::default_lua_path().ok());
        let Some(path) = path else {
            log::error!(
                target: crate::logging::LOG_TARGET_CONFIG,
                "Unable to determine Lua config path"
            );
            return;
        };
        if !path.exists() {
            let language = crate::config::template::resolve_template_language(
                Some(self.nebula_language_preference.as_str()),
                None,
                crate::config::template::system_locale().as_deref(),
            )
            .unwrap_or(crate::config::template::TemplateLanguage::EnUs);
            if let Err(error) = crate::config::template::ensure_user_lua_config(&path, language) {
                log::error!(
                    target: crate::logging::LOG_TARGET_CONFIG,
                    "Unable to create Lua config {:?}: {error}",
                    path
                );
                return;
            }
        }
        #[cfg(windows)]
        let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&path).spawn();
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
        self.pending_update.dirty = true;
    }

    pub fn reset_appearance_settings(&mut self) {
        self.nebula_theme_preference = NebulaTheme::default();
        self.nebula_follow_system_theme = false;
        self.window.set_theme(self.nebula_window_theme_override);
        self.nebula_theme = self.nebula_theme_preference;
        let defaults = self.nebula_default_colors;
        self.nebula_theme.apply_term_colors(&mut self.colors, &defaults);
        write_nebula_prompt_theme(self.nebula_theme);
        self.nebula_window_opacity = 1.0;
        self.nebula_background = None;
        self.nebula_background_image = None;
        self.nebula_background_image_opacity = 0.38;
        self.nebula_background_image_fit = BackgroundImageFit::default();
        self.nebula_background_image_alignment = BackgroundImageAlignment::default();
        self.nebula_background_image_cover_chrome = false;
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

    /// Open the new-tab dropdown: detected shells (installed-shell order) plus
    /// any config profiles. Detection runs once and is cached — the chevron
    /// beside the "+" opens this, mirroring Windows Terminal's profile menu.
    pub fn open_shell_menu(&mut self, profiles: &[String]) {
        let shells =
            self.nebula_detected_shells.get_or_insert_with(crate::shell_detect::detect_shells);
        self.nebula_palette.set_shell_menu(shells, profiles);
        self.nebula_palette.open_profiles();
        self.pending_update.dirty = true;
    }

    /// Open a terminal-directory picker backed by the same frecency model as
    /// ghost text and filesystem completion. No shell command is installed.
    pub fn open_directory_picker(&mut self) {
        let paths = self.directory_history.search("", 128);
        self.nebula_palette.set_directories(paths);
        self.nebula_palette.open_directories();
        self.pending_update.dirty = true;
    }

    fn refresh_directory_picker(&mut self) {
        if !self.nebula_palette.is_picking_directory() {
            return;
        }
        let query = self.nebula_palette.query().to_owned();
        let paths = self.directory_history.search(&query, 128);
        self.nebula_palette.set_directories(paths);
    }

    pub fn command_palette_open(&self) -> bool {
        self.nebula_palette.is_open()
    }

    pub fn command_palette_picking_default(&self) -> bool {
        self.nebula_palette.is_picking_default()
    }

    pub fn command_palette_picker_open(&self) -> bool {
        self.nebula_palette.is_picker()
    }

    pub fn close_command_palette(&mut self) {
        self.nebula_palette.close();
        self.pending_update.dirty = true;
    }

    pub fn palette_input_char(&mut self, c: char) {
        self.nebula_palette.input_char(c);
        self.refresh_directory_picker();
        self.pending_update.dirty = true;
    }

    pub fn palette_input_text(&mut self, text: &str) {
        self.nebula_palette.input_text(text);
        self.refresh_directory_picker();
        self.pending_update.dirty = true;
    }

    pub fn palette_select_all(&mut self) {
        self.nebula_palette.select_all();
        self.pending_update.dirty = true;
    }

    pub fn palette_selected_text(&self) -> Option<String> {
        self.nebula_palette.selected_text()
    }

    pub fn palette_backspace(&mut self) {
        self.nebula_palette.backspace();
        self.refresh_directory_picker();
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

    /// Update palette hover state. `row` is the visual row index, or `None` when
    /// the mouse left the palette area.
    pub fn palette_hover(&mut self, pos: (f32, f32), row: Option<usize>) -> bool {
        if self.nebula_palette.pointer_hover(pos, row) {
            self.pending_update.dirty = true;
            return true;
        }
        false
    }

    /// The number of visible palette results (for hover boundary checking).
    pub fn nebula_palette_visible_count(&self) -> usize {
        self.nebula_palette.visible_count()
    }

    /// Toggle the right-side drawer (directory tree / git status).
    pub fn toggle_side_panel(&mut self, view: side_panel::PanelView) {
        let was_open = self.nebula_side_panel.open;
        if self.nebula_sftp_panel.take().is_some() {
            self.nebula_side_panel.open = true;
            self.nebula_side_panel.view = view;
        } else {
            self.nebula_side_panel.toggle(view);
        }
        // The drawer reserves real grid width, so opening/closing it (not
        // just switching views) must reflow the grid like the left sidebar.
        if self.nebula_side_panel.open != was_open {
            let size =
                PhysicalSize::new(self.size_info.width() as u32, self.size_info.height() as u32);
            self.pending_update.set_dimensions(size);
        }
        self.window.request_redraw();
        self.pending_update.dirty = true;
    }

    // ---- tab rename caret editing (the rename box is a real text field) ----

    /// Insert `text` at the caret. A pending select-all is replaced wholesale
    /// (type-to-overwrite), matching every native text field.
    pub fn tab_rename_insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|character| !character.is_control()).collect();
        if text.is_empty() {
            return;
        }
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
        buf.insert_str(byte, &text);
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

    pub fn tab_rename_select_all(&mut self) {
        if let Some((_, text)) = self.nebula_tab_rename.as_ref() {
            self.nebula_tab_rename_select_all = !text.is_empty();
            self.nebula_tab_rename_caret = text.chars().count();
            self.pending_update.dirty = true;
        }
    }

    pub fn tab_rename_selected_text(&self) -> Option<String> {
        self.nebula_tab_rename_select_all
            .then(|| self.nebula_tab_rename.as_ref().map(|(_, text)| text.clone()))
            .flatten()
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
        if self.sftp_view_active() {
            return;
        }
        if self.nebula_side_panel.sync(cwd) {
            self.pending_update.dirty = true;
        }
    }

    /// Whether the drawer currently shows the SFTP view. The SFTP panel is a
    /// window-level object, but its VIEW follows the focused pane: focusing a
    /// local tab flips the drawer back to the directory tree (the SFTP
    /// connection stays warm for the next switch), so the tree keeps
    /// following tab switches instead of being captured by one SSH session.
    pub(crate) fn sftp_view_active(&self) -> bool {
        self.nebula_sftp_panel.is_some() && self.nebula_sftp_routed
    }

    /// Re-route the drawer to the focused pane's identity, called every draw.
    /// `focused_ssh` is the pane's stable SSH destination, `None` for local
    /// panes.
    pub fn route_side_panel(&mut self, focused_ssh: Option<&str>) {
        let routed = match (self.nebula_sftp_panel.as_ref(), focused_ssh) {
            (Some(panel), Some(destination)) => panel.snapshot().destination == destination,
            _ => false,
        };
        if self.nebula_sftp_routed != routed {
            self.nebula_sftp_routed = routed;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn choose_side_panel_directory(&mut self) {
        let Some(path) = file_dialog::pick_side_panel_directory(&self.window) else {
            return;
        };
        if self.nebula_side_panel.set_custom_root(path) {
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    pub fn follow_focused_directory(&mut self) {
        if self.nebula_side_panel.clear_custom_root() {
            self.pending_update.dirty = true;
            self.window.request_redraw();
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

    pub fn open_sftp_panel(
        &mut self,
        destination: String,
        proxy: winit::event_loop::EventLoopProxy<crate::event::Event>,
    ) -> Result<(), String> {
        let was_open = self.nebula_side_panel.open;
        let controller = crate::ssh_sftp::SftpController::new(destination, proxy, self.window.id())
            .map_err(|err| format!("无法打开 SFTP: {err}"))?;
        self.nebula_side_panel.search_unfocus(false);
        self.nebula_side_panel.commit_unfocus();
        self.nebula_side_panel.open = true;
        self.nebula_sftp_panel = Some(sftp_panel::SftpPanel::new(controller));
        if !was_open {
            let size =
                PhysicalSize::new(self.size_info.width() as u32, self.size_info.height() as u32);
            self.pending_update.set_dimensions(size);
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
        Ok(())
    }

    pub fn close_sftp_panel(&mut self) {
        if let Some(panel) = self.nebula_sftp_panel.take() {
            panel.cancel_transfer();
        }
        if self.nebula_side_panel.open {
            self.nebula_side_panel.open = false;
            let size =
                PhysicalSize::new(self.size_info.width() as u32, self.size_info.height() as u32);
            self.pending_update.set_dimensions(size);
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn sftp_layout(&self) -> sftp_panel::SftpLayout {
        sftp_panel::layout(&self.side_panel_layout(), self.window.scale_factor as f32)
    }

    pub fn sftp_hit(&self, x: f32, y: f32) -> sftp_panel::SftpHit {
        // A hidden SFTP view (drawer re-routed to a local pane) must not eat
        // clicks that belong to the directory tree drawn in its place.
        if !self.sftp_view_active() {
            return sftp_panel::SftpHit::None;
        }
        let Some(panel) = self.nebula_sftp_panel.as_ref() else {
            return sftp_panel::SftpHit::None;
        };
        let working = panel.snapshot().phase == crate::ssh_sftp::SftpPhase::Working;
        sftp_panel::hit_test(&self.sftp_layout(), working, x, y)
    }

    pub fn sftp_set_hover(&mut self, hit: sftp_panel::SftpHit) -> bool {
        self.nebula_sftp_panel.as_mut().is_some_and(|panel| panel.set_hover(hit))
    }

    pub fn sftp_click(&mut self, hit: sftp_panel::SftpHit) {
        use sftp_panel::SftpHit;
        match hit {
            SftpHit::Close => self.close_sftp_panel(),
            SftpHit::Path => {
                if let Some(panel) = self.nebula_sftp_panel.as_mut() {
                    panel.begin_path();
                }
            },
            SftpHit::Filter => {
                if let Some(panel) = self.nebula_sftp_panel.as_mut() {
                    panel.begin_filter();
                }
            },
            SftpHit::Row(index) => {
                let selected =
                    self.nebula_sftp_panel.as_mut().and_then(|panel| panel.select_row(index));
                if let Some((entry, true)) = selected {
                    let navigated =
                        self.nebula_sftp_panel.as_mut().is_some_and(|panel| panel.navigate(&entry));
                    if !navigated {
                        self.sftp_download_entry(entry);
                    }
                }
            },
            SftpHit::Cancel => {
                if let Some(panel) = self.nebula_sftp_panel.as_ref() {
                    panel.cancel_transfer();
                }
            },
            SftpHit::None | SftpHit::Inside => {},
        }
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    pub fn sftp_refresh(&mut self) {
        if let Some(panel) = self.nebula_sftp_panel.as_ref() {
            panel.refresh();
        }
    }

    pub fn sftp_pick_upload_files(&mut self) {
        let paths = file_dialog::pick_upload_files(&self.window);
        if !paths.is_empty()
            && let Some(panel) = self.nebula_sftp_panel.as_ref()
        {
            panel.upload_paths(paths);
        }
    }

    pub fn sftp_pick_upload_directory(&mut self) {
        if let Some(path) = file_dialog::pick_upload_directory(&self.window)
            && let Some(panel) = self.nebula_sftp_panel.as_ref()
        {
            panel.upload_paths(vec![path]);
        }
    }

    pub fn sftp_begin_create_directory(&mut self) {
        if let Some(panel) = self.nebula_sftp_panel.as_mut() {
            panel.begin_create_directory();
        }
    }

    pub fn sftp_upload_dropped_paths(&mut self, paths: Vec<std::path::PathBuf>) -> bool {
        if paths.is_empty() {
            return false;
        }
        let Some(panel) = self.nebula_sftp_panel.as_ref() else { return false };
        panel.upload_paths(paths);
        self.pending_update.dirty = true;
        self.window.request_redraw();
        true
    }

    pub fn sftp_download_row(&mut self, index: usize) {
        if let Some(entry) =
            self.nebula_sftp_panel.as_ref().and_then(|panel| panel.visible_entry(index))
        {
            self.sftp_download_entry(entry);
        }
    }

    fn sftp_download_entry(&mut self, entry: crate::ssh_sftp::SftpEntry) {
        let Some(directory) = file_dialog::pick_download_directory(&self.window) else {
            return;
        };
        if let Some(panel) = self.nebula_sftp_panel.as_ref() {
            panel.download(entry, directory);
        }
    }

    pub fn sftp_begin_rename_row(&mut self, index: usize) {
        let entry = self.nebula_sftp_panel.as_ref().and_then(|panel| panel.visible_entry(index));
        if let (Some(panel), Some(entry)) = (self.nebula_sftp_panel.as_mut(), entry) {
            panel.begin_rename(entry);
        }
    }

    pub fn sftp_request_delete_row(&mut self, index: usize) {
        if let Some(entry) =
            self.nebula_sftp_panel.as_ref().and_then(|panel| panel.visible_entry(index))
        {
            self.nebula_confirm = Some(NebulaConfirm::DeleteSftp { entry });
        }
    }

    pub fn sftp_confirm_delete(&mut self, entry: crate::ssh_sftp::SftpEntry) {
        self.nebula_confirm = None;
        if let Some(panel) = self.nebula_sftp_panel.as_ref() {
            panel.delete(entry);
        }
    }

    pub fn step_chrome_anims(&mut self) {
        self.nebula_ui_anims.step(
            !self.nebula_sidebar_collapsed,
            self.nebula_side_panel.open,
            self.nebula_ssh_editor_open,
        );
    }

    pub fn chrome_animating(&self) -> bool {
        self.nebula_ui_anims.animating(!self.nebula_sidebar_collapsed, self.nebula_side_panel.open)
    }

    pub fn left_sidebar_progress(&self) -> f32 {
        self.nebula_ui_anims.left_sidebar.value()
    }

    pub fn left_sidebar_visible(&self) -> bool {
        self.nebula_ui_anims.left_sidebar.visible(!self.nebula_sidebar_collapsed)
    }

    /// DPI 变化时按同一比例重标 UI 角色字号（等价于配置字号 × 新缩放）。
    pub(crate) fn rescale_ui_font(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.nebula_ui_font.px *= factor;
        }
    }

    /// UI 角色字号相对当前终端字号的比率。仅供尚未角色化的历史缩放路径
    /// （`begin_chrome_text_scaled`、链接预览的手动锚定）使用；新代码一律
    /// 走 `draw_chrome_text*` / `draw_ui_text*`，它们直接从字体角色取真实
    /// 字号与 metrics。
    pub(crate) fn ui_text_scale(&self) -> f32 {
        let cur = self.font_size.as_px();
        if cur <= 0.0 || self.nebula_ui_font.px <= 0.0 {
            return 1.0;
        }
        self.nebula_ui_font.px / cur
    }

    /// [`Self::size_info`] 的 UI 版本：cell 尺寸来自 UI 字体角色的真实
    /// 栅格 metrics（[`Self::refresh_ui_font`] 量取）。chrome / 设置 /
    /// 浮层的布局与命中测试统一用它，与按同一角色栅格化的 UI 文本严格
    /// 同源；终端网格、damage、光标继续用原 `size_info`。
    pub(crate) fn ui_size_info(&self) -> SizeInfo {
        let mut ui = self.size_info;
        let (cell_w, cell_h) = self.nebula_ui_font.cell;
        ui.cell_width = cell_w;
        ui.cell_height = cell_h;
        ui
    }

    /// Pin the glyph cache's UI font role to the anchor size and refresh the
    /// cell the chrome layout steps by. Runs at construction and on every
    /// terminal font change — zoom, family, DPI all funnel through the font
    /// update, so this is the single place the role can go stale.
    fn refresh_ui_font(&mut self, config: &UiConfig) {
        let ui_size = FontSize::from_px(self.nebula_ui_font.px);
        let metrics = self.glyph_cache.set_ui_font_size(ui_size);
        self.nebula_ui_font.cell = compute_cell_size(config, &metrics);
        let ratio = self.ui_text_scale();
        // Unconditional breadcrumb (tiny, a handful of lines per session):
        // diagnosing "the sidebar zooms with the terminal" reports needs this
        // from USER instances, which never run with NEBULA_DEBUG_LOG set.
        let line = format!(
            "[{}] ui_anchor ratio={ratio:.3} ui_font_px={:.1} font_px={:.1} scale={:.2} term_cell={}x{} ui_cell={:?}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            self.nebula_ui_font.px,
            self.font_size.as_px(),
            self.window.scale_factor,
            self.size_info.cell_width,
            self.size_info.cell_height,
            self.nebula_ui_font.cell
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(nebula_data_dir().join("ui_anchor.log"))
        {
            use std::io::Write as _;
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Geometry of the rounded terminal card in physical pixels `(x, y, w, h)`.
    /// The card floats on the shell backdrop: flush-ish against the sidebar on
    /// the left and the top bar above (they share the shell color, so no seam
    /// is needed there), with a visible [`UI_CARD_SEAM_LOGICAL`] gap of shell
    /// color on the right and bottom edges. The grid's own padding
    /// (`content_pad_x` / `chrome_reserve`) is larger than the card inset, so
    /// all cell content lands inside the card.
    pub(crate) fn terminal_card_rect(&self) -> (f32, f32, f32, f32) {
        let scale = self.window.scale_factor as f32;
        let s = |v: f32| (v * scale).round();
        let seam = s(UI_CARD_SEAM_LOGICAL);
        // Left edge rides the sidebar's fold animation (same swift-out cubic
        // as the panel slide in `chrome_tab_layout`), so collapsing the
        // sidebar reads as the terminal card gliding left to claim the space
        // instead of snapping. Resting expanded: just past the sidebar
        // panel's right edge (`sw - 12` logical, see `chrome_tab_layout`);
        // resting collapsed: the chrome margin.
        let t = self.left_sidebar_progress().clamp(0.0, 1.0);
        let sw = (SIDEBAR_W_LOGICAL * scale).round();
        let x = s(8.0) + t * (sw - s(4.0) - s(8.0));
        // Top edge: the top bar's bottom (margin 8 + bar height 40, matching
        // `draw_chrome`), plus a seam so the card visibly floats below it.
        let y = s(8.0 + 40.0) + seam;
        // Right edge follows the file/git drawer the same way: as it slides
        // in, the card cedes its width (drawer width + margin) plus the seam.
        let dt = self.nebula_ui_anims.right_drawer.value().clamp(0.0, 1.0);
        let drawer = dt * (side_panel::PANEL_W_LOGICAL * scale + s(8.0));
        let w = (self.size_info.width() - drawer - seam - x).max(0.0);
        let h = (self.size_info.height() - seam - y).max(0.0);
        (x, y, w, h)
    }

    pub fn side_panel_visible(&self) -> bool {
        self.nebula_ui_anims.right_drawer.visible(self.nebula_side_panel.open)
    }

    /// Sidebar content model for `chrome_tab_layout` — the single place the
    /// section states are read, so drawing / hit-testing / wheel agree.
    pub(super) fn sidebar_model(&self) -> chrome::SidebarModel {
        chrome::SidebarModel {
            tab_count: self.nebula_tab_labels.len().max(1),
            host_count: self.nebula_ssh_hosts.len(),
            tabs_open: self.nebula_tabs_section_open,
            hosts_open: self.nebula_hosts_section_open,
            tabs_scroll: self.nebula_tabs_scroll,
            hosts_scroll: self.nebula_hosts_scroll,
        }
    }

    /// Toggle a sidebar section's accordion fold (click on its caption).
    pub fn toggle_sidebar_section(&mut self, hosts: bool) {
        if hosts {
            self.nebula_hosts_section_open = !self.nebula_hosts_section_open;
        } else {
            self.nebula_tabs_section_open = !self.nebula_tabs_section_open;
        }
        self.pending_update.dirty = true;
    }

    /// Toggle the queue entry now; the expanded panel will consume the same
    /// state in the next integration stage, so the entry's hit contract does
    /// not need to change when real queue content lands.
    pub fn toggle_message_queue_entry(&mut self) {
        self.nebula_message_queue_entry.toggle();
        self.pending_update.dirty = true;
        self.window.request_redraw();
    }

    /// Route a mouse-wheel tick over the sidebar into the section under the
    /// pointer. Returns true when consumed (pointer was over a section band).
    pub fn sidebar_wheel(&mut self, x: f32, y: f32, rows: i32) -> bool {
        if !self.left_sidebar_visible() {
            return false;
        }
        let layout = chrome_tab_layout(
            &self.ui_size_info(),
            self.window.scale_factor as f32,
            self.sidebar_model(),
            self.left_sidebar_progress(),
        );
        let (px, _, pw, _) = layout.panel;
        if pw <= 0.0 || x < px || x > px + pw {
            return false;
        }
        let scroll =
            |cur: usize, max: usize| -> usize { (cur as i32 + rows).clamp(0, max as i32) as usize };
        // Band membership includes each section's header so the wheel works
        // right up against the caption.
        if y >= layout.tabs_header.1 && y <= layout.tabs_band.1 {
            self.nebula_tabs_scroll = scroll(self.nebula_tabs_scroll, layout.tabs_max_scroll);
        } else if y >= layout.hosts_header.1
            && y <= layout.hosts_band.1.max(layout.hosts_header.1 + layout.hosts_header.3)
        {
            self.nebula_hosts_scroll = scroll(self.nebula_hosts_scroll, layout.hosts_max_scroll);
        } else {
            return false;
        }
        self.pending_update.dirty = true;
        true
    }

    /// Auto-save an SSH destination the user typed and successfully connected
    /// to — armed at OSC 133;C, confirmed by a remote `NEBULA|` title or a
    /// session that outlived [`crate::ssh::SAVE_MIN_SESSION`]. Tabby-style
    /// recents: most recent first, deduped, capped. An already-listed host
    /// only refreshes its recency (for the next launch) — the visible list
    /// never jumps while the user is looking at it.
    pub fn nebula_save_ssh_host(&mut self, host: &str) {
        const SAVED_HOSTS_CAP: usize = 20;
        if host.is_empty() {
            return;
        }
        self.nebula_saved_hosts.retain(|h| h != host);
        self.nebula_hidden_hosts.retain(|h| h != host);
        self.nebula_saved_hosts.insert(0, host.to_owned());
        self.nebula_saved_hosts.truncate(SAVED_HOSTS_CAP);
        if !self.nebula_ssh_hosts.iter().any(|h| h == host) {
            // New host: insert below the pinned block, above everything else.
            let at = self
                .nebula_ssh_hosts
                .iter()
                .take_while(|h| self.nebula_pinned_hosts.contains(h))
                .count();
            self.nebula_ssh_hosts.insert(at, host.to_owned());
        }
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    // ---- 键位自定义（spec 002）----

    /// 设置页点击某行的 keycap：进入捕获态（下一次按键成为新绑定）。
    pub fn keymap_begin_capture(&mut self, row: usize) {
        if row < keymap::EDITABLE_ACTIONS.len() {
            self.nebula_keymap_capture = Some(row);
            self.nebula_keymap_capture_preview.clear();
            self.pending_update.dirty = true;
        }
    }

    pub fn keymap_cancel_capture(&mut self) {
        if self.nebula_keymap_capture.take().is_some() {
            self.nebula_keymap_capture_preview.clear();
            self.pending_update.dirty = true;
        }
    }

    /// 捕获态的实时修饰键回显（ModifiersChanged 与纯修饰键按下都会走到
    /// 这里）：按住 Ctrl 立即显示 "Ctrl+…"，全部松开回到占位提示。
    pub fn keymap_capture_preview(&mut self, mods: winit::keyboard::ModifiersState) {
        if self.nebula_keymap_capture.is_none() {
            return;
        }
        let prefix = keymap::mods_prefix(mods);
        if self.nebula_keymap_capture_preview != prefix {
            self.nebula_keymap_capture_preview = prefix;
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }

    /// 捕获完成：`combo` 归属该行动作。同 combo 的旧自定义行被移除（键随
    /// 最后写入者），该动作旧的自定义行也移除（一动作一自定义键）。
    pub fn keymap_assign(&mut self, row: usize, combo: String) {
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row) else { return };
        let name = keymap::action_storage_name(action);
        self.nebula_keybinds.retain(|(c, a)| c != &combo && !a.eq_ignore_ascii_case(&name));
        self.nebula_keybinds.push((combo, name));
        self.keymap_commit();
    }

    /// 捕获态里按 Backspace：删除该动作的自定义绑定，回落内置默认。
    pub fn keymap_clear_custom(&mut self, row: usize) {
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row) else { return };
        let name = keymap::action_storage_name(action);
        self.nebula_keybinds.retain(|(_, a)| !a.eq_ignore_ascii_case(&name));
        self.keymap_commit();
    }

    fn keymap_commit(&mut self) {
        self.nebula_keymap_capture = None;
        self.nebula_keymap_capture_preview.clear();
        self.nebula_keymap = keymap::build_bindings(&self.nebula_keybinds);
        self.persist_nebula_settings();
        self.pending_update.dirty = true;
    }

    pub(crate) fn persist_nebula_settings(&mut self) {
        settings::nebula_settings_write(&settings::NebulaRuntimeSettings {
            language: self.nebula_language_preference,
            ghost: self.nebula_ghost_enabled,
            accept: self.nebula_accept,
            shell: self.nebula_shell,
            shell_id: self.nebula_shell_id.clone(),
            startup_directory: self.nebula_startup_directory.clone(),
            font_family: self.nebula_font_family.clone(),
            fetch: self.nebula_fetch_enabled,
            powerline: self.nebula_powerline_enabled,
            keep_session: self.nebula_keep_session,
            opacity: self.nebula_window_opacity,
            background: self.nebula_background,
            background_image: self.nebula_background_image.clone(),
            background_image_opacity: self.nebula_background_image_opacity,
            background_image_fit: self.nebula_background_image_fit,
            background_image_alignment: self.nebula_background_image_alignment,
            background_image_cover_chrome: self.nebula_background_image_cover_chrome,
            font_size: Some(self.font_size.as_px() / self.window.scale_factor as f32),
            cursor_shape: self.nebula_cursor_shape,
            cursor_blink: self.nebula_cursor_blink,
            copy_on_select: self.nebula_copy_on_select,
            cjk_bold_regular: self.nebula_cjk_bold_regular,
            theme: self.nebula_theme_preference,
            follow_system_theme: self.nebula_follow_system_theme,
            pinned_hosts: self.nebula_pinned_hosts.clone(),
            saved_hosts: self.nebula_saved_hosts.clone(),
            hidden_hosts: self.nebula_hidden_hosts.clone(),
            keybinds: self.nebula_keybinds.clone(),
        });
        self.nebula_settings_mtime = settings::nebula_settings_mtime();
    }

    fn reload_nebula_settings_if_changed(&mut self, config: &UiConfig) {
        let mtime = settings::nebula_settings_mtime();
        if mtime == self.nebula_settings_mtime {
            return;
        }

        let settings = settings::nebula_settings_load(config);
        self.nebula_language_preference = settings.language;
        self.nebula_language = settings.language.resolved();
        self.nebula_palette.set_language(self.nebula_language);
        let image_changed = settings.background_image != self.nebula_background_image;
        let font_changed = settings.font_family != self.nebula_font_family;
        self.nebula_theme_preference = settings.theme;
        let follow_system_changed = self.nebula_follow_system_theme != settings.follow_system_theme;
        self.nebula_follow_system_theme = settings.follow_system_theme;
        if follow_system_changed {
            self.window.set_theme(if settings.follow_system_theme {
                None
            } else {
                self.nebula_window_theme_override
            });
            if settings.follow_system_theme {
                self.nebula_system_theme =
                    system_theme_snapshot(self.nebula_system_theme, self.window.theme());
            }
        }
        let active_theme = if settings.follow_system_theme {
            self.nebula_system_theme
                .map(|system| {
                    settings.theme.for_system_appearance(matches!(system, WinitTheme::Light))
                })
                .unwrap_or(settings.theme)
        } else {
            settings.theme
        };
        if active_theme != self.nebula_theme {
            // Hand-edited theme or automatic-mode setting: apply and publish
            // it exactly like an in-panel selection would.
            self.apply_nebula_theme(active_theme);
        }
        self.nebula_ghost_enabled = settings.ghost;
        self.nebula_accept = settings.accept;
        self.nebula_shell = settings.shell;
        self.nebula_shell_id = settings.shell_id;
        self.nebula_startup_directory = settings.startup_directory;
        self.nebula_font_family = settings.font_family;
        if font_changed {
            #[cfg(windows)]
            {
                self.nebula_font_families = self.glyph_cache.refresh_private_fonts();
                self.nebula_font_families
                    .retain(|family| family != crate::font_install::REQUIRED_FONT_FAMILY);
                self.nebula_font_families
                    .insert(0, crate::font_install::REQUIRED_FONT_FAMILY.to_owned());
            }
            let font = self.effective_font(&config.font).with_size(self.font_size);
            self.pending_update.set_font(font);
        }
        self.nebula_fetch_enabled = settings.fetch;
        self.nebula_powerline_enabled = settings.powerline;
        self.nebula_keep_session = settings.keep_session;
        if self.nebula_cjk_bold_regular != settings.cjk_bold_regular {
            // 字形层策略变了：已缓存的 bold CJK 位图作废，清缓存重栅格。
            self.nebula_cjk_bold_regular = settings.cjk_bold_regular;
            self.glyph_cache.wide_bold_use_regular = settings.cjk_bold_regular;
            self.reset_glyph_cache();
        }
        self.nebula_window_opacity = settings.opacity;
        self.nebula_background = if settings.follow_system_theme {
            Some(active_theme.palette().term_bg)
        } else {
            settings.background
        };
        self.nebula_background_image = settings.background_image;
        self.nebula_background_image_opacity = settings.background_image_opacity;
        self.nebula_background_image_fit = settings.background_image_fit;
        self.nebula_background_image_alignment = settings.background_image_alignment;
        self.nebula_background_image_cover_chrome = settings.background_image_cover_chrome;
        // Sync the host lists too: another window shares the settings file,
        // and skipping this would let this window's next persist overwrite a
        // host that window just saved or pinned.
        self.nebula_pinned_hosts = settings.pinned_hosts;
        self.nebula_saved_hosts = settings.saved_hosts;
        self.nebula_hidden_hosts = settings.hidden_hosts;
        // Hand-edited keybind lines take effect on the next keypress; an
        // in-flight capture is dropped so it can't overwrite the file edit.
        self.nebula_keymap = keymap::build_bindings(&settings.keybinds);
        self.nebula_keybinds = settings.keybinds;
        self.nebula_keymap_capture = None;
        self.nebula_ssh_hosts = merge_ssh_hosts(
            &self.nebula_saved_hosts,
            &self.nebula_pinned_hosts,
            &self.nebula_hidden_hosts,
        );
        if image_changed {
            self.renderer.invalidate_background_image();
        }
        self.nebula_settings_mtime = mtime;
        self.update_window_transparency();
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
        let target = if self.nebula_background_image_cover_chrome {
            (0.0, 0.0, self.size_info.width(), self.size_info.height())
        } else {
            self.terminal_card_rect()
        };
        // 卡片模式必须携带卡片圆角：矩形壁纸盖上去会吃掉终端卡的圆角。
        let clip_radius = if self.nebula_background_image_cover_chrome {
            0.0
        } else {
            (UI_SHELL_RADIUS_LOGICAL * self.window.scale_factor as f32).round()
        };
        self.renderer.draw_background_image(
            &self.size_info,
            Path::new(path),
            self.nebula_background_image_opacity,
            self.nebula_background_image_fit,
            self.nebula_background_image_alignment,
            target,
            target,
            clip_radius,
        );
    }

    /// Compose the stable frame backdrop once per frame.
    ///
    /// 层模型（2026-07-24 修订）：清屏完全透明，终端卡底永远先铺主题底
    /// 色（用户透明度），壁纸再以自身不透明度叠在其上——降低壁纸不透
    /// 明度时图片淡向主题底色（浅色主题→白、深色主题→黑），而不是透出
    /// 窗口后面的桌面（旧模型有壁纸时不画卡底，深色主题下低不透明度会
    /// 透出刺眼的白）。卡以外的壳由 chrome pass 的一体化壳层负责（同一
    /// 用户透明度）。
    fn draw_window_backdrop(&mut self, terminal_background: Rgb) {
        // rgb 保留壳色：不透明窗口下 DWM 忽略 alpha，尚未被壳/卡覆盖的
        // 像素以壳色兜底而不是黑底。
        self.renderer.clear(self.nebula_theme.palette().shell_bg, 0.0);
        {
            let (card_x, card_y, card_w, card_h) = self.terminal_card_rect();
            let scale = self.window.scale_factor as f32;
            let alpha = (self.nebula_window_opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            // 与 chrome 壳层的凹角同径同 round：半径差出小数像素会让两侧
            // 的圆弧 AA 错位成一圈细缝。
            let card = UiQuad::solid(
                card_x,
                card_y,
                card_w,
                card_h,
                (UI_SHELL_RADIUS_LOGICAL * scale).round(),
                Rgba::new(
                    terminal_background.r,
                    terminal_background.g,
                    terminal_background.b,
                    alpha,
                ),
            );
            self.renderer.draw_ui(&self.size_info, &[card]);
        }

        // The image is intentionally independent of the terminal tint: its own
        // opacity means image strength, not a value that disappears at 100%
        // terminal opacity.
        self.draw_background_image();
    }

    /// Whether a wallpaper path is configured for the terminal card.
    fn has_background_image(&self) -> bool {
        self.nebula_background_image
            .as_deref()
            .map(|p| !p.trim().trim_matches('"').is_empty())
            .unwrap_or(false)
    }

    /// Sync the OS transparency flag with the user opacity slider. 壁纸不
    /// 透明度不再参与：卡底永远先铺主题底色，壁纸变淡是淡向主题色而非
    /// 透出窗口后面的桌面。
    fn update_window_transparency(&mut self) {
        let transparent = self.nebula_window_opacity < 1.0;
        self.window.set_transparent(transparent);
        #[cfg(target_os = "macos")]
        self.window.set_has_shadow(!transparent);
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

            // Every zoom / font / DPI change funnels through a font update,
            // so this is the single point where the UI font role can go
            // stale.
            self.refresh_ui_font(config);

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
        // The file/git drawer occupies real layout space: the grid cedes its
        // width (plus the window margin) on the right, exactly like the left
        // sidebar reserve — it does not float over the terminal.
        let drawer = if self.nebula_side_panel.open {
            (side_panel::PANEL_W_LOGICAL * scale + 8.0 * scale).round()
        } else {
            0.0
        };
        let mut new_size = SizeInfo::new_fully_asymmetric(
            width,
            height,
            cell_width,
            cell_height,
            padding.0 + content_pad + sidebar,
            padding.0 + content_pad + drawer,
            padding.1 + chrome,
            padding.1 + bottom_content_reserve(scale),
        );

        // Update number of column/lines in the viewport.
        let search_active = search_state.history_index.is_some();
        let message_bar_lines = message_buffer.message().map_or(0, |m| m.text(&new_size).len());
        let search_lines = usize::from(search_active);
        new_size.reserve_lines(message_bar_lines + search_lines);

        // Update resize increments.
        if config.window.resize_increments {
            let increments = self
                .window
                .allows_drag_resize()
                .then_some(PhysicalSize::new(cell_width, cell_height));
            self.window.set_resize_increments(increments);
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
                    Some(ResizeHud::new(new_size.columns(), new_size.screen_lines()));
            }
            self.nebula_resize_hud_armed = true;
            nebula_link_log(format!(
                "grid_resize {}x{} px={width}x{height} pad_x={} pad_r={} pad_y={} \
                 cell={cell_width}x{cell_height} drawer={drawer} sidebar={sidebar} \
                 reserved={}",
                new_size.columns(),
                new_size.screen_lines(),
                new_size.padding_x(),
                new_size.padding_right(),
                new_size.padding_y(),
                message_bar_lines + search_lines,
            ));
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

        // 把设置页的光标默认值同步进每一个被渲染的终端。事件路径只覆盖
        // "当前聚焦"的那一个 Term，新建 tab、分屏或后台 pane 都会漏掉；
        // set_default_cursor_style 内部有相等短路，逐帧调用无重绘代价。
        terminal.set_default_cursor_style(self.nebula_default_cursor_style());

        // Tell the renderer the full window height so pane viewports flip
        // correctly into OpenGL's bottom-left origin — matters for top/bottom
        // splits, where panes occupy different vertical bands of the window.
        self.renderer.set_window_height(self.size_info.height());

        pane_state.terminal_math.observe_program(pane_state.running_program.as_deref());

        // Collect renderable content before the terminal is dropped.
        let custom_background = self.nebula_background;
        let clickable_matches = hint::visible_clickable_matches(&terminal, config);
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
        nebula_debug_log(format!(
            "render_pane clear_first={clear_first} view={}x{} pad=({:.0},{:.0},{:.0},{:.0}) selection={selection_range:?}",
            view.width(),
            view.height(),
            view.padding_x(),
            view.padding_right(),
            view.padding_y(),
            view.padding_bottom(),
        ));
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

        // 打字（含 IME 组词）不影响网格内容，扫描保持开启；一旦这里随
        // preedit 关断，中文输入的每次拼音组合都会让全部公式闪回原文。
        //
        // 扫描本身不按 AI CLI 探测门控：$$/\[ \]/\( \) 定界符加上
        // plausible_math_source 已经足够抗误报，而 WSL/SSH 里跑的 AI CLI
        // 在本机进程树上只留下 wsl.exe/ssh.exe，按进程门控会把远程会话的
        // 公式全部关掉。只有误报率最高的裸 $…$ 仍要求 AI CLI 上下文
        // （进程探测命中，或本 pane 已渲染过块级公式）。
        let allow_inline_dollar = pane_state.terminal_math.inline_dollar_enabled();
        let terminal_math_overlays = if !alt_screen
            && !vi_mode
            && search_state.regex().is_none()
            && selection_range.is_none()
        {
            let visible_cursor = term::point_to_viewport(display_offset, cursor_point);
            terminal_math::scan_visible(
                &mut pane_state.terminal_math,
                &terminal,
                &view,
                &grid_cells,
                allow_inline_dollar,
                visible_cursor,
                foreground_color,
            )
        } else {
            Vec::new()
        };
        let math_pixel_size = self.glyph_cache.font_size.as_px();
        let math_pixels_per_point = self.window.scale_factor as f32 * 96.0 / 72.27;
        let prepared_math = terminal_math::prepare_overlays(
            &mut pane_state.terminal_math,
            &terminal_math_overlays,
            &view,
            math_pixel_size,
            math_pixels_per_point,
        );
        let math_coverage =
            terminal_math::CoverageMask::build(&terminal_math_overlays, &prepared_math);

        // Add damage from the terminal, keeping a pane-local copy: the shared
        // tracker gets flooded with a full-window mark every frame further
        // down, so "did the grid actually change?" (hint invalidation) must
        // be judged from the terminal's own report captured here.
        let mut term_damage_full = false;
        let mut term_damage_lines = Vec::new();
        match terminal.damage() {
            TermDamage::Full => {
                term_damage_full = true;
                self.damage_tracker.frame().mark_fully_damaged();
            },
            TermDamage::Partial(damaged_lines) => {
                for damage in damaged_lines {
                    self.damage_tracker.frame().damage_line(damage);
                    term_damage_lines.push(damage);
                }
            },
        }
        terminal.reset_damage();

        // Drop terminal as early as possible to free lock.
        drop(terminal);

        // Invalidate highlighted hints if grid has changed. Only the pane
        // that owns the hover may judge that: `highlighted_hint` is hit-tested
        // against the focused pane, so a background pane's output (build log,
        // `top`) must not tear down the foreground's highlight.
        if force_focus != Some(false) {
            self.validate_hint_highlights(display_offset, term_damage_full, &term_damage_lines);
        }

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
            // Layer model: the window clears to the opaque shell color (the
            // chrome backdrop), then the terminal is painted as a rounded
            // `term_bg` card floating on it. Default-background cells draw no
            // background of their own (bg_alpha == 0), so they show the card.
            nebula_debug_log(format!(
                "render_clear path=pane window={}x{} alpha={:.3}",
                self.size_info.width(),
                self.size_info.height(),
                self.nebula_window_opacity,
            ));
            self.draw_window_backdrop(background_color);
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
            let mut clickable_index = 0usize;

            let cells = grid_cells.into_iter().filter_map(|mut cell| {
                // 会被公式覆盖的源码字形直接不画：公式落在真实窗口背景上，
                // 不再需要事后用不透明色块把源文本盖掉。
                if !math_coverage.is_empty() && math_coverage.covers(cell.point) {
                    return None;
                }
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

                let point = term::viewport_to_point(display_offset, cell.point);
                while clickable_matches
                    .get(clickable_index)
                    .is_some_and(|bounds| bounds.end() < &point)
                {
                    clickable_index += 1;
                }
                let is_clickable = clickable_matches
                    .get(clickable_index)
                    .is_some_and(|bounds| bounds.contains(&point));
                if is_clickable {
                    // 点击目标的虚线直接继承每个 cell 的文字色；不能统一成主题色，
                    // 否则 ls 的目录/可执行文件颜色语义会被下划线悄悄抹平。
                    cell.flags.remove(Flags::ALL_UNDERLINES);
                    cell.flags.insert(Flags::DASHED_UNDERLINE);
                    cell.underline = cell.fg;
                }

                // Underline hints hovered by mouse or vi mode cursor. Persistent
                // clickable ranges stay dashed; other hint states retain the
                // stronger solid underline used by keyboard/vi highlighting.
                if has_highlighted_hint {
                    let hyperlink = cell.extra.as_ref().and_then(|extra| extra.hyperlink.as_ref());

                    let should_highlight = |hint: &Option<HintMatch>| {
                        hint.as_ref().is_some_and(|hint| hint.should_highlight(point, hyperlink))
                    };
                    if should_highlight(highlighted_hint) || should_highlight(vi_highlighted_hint) {
                        damage_tracker.frame().damage_point(cell.point);
                        if !is_clickable {
                            cell.flags.insert(Flags::UNDERLINE);
                        }
                    }
                }

                // Update underline/strikeout.
                lines.update(&cell);

                Some(cell)
            });
            self.renderer.draw_cells(&size_info, glyph_cache, cells);
        }

        let mut rects = lines.rects(&metrics, &size_info);

        if alt_screen {
            if let Some(pad_bg) = grid_pad_bg {
                let (_, card_y, _, card_h) = self.terminal_card_rect();
                let x = size_info.padding_x();
                let w = size_info.width() - size_info.padding_x() - size_info.padding_right();
                // 备用屏幕会给整张网格着色。补齐背景时只能填当前 Pane 的边缘；
                // 下方 Pane 若从整张卡片顶部开始填，会在最后绘制时盖住上方 Pane。
                for (y, height) in
                    alt_screen_vertical_padding_bands(&self.size_info, &size_info, card_y, card_h)
                        .into_iter()
                        .flatten()
                {
                    rects.push(RenderRect::new(x, y, w, height, pad_bg, 1.0));
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
            let bar = message_bar::message_bar_rect(&size_info, search_offset != 0);

            let bg = match message.ty() {
                MessageType::Error => config.colors.normal.red,
                MessageType::Warning => config.colors.normal.yellow,
            };

            let x = bar.x as i32;
            let y = bar.y as i32;
            let width = bar.width as i32;
            let height = bar.height as i32;
            let message_bar_rect = RenderRect::new(bar.x, bar.y, bar.width, bar.height, bg, 1.);

            // Push message_bar in the end, so it'll be above all other content.
            rects.push(message_bar_rect);

            // Always damage message bar, since it could have messages of the same size in it.
            self.damage_tracker.frame().add_viewport_rect(&size_info, x, y, width, height);

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

        terminal_math::draw_overlays(
            &mut self.renderer,
            &mut self.glyph_cache,
            &mut pane_state.terminal_math,
            &terminal_math_overlays,
            &prepared_math,
            &size_info,
            math_pixels_per_point,
        );

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

    /// Draw a document-viewer tab's frame: the shell backdrop and terminal
    /// card exactly like a pane frame (same layer model), then the document
    /// instead of a grid, then the normal chrome via `present_frame`.
    pub fn draw_doc_frame(
        &mut self,
        doc: &mut markdown_view::DocView,
        view: SizeInfo,
        scheduler: &mut Scheduler,
    ) {
        self.renderer.set_window_height(self.size_info.height());

        nebula_debug_log(format!(
            "render_clear path=document window={}x{} alpha={:.3}",
            self.size_info.width(),
            self.size_info.height(),
            self.nebula_window_opacity,
        ));
        let card_bg = self.nebula_background.unwrap_or(self.colors[NamedColor::Background]);
        self.draw_window_backdrop(card_bg);
        let (cx, cy, cw, ch) = self.terminal_card_rect();
        let scale = self.window.scale_factor as f32;

        // The document reads inside the card, inset off its rounded corners.
        let area = (
            (cx + 4.0 * scale).max(view.padding_x()),
            (cy + 4.0 * scale).max(view.padding_y()),
            (cw - 8.0 * scale).min(view.width()),
            (ch - 8.0 * scale).min(view.height()),
        );
        let skin = self.nebula_theme.skin();
        let size = self.size_info;
        markdown_view::draw(
            doc,
            &mut self.renderer,
            &mut self.glyph_cache,
            &size,
            &skin,
            area,
            scale,
        );

        self.present_frame(scheduler);
    }

    /// Draw the Settings special tab. Its controls are emitted by the chrome
    /// pass so they retain the same hit geometry and icon texture pipeline as
    /// the rest of Nebula, but the base is a normal tab content card.
    pub fn draw_settings_frame(&mut self, scheduler: &mut Scheduler) {
        self.renderer.set_window_height(self.size_info.height());

        nebula_debug_log(format!(
            "render_clear path=settings window={}x{} alpha={:.3}",
            self.size_info.width(),
            self.size_info.height(),
            self.nebula_window_opacity,
        ));
        let card_bg = self.nebula_background.unwrap_or(self.colors[NamedColor::Background]);
        self.draw_window_backdrop(card_bg);

        self.present_frame(scheduler);
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
        if let Some(mut reveal) = self.nebula_split_reveal {
            reveal.motion.step(self.nebula_ui_anims.frame());
            let e = reveal.motion.value();
            if !reveal.motion.is_active() {
                self.nebula_split_reveal = None;
            } else {
                self.nebula_split_reveal = Some(reveal);
                let (x, y, w, h) = reveal.rect;
                let bg = self.nebula_background.unwrap_or(Rgb::new(15, 17, 26));
                let cover = Rgba::new(bg.r, bg.g, bg.b, 255);
                let (cx, cy, cw, chh) = match reveal.direction {
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

    /// Centered modal for confirmations and mandatory setup gates.
    fn draw_confirm_modal(&mut self) {
        let Some(confirm) = self.nebula_confirm.clone() else {
            self.nebula_confirm_buttons = None;
            return;
        };
        let size = self.ui_size_info();
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

        let (title, body, danger) = match &confirm {
            NebulaConfirm::EnableBackgroundImageCoverChrome => (
                "让背景图覆盖窗口控件区域？".to_owned(),
                "背景图会延伸到标题栏、窗口按钮、Tab 与 SSH 侧栏下方，低对比度图片可能影响操作可见性；界面仍会保留最低不透明度保护。".to_owned(),
                false,
            ),
            NebulaConfirm::InstallRequiredFont { .. } => (
                "建议安装终端字体".to_owned(),
                "未检测到 Maple Mono Nerd Font；缺少图标时可安装后重启 Nebula。".to_owned(),
                false,
            ),
            NebulaConfirm::ClosePane { process, .. } => (
                "关闭此分栏？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                true,
            ),
            NebulaConfirm::CloseTab { process, .. } => (
                "关闭此标签页？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                true,
            ),
            NebulaConfirm::CloseWindow { process } => (
                "关闭整个窗口？".to_owned(),
                format!("{process} 仍在运行，关闭会中止它。"),
                true,
            ),
            NebulaConfirm::Paste { lines, .. } => (
                format!("粘贴 {lines} 行文本？"),
                "多行粘贴会被 shell 逐行执行，请确认来源可信。".to_owned(),
                false,
            ),
            NebulaConfirm::DeleteSsh { host, from_config } => {
                let host = truncate_tab_label(host, 28);
                if *from_config {
                    (
                        format!("隐藏 SSH 主机 {host}？"),
                        "只从 Nebula 隐藏；~/.ssh/config 不会修改，保存的密码将在撤销期后清除。"
                            .to_owned(),
                        true,
                    )
                } else {
                    (
                        format!("删除 SSH 主机 {host}？"),
                        "会从主机列表移除，保存的 Windows 密码将在撤销期后清除。".to_owned(),
                        true,
                    )
                }
            },
            NebulaConfirm::DeleteSftp { entry } => (
                format!("删除远端项目 {}？", truncate_tab_label(&entry.name, 28)),
                if entry.kind == crate::ssh_sftp::SftpEntryKind::Directory {
                    "文件夹及其全部远端内容会被递归删除，此操作无法撤销。".to_owned()
                } else {
                    "远端文件会被永久删除，此操作无法撤销。".to_owned()
                },
                true,
            ),
            NebulaConfirm::DeleteFileTreePath { path, is_dir } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                (
                    format!("删除 {}？", truncate_tab_label(&name, 28)),
                    if *is_dir {
                        "文件夹及其全部内容会移入回收站。".to_owned()
                    } else {
                        "文件会移入回收站。".to_owned()
                    },
                    true,
                )
            },
        };

        let text_w = |t: &str| -> f32 {
            let cols: usize = t.chars().map(|c| c.width().unwrap_or(1)).sum();
            cols as f32 * cell_w
        };

        // Buttons: right-aligned row, primary rightmost (Windows order). 文案
        // 统一"是 / 否"（2026-07-23 用户裁定）。
        //
        // 2026-07-27 用户反馈：Enter / Esc 一直生效，但按钮上只有"是""否"
        // 两个字，键位从没画出来——同文件的 SSH 撤销条却老实写着 Ctrl+Z，
        // 标准不一致。`can_dismiss()` 恒真，故两个键位都名副其实。
        //
        // 键位画成键帽（描边小方框）而不是裸文字：v0.5/v0.6 起 welcome 页的
        // 快捷键就是灰底药丸 + 亮墨的键帽（`welcome.rs` 的 `kbd`），用户记
        // 的"白色的框"就是它。那边是终端文本（ANSI + powerline 圆头字形），
        // 这里走 draw_ui + chrome text，没法照搬，所以用本文件既有的惯用法
        // 手画：外圈描边 quad + 内层填充 quad，只露 1px 圆环。描边取按钮
        // 自己的墨色，深色主题下自然读作白框，浅色主题下是深框。
        let language = self.ui_language();
        let primary_label = language.pick("是", "Yes");
        let cancel_label = language.pick("否", "No");
        let primary_key = "Enter";
        let cancel_key = "Esc";
        let btn_h = s(34.0);
        let btn_pad = s(18.0);
        let btn_min_w = s(88.0);
        // Keycap: text plus breathing room, and a hair taller than the glyph so
        // the ring never clips ascenders. Gap sits between label and cap.
        let cap_pad = s(6.0);
        let cap_h = cell_h + s(6.0);
        let cap_r = s(4.0);
        let key_gap = s(8.0);
        let cap_w = |key: &str| text_w(key) + 2.0 * cap_pad;
        let btn_w = |label: &str, key: &str| -> f32 {
            (text_w(label) + key_gap + cap_w(key) + 2.0 * btn_pad).max(btn_min_w)
        };
        let primary_w = btn_w(primary_label, primary_key);
        let cancel_w = btn_w(cancel_label, cancel_key);

        // Card sized to title/buttons, clamped into the window, and capped at
        // 520 logical px: a long body WRAPS instead of stretching the card
        // into a full-width banner (2026-07-23 用户反馈"警告框太宽").
        let pad = s(26.0);
        let head_w = text_w(&title).max(primary_w + s(12.0) + cancel_w);
        let box_w = (head_w + 2.0 * pad).max(s(380.0)).min(s(520.0)).min(size.width() - s(32.0));
        let body_cols = (((box_w - 2.0 * pad) / cell_w).floor() as usize).max(8);
        let body_lines = wrap_display_cols(&body, body_cols);
        let line_h = cell_h + s(6.0);
        let body_h = body_lines.len() as f32 * line_h - s(6.0);
        let box_h = pad + cell_h + s(10.0) + body_h + s(24.0) + btn_h + pad * 0.75;
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
        // Ink first: the keycap ring is derived from the ink it wraps, so both
        // buttons' text colors have to exist before the quads are built.
        let on_primary = if danger { Rgb::new(255, 244, 246) } else { sk.ink_on_accent };
        // Keycap geometry, shared by the ring quads and the glyph runs below.
        let cap_y = btn_y + (btn_h - cap_h) / 2.0;
        let cancel_cap_x = cancel_x + btn_pad + text_w(cancel_label) + key_gap;
        let primary_cap_x = primary_x + btn_pad + text_w(primary_label) + key_gap;

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

        // Keycaps: a 1px ring in the button's own ink, with the interior
        // re-stacking that button's own fills so the cap reads as an outline
        // rather than a second filled control competing with the primary
        // action. Ring alpha stays low — at full strength it would out-shout
        // the label it annotates, especially on the accent/danger fill.
        // Returns its quads instead of pushing: a closure capturing `quads`
        // mutably would lock it for the rest of the batch.
        let keycap = |x: f32, ink: Rgb, fills: &[Rgba], key: &str| -> Vec<UiQuad> {
            let w = cap_w(key);
            let mut out = vec![UiQuad::solid(
                x,
                cap_y,
                w,
                cap_h,
                cap_r,
                Rgba::new(ink.r, ink.g, ink.b, 120),
            )];
            out.extend(fills.iter().map(|fill| {
                UiQuad::solid(
                    x + s(1.0),
                    cap_y + s(1.0),
                    w - s(2.0),
                    cap_h - s(2.0),
                    cap_r - s(1.0),
                    *fill,
                )
            }));
            out
        };
        // The ghost button's visible fill is panel+surface stacked; the cap
        // interior re-stacks both so it matches the button exactly.
        quads.extend(keycap(cancel_cap_x, txt, &[sk.panel, sk.surface], cancel_key));
        quads.extend(keycap(primary_cap_x, on_primary, &[primary_fill], primary_key));
        self.renderer.draw_ui(&size, &quads);

        // Text: free-pixel chrome text (no opaque cell backgrounds), left
        // aligned like a native Windows dialog.
        let glyph_cache = &mut self.glyph_cache;
        let tx = bx + pad;
        self.renderer.draw_chrome_text(&size, tx, by + pad, txt, &title, glyph_cache);
        let btn_text_y = btn_y + (btn_h - cell_h) / 2.0;
        // Body wraps to the card's inner width; lines carry a small leading.
        let mut line_y = by + pad + cell_h + s(10.0);
        for line in &body_lines {
            self.renderer.draw_chrome_text(&size, tx, line_y, dim, line, glyph_cache);
            line_y += line_h;
        }
        self.renderer.draw_chrome_text(
            &size,
            cancel_x + btn_pad,
            btn_text_y,
            txt,
            cancel_label,
            glyph_cache,
        );
        // Key text is centered in its cap. The cap shares the button's text
        // centerline by construction, so `btn_text_y` needs no adjustment.
        // Ink stays full strength: the ring already marks this run as a key,
        // dimming it too would push it under the contrast floor.
        self.renderer.draw_chrome_text(
            &size,
            cancel_cap_x + cap_pad,
            btn_text_y,
            txt,
            cancel_key,
            glyph_cache,
        );
        // Danger keeps pale ink (red is dark in both modes); the accent
        // button contrast flips with the theme.
        self.renderer.draw_chrome_text(
            &size,
            primary_x + btn_pad,
            btn_text_y,
            on_primary,
            primary_label,
            glyph_cache,
        );
        self.renderer.draw_chrome_text(
            &size,
            primary_cap_x + cap_pad,
            btn_text_y,
            on_primary,
            primary_key,
            glyph_cache,
        );
    }

    /// Bottom-center reversible-action bar for SSH deletion. Its action rect is
    /// published to input after layout, keeping hover/click geometry identical
    /// to the pixels on screen.
    /// 助手建议条（spec 001）：底部居中浮条，SSH 撤销条同款组件语言（中性
    /// 壳、accent/danger 只在 ✦/⚠ 一处，渐变预算不动）。Pending 一行"正在
    /// 分析"，Ready 是图标 + 命令 + 暗色解释 + 键位提示；一律只贴不执行。
    /// 撤销条在场时让位——它 8 秒自清，之后建议条自然浮现。
    fn draw_ai_fix_bar(&mut self) {
        use crate::ai_assistant::AiFixState;
        if self.nebula_ssh_delete_undo.is_some() {
            return;
        }
        let Some(state) = self.nebula_ai_fix_bar.clone() else { return };

        let size = self.ui_size_info();
        let scale = self.window.scale_factor as f32;
        let s = |value: f32| value * scale;
        let cell_w = size.cell_width();
        let cell_h = size.cell_height();
        let sk = self.nebula_theme.skin();
        let language = self.ui_language();
        let text_cols =
            |text: &str| -> usize { text.chars().map(|ch| ch.width().unwrap_or(1).max(1)).sum() };

        let accent = Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255);
        let (icon, icon_color, command, explain, hint) = match &state {
            AiFixState::Pending { .. } => (
                "✦",
                accent,
                language.pick("正在分析失败原因…", "Analyzing failure…").to_owned(),
                String::new(),
                String::new(),
            ),
            AiFixState::Ready { fix, .. } => (
                if fix.danger { "⚠" } else { "✦" },
                if fix.danger { sk.danger } else { accent },
                fix.command.clone(),
                fix.explain.clone(),
                language.pick("Ctrl+. 贴入 · Esc 关闭", "Ctrl+. paste · Esc dismiss").to_owned(),
            ),
        };

        // Budget: icon + command are non-negotiable; the explain is the first
        // thing dropped, then the command itself is HEAD-truncated (unlike
        // paths, a command's identity lives at its start).
        let pad = s(14.0);
        let gap = s(10.0);
        let max_w = size.width() - s(24.0);
        let fixed = pad * 2.0 + cell_w * 2.0 + gap + text_cols(&hint) as f32 * cell_w;
        let cmd_budget = (((max_w - fixed) / cell_w) as usize).max(12);
        let command = truncate_tab_label(&command, cmd_budget.min(96));
        let explain_budget =
            cmd_budget.saturating_sub(text_cols(&command)).saturating_sub(3).min(60);
        let explain = if text_cols(&explain) + 8 > explain_budget {
            String::new()
        } else {
            explain
        };

        let mut content_cols = 2 + text_cols(&command);
        if !explain.is_empty() {
            content_cols += 3 + text_cols(&explain);
        }
        if !hint.is_empty() {
            content_cols += 2 + text_cols(&hint);
        }
        let bar_h = s(44.0).max(cell_h + s(12.0));
        let bar_w = (pad * 2.0 + content_cols as f32 * cell_w).max(s(320.0)).min(max_w);
        let bar_x = (size.width() - bar_w) * 0.5;
        let bar_y = size.height() - bar_h - s(18.0);

        let quads = vec![
            UiQuad::glow(
                bar_x - s(10.0),
                bar_y - s(8.0),
                bar_w + s(20.0),
                bar_h + s(16.0),
                Rgba::new(0, 0, 0, 72),
            ),
            UiQuad::solid(
                bar_x - s(1.0),
                bar_y - s(1.0),
                bar_w + s(2.0),
                bar_h + s(2.0),
                s(11.0),
                sk.hairline,
            ),
            UiQuad::solid(bar_x, bar_y, bar_w, bar_h, s(10.0), sk.panel),
        ];
        self.renderer.draw_ui(&size, &quads);

        let text_y = bar_y + (bar_h - cell_h) * 0.5;
        let mut x = bar_x + pad;
        let gc = &mut self.glyph_cache;
        self.renderer.draw_chrome_text(
            &size,
            x,
            text_y,
            Rgb::new(icon_color.r, icon_color.g, icon_color.b),
            icon,
            gc,
        );
        x += cell_w * 2.0;
        self.renderer.draw_chrome_text(&size, x, text_y, sk.ink_strong, &command, gc);
        x += text_cols(&command) as f32 * cell_w;
        if !explain.is_empty() {
            self.renderer.draw_chrome_text(
                &size,
                x + cell_w,
                text_y,
                sk.ink_dim,
                &format!("— {explain}"),
                gc,
            );
            x += (3 + text_cols(&explain)) as f32 * cell_w;
        }
        if !hint.is_empty() {
            let hint_x = (bar_x + bar_w - pad - text_cols(&hint) as f32 * cell_w).max(x + gap);
            self.renderer.draw_chrome_text(&size, hint_x, text_y, sk.ink_dim, &hint, gc);
        }
    }

    fn draw_ssh_delete_undo(&mut self) {
        let Some(undo) = self.nebula_ssh_delete_undo.as_ref() else {
            self.nebula_ssh_delete_undo_rect = None;
            self.nebula_ssh_delete_undo_hover = false;
            return;
        };
        if undo.started_at.elapsed() >= SSH_DELETE_UNDO_DURATION {
            self.expire_ssh_delete_undo();
            return;
        }

        let size = self.ui_size_info();
        let scale = self.window.scale_factor as f32;
        let s = |value: f32| value * scale;
        let cell_w = size.cell_width();
        let cell_h = size.cell_height();
        let sk = self.nebula_theme.skin();

        let fixed_cols = 20usize;
        let host_budget = (((size.width() - s(300.0)).max(cell_w * 8.0) / cell_w) as usize)
            .saturating_sub(fixed_cols)
            .max(8);
        let host = truncate_tab_label(&undo.host, host_budget.min(28));
        let message = if undo.from_config {
            format!("已隐藏 {host}（SSH config 未修改）")
        } else {
            format!("已移除 {host}")
        };
        let hint = "Ctrl+Z";
        let action = "撤销";
        let text_cols =
            |text: &str| -> usize { text.chars().map(|ch| ch.width().unwrap_or(1).max(1)).sum() };

        let pad = s(14.0);
        let gap = s(12.0);
        let action_w = s(76.0);
        let bar_h = s(48.0).max(cell_h + s(12.0));
        let content_w = (text_cols(&message) + text_cols(hint) + 2) as f32 * cell_w;
        let bar_w =
            (pad * 2.0 + content_w + gap + action_w).max(s(360.0)).min(size.width() - s(24.0));
        let bar_x = (size.width() - bar_w) * 0.5;
        let bar_y = size.height() - bar_h - s(18.0);
        let action_rect =
            (bar_x + bar_w - pad - action_w, bar_y + (bar_h - s(34.0)) * 0.5, action_w, s(34.0));
        self.nebula_ssh_delete_undo_rect = Some(action_rect);

        let mut quads = vec![
            UiQuad::glow(
                bar_x - s(10.0),
                bar_y - s(8.0),
                bar_w + s(20.0),
                bar_h + s(16.0),
                Rgba::new(0, 0, 0, 72),
            ),
            UiQuad::solid(
                bar_x - s(1.0),
                bar_y - s(1.0),
                bar_w + s(2.0),
                bar_h + s(2.0),
                s(11.0),
                sk.hairline,
            ),
            UiQuad::solid(bar_x, bar_y, bar_w, bar_h, s(10.0), sk.panel),
            UiQuad::solid(
                action_rect.0,
                action_rect.1,
                action_rect.2,
                action_rect.3,
                s(7.0),
                if self.nebula_ssh_delete_undo_hover { sk.hover_strong } else { sk.surface },
            ),
        ];
        if self.nebula_ssh_delete_undo_hover {
            quads.push(UiQuad::solid(
                action_rect.0,
                action_rect.1 + action_rect.3 - s(2.0),
                action_rect.2,
                s(2.0),
                s(1.0),
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 220),
            ));
        }
        self.renderer.draw_ui(&size, &quads);

        let text_y = bar_y + (bar_h - cell_h) * 0.5;
        let message_x = bar_x + pad;
        self.renderer.draw_chrome_text(
            &size,
            message_x,
            text_y,
            sk.ink,
            &message,
            &mut self.glyph_cache,
        );
        let hint_x = action_rect.0 - gap - text_cols(hint) as f32 * cell_w;
        self.renderer.draw_chrome_text(
            &size,
            hint_x,
            text_y,
            sk.ink_faint,
            hint,
            &mut self.glyph_cache,
        );
        let action_x = action_rect.0 + (action_rect.2 - text_cols(action) as f32 * cell_w) * 0.5;
        let action_y = action_rect.1 + (action_rect.3 - cell_h) * 0.5;
        self.renderer.draw_chrome_text_styled(
            &size,
            action_x,
            action_y,
            if self.nebula_ssh_delete_undo_hover { sk.ink_strong } else { sk.accent },
            nebula_terminal::term::cell::Flags::BOLD,
            action,
            &mut self.glyph_cache,
        );
    }

    /// Draw the window chrome and present the accumulated frame.
    /// Overlay a transient, fading "cols × rows" HUD centered in the window,
    /// shown briefly after a resize (a resize overlay HUD). Keeps requesting
    /// redraws until it fades out, then clears itself.
    fn draw_resize_hud(&mut self) {
        let Some(mut hud) = self.nebula_resize_hud else { return };
        hud.opacity.step(self.nebula_ui_anims.frame());
        if !hud.opacity.is_active() {
            self.nebula_resize_hud = None;
            return;
        }
        self.nebula_resize_hud = Some(hud);
        let cols = hud.columns;
        let rows = hud.rows;
        let fade = hud.opacity.value().clamp(0.0, 1.0);

        // UI-anchored metrics: the HUD is chrome, so its box and label must
        // not inflate with the terminal zoom it is reporting.
        let size = self.ui_size_info();
        let scale = self.window.scale_factor as f32;
        let cw = size.cell_width();
        let ch = size.cell_height();

        let text = format!("{cols} × {rows}");
        let text_cols: usize = text.chars().map(|c| c.width().unwrap_or(1)).sum();

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

        // The label shares the box's pixel coordinate system — the old
        // grid-cell placement centered on the TERMINAL area (whose origin
        // carries the asymmetric sidebar padding), so the text drifted out of
        // the window-centered box whenever the sidebar was open. Ink fades
        // with the box by mixing toward the panel color.
        let mix = |a: u8, b: u8| (a as f32 * fade + b as f32 * (1.0 - fade)).round() as u8;
        let ink = Rgb::new(
            mix(sk.ink_strong.r, hud_rgb.r),
            mix(sk.ink_strong.g, hud_rgb.g),
            mix(sk.ink_strong.b, hud_rgb.b),
        );
        let glyph_cache = &mut self.glyph_cache;
        self.renderer.draw_chrome_text(
            &size,
            box_x + pad,
            box_y + pad,
            ink,
            &text,
            glyph_cache,
        );

        // Keep the frame loop alive so the HUD animates out.
        self.window.request_redraw();
    }

    fn present_frame(&mut self, scheduler: &mut Scheduler) {
        // 本帧的 UI 锚定比率：chrome/设置/浮层文本按它反向补偿终端缩放。
        // 终端网格与文档正文不经过 chrome-text 路径，不受影响。
        let ui_scale = self.ui_text_scale();
        self.renderer.set_ui_text_scale(ui_scale);
        nebula_debug_log(format!(
            "render_present window={}x{} pane_view={} frame_images={} chrome_logos={}",
            self.size_info.width(),
            self.size_info.height(),
            self.nebula_pane_view.is_some(),
            self.nebula_frame_images.len(),
            self.nebula_chrome_logo_draws.len(),
        ));
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
        context_menu::draw(self);
        self.draw_ssh_delete_undo();
        self.draw_ai_fix_bar();
        self.draw_ssh_editor_modal();
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
        self.nebula_config_paths.clone_from(&config.config_paths);
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

    /// Feed the shared directory model from an authoritative shell report.
    pub fn nebula_record_directory(&self, cwd: &str) {
        self.directory_history.record(cwd);
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
        if let Some(rem) = self.directory_history.hint(&line, &state.cwd) {
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
                    nebula_debug_log(format!(
                        "suggest_result kind=command rem={:?}",
                        state.suggestion
                    ));
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
        let matches = if want_dir {
            self.directory_history.rank_file_suggestions(matches, &state.cwd)
        } else {
            matches
        };
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
    /// Draw a compact "open this link" tooltip near the hovered hint.
    ///
    /// 2026-07-23 用户反馈重构：上一版是整行 opaque `draw_string`，锚在鼠
    /// 标 cell 上——指针沿链接滑动时提示逐格跳动（被感知为"闪烁"），路
    /// 径还能占满整行（"显示太长"）。现在锚定到 hint 自己的起始 cell（指
    /// 针滑动时纹丝不动）、提示词压缩为 `Ctrl+点击`，并以 0.85× UI 锚定
    /// 字号画进圆角小气泡。
    ///
    /// 2026-07-26 用户反馈"显示不全"三连修：① file URI percent-decode 后
    /// 再展示（见 [`strip_file_scheme`]）；② 48 列硬帽退役，预算放开到整
    /// 个视口宽（fit_tail 仍兜底真溢出）；③ 气泡宽度按渲染器真实步进
    /// `average_advance × scale` 量取——`cell_w` 是 floor 后的值，48 列累
    /// 积下来尾部文字会戳出气泡右缘。
    fn draw_hyperlink_preview(
        &mut self,
        config: &UiConfig,
        _cursor_point: Option<Point>,
        display_offset: usize,
    ) {
        let num_cols = self.size_info.columns();

        // The destination under the mouse (first highlighted hint with a URI)
        // plus that hint's start cell as the anchor.
        let Some((uri, hint_start)) = self
            .highlighted_hint
            .iter()
            .chain(&self.vi_highlighted_hint)
            .find_map(|hint| {
                hint.hyperlink().map(|h| (h.uri().to_owned(), *hint.bounds().start()))
            })
        else {
            return;
        };
        // Hint start scrolled out of the viewport → fall back to the mouse cell.
        let anchor = term::point_to_viewport(display_offset, hint_start).or_else(|| {
            self.hint_mouse_point.and_then(|p| term::point_to_viewport(display_offset, p))
        });
        let Some(anchor) = anchor else {
            return;
        };

        // Strip the `file://` scheme (and its leading slash before a Windows
        // drive) so a local path reads as a path, not a URL.
        let target = strip_file_scheme(&uri);
        const HINT: &str = " · Ctrl+点击";
        let width = |s: &str| -> usize { s.chars().map(|c| c.width().unwrap_or(0)).sum() };
        let hint_w = width(HINT);
        let target_budget = num_cols.saturating_sub(hint_w + 1);
        let target = fit_tail(&target, target_budget);
        let label = format!("{target}{HINT}");

        // Position: one row below the hint's first cell, or above on the last row.
        let line = if anchor.line + 1 < self.size_info.screen_lines() {
            anchor.line + 1
        } else {
            anchor.line.saturating_sub(1)
        };

        // Damage every row the bubble touches, this frame and next (it can
        // appear/vanish). The bubble is taller than one cell row (0.85·cell
        // plus padding and border, centered on its row), so it bleeds into
        // both neighbours — an un-damaged neighbour ghosts the bubble's edges
        // on partial-present paths.
        let last_line = self.size_info.screen_lines().saturating_sub(1);
        for touched in line.saturating_sub(1)..=(line + 1).min(last_line) {
            let damage = LineDamageBounds::new(touched, 0, num_cols);
            self.damage_tracker.frame().damage_line(damage);
            self.damage_tracker.next_frame().damage_line(damage);
        }

        let scale_px = self.window.scale_factor as f32;
        let s = |v: f32| v * scale_px;
        let text_scale = 0.85 * self.ui_text_scale();
        let cell_w = self.size_info.cell_width();
        let cell_h = self.size_info.cell_height();
        // Measure with the renderer's REAL step for scaled doc text — the
        // unfloored design advance (`draw_doc_text_tracked` walks
        // `average_advance × scale`). `cell_w` is that advance floored; the
        // fraction lost per column made long labels poke out of the bubble.
        let advance = self.glyph_cache.font_metrics().average_advance as f32 * text_scale;
        let label_px = width(&label) as f32 * advance;
        let pad_x = s(8.0);
        let bubble_w = label_px + 2.0 * pad_x;
        let bubble_h = cell_h * text_scale + s(8.0);
        let max_x = (self.size_info.width() - self.size_info.padding_right() - bubble_w).max(0.0);
        let x = (self.size_info.padding_x() + anchor.column.0 as f32 * cell_w).min(max_x);
        let y = self.size_info.padding_y() + line as f32 * cell_h + (cell_h - bubble_h) * 0.5;

        let fg = config.colors.footer_bar_foreground();
        let bg = config.colors.footer_bar_background();
        let quads = [
            UiQuad::solid(
                x - s(1.0),
                y - s(1.0),
                bubble_w + s(2.0),
                bubble_h + s(2.0),
                s(7.0),
                Rgba::new(fg.r, fg.g, fg.b, 46),
            ),
            UiQuad::solid(x, y, bubble_w, bubble_h, s(6.0), Rgba::new(bg.r, bg.g, bg.b, 240)),
        ];
        self.renderer.draw_ui(&self.size_info, &quads);

        let glyph_cache = &mut self.glyph_cache;
        let size = self.size_info;
        self.renderer.draw_doc_text_tracked(
            &size,
            x + pad_x,
            y + (bubble_h - cell_h * text_scale) * 0.5,
            text_scale,
            0.0,
            fg,
            Flags::empty(),
            &label,
            glyph_cache,
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
    ///
    /// 2026-07-26 闪烁根因：这里原本拿共享 damage tracker 的 `intersects`
    /// 判断"hint 底下的网格变没变"，可 Nebula 每帧把 frame/next_frame 都标
    /// 成全窗 damage（全窗重绘呈现模型），`intersects` 的 `full ||` 短路恒
    /// 真——悬停高亮活不过两帧就被掐灭，鼠标一动重新点亮又立刻熄灭，ls
    /// 里扫过文件时下划线和气泡狂闪。改判终端自己上报的本帧 damage
    /// （`draw_pane` 在污染 tracker 之前捕获），语义回到上游本意。
    fn validate_hint_highlights(
        &mut self,
        display_offset: usize,
        term_damage_full: bool,
        term_damage_lines: &[LineDamageBounds],
    ) {
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

            // Clear hints whose underlying grid content actually changed.
            let grid_changed = term_damage_full
                || term_damage_lines.iter().any(|l| {
                    l.line >= start.line
                        && l.line <= end.line
                        // On the hint's first/last line only the hint's own
                        // column span counts; interior lines count wholly.
                        && (l.line != start.line || l.right >= start.column.0)
                        && (l.line != end.line || l.left <= end.column.0)
                });
            if grid_changed {
                if reset_mouse {
                    self.window.set_mouse_cursor(CursorIcon::Default);
                }
                self.damage_tracker.frame().mark_fully_damaged();
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
    let pad_top = padding.1 + chrome;
    let pad_bottom = padding.1 + bottom_content_reserve(scale_factor);
    let height = (pad_top + grid_height + pad_bottom).floor();

    PhysicalSize::new(width as u32, height as u32)
}

#[cfg(test)]
mod nebula_ux_tests {
    use nebula_terminal::grid::Dimensions;
    use winit::window::Theme as WinitTheme;

    use super::{
        NebulaConfirm, SizeInfo, alt_screen_vertical_padding_bands, nebula_command_hint,
        percent_decode_lossy, remove_ssh_host_from_lists, replays_untrusted_terminal_output,
        restore_ssh_host_to_lists, strip_file_scheme, system_theme_snapshot,
    };

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn file_uri_tooltip_shows_decoded_path() {
        // `ls --hyperlink` percent-encodes CJK names; the tooltip must not.
        assert_eq!(
            strip_file_scheme("file:///D:/%E6%98%9F%E9%9B%B2/read%20me.txt"),
            "D:/星雲/read me.txt"
        );
        // Non-file URIs keep their encoding — it is part of their identity.
        assert_eq!(strip_file_scheme("https://a.b/c%20d"), "https://a.b/c%20d");
        // Malformed escapes and non-UTF-8 decodes survive verbatim.
        assert_eq!(percent_decode_lossy("100%"), "100%");
        assert_eq!(percent_decode_lossy("%zz"), "%zz");
        assert_eq!(percent_decode_lossy("%ff%fe"), "%ff%fe");
    }

    #[test]
    fn log_replay_commands_do_not_receive_terminal_query_answers() {
        for command in [
            "docker logs app",
            "docker compose logs -f api",
            "podman logs app",
            "kubectl logs pod/api",
            "journalctl -f -u nebula",
        ] {
            assert!(replays_untrusted_terminal_output(command), "{command}");
        }
        for command in ["docker run app", "kubectl exec pod -- sh", "cargo test", "nvim"] {
            assert!(!replays_untrusted_terminal_output(command), "{command}");
        }
    }

    #[test]
    fn exact_path_command_suppresses_longer_neighbor_completion() {
        let commands = strings(&["claude", "claude-agent-acp"]);
        assert_eq!(nebula_command_hint(&commands, "clau"), Some("de"));
        assert_eq!(nebula_command_hint(&commands, "claude"), None);

        #[cfg(windows)]
        assert_eq!(nebula_command_hint(&commands, "CLAUDE"), None);
    }

    #[test]
    fn system_theme_snapshot_beats_a_stale_window_override() {
        assert_eq!(
            system_theme_snapshot(Some(WinitTheme::Dark), Some(WinitTheme::Light)),
            Some(WinitTheme::Dark)
        );
        assert_eq!(system_theme_snapshot(None, Some(WinitTheme::Light)), Some(WinitTheme::Light));
    }

    #[test]
    fn ssh_delete_undo_restores_saved_and_pinned_order() {
        let mut saved = strings(&["alpha", "target", "omega"]);
        let mut pinned = strings(&["target", "alpha"]);
        let mut hidden = strings(&["already-hidden"]);

        let snapshot = remove_ssh_host_from_lists("target", &mut saved, &mut pinned, &mut hidden);
        assert_eq!(snapshot, (Some(1), Some(0), false));
        assert_eq!(saved, strings(&["alpha", "omega"]));
        assert_eq!(pinned, strings(&["alpha"]));
        assert_eq!(hidden, strings(&["already-hidden", "target"]));

        restore_ssh_host_to_lists(
            "target",
            snapshot.0,
            snapshot.1,
            snapshot.2,
            &mut saved,
            &mut pinned,
            &mut hidden,
        );
        assert_eq!(saved, strings(&["alpha", "target", "omega"]));
        assert_eq!(pinned, strings(&["target", "alpha"]));
        assert_eq!(hidden, strings(&["already-hidden"]));
    }

    #[test]
    fn ssh_config_only_hide_is_fully_reversible() {
        let mut saved = Vec::new();
        let mut pinned = Vec::new();
        let mut hidden = Vec::new();

        let snapshot =
            remove_ssh_host_from_lists("config-alias", &mut saved, &mut pinned, &mut hidden);
        assert_eq!(snapshot, (None, None, false));
        assert_eq!(hidden, strings(&["config-alias"]));

        restore_ssh_host_to_lists(
            "config-alias",
            snapshot.0,
            snapshot.1,
            snapshot.2,
            &mut saved,
            &mut pinned,
            &mut hidden,
        );
        assert!(saved.is_empty());
        assert!(pinned.is_empty());
        assert!(hidden.is_empty());
    }

    #[test]
    fn asymmetric_bottom_reserve_recovers_rows_hidden_by_top_chrome() {
        let size = SizeInfo::new_fully_asymmetric(1000.0, 1000.0, 10.0, 20.0, 0.0, 0.0, 64.0, 16.0);
        assert_eq!(size.screen_lines(), 46);
        assert_eq!(size.padding_y(), 64.0);
        assert_eq!(size.padding_bottom(), 16.0);

        let old_symmetric = SizeInfo::new_asymmetric(1000.0, 1000.0, 10.0, 20.0, 0.0, 0.0, 64.0);
        assert_eq!(old_symmetric.screen_lines(), 43);
    }

    #[test]
    fn alternate_screen_padding_stays_inside_stacked_panes() {
        let window =
            SizeInfo::new_fully_asymmetric(1000.0, 700.0, 10.0, 20.0, 100.0, 20.0, 80.0, 20.0);
        let top =
            SizeInfo::new_fully_asymmetric(1000.0, 700.0, 10.0, 20.0, 100.0, 20.0, 80.0, 324.0);
        let bottom =
            SizeInfo::new_fully_asymmetric(1000.0, 700.0, 10.0, 20.0, 100.0, 20.0, 384.0, 20.0);

        assert_eq!(
            alt_screen_vertical_padding_bands(&window, &top, 56.0, 636.0),
            [Some((56.0, 24.0)), Some((360.0, 16.0))]
        );
        assert_eq!(
            alt_screen_vertical_padding_bands(&window, &bottom, 56.0, 636.0),
            [None, Some((664.0, 28.0))]
        );
    }

    #[test]
    fn missing_font_notice_can_be_dismissed() {
        let confirm =
            NebulaConfirm::InstallRequiredFont { directory: std::path::PathBuf::from("fonts") };

        assert!(confirm.can_dismiss());
    }
}
