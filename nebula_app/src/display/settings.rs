//! Settings special tab for Nebula's runtime appearance and completion settings.
//!
//! Mirrors the `command_palette` split, but goes one step further: besides the
//! *model* (sections, hit-testing, geometry, and the `nebula_settings.txt`
//! runtime store) this module also owns the panel's *rendering* — both the
//! background [`push_quads`] and the [`draw_text`] labels — so the giant
//! `display::mod` no longer carries the settings UI. The input layer stays the
//! only place that mutates state, reaching the `Display` methods that wrap this
//! model; rendering reads a snapshot [`SettingsView`] handed in each frame.
//!
//! Being a descendant module of `display`, this file can freely use the parent's
//! private helpers (`contains_rect`, `truncate_tab_label`, `nebula_data_dir`,
//! `NebulaTheme::palette`, `AcceptKey`, …) via `super::` — no visibility
//! churn needed in `mod.rs`.

use unicode_width::UnicodeWidthChar;

use nebula_terminal::vte::ansi::CursorShape;

use crate::config::UiConfig;
use crate::display::color::Rgb;
use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use super::theme::Skin;
use super::{icons, widgets};
use super::{
    AcceptKey, LanguagePreference, NebulaShell, NebulaTheme, SizeInfo, UiLanguage,
    chrome_settings_button_rect, contains_rect, nebula_data_dir, truncate_tab_label,
};

// Visual language: one flat panel color, one hairline, three text grays, ONE
// accent — hierarchy comes from typography and spacing. Every color is a
// [`Skin`] token from `display::theme` (single source of truth), so the page
// flips correctly between the light and dark theme families.

/// Sidebar sections of the settings panel. Deliberately small: only sections
/// with real functionality behind them are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NebulaSettingsSection {
    /// Themes, custom colors, wallpaper, cursor and window opacity.
    #[default]
    Appearance,
    /// Completion behaviour plus the raw `nebula_settings.txt` config file.
    Profiles,
    /// Selection/clipboard behaviour (Windows Terminal's "交互" page).
    Interaction,
    /// Read-only shortcut sheet + pointer to `[[keyboard.bindings]]` remapping.
    Keymap,
    /// Power-user switches (session residency on close, …).
    Advanced,
}

impl NebulaSettingsSection {
    fn label(self, language: UiLanguage) -> &'static str {
        match self {
            Self::Appearance => language.pick("外观", "Appearance"),
            Self::Profiles => language.pick("配置文件", "Profiles"),
            Self::Interaction => language.pick("交互", "Interaction"),
            Self::Keymap => language.pick("按键映射", "Key bindings"),
            Self::Advanced => language.pick("高级", "Advanced"),
        }
    }
}

/// Shortcut sheet shown in 设置→按键映射. Read-only for now: the combos on the
/// right are Nebula's effective defaults; `[[keyboard.bindings]]` in the config
/// file (设置→配置文件→打开配置文件) remaps the standard actions.
pub(super) const KEYMAP_ROWS: &[(&str, &str, &str)] = &[
    ("新建标签页", "New tab", "Ctrl+Shift+T"),
    ("关闭标签页 / 分屏", "Close tab / pane", "Ctrl+Shift+W"),
    ("下一个 / 上一个标签页", "Next / previous tab", "Ctrl+Tab / Ctrl+Shift+Tab"),
    ("切换到第 N 个标签页", "Select tab N", "Alt+1..9 / Ctrl+1..9"),
    ("新建窗口", "New window", "Ctrl+Shift+E"),
    ("命令面板", "Command palette", "Ctrl+Shift+P"),
    ("左右 / 上下分屏", "Split right / down", "Ctrl+Shift+D / Ctrl+Shift+S"),
    ("分屏焦点切换", "Move pane focus", "Ctrl+Alt+Arrow"),
    ("放大当前分屏", "Zoom current pane", "Ctrl+Shift+Enter"),
    ("启动 Profile N", "Launch Profile N", "Ctrl+Shift+1..9"),
    ("目录树 / Git 面板", "Files / Git panel", "Ctrl+Shift+O / Ctrl+Shift+G"),
    ("搜索（向前 / 向后）", "Search forward / backward", "Ctrl+Shift+F / Ctrl+Shift+B"),
    ("复制 / 粘贴", "Copy / paste", "Ctrl+Shift+C / Ctrl+V"),
    ("字号 增 / 减 / 重置", "Font size up / down / reset", "Ctrl+= / Ctrl+- / Ctrl+0"),
    ("全屏", "Fullscreen", "Alt+Enter"),
];

/// Which independently draggable opacity control is being adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsOpacityTarget {
    Terminal,
    BackgroundImage,
}

/// Which inline dropdown (combobox) is currently expanded. At most one at a
/// time; the option list floats over later rows instead of pushing them down.
/// 用户范式（2026-07-23）：凡是多选项的设置一律做成 WT 风格内嵌下拉框，
/// 不再用"点击循环切换"——所有选项必须先可见再选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsDropdown {
    Shell,
    Font,
    BackgroundFit,
    BackgroundAlignment,
    Language,
    Accept,
    CursorShape,
    /// 背景色：色板网格 + 16 进制输入的专用浮层（不是通用行列表）。
    BackgroundColor,
}

/// 背景色色盘的预设色板（2 行 × 6 列）。前排偏星云/终端惯用暗底，尾部
/// 提供近黑与亮底；任意颜色可用下方的 16 进制输入框手动指定。
pub(crate) const BACKGROUND_SWATCHES: [Rgb; 12] = [
    Rgb::new(8, 10, 24),
    Rgb::new(12, 16, 28),
    Rgb::new(18, 14, 32),
    Rgb::new(24, 24, 37),
    Rgb::new(0, 43, 54),
    Rgb::new(6, 26, 28),
    Rgb::new(40, 42, 54),
    Rgb::new(30, 30, 30),
    Rgb::new(12, 12, 12),
    Rgb::new(0, 0, 0),
    Rgb::new(253, 246, 227),
    Rgb::new(255, 255, 255),
];

pub(super) const BACKGROUND_FIT_OPTIONS: [BackgroundImageFit; 4] = [
    BackgroundImageFit::Fill,
    BackgroundImageFit::Uniform,
    BackgroundImageFit::UniformToFill,
    BackgroundImageFit::None,
];

pub(super) const BACKGROUND_ALIGNMENT_OPTIONS: [BackgroundImageAlignment; 9] = [
    BackgroundImageAlignment::TopLeft,
    BackgroundImageAlignment::Top,
    BackgroundImageAlignment::TopRight,
    BackgroundImageAlignment::Left,
    BackgroundImageAlignment::Center,
    BackgroundImageAlignment::Right,
    BackgroundImageAlignment::BottomLeft,
    BackgroundImageAlignment::Bottom,
    BackgroundImageAlignment::BottomRight,
];

pub(super) const LANGUAGE_OPTIONS: [LanguagePreference; 3] =
    [LanguagePreference::System, LanguagePreference::ZhCn, LanguagePreference::EnUs];

pub(super) const ACCEPT_OPTIONS: [AcceptKey; 3] =
    [AcceptKey::Both, AcceptKey::Tab, AcceptKey::Right];

/// Order mirrors the Windows Terminal appearance page the user referenced.
pub(super) const CURSOR_SHAPE_OPTIONS: [CursorShape; 4] =
    [CursorShape::Beam, CursorShape::Underline, CursorShape::Block, CursorShape::HollowBlock];

pub(super) fn cursor_shape_label(shape: CursorShape, language: UiLanguage) -> &'static str {
    match shape {
        CursorShape::Beam => language.pick("条形（│）", "Bar (│)"),
        CursorShape::Underline => language.pick("下划线（_）", "Underscore (_)"),
        CursorShape::Block => language.pick("实心框（█）", "Filled box (█)"),
        CursorShape::HollowBlock => language.pick("空心框（□）", "Empty box (□)"),
        CursorShape::Hidden => language.pick("隐藏", "Hidden"),
    }
}

fn accept_label(accept: AcceptKey, language: UiLanguage) -> &'static str {
    match accept {
        AcceptKey::Right => language.pick("右方向键", "Right arrow"),
        AcceptKey::Tab => "Tab",
        AcceptKey::Both => language.pick("Tab 或右方向键", "Tab or Right arrow"),
    }
}

fn language_label(preference: LanguagePreference, language: UiLanguage) -> &'static str {
    match preference {
        LanguagePreference::System => language.pick("跟随系统", "Follow system"),
        LanguagePreference::ZhCn => "简体中文",
        LanguagePreference::EnUs => "English",
    }
}

/// Hit result for the top-left Nebula settings affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    None,
    Toggle,
    Panel,
    Nav(NebulaSettingsSection),
    Theme(NebulaTheme),
    Language(LanguagePreference),
    SystemThemeToggle,
    GhostToggle,
    AcceptCycle,
    ShellCycle,
    StartupDirectory,
    StartupDirectoryClear,
    /// One of the expanded shell picker rows (index into detected_shells).
    ShellPickerRow(usize),
    FontCycle,
    /// Imported-font picker rows; the final row is always "导入字体…".
    FontPickerRow(usize),
    /// Font-size spinner steppers on the "字号" row.
    FontSizeUp,
    FontSizeDown,
    /// Cursor group: shape dropdown + its option rows, and the blink toggle.
    CursorShapeDropdown,
    CursorShapeOption(usize),
    CursorBlinkToggle,
    /// 交互: copy-on-select toggle row.
    CopyOnSelectToggle,
    /// Language combobox trigger (options resolve to [`SettingsHit::Language`]).
    LanguageDropdown,
    /// Expanded dropdown option rows for the cycle-style settings.
    AcceptOption(usize),
    FitOption(usize),
    AlignOption(usize),
    /// Restore one address from the persistent hidden-host list.
    RestoreHiddenSsh(usize),
    FetchToggle,
    PowerlineToggle,
    OpacitySlider,
    BackgroundColor,
    /// 背景色浮层：色板网格里的一格。
    BackgroundSwatch(usize),
    /// 背景色浮层：16 进制输入框。
    BackgroundHexInput,
    /// 背景色浮层内部的空白（吞掉点击且不关闭浮层）。
    BackgroundPopupPanel,
    BackgroundImage,
    BackgroundImageClear,
    BackgroundImageFit,
    BackgroundImageAlignment,
    BackgroundImageCoverChrome,
    BackgroundImageOpacitySlider,
    OpenConfigFile,
    Reset,
    /// 高级: keep the resident server (detach) on window close.
    KeepSessionToggle,
}

// ---- runtime settings store (`Nebula/nebula_settings.txt`) ----

pub(super) struct NebulaRuntimeSettings {
    pub(super) language: LanguagePreference,
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    pub(super) shell: NebulaShell,
    /// Raw default-shell id (`shell=<id>`), when the user picked a detected
    /// shell the 2-value `shell` enum can't represent (cmd, pwsh, nushell, a
    /// WSL distro). `None` = the enum value is authoritative. Written verbatim
    /// so `shell_detect::resolve_id` and the PTY layer both see the real id.
    pub(super) shell_id: Option<String>,
    /// Default working directory for fresh terminal tabs. `None` inherits the
    /// focused pane (or the process cwd for the first window).
    pub(super) startup_directory: Option<std::path::PathBuf>,
    pub(super) font_family: String,
    /// Terminal font size in LOGICAL px (`None` = follow the config file).
    /// Persisted so the settings spinner and Ctrl+wheel zoom survive restarts.
    pub(super) font_size: Option<f32>,
    /// Default cursor shape; escape sequences (vim, claude) still override.
    pub(super) cursor_shape: CursorShape,
    /// Default-on: a static cursor reads as a hang ("没有活动感").
    pub(super) cursor_blink: bool,
    /// 交互: selecting text copies it to the clipboard immediately (WT's
    /// copyOnSelect). Off = right-click copies instead.
    pub(super) copy_on_select: bool,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    /// Window close keeps the PTYs alive in the resident process (detach /
    /// re-attach session restore). Off = closing a window kills its shells.
    pub(super) keep_session: bool,
    pub(super) opacity: f32,
    pub(super) background: Option<Rgb>,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    pub(super) background_image_fit: BackgroundImageFit,
    pub(super) background_image_alignment: BackgroundImageAlignment,
    pub(super) background_image_cover_chrome: bool,
    /// Chrome theme. Persisted so a restart keeps the chosen look AND the
    /// powerline bridge file gets rewritten with the right name on boot
    /// (it used to be reset to the default theme every launch).
    pub(super) theme: NebulaTheme,
    /// Automatically choose the light/dark member of the selected theme
    /// family when the operating system appearance changes.
    pub(super) follow_system_theme: bool,
    /// SSH host aliases pinned to the top of the sidebar's "SSH HOSTS"
    /// section (right-click a host row), in pinned order.
    pub(super) pinned_hosts: Vec<String>,
    /// SSH destinations auto-saved after a successful typed `ssh` connection,
    /// most recent first (see `Display::nebula_save_ssh_host`).
    pub(super) saved_hosts: Vec<String>,
    /// SSH aliases explicitly removed from the sidebar. This is separate from
    /// `saved_hosts` because entries discovered in `~/.ssh/config` would
    /// otherwise reappear on the very next merge.
    pub(super) hidden_hosts: Vec<String>,
}

/// Load runtime UI settings from `Nebula/nebula_settings.txt`; defaults when
/// absent. Format is one `key=value` per line so power users can edit it while
/// the graphical settings page catches up.
pub(super) fn nebula_settings_load(config: &UiConfig) -> NebulaRuntimeSettings {
    let path = nebula_data_dir().join("nebula_settings.txt");
    let mut settings = NebulaRuntimeSettings {
        language: LanguagePreference::System,
        ghost: true,
        accept: AcceptKey::Both,
        shell: NebulaShell::PowerShell,
        shell_id: None,
        startup_directory: None,
        font_family: config.font.normal().family.clone(),
        font_size: None,
        cursor_shape: CursorShape::Beam,
        cursor_blink: true,
        copy_on_select: true,
        // Off by default: the welcome screen pipes a whole script through the
        // fresh shell and repaints on resize — real startup-latency cost on
        // the critical path (user ruling: startup speed outranks the art).
        fetch: false,
        powerline: true,
        // Off by default (user ruling 2026-07-12): a plain terminal should die
        // clean on close. Residency leaves shells running in the background,
        // which reads as "the app didn't really exit" — opt IN, not out.
        keep_session: false,
        opacity: config.window_opacity(),
        background: None,
        background_image: None,
        background_image_opacity: 0.38,
        background_image_fit: BackgroundImageFit::default(),
        background_image_alignment: BackgroundImageAlignment::default(),
        background_image_cover_chrome: false,
        theme: NebulaTheme::default(),
        // Preserve existing installations: automatic switching is opt-in so
        // an update never replaces an explicitly selected theme unexpectedly.
        follow_system_theme: false,
        pinned_hosts: Vec::new(),
        saved_hosts: Vec::new(),
        hidden_hosts: Vec::new(),
    };
    if let Ok(data) = std::fs::read_to_string(path) {
        for line in data.lines() {
            match line.split_once('=') {
                Some(("language", v)) => {
                    if let Some(language) = LanguagePreference::parse(v) {
                        settings.language = language;
                    }
                },
                Some(("ghost", v)) => settings.ghost = v.trim() != "0",
                Some(("theme", v)) => {
                    if let Some(theme) = NebulaTheme::from_prompt_name(v.trim()) {
                        settings.theme = theme;
                    }
                },
                Some(("accept", "right")) => settings.accept = AcceptKey::Right,
                Some(("accept", "tab")) => settings.accept = AcceptKey::Tab,
                Some(("accept", "both")) => settings.accept = AcceptKey::Both,
                Some(("shell" | "executor", v)) => {
                    let v = v.trim();
                    if let Some(shell) = NebulaShell::from_settings(v) {
                        settings.shell = shell;
                    }
                    // Preserve the raw id for detected shells the enum can't
                    // represent (cmd, pwsh, nushell, wsl:<distro>); the enum
                    // still tracks the PTY-integrated executor family so the
                    // prompt bootstrap picks the right base.
                    if !v.is_empty() {
                        settings.shell_id = Some(v.to_owned());
                    }
                },
                Some(("font_family", v)) => {
                    let family = v.trim();
                    if !family.is_empty() {
                        settings.font_family = family.to_owned();
                    }
                },
                Some(("font_size", v)) => {
                    if let Ok(size) = v.trim().parse::<f32>() {
                        settings.font_size = Some(size.clamp(6.0, 72.0));
                    }
                },
                Some(("cursor_shape", v)) => {
                    if let Some(shape) = parse_cursor_shape(v) {
                        settings.cursor_shape = shape;
                    }
                },
                Some(("cursor_blink", v)) => settings.cursor_blink = parse_bool(v, true),
                Some(("copy_on_select", v)) => settings.copy_on_select = parse_bool(v, true),
                Some(("startup_directory", v)) => {
                    let path = std::path::PathBuf::from(v.trim());
                    if path.is_dir() {
                        settings.startup_directory = Some(path);
                    }
                },
                Some(("fetch", v)) => settings.fetch = parse_bool(v, true),
                Some(("powerline", v)) => settings.powerline = parse_bool(v, true),
                Some(("keep_session", v)) => settings.keep_session = parse_bool(v, false),
                Some(("opacity", v)) => {
                    if let Ok(opacity) = v.trim().parse::<f32>() {
                        settings.opacity = opacity.clamp(0.0, 1.0);
                    }
                },
                Some(("background", v)) => settings.background = parse_hex_rgb(v.trim()),
                Some(("background_image", v)) => {
                    let v = v.trim();
                    settings.background_image = (!v.is_empty()).then(|| v.to_owned());
                },
                Some(("background_image_opacity", v)) => {
                    if let Ok(opacity) = v.trim().parse::<f32>() {
                        settings.background_image_opacity = opacity.clamp(0.0, 1.0);
                    }
                },
                Some(("background_image_fit", v)) => {
                    if let Some(fit) = BackgroundImageFit::parse(v) {
                        settings.background_image_fit = fit;
                    }
                },
                Some(("background_image_alignment", v)) => {
                    if let Some(alignment) = BackgroundImageAlignment::parse(v) {
                        settings.background_image_alignment = alignment;
                    }
                },
                Some(("background_image_cover_chrome", v)) => {
                    settings.background_image_cover_chrome = parse_bool(v, false);
                },
                Some(("pinned_hosts", v)) => {
                    settings.pinned_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                Some(("saved_hosts", v)) => {
                    settings.saved_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                Some(("follow_system_theme", v)) => {
                    settings.follow_system_theme = parse_bool(v, false)
                },
                Some(("hidden_hosts", v)) => {
                    settings.hidden_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                _ => {},
            }
        }
    }
    settings
}

fn parse_bool(value: &str, default: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn parse_cursor_shape(value: &str) -> Option<CursorShape> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Some(CursorShape::Block),
        "beam" | "bar" => Some(CursorShape::Beam),
        "underline" => Some(CursorShape::Underline),
        "hollow" => Some(CursorShape::HollowBlock),
        _ => None,
    }
}

pub(super) fn cursor_shape_settings_value(shape: CursorShape) -> &'static str {
    match shape {
        CursorShape::Beam => "beam",
        CursorShape::Underline => "underline",
        CursorShape::HollowBlock => "hollow",
        CursorShape::Block | CursorShape::Hidden => "block",
    }
}

pub(crate) fn parse_hex_rgb(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb::new(r, g, b))
}

fn format_hex_rgb(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)
}

fn background_image_fit_label(fit: BackgroundImageFit, language: UiLanguage) -> &'static str {
    match fit {
        BackgroundImageFit::Fill => language.pick("拉伸", "Fill"),
        BackgroundImageFit::Uniform => language.pick("适应", "Uniform"),
        BackgroundImageFit::UniformToFill => language.pick("填充", "Uniform to fill"),
        BackgroundImageFit::None => language.pick("原始尺寸", "None"),
    }
}

fn background_image_alignment_label(
    alignment: BackgroundImageAlignment,
    language: UiLanguage,
) -> &'static str {
    match alignment {
        BackgroundImageAlignment::TopLeft => language.pick("左上", "Top left"),
        BackgroundImageAlignment::Top => language.pick("顶部", "Top"),
        BackgroundImageAlignment::TopRight => language.pick("右上", "Top right"),
        BackgroundImageAlignment::Left => language.pick("左侧", "Left"),
        BackgroundImageAlignment::Center => language.pick("居中", "Center"),
        BackgroundImageAlignment::Right => language.pick("右侧", "Right"),
        BackgroundImageAlignment::BottomLeft => language.pick("左下", "Bottom left"),
        BackgroundImageAlignment::Bottom => language.pick("底部", "Bottom"),
        BackgroundImageAlignment::BottomRight => language.pick("右下", "Bottom right"),
    }
}

pub(super) fn nebula_settings_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(nebula_data_dir().join("nebula_settings.txt"))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Persist runtime settings next to the history file.
pub(super) fn nebula_settings_write(settings: &NebulaRuntimeSettings) {
    let accept = match settings.accept {
        AcceptKey::Right => "right",
        AcceptKey::Tab => "tab",
        AcceptKey::Both => "both",
    };
    let background = settings.background.map(format_hex_rgb).unwrap_or_default();
    let background_image = settings.background_image.as_deref().unwrap_or("");
    // A picked detected-shell id (cmd/pwsh/nu/wsl:X) is written verbatim; the
    // 2-value enum is the fallback for the built-in powershell/bash choice.
    let shell =
        settings.shell_id.clone().unwrap_or_else(|| settings.shell.settings_value().to_owned());
    let startup_directory = settings
        .startup_directory
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let theme = settings.theme.prompt_name();
    let path = nebula_data_dir().join("nebula_settings.txt");
    let pinned_hosts = settings.pinned_hosts.join(",");
    let saved_hosts = settings.saved_hosts.join(",");
    let hidden_hosts = settings.hidden_hosts.join(",");
    let font_size =
        settings.font_size.map(|size| format!("{size:.1}")).unwrap_or_default();
    let _ = std::fs::write(
        path,
        format!(
            "language={}\ntheme={theme}\nfollow_system_theme={}\nghost={}\naccept={accept}\nshell={shell}\nstartup_directory={startup_directory}\nfont_family={}\nfont_size={font_size}\ncursor_shape={}\ncursor_blink={}\ncopy_on_select={}\nfetch={}\npowerline={}\nkeep_session={}\nopacity={:.2}\nbackground={background}\nbackground_image={background_image}\nbackground_image_opacity={:.2}\nbackground_image_fit={}\nbackground_image_alignment={}\nbackground_image_cover_chrome={}\npinned_hosts={pinned_hosts}\nsaved_hosts={saved_hosts}\nhidden_hosts={hidden_hosts}\n",
            settings.language.as_str(),
            settings.follow_system_theme as u8,
            settings.ghost as u8,
            settings.font_family,
            cursor_shape_settings_value(settings.cursor_shape),
            settings.cursor_blink as u8,
            settings.copy_on_select as u8,
            settings.fetch as u8,
            settings.powerline as u8,
            settings.keep_session as u8,
            settings.opacity,
            settings.background_image_opacity,
            settings.background_image_fit.settings_value(),
            settings.background_image_alignment.settings_value(),
            settings.background_image_cover_chrome as u8,
        ),
    );
}

// ---- geometry + hit-testing ----

#[derive(Debug, Clone, Copy)]
struct SettingsGeometry {
    gear: (f32, f32, f32, f32),
    popup: (f32, f32, f32, f32),
    sidebar: (f32, f32, f32, f32),
    content: (f32, f32, f32, f32),
    nav: [(NebulaSettingsSection, f32, f32, f32, f32); 5],
    options: [(NebulaTheme, f32, f32, f32, f32); 7],
    /// Live terminal preview card at the top of Appearance: configure →
    /// immediately see (font, size, colors, wallpaper opacity, cursor).
    preview: (f32, f32, f32, f32),
    system_theme: (f32, f32, f32, f32),
    shell: (f32, f32, f32, f32),
    startup_directory: (f32, f32, f32, f32),
    startup_directory_clear: (f32, f32, f32, f32),
    font: (f32, f32, f32, f32),
    /// "字号" spinner row; the value box + steppers derive via `widgets`.
    font_size_row: (f32, f32, f32, f32),
    fetch: (f32, f32, f32, f32),
    powerline: (f32, f32, f32, f32),
    ghost: (f32, f32, f32, f32),
    accept: (f32, f32, f32, f32),
    open_config_file: (f32, f32, f32, f32),
    hidden_host_row0: (f32, f32, f32, f32),
    hidden_host_count: usize,
    /// Full-width "窗口透明度" row and its draggable track.
    language_row: (f32, f32, f32, f32),
    opacity_row: (f32, f32, f32, f32),
    opacity_slider: (f32, f32, f32, f32),
    /// Cursor group: shape combobox row + blink toggle row.
    cursor_shape_row: (f32, f32, f32, f32),
    cursor_blink_row: (f32, f32, f32, f32),
    background: (f32, f32, f32, f32),
    background_image: (f32, f32, f32, f32),
    background_image_clear: (f32, f32, f32, f32),
    background_image_fit: (f32, f32, f32, f32),
    background_image_alignment: (f32, f32, f32, f32),
    background_image_cover_chrome: (f32, f32, f32, f32),
    background_image_opacity_row: (f32, f32, f32, f32),
    background_image_opacity_slider: (f32, f32, f32, f32),
    /// 交互: copy-on-select toggle row.
    copy_on_select: (f32, f32, f32, f32),
    reset: (f32, f32, f32, f32),
    /// Top edge of the scrollable content viewport (just below the fixed
    /// header band); everything above it never scrolls.
    content_top: f32,
    /// Total designed content height per section (scaled px, measured from
    /// `content_top`). `max_scroll = (height - viewport).max(0)`.
    appearance_h: f32,
    profiles_h: f32,
    interaction_h: f32,
    keymap_h: f32,
    /// First keymap row rect; row `i` sits `i * row_h` below it.
    keymap_row0: (f32, f32, f32, f32),
    keymap_row_h: f32,
    advanced_h: f32,
    keep_session: (f32, f32, f32, f32),
}

/// Scrollable-content viewport height for the Settings tab.
fn settings_viewport_h(popup_h: f32, scale_factor: f32) -> f32 {
    popup_h - 72.0 * scale_factor
}

/// Max scroll offset for `section` at the current window size. The input
/// layer clamps its accumulated wheel delta with this. Dropdown popups float
/// over rows, so they never change a section's content height.
pub(super) fn settings_max_scroll(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    section: NebulaSettingsSection,
    hidden_host_count: usize,
) -> f32 {
    let geometry = settings_geometry(size_info, scale_factor, area, 0.0, hidden_host_count);
    let (_, _, _, ph) = geometry.popup;
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
        NebulaSettingsSection::Interaction => geometry.interaction_h,
        NebulaSettingsSection::Keymap => geometry.keymap_h,
        NebulaSettingsSection::Advanced => geometry.advanced_h,
    };
    (content_h - settings_viewport_h(ph, scale_factor)).max(0.0)
}

fn settings_geometry(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_host_count: usize,
) -> SettingsGeometry {
    let s = |v: f32| v * scale_factor;
    let gear = chrome_settings_button_rect(size_info, scale_factor);

    // The settings surface is the active tab's content card. Keeping the
    // geometry rooted in that card makes sidebar/drawer animations and DPI
    // changes follow the exact same bounds as terminal and document tabs.
    let (popup_x, popup_y, popup_w, popup_h) = area;
    let sidebar_w = s(220.0).min(popup_w * 0.30);
    let sidebar = (popup_x, popup_y, sidebar_w, popup_h);
    let content_x = popup_x + sidebar_w + s(16.0);
    let content_w = (popup_w - sidebar_w - s(16.0)).max(s(240.0));
    let content = (content_x, popup_y, content_w, popup_h);

    // The header band (big section title) is fixed; everything below it
    // scrolls by `scroll` px. `at` maps a design-space Y to screen space.
    let content_top = popup_y + s(72.0);
    let at = |design_y: f32| popup_y + s(design_y) - scroll;

    // ---- vertical rhythm (design px from the popup top) ----
    // Mirrors the HTML design sheet's breathing room: a group title hangs
    // 42px above its first row (title + 16px gap); rows inside a group are
    // CONTIGUOUS — one hairline frame around the block, hairline separators
    // between rows — and a finished group leaves 32px before the next title,
    // so `74 = 32 (section gap) + 42 (hanging title)`.
    const ROW_H: f32 = 44.0;
    const GROUP_ADVANCE: f32 = 74.0;
    // 预览卡设计高度：两行示例文本 + 光标演示的呼吸空间。
    const PREVIEW_H: f32 = 150.0;

    // Live preview leads the page: configure → see it immediately.
    let preview_y0 = 146.0;
    let preview = (content_x + s(24.0), at(preview_y0), content_w - s(48.0), s(PREVIEW_H));

    // Theme cards flow as a 4 + 3 grid with the design sheet's 20px gaps:
    // slot i sits at column i%4 on row i/4. The row pitch reserves a strip
    // under each card for its label (12px gap + text + 20px grid row gap).
    let card_gap = s(20.0);
    let card_w = ((content_w - s(48.0) - 3.0 * card_gap) / 4.0).clamp(s(88.0), s(170.0));
    let card_h = s(64.0);
    let card_y0 = preview_y0 + PREVIEW_H + GROUP_ADVANCE;
    let card_x = content_x + s(24.0);
    let card_row_pitch = card_h + s(48.0);
    let card = |i: f32| card_x + (i % 4.0) * (card_w + card_gap);
    let card_slot_y = |i: f32| at(card_y0) + (i / 4.0).floor() * card_row_pitch;

    let row_x = content_x + s(24.0);
    let row_w = content_w - s(48.0);
    let row_h = s(ROW_H);

    // Appearance: preview, cards, colors, cursor and interface groups.
    let system_theme_y0 = card_y0 + 2.0 * (64.0 + 48.0) + GROUP_ADVANCE;
    let color_y0 = system_theme_y0 + ROW_H + GROUP_ADVANCE;
    // Match Windows Terminal's background-image controls: path, stretch,
    // alignment and an independent image-opacity slider.
    let background_image_y0 = color_y0 + ROW_H;
    let background_image_fit_y0 = background_image_y0 + ROW_H;
    let background_image_alignment_y0 = background_image_fit_y0 + ROW_H;
    let background_image_opacity_y0 = background_image_alignment_y0 + ROW_H;
    let background_image_cover_chrome_y0 = background_image_opacity_y0 + ROW_H;
    // 光标组：形状下拉 + 闪烁开关。
    let cursor_y0 = color_y0 + 6.0 * ROW_H + GROUP_ADVANCE;
    let iface_y0 = cursor_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let opacity_y0 = iface_y0 + ROW_H;
    let appearance_h = s(opacity_y0 + ROW_H + 32.0 - 72.0);
    // 宽命中区包住细轨道，拖拽时无需精确点中 4px 线条。
    let opacity_row = (row_x, at(opacity_y0), row_w, row_h);
    let slider_x = row_x + row_w - s(212.0);
    let slider_w = s(188.0).min(row_w * 0.42).max(s(96.0));
    let opacity_slider = (slider_x, at(opacity_y0) + s(4.0), slider_w, s(36.0));
    let background_image_opacity_row = (row_x, at(background_image_opacity_y0), row_w, row_h);
    let background_image_opacity_slider =
        (slider_x, at(background_image_opacity_y0) + s(4.0), slider_w, s(36.0));

    // Sidebar navigation rows. The rects line up with the active-row
    // highlight drawn while rendering. 4px between items — same breathing as
    // the design sheet's nav menu.
    let nav_x = popup_x + s(24.0);
    let nav_w = sidebar_w - s(48.0);
    let nav_h = s(44.0);
    let nav_gap = s(4.0);
    let nav_y0 = popup_y + s(88.0);
    let nav_slot = |i: f32| nav_y0 + i * (nav_h + nav_gap);
    let nav = [
        (NebulaSettingsSection::Appearance, nav_x, nav_slot(0.0), nav_w, nav_h),
        (NebulaSettingsSection::Profiles, nav_x, nav_slot(1.0), nav_w, nav_h),
        (NebulaSettingsSection::Interaction, nav_x, nav_slot(2.0), nav_w, nav_h),
        (NebulaSettingsSection::Keymap, nav_x, nav_slot(3.0), nav_w, nav_h),
        (NebulaSettingsSection::Advanced, nav_x, nav_slot(4.0), nav_w, nav_h),
    ];

    // Profiles: dropdown popups FLOAT over later rows (Windows 11 combobox),
    // so every row keeps a fixed offset — no picker shove, no scroll jumps.
    let shell_y0 = 146.0;
    let startup_directory_y0 = shell_y0 + ROW_H;
    let font_y0 = startup_directory_y0 + ROW_H;
    let font_size_y0 = font_y0 + ROW_H;
    let fetch_y0 = font_size_y0 + ROW_H;
    let ghost_y0 = fetch_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let open_y0 = ghost_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let hidden_y0 = open_y0 + ROW_H + GROUP_ADVANCE;
    let profiles_end = if hidden_host_count == 0 {
        open_y0 + ROW_H
    } else {
        hidden_y0 + hidden_host_count as f32 * ROW_H
    };
    let profiles_h = s(profiles_end + 32.0 - 72.0);

    // 交互: a single copy-on-select toggle row for now.
    let interaction_y0 = 146.0;
    let interaction_h = s(interaction_y0 + ROW_H + 32.0 - 72.0);
    let copy_on_select = (row_x, at(interaction_y0), row_w, row_h);

    // Keymap: one contiguous group of read-only shortcut rows.
    let keymap_y0 = 146.0;
    let keymap_h = s(keymap_y0 + KEYMAP_ROWS.len() as f32 * ROW_H + 32.0 - 72.0);
    let keymap_row0 = (row_x, at(keymap_y0), row_w, row_h);

    // Advanced: a single session-residency toggle row.
    let advanced_y0 = 146.0;
    let advanced_h = s(advanced_y0 + ROW_H + 32.0 - 72.0);
    let keep_session = (row_x, at(advanced_y0), row_w, row_h);

    SettingsGeometry {
        gear,
        popup: (popup_x, popup_y, popup_w, popup_h),
        sidebar,
        content,
        nav,
        options: [
            (NebulaTheme::Nebula, card(0.0), card_slot_y(0.0), card_w, card_h),
            (NebulaTheme::SilverLight, card(1.0), card_slot_y(1.0), card_w, card_h),
            (NebulaTheme::SteelDark, card(2.0), card_slot_y(2.0), card_w, card_h),
            (NebulaTheme::LimestoneLight, card(3.0), card_slot_y(3.0), card_w, card_h),
            (NebulaTheme::CoalDark, card(4.0), card_slot_y(4.0), card_w, card_h),
            (NebulaTheme::LinenLight, card(5.0), card_slot_y(5.0), card_w, card_h),
            (NebulaTheme::MossDark, card(6.0), card_slot_y(6.0), card_w, card_h),
        ],
        preview,
        system_theme: (row_x, at(system_theme_y0), row_w, row_h),
        background: (row_x, at(color_y0), row_w, row_h),
        background_image: (row_x, at(background_image_y0), row_w, row_h),
        background_image_clear: (
            row_x + row_w - s(48.0),
            at(background_image_y0) + s(5.0),
            s(36.0),
            s(34.0),
        ),
        background_image_fit: (row_x, at(background_image_fit_y0), row_w, row_h),
        background_image_alignment: (row_x, at(background_image_alignment_y0), row_w, row_h),
        background_image_cover_chrome: (row_x, at(background_image_cover_chrome_y0), row_w, row_h),
        background_image_opacity_row,
        background_image_opacity_slider,
        cursor_shape_row: (row_x, at(cursor_y0), row_w, row_h),
        cursor_blink_row: (row_x, at(cursor_y0 + ROW_H), row_w, row_h),
        language_row: (row_x, at(iface_y0), row_w, row_h),
        opacity_row,
        opacity_slider,
        shell: (row_x, at(shell_y0), row_w, row_h),
        startup_directory: (row_x, at(startup_directory_y0), row_w, row_h),
        startup_directory_clear: (
            row_x + row_w - s(82.0),
            at(startup_directory_y0) + s(5.0),
            s(72.0),
            s(34.0),
        ),
        font: (row_x, at(font_y0), row_w, row_h),
        font_size_row: (row_x, at(font_size_y0), row_w, row_h),
        fetch: (row_x, at(fetch_y0), row_w, row_h),
        powerline: (row_x, at(fetch_y0 + ROW_H), row_w, row_h),
        ghost: (row_x, at(ghost_y0), row_w, row_h),
        accept: (row_x, at(ghost_y0 + ROW_H), row_w, row_h),
        open_config_file: (row_x, at(open_y0), row_w, row_h),
        hidden_host_row0: (row_x, at(hidden_y0), row_w, row_h),
        hidden_host_count,
        copy_on_select,
        reset: (popup_x + popup_w - s(170.0), popup_y + s(24.0), s(150.0), s(42.0)),
        content_top,
        appearance_h,
        profiles_h,
        interaction_h,
        keymap_h,
        keymap_row0,
        keymap_row_h: row_h,
        advanced_h,
        keep_session,
    }
}

pub(crate) fn opacity_slider_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    target: SettingsOpacityTarget,
) -> (f32, f32, f32, f32) {
    let geometry = settings_geometry(size_info, scale_factor, area, scroll, 0);
    match target {
        SettingsOpacityTarget::Terminal => geometry.opacity_slider,
        SettingsOpacityTarget::BackgroundImage => geometry.background_image_opacity_slider,
    }
}

pub(crate) fn opacity_from_pointer(pointer_x: f32, slider: (f32, f32, f32, f32)) -> f32 {
    ((pointer_x - slider.0) / slider.2.max(1.0)).clamp(0.0, 1.0)
}

/// Appearance 预览卡的壁纸绘制矩形：`(fit 目标, 实际允许触碰的裁剪带)`。
/// 裁剪带是预览与设置卡内容区的竖向交集——预览滚到 header 之下时壁纸不
/// 能跟着涂出去。完全滚出可视区时返回 `None`。
pub(super) fn appearance_preview_wallpaper_rects(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_hosts: usize,
) -> Option<((f32, f32, f32, f32), (f32, f32, f32, f32))> {
    let geometry = settings_geometry(size_info, scale_factor, area, scroll, hidden_hosts);
    let (vx, vy, vw, vh) = geometry.preview;
    let (_, content_y, _, _) = geometry.content;
    let (_, py, _, ph) = geometry.popup;
    let top = vy.max(content_y);
    let bottom = (vy + vh).min(py + ph);
    if bottom <= top || vw <= 0.0 {
        return None;
    }
    Some(((vx, vy, vw, vh), (vx, top, vw, bottom - top)))
}

/// The combobox anchor rect + option count for `dropdown`, IF it belongs to
/// the active section. Hit-testing, popup quads and popup text all resolve
/// the floating list through this one helper so the three can never disagree.
/// 背景色浮层的几何：面板矩形、12 个色板格、16 进制输入框。绘制与命中
/// 测试共用这一个来源（组件化范式：几何同源，控件与点击区不漂移）。
pub(super) struct BackgroundColorPopup {
    pub(super) rect: (f32, f32, f32, f32),
    pub(super) swatch: [(f32, f32, f32, f32); 12],
    pub(super) hex: (f32, f32, f32, f32),
}

pub(super) fn background_color_popup(
    geometry: &SettingsGeometry,
    scale: f32,
) -> BackgroundColorPopup {
    let s = |v: f32| v * scale;
    let (ax, ay, aw, ah) = widgets::combobox_rect(geometry.background, scale);
    const COLS: usize = 6;
    let cell = s(30.0);
    let gap = s(8.0);
    let pad = s(12.0);
    let grid_w = COLS as f32 * cell + (COLS - 1) as f32 * gap;
    let grid_h = 2.0 * cell + gap;
    let hex_h = s(34.0);
    let w = (grid_w + 2.0 * pad).max(aw);
    let h = pad + grid_h + gap + hex_h + pad;
    // 与 combobox 浮层同规则：锚行右缘对齐，紧贴行下方展开。
    let x = ax + aw - w;
    let y = ay + ah + s(6.0);
    let mut swatch = [(0.0, 0.0, 0.0, 0.0); 12];
    for (i, rect) in swatch.iter_mut().enumerate() {
        let row = i / COLS;
        let col = i % COLS;
        *rect = (
            x + pad + col as f32 * (cell + gap),
            y + pad + row as f32 * (cell + gap),
            cell,
            cell,
        );
    }
    let hex = (x + pad, y + pad + grid_h + gap, w - 2.0 * pad, hex_h);
    BackgroundColorPopup { rect: (x, y, w, h), swatch, hex }
}

fn dropdown_anchor(
    geometry: &SettingsGeometry,
    section: NebulaSettingsSection,
    dropdown: SettingsDropdown,
    shell_count: usize,
    font_count: usize,
    scale: f32,
) -> Option<((f32, f32, f32, f32), usize)> {
    use NebulaSettingsSection as Section;
    let anchor = |row| widgets::combobox_rect(row, scale);
    match (section, dropdown) {
        (Section::Profiles, SettingsDropdown::Shell) => Some((anchor(geometry.shell), shell_count)),
        (Section::Profiles, SettingsDropdown::Font) => Some((anchor(geometry.font), font_count)),
        (Section::Profiles, SettingsDropdown::Accept) => {
            Some((anchor(geometry.accept), ACCEPT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::BackgroundFit) => {
            Some((anchor(geometry.background_image_fit), BACKGROUND_FIT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::BackgroundAlignment) => {
            Some((anchor(geometry.background_image_alignment), BACKGROUND_ALIGNMENT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::Language) => {
            Some((anchor(geometry.language_row), LANGUAGE_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::CursorShape) => {
            Some((anchor(geometry.cursor_shape_row), CURSOR_SHAPE_OPTIONS.len()))
        },
        _ => None,
    }
}

/// Hit-test the top-left settings button and its popup. `scroll` must be the
/// same offset the renderer used, so hits land on what the user actually sees;
/// rows scrolled out of the content viewport don't respond.
#[allow(clippy::too_many_arguments)]
pub fn settings_hit(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    x: f32,
    y: f32,
    popup_open: bool,
    section: NebulaSettingsSection,
    scroll: f32,
    dropdown: Option<SettingsDropdown>,
    shell_count: usize,
    font_count: usize,
    hidden_host_count: usize,
) -> SettingsHit {
    let geometry = settings_geometry(size_info, scale_factor, area, scroll, hidden_host_count);
    let s = |v: f32| v * scale_factor;

    if contains_rect(geometry.gear, x, y) {
        return SettingsHit::Toggle;
    }

    if !popup_open {
        return SettingsHit::None;
    }

    // Scrolled content only responds inside its viewport (below the fixed
    // header, above the popup's bottom edge).
    let (_, py, _, ph) = geometry.popup;
    let in_viewport = y >= geometry.content_top && y <= py + ph;

    // An expanded dropdown owns the pointer first: its floating option list
    // covers later rows, and those must not react through it.
    if let Some(dropdown) = dropdown {
        // 背景色是专用浮层（色板网格 + hex 输入），不走通用行列表。
        if dropdown == SettingsDropdown::BackgroundColor {
            if section == NebulaSettingsSection::Appearance {
                let popup = background_color_popup(&geometry, scale_factor);
                for (index, rect) in popup.swatch.iter().enumerate() {
                    if contains_rect(*rect, x, y) {
                        return SettingsHit::BackgroundSwatch(index);
                    }
                }
                if contains_rect(popup.hex, x, y) {
                    return SettingsHit::BackgroundHexInput;
                }
                if contains_rect(popup.rect, x, y) {
                    return SettingsHit::BackgroundPopupPanel;
                }
            }
        } else if let Some((anchor, count)) =
            dropdown_anchor(&geometry, section, dropdown, shell_count, font_count, scale_factor)
        {
            let popup = widgets::combobox_popup_rect(
                anchor,
                count,
                scale_factor,
                geometry.content_top,
                py + ph - s(6.0),
            );
            if let Some(index) = widgets::popup_row_at(popup, count, scale_factor, x, y) {
                return match dropdown {
                    SettingsDropdown::Shell => SettingsHit::ShellPickerRow(index),
                    SettingsDropdown::Font => SettingsHit::FontPickerRow(index),
                    SettingsDropdown::BackgroundFit => SettingsHit::FitOption(index),
                    SettingsDropdown::BackgroundAlignment => SettingsHit::AlignOption(index),
                    SettingsDropdown::Language => SettingsHit::Language(LANGUAGE_OPTIONS[index]),
                    SettingsDropdown::Accept => SettingsHit::AcceptOption(index),
                    SettingsDropdown::CursorShape => SettingsHit::CursorShapeOption(index),
                    // 背景色浮层在上方特判处理，走不到通用行列表。
                    SettingsDropdown::BackgroundColor => SettingsHit::Panel,
                };
            }
            if contains_rect(popup, x, y) {
                // Padding strip inside the floating list: swallow the click
                // so rows underneath cannot react through the popup.
                return SettingsHit::Panel;
            }
        }
    }

    // Sidebar navigation and the header reset button are available from every
    // section.
    for (nav_section, nx, ny, nw, nh) in geometry.nav {
        if contains_rect((nx, ny, nw, nh), x, y) {
            return SettingsHit::Nav(nav_section);
        }
    }
    if contains_rect(geometry.reset, x, y) {
        return SettingsHit::Reset;
    }

    if in_viewport {
        match section {
            NebulaSettingsSection::Appearance => {
                for (theme, ox, oy, ow, oh) in geometry.options {
                    if contains_rect((ox, oy, ow, oh), x, y) {
                        return SettingsHit::Theme(theme);
                    }
                }
                if contains_rect(geometry.system_theme, x, y) {
                    return SettingsHit::SystemThemeToggle;
                }
                if contains_rect(geometry.background, x, y) {
                    return SettingsHit::BackgroundColor;
                }
                if contains_rect(geometry.background_image_clear, x, y) {
                    return SettingsHit::BackgroundImageClear;
                }
                if contains_rect(geometry.background_image, x, y) {
                    return SettingsHit::BackgroundImage;
                }
                if contains_rect(geometry.background_image_fit, x, y) {
                    return SettingsHit::BackgroundImageFit;
                }
                if contains_rect(geometry.background_image_alignment, x, y) {
                    return SettingsHit::BackgroundImageAlignment;
                }
                if contains_rect(geometry.background_image_opacity_slider, x, y) {
                    return SettingsHit::BackgroundImageOpacitySlider;
                }
                if contains_rect(geometry.background_image_cover_chrome, x, y) {
                    return SettingsHit::BackgroundImageCoverChrome;
                }
                if contains_rect(geometry.cursor_shape_row, x, y) {
                    return SettingsHit::CursorShapeDropdown;
                }
                if contains_rect(geometry.cursor_blink_row, x, y) {
                    return SettingsHit::CursorBlinkToggle;
                }
                if contains_rect(geometry.language_row, x, y) {
                    return SettingsHit::LanguageDropdown;
                }
                if contains_rect(geometry.opacity_slider, x, y) {
                    return SettingsHit::OpacitySlider;
                }
            },
            NebulaSettingsSection::Profiles => {
                if contains_rect(geometry.shell, x, y) {
                    return SettingsHit::ShellCycle;
                }
                if contains_rect(geometry.startup_directory_clear, x, y) {
                    return SettingsHit::StartupDirectoryClear;
                }
                if contains_rect(geometry.startup_directory, x, y) {
                    return SettingsHit::StartupDirectory;
                }
                if contains_rect(geometry.font, x, y) {
                    return SettingsHit::FontCycle;
                }
                {
                    let (_, up, down) =
                        widgets::spinner_rects(geometry.font_size_row, scale_factor);
                    if contains_rect(up, x, y) {
                        return SettingsHit::FontSizeUp;
                    }
                    if contains_rect(down, x, y) {
                        return SettingsHit::FontSizeDown;
                    }
                }
                if contains_rect(geometry.fetch, x, y) {
                    return SettingsHit::FetchToggle;
                }
                if contains_rect(geometry.powerline, x, y) {
                    return SettingsHit::PowerlineToggle;
                }
                if contains_rect(geometry.ghost, x, y) {
                    return SettingsHit::GhostToggle;
                }
                if contains_rect(geometry.accept, x, y) {
                    return SettingsHit::AcceptCycle;
                }
                if contains_rect(geometry.open_config_file, x, y) {
                    return SettingsHit::OpenConfigFile;
                }
                let (row_x, row_y, row_w, row_h) = geometry.hidden_host_row0;
                for index in 0..geometry.hidden_host_count {
                    let rect = (row_x, row_y + index as f32 * row_h, row_w, row_h);
                    if contains_rect(rect, x, y) {
                        return SettingsHit::RestoreHiddenSsh(index);
                    }
                }
            },
            NebulaSettingsSection::Interaction => {
                if contains_rect(geometry.copy_on_select, x, y) {
                    return SettingsHit::CopyOnSelectToggle;
                }
            },
            NebulaSettingsSection::Keymap => {},
            NebulaSettingsSection::Advanced => {
                if contains_rect(geometry.keep_session, x, y) {
                    return SettingsHit::KeepSessionToggle;
                }
            },
        }
    }

    if contains_rect(geometry.popup, x, y) { SettingsHit::Panel } else { SettingsHit::None }
}

// ---- rendering ----

/// A per-frame snapshot of the display state the settings render reads. Owns its
/// data (notably the wallpaper path) so the caller can hand it in by reference
/// while still borrowing `&mut renderer` for [`draw_text`].
pub(super) struct SettingsView {
    /// The active tab's content card in physical pixels. Settings fills this
    /// area like any other tab instead of inventing a second floating window.
    pub(super) area: (f32, f32, f32, f32),
    pub(super) language_preference: LanguagePreference,
    pub(super) language: UiLanguage,
    pub(super) section: NebulaSettingsSection,
    pub(super) hover: SettingsHit,
    pub(super) theme: NebulaTheme,
    pub(super) follow_system_theme: bool,
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    /// Pre-rendered "默认 Shell" value (icon + name) — resolved by `Display`
    /// from the rich `shell_id` when set, else the 2-value enum label.
    pub(super) shell_label: String,
    /// Which combobox is expanded, if any (floating option list).
    pub(super) dropdown: Option<SettingsDropdown>,
    /// Detected shells for the picker (cached once per process).
    pub(super) shells: Vec<(String, String, String)>, // (id, name, program)
    pub(super) shell_id: Option<String>,
    pub(super) startup_directory: Option<String>,
    pub(super) font_family: String,
    /// Current terminal font size in LOGICAL px, for the spinner value box.
    pub(super) font_size_px: f32,
    /// Private families plus Maple; the import action is rendered separately.
    pub(super) fonts: Vec<String>,
    pub(super) font_notice: Option<String>,
    /// Persistent soft-deleted destinations. Rows provide a discoverable
    /// recovery path after the short Undo bar has expired.
    pub(super) hidden_hosts: Vec<String>,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    pub(super) keep_session: bool,
    pub(super) opacity: f32,
    /// Which opacity slider is mid-drag, for thumb-dot grow feedback.
    pub(super) dragging_opacity: Option<SettingsOpacityTarget>,
    pub(super) cursor_shape: CursorShape,
    pub(super) cursor_blink: bool,
    pub(super) copy_on_select: bool,
    /// Live-preview colors: the ACTUAL terminal background/foreground the
    /// grid would use right now (custom background wins over the theme).
    pub(super) preview_bg: Rgb,
    pub(super) preview_fg: Rgb,
    pub(super) background: Option<Rgb>,
    /// 背景色浮层的 16 进制草稿（形如 `#0A0C18`）与输入聚焦态。
    pub(super) bg_hex_input: String,
    pub(super) bg_hex_active: bool,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    pub(super) background_image_fit: BackgroundImageFit,
    pub(super) background_image_alignment: BackgroundImageAlignment,
    pub(super) background_image_cover_chrome: bool,
    /// Content scroll offset in scaled px (0 = top). Owned by `Display`,
    /// clamped there against [`settings_max_scroll`].
    pub(super) scroll: f32,
}

/// Preview sample layout shared by the quad pass (cursor demo) and the text
/// pass (sample lines): 16px inner pad, 1.4× line pitch.
fn preview_line_y(top: f32, cell_h: f32, line: f32, scale: f32) -> f32 {
    top + 16.0 * scale + line * (cell_h * 1.4)
}
/// Columns of "❯ " before the demo cursor on the preview's prompt line.
const PREVIEW_PROMPT_COLS: usize = 2;

fn dropdown_selected_index(view: &SettingsView, dropdown: SettingsDropdown) -> Option<usize> {
    match dropdown {
        SettingsDropdown::Shell => view
            .shells
            .iter()
            .position(|(id, _, _)| view.shell_id.as_deref() == Some(id.as_str())),
        SettingsDropdown::Font => view.fonts.iter().position(|family| family == &view.font_family),
        SettingsDropdown::BackgroundFit => {
            BACKGROUND_FIT_OPTIONS.iter().position(|fit| *fit == view.background_image_fit)
        },
        SettingsDropdown::BackgroundAlignment => BACKGROUND_ALIGNMENT_OPTIONS
            .iter()
            .position(|alignment| *alignment == view.background_image_alignment),
        SettingsDropdown::Language => {
            LANGUAGE_OPTIONS.iter().position(|preference| *preference == view.language_preference)
        },
        SettingsDropdown::Accept => ACCEPT_OPTIONS.iter().position(|key| *key == view.accept),
        SettingsDropdown::CursorShape => {
            CURSOR_SHAPE_OPTIONS.iter().position(|shape| *shape == view.cursor_shape)
        },
        SettingsDropdown::BackgroundColor => view
            .background
            .and_then(|current| BACKGROUND_SWATCHES.iter().position(|color| *color == current)),
    }
}

fn dropdown_hover_index(hover: SettingsHit, dropdown: SettingsDropdown) -> Option<usize> {
    match (dropdown, hover) {
        (SettingsDropdown::Shell, SettingsHit::ShellPickerRow(index)) => Some(index),
        (SettingsDropdown::Font, SettingsHit::FontPickerRow(index)) => Some(index),
        (SettingsDropdown::BackgroundFit, SettingsHit::FitOption(index)) => Some(index),
        (SettingsDropdown::BackgroundAlignment, SettingsHit::AlignOption(index)) => Some(index),
        (SettingsDropdown::Language, SettingsHit::Language(preference)) => {
            LANGUAGE_OPTIONS.iter().position(|option| *option == preference)
        },
        (SettingsDropdown::Accept, SettingsHit::AcceptOption(index)) => Some(index),
        (SettingsDropdown::CursorShape, SettingsHit::CursorShapeOption(index)) => Some(index),
        _ => None,
    }
}

/// Push the Settings tab's background quads, navigation, rows and controls.
pub(super) fn push_quads(
    view: &SettingsView,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let sk = view.theme.skin();

    let geometry = settings_geometry(size, scale, view.area, view.scroll, view.hidden_hosts.len());
    let (px, py, pw, ph) = geometry.popup;
    // Header band height: the title row sits above the content, and the header
    // separator + big title are all measured from here.
    let header_h = s(72.0);
    // Scrolled content is clipped EXACTLY at the viewport edges: quads that
    // cross the fixed header separator or the popup's bottom edge are cut at
    // the line via [`UiQuad::clip_y`] (uv-remapped, so rounded corners and
    // glows are truncated mid-shape instead of bleeding past the hairline).
    let clip_top = geometry.content_top;
    let clip_bot = py + ph - s(6.0);
    let clip = |quads: &mut Vec<UiQuad>, quad: UiQuad| {
        if let Some(quad) = quad.clip_y(clip_top, clip_bot) {
            quads.push(quad);
        }
    };
    // 通用组件（widgets）不感知视口裁剪：输出先落到 staged，再统一过 clip。
    let mut staged: Vec<UiQuad> = Vec::new();

    // The page is flush with the active tab card. No veil, drop shadow or
    // second window outline: depth belongs to the app shell, not this page.
    quads.push(UiQuad::solid(px, py, pw, ph, s(12.0), sk.panel));
    let (side_x, side_y, side_w, side_h) = geometry.sidebar;

    // Sidebar: no fill of its own — just a hairline separator on its right
    // edge. Hierarchy comes from the nav rows, not a competing surface.
    quads.push(UiQuad::solid(
        side_x + side_w - s(1.0),
        side_y + s(16.0),
        s(1.0),
        side_h - s(32.0),
        0.0,
        sk.hairline,
    ));

    // Header separator under the panel title row, only in the content area
    // (right of the sidebar). The sidebar's own separator runs full height.
    let content_x = side_x + side_w;
    quads.push(UiQuad::solid(
        content_x,
        py + header_h,
        px + pw - content_x - s(1.0),
        s(1.0),
        0.0,
        sk.hairline,
    ));

    // Sidebar navigation: the active row is a floating pill — a soft accent
    // wash inside a hairline accent border (design language: no accent bar,
    // no vertical line); hover stays a quiet wash.
    let section = view.section;
    for (nav_section, nx, ny, nw, nh) in geometry.nav {
        if nav_section == section {
            quads.push(UiQuad::solid(
                nx - s(1.0),
                ny - s(1.0),
                nw + s(2.0),
                nh + s(2.0),
                s(9.0),
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 40),
            ));
            quads.push(UiQuad::solid(nx, ny, nw, nh, s(8.0), sk.panel));
            quads.push(UiQuad::solid(nx, ny, nw, nh, s(8.0), sk.accent_soft));
        } else if view.hover == SettingsHit::Nav(nav_section) {
            quads.push(UiQuad::solid(nx, ny, nw, nh, s(8.0), sk.hover));
        }
    }

    // Reset: a quiet ghost button in the header (hairline, no fill until hover).
    {
        let (rx, ry, rw, rh) = geometry.reset;
        quads.push(UiQuad::solid(
            rx - s(1.0),
            ry - s(1.0),
            rw + s(2.0),
            rh + s(2.0),
            s(9.0),
            sk.hairline,
        ));
        quads.push(UiQuad::solid(rx, ry, rw, rh, s(8.0), sk.surface));
        if view.hover == SettingsHit::Reset {
            quads.push(UiQuad::solid(rx, ry, rw, rh, s(8.0), sk.hover));
        }
    }

    // One framed group of rows: a hairline border around the whole block and
    // hairline separators between rows — the block reads as ONE plate (the
    // design sheet's `.settings-list`), not a stack of separate pills. Rows
    // stay transparent; hover marks the row with an inset rounded wash.
    let row_h = geometry.background.3;
    let group_frame = |quads: &mut Vec<UiQuad>, first_row: (f32, f32, f32, f32), rows: usize| {
        let (gx, gy, gw, _) = first_row;
        let gh = rows as f32 * row_h;
        clip(
            quads,
            UiQuad::solid(gx - s(1.0), gy - s(1.0), gw + s(2.0), gh + s(2.0), s(9.0), sk.hairline),
        );
        clip(quads, UiQuad::solid(gx, gy, gw, gh, s(8.0), sk.panel));
        for i in 1..rows {
            clip(
                quads,
                UiQuad::solid(
                    gx + s(1.0),
                    gy + i as f32 * row_h,
                    gw - s(2.0),
                    s(1.0),
                    0.0,
                    sk.hairline,
                ),
            );
        }
    };
    let row_hover = |quads: &mut Vec<UiQuad>, rect: (f32, f32, f32, f32), hovered: bool| {
        if hovered {
            let (rx, ry, rw, rh) = rect;
            clip(
                quads,
                UiQuad::solid(rx + s(2.0), ry + s(2.0), rw - s(4.0), rh - s(4.0), s(6.0), sk.hover),
            );
        }
    };
    // Widget wrappers: stage → viewport-clip → push. Every multi-option row
    // shares ONE combobox component (user ruling 2026-07-23), hover/press
    // feedback included, so no page ever hand-rolls its own control again.
    let combobox =
        |quads: &mut Vec<UiQuad>, staged: &mut Vec<UiQuad>, row, hot: bool, open: bool| {
            widgets::push_combobox(staged, widgets::combobox_rect(row, scale), scale, &sk, hot, open);
            for quad in staged.drain(..) {
                clip(quads, quad);
            }
        };
    let slider = |quads: &mut Vec<UiQuad>, staged: &mut Vec<UiQuad>, hit, value: f32, hot: bool| {
        widgets::push_slider(staged, hit, value, scale, &sk, hot);
        for quad in staged.drain(..) {
            clip(quads, quad);
        }
    };
    let toggle = |quads: &mut Vec<UiQuad>, staged: &mut Vec<UiQuad>, row, on: bool| {
        widgets::push_toggle(staged, row, on, scale, &sk);
        for quad in staged.drain(..) {
            clip(quads, quad);
        }
    };

    match section {
        NebulaSettingsSection::Appearance => {
            // ---- Live preview card: configure → immediately see ----
            // Terminal colors, font family/size (text pass), and the demo
            // cursor all read the same state the real grid uses.
            {
                let (vx, vy, vw, vh) = geometry.preview;
                clip(
                    quads,
                    UiQuad::solid(
                        vx - s(1.0),
                        vy - s(1.0),
                        vw + s(2.0),
                        vh + s(2.0),
                        s(11.0),
                        sk.hairline,
                    ),
                );
                clip(
                    quads,
                    UiQuad::solid(
                        vx,
                        vy,
                        vw,
                        vh,
                        s(10.0),
                        Rgba::new(view.preview_bg.r, view.preview_bg.g, view.preview_bg.b, 255),
                    ),
                );
                // Demo cursor on the prompt line, driven by the REAL shape +
                // blink settings (shares the UI caret's 500ms phase).
                if !view.cursor_blink || super::caret_blink_on() {
                    let cell_w = size.cell_width();
                    let cell_h = size.cell_height();
                    let cursor_x = vx + s(16.0) + PREVIEW_PROMPT_COLS as f32 * cell_w;
                    let cursor_y = preview_line_y(vy, cell_h, 2.0, scale);
                    let ink =
                        Rgba::new(view.preview_fg.r, view.preview_fg.g, view.preview_fg.b, 235);
                    let bg =
                        Rgba::new(view.preview_bg.r, view.preview_bg.g, view.preview_bg.b, 255);
                    let stroke = (1.5 * scale).max(1.0);
                    let beam_w = (2.0 * scale).max(1.0);
                    match view.cursor_shape {
                        CursorShape::Beam => {
                            clip(quads, UiQuad::solid(cursor_x, cursor_y, beam_w, cell_h, 0.0, ink));
                        },
                        CursorShape::Underline => {
                            clip(
                                quads,
                                UiQuad::solid(
                                    cursor_x,
                                    cursor_y + cell_h - beam_w,
                                    cell_w,
                                    beam_w,
                                    0.0,
                                    ink,
                                ),
                            );
                        },
                        CursorShape::HollowBlock => {
                            clip(quads, UiQuad::solid(cursor_x, cursor_y, cell_w, cell_h, 0.0, ink));
                            clip(
                                quads,
                                UiQuad::solid(
                                    cursor_x + stroke,
                                    cursor_y + stroke,
                                    cell_w - 2.0 * stroke,
                                    cell_h - 2.0 * stroke,
                                    0.0,
                                    bg,
                                ),
                            );
                        },
                        CursorShape::Hidden => {},
                        CursorShape::Block => {
                            clip(quads, UiQuad::solid(cursor_x, cursor_y, cell_w, cell_h, 0.0, ink));
                        },
                    }
                }
            }

            // Theme cards ARE the swatches: each card is filled with its own
            // theme's real panel color. Selection = accent ring + halo; hover
            // = the card lifts 2px and glows softly (the design sheet's
            // floating-card hover) — no wash, so the swatch color stays true.
            for (theme, ox, oy, ow, oh) in geometry.options {
                let selected = theme == view.theme;
                let hovered = view.hover == SettingsHit::Theme(theme);
                let lift = if hovered && !selected { s(2.0) } else { 0.0 };
                let oy = oy - lift;
                let stroke = if selected {
                    Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255)
                } else {
                    sk.hairline
                };
                let stroke_w = if selected { s(2.0) } else { s(1.0) };
                if selected {
                    // Selected card glows softly: the accent ring plus a
                    // diffuse halo, per the design sheet's lit-control look.
                    clip(
                        quads,
                        UiQuad::glow(
                            ox - s(14.0),
                            oy - s(14.0),
                            ow + s(28.0),
                            oh + s(28.0),
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 66),
                        ),
                    );
                } else if hovered {
                    // Hover halo: same shape, fainter — enough 辉光 to read
                    // as "lit up" without competing with the selected card.
                    clip(
                        quads,
                        UiQuad::glow(
                            ox - s(12.0),
                            oy - s(10.0),
                            ow + s(24.0),
                            oh + s(26.0),
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 38),
                        ),
                    );
                }
                clip(
                    quads,
                    UiQuad::solid(
                        ox - stroke_w,
                        oy - stroke_w,
                        ow + 2.0 * stroke_w,
                        oh + 2.0 * stroke_w,
                        s(9.0),
                        stroke,
                    ),
                );
                let mut card_bg = theme.palette().panel;
                card_bg.a = 255;
                clip(quads, UiQuad::solid(ox, oy, ow, oh, s(8.0), card_bg));
            }

            group_frame(quads, geometry.system_theme, 1);
            row_hover(quads, geometry.system_theme, view.hover == SettingsHit::SystemThemeToggle);
            toggle(quads, &mut staged, geometry.system_theme, view.follow_system_theme);

            // 自定义背景和界面都使用连续分组，避免设置 Tab 内再次出现
            // 漂浮卡片语言。
            group_frame(quads, geometry.background, 6);
            row_hover(quads, geometry.background, view.hover == SettingsHit::BackgroundColor);
            // 背景色也是多选项设置：同一 combobox 组件，浮层换成色板+hex。
            combobox(
                quads,
                &mut staged,
                geometry.background,
                view.hover == SettingsHit::BackgroundColor,
                view.dropdown == Some(SettingsDropdown::BackgroundColor),
            );
            row_hover(quads, geometry.background_image, view.hover == SettingsHit::BackgroundImage);
            if view.background_image.is_some() {
                row_hover(
                    quads,
                    geometry.background_image_clear,
                    view.hover == SettingsHit::BackgroundImageClear,
                );
            }
            combobox(
                quads,
                &mut staged,
                geometry.background_image_fit,
                view.hover == SettingsHit::BackgroundImageFit,
                view.dropdown == Some(SettingsDropdown::BackgroundFit),
            );
            combobox(
                quads,
                &mut staged,
                geometry.background_image_alignment,
                view.hover == SettingsHit::BackgroundImageAlignment,
                view.dropdown == Some(SettingsDropdown::BackgroundAlignment),
            );
            slider(
                quads,
                &mut staged,
                geometry.background_image_opacity_slider,
                view.background_image_opacity,
                view.hover == SettingsHit::BackgroundImageOpacitySlider
                    || view.dragging_opacity == Some(SettingsOpacityTarget::BackgroundImage),
            );
            row_hover(
                quads,
                geometry.background_image_cover_chrome,
                view.hover == SettingsHit::BackgroundImageCoverChrome,
            );
            toggle(
                quads,
                &mut staged,
                geometry.background_image_cover_chrome,
                view.background_image_cover_chrome,
            );

            // 光标组：形状下拉 + 闪烁开关。
            group_frame(quads, geometry.cursor_shape_row, 2);
            row_hover(
                quads,
                geometry.cursor_shape_row,
                view.hover == SettingsHit::CursorShapeDropdown,
            );
            combobox(
                quads,
                &mut staged,
                geometry.cursor_shape_row,
                view.hover == SettingsHit::CursorShapeDropdown,
                view.dropdown == Some(SettingsDropdown::CursorShape),
            );
            row_hover(quads, geometry.cursor_blink_row, view.hover == SettingsHit::CursorBlinkToggle);
            toggle(quads, &mut staged, geometry.cursor_blink_row, view.cursor_blink);

            // 界面组：语言（同一通用下拉组件）+ 终端不透明度。
            group_frame(quads, geometry.language_row, 2);
            row_hover(quads, geometry.language_row, view.hover == SettingsHit::LanguageDropdown);
            combobox(
                quads,
                &mut staged,
                geometry.language_row,
                view.hover == SettingsHit::LanguageDropdown,
                view.dropdown == Some(SettingsDropdown::Language),
            );
            slider(
                quads,
                &mut staged,
                geometry.opacity_slider,
                view.opacity,
                view.hover == SettingsHit::OpacitySlider
                    || view.dragging_opacity == Some(SettingsOpacityTarget::Terminal),
            );
        },
        NebulaSettingsSection::Profiles => {
            // 终端组：Shell / 启动目录 / 字体+字号。下拉列表是浮层，行的
            // hairline 分组保持固定，不再被展开的列表推开。
            group_frame(quads, geometry.shell, 1);
            group_frame(quads, geometry.startup_directory, 1);
            group_frame(quads, geometry.font, 2);
            group_frame(quads, geometry.fetch, 2);
            group_frame(quads, geometry.ghost, 2);
            group_frame(quads, geometry.open_config_file, 1);
            if geometry.hidden_host_count > 0 {
                group_frame(quads, geometry.hidden_host_row0, geometry.hidden_host_count);
                for index in 0..geometry.hidden_host_count {
                    let mut rect = geometry.hidden_host_row0;
                    rect.1 += index as f32 * rect.3;
                    row_hover(quads, rect, view.hover == SettingsHit::RestoreHiddenSsh(index));
                }
            }
            for (hit, rect) in [
                (SettingsHit::ShellCycle, geometry.shell),
                (SettingsHit::StartupDirectory, geometry.startup_directory),
                (SettingsHit::FontCycle, geometry.font),
                (SettingsHit::FetchToggle, geometry.fetch),
                (SettingsHit::PowerlineToggle, geometry.powerline),
                (SettingsHit::GhostToggle, geometry.ghost),
                (SettingsHit::AcceptCycle, geometry.accept),
                (SettingsHit::OpenConfigFile, geometry.open_config_file),
            ] {
                row_hover(quads, rect, view.hover == hit);
            }
            if view.startup_directory.is_some() {
                row_hover(
                    quads,
                    geometry.startup_directory_clear,
                    view.hover == SettingsHit::StartupDirectoryClear,
                );
            }
            combobox(
                quads,
                &mut staged,
                geometry.shell,
                view.hover == SettingsHit::ShellCycle,
                view.dropdown == Some(SettingsDropdown::Shell),
            );
            combobox(
                quads,
                &mut staged,
                geometry.font,
                view.hover == SettingsHit::FontCycle,
                view.dropdown == Some(SettingsDropdown::Font),
            );
            widgets::push_spinner(
                &mut staged,
                geometry.font_size_row,
                scale,
                &sk,
                view.hover == SettingsHit::FontSizeUp,
                view.hover == SettingsHit::FontSizeDown,
            );
            for quad in staged.drain(..) {
                clip(quads, quad);
            }
            combobox(
                quads,
                &mut staged,
                geometry.accept,
                view.hover == SettingsHit::AcceptCycle,
                view.dropdown == Some(SettingsDropdown::Accept),
            );
            // Boolean rows render a real switch instead of an "On/Off" string.
            for (rect, on) in [
                (geometry.fetch, view.fetch),
                (geometry.powerline, view.powerline),
                (geometry.ghost, view.ghost),
            ] {
                toggle(quads, &mut staged, rect, on);
            }
        },
        NebulaSettingsSection::Interaction => {
            group_frame(quads, geometry.copy_on_select, 1);
            row_hover(
                quads,
                geometry.copy_on_select,
                view.hover == SettingsHit::CopyOnSelectToggle,
            );
            toggle(quads, &mut staged, geometry.copy_on_select, view.copy_on_select);
        },
        NebulaSettingsSection::Keymap => {
            group_frame(quads, geometry.keymap_row0, KEYMAP_ROWS.len());
            // Keymap rows are read-only — no interaction, no hover.
        },
        NebulaSettingsSection::Advanced => {
            group_frame(quads, geometry.keep_session, 1);
            row_hover(quads, geometry.keep_session, view.hover == SettingsHit::KeepSessionToggle);
            toggle(quads, &mut staged, geometry.keep_session, view.keep_session);
        },
    }

    // Overlay scrollbar on the content viewport's right edge, only when the
    // section actually overflows (same style as the pane scrollbar: thin
    // rounded thumb, no track).
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
        NebulaSettingsSection::Interaction => geometry.interaction_h,
        NebulaSettingsSection::Keymap => geometry.keymap_h,
        NebulaSettingsSection::Advanced => geometry.advanced_h,
    };
    let viewport_h = settings_viewport_h(ph, scale);
    if content_h > viewport_h {
        let max_scroll = content_h - viewport_h;
        let frac = (view.scroll / max_scroll).clamp(0.0, 1.0);
        let track_h = viewport_h - s(12.0);
        let thumb_h = (track_h * viewport_h / content_h).max(s(28.0));
        let ty = clip_top + s(6.0) + (track_h - thumb_h) * frac;
        let tx = px + pw - s(7.0);
        quads.push(UiQuad::solid(
            tx,
            ty,
            s(4.0),
            thumb_h,
            s(2.0),
            sk.scrollbar_thumb.with_alpha(0.45),
        ));
    }
}

/// The floating dropdown option list. `draw_chrome` paints these AFTER the
/// base text pass (a separate `draw_ui` call), so page labels can never bleed
/// through the popup plate — the same modal layering rule the command palette
/// needed.
pub(super) fn push_popup_quads(
    view: &SettingsView,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    scale: f32,
) {
    let Some(dropdown) = view.dropdown else { return };
    let s = |v: f32| v * scale;
    let sk = view.theme.skin();
    let geometry = settings_geometry(size, scale, view.area, view.scroll, view.hidden_hosts.len());
    // 背景色专用浮层：色板网格 + hex 输入框（几何与 hit 同源）。
    if dropdown == SettingsDropdown::BackgroundColor {
        if view.section != NebulaSettingsSection::Appearance {
            return;
        }
        let popup = background_color_popup(&geometry, scale);
        let (px2, py2, pw2, ph2) = popup.rect;
        // 与通用 combobox 浮层同一套皮肤：柔和投影 + hairline + 不透明面板。
        quads.push(UiQuad::glow(
            px2 - s(14.0),
            py2 - s(10.0),
            pw2 + s(28.0),
            ph2 + s(26.0),
            Rgba::new(0, 0, 0, 70),
        ));
        quads.push(UiQuad::solid(
            px2 - s(1.0),
            py2 - s(1.0),
            pw2 + s(2.0),
            ph2 + s(2.0),
            s(11.0),
            sk.hairline,
        ));
        let mut plate = sk.panel;
        plate.a = 255;
        quads.push(UiQuad::solid(px2, py2, pw2, ph2, s(10.0), plate));
        quads.push(UiQuad::solid(px2, py2, pw2, ph2, s(10.0), sk.surface));

        let selected = dropdown_selected_index(view, dropdown);
        for (index, rect) in popup.swatch.iter().enumerate() {
            let (sx, sy, sw2, sh2) = *rect;
            let hovered = view.hover == SettingsHit::BackgroundSwatch(index);
            if selected == Some(index) || hovered {
                let ring = if selected == Some(index) { sk.accent } else { sk.ink_dim };
                quads.push(UiQuad::solid(
                    sx - s(2.0),
                    sy - s(2.0),
                    sw2 + s(4.0),
                    sh2 + s(4.0),
                    s(8.0),
                    Rgba::new(ring.r, ring.g, ring.b, 255),
                ));
            }
            // 每格带 1px hairline 描边：亮色格在浅色面板上也有边界。
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw2 + s(2.0),
                sh2 + s(2.0),
                s(7.0),
                sk.hairline,
            ));
            let color = BACKGROUND_SWATCHES[index];
            quads.push(UiQuad::solid(sx, sy, sw2, sh2, s(6.0), Rgba::new(color.r, color.g, color.b, 255)));
        }

        // hex 输入框：聚焦态用主题色描边；caret 与 UI 光标共用 500ms 相位。
        let (hx, hy, hw, hh) = popup.hex;
        let focused = view.bg_hex_active;
        let border = if focused { sk.accent } else { sk.ink_dim };
        let border_alpha = if focused { 255 } else { 120 };
        quads.push(UiQuad::solid(
            hx - s(1.0),
            hy - s(1.0),
            hw + s(2.0),
            hh + s(2.0),
            s(8.0),
            Rgba::new(border.r, border.g, border.b, border_alpha),
        ));
        quads.push(UiQuad::solid(hx, hy, hw, hh, s(7.0), sk.surface));
        if focused && super::caret_blink_on() {
            let cell_w = size.cell_width();
            let caret_x = hx + s(12.0) + view.bg_hex_input.chars().count() as f32 * cell_w;
            let caret_h = hh - s(12.0);
            quads.push(UiQuad::solid(
                caret_x.min(hx + hw - s(6.0)),
                hy + (hh - caret_h) / 2.0,
                (1.5 * scale).max(1.0),
                caret_h,
                0.0,
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
            ));
        }
        return;
    }
    let (_, py, _, ph) = geometry.popup;
    let Some((anchor, count)) = dropdown_anchor(
        &geometry,
        view.section,
        dropdown,
        view.shells.len(),
        view.fonts.len() + 1,
        scale,
    ) else {
        return;
    };
    let popup =
        widgets::combobox_popup_rect(anchor, count, scale, geometry.content_top, py + ph - s(6.0));
    let selected = dropdown_selected_index(view, dropdown);
    let hover = dropdown_hover_index(view.hover, dropdown);
    widgets::push_combobox_popup(quads, popup, count, selected, hover, scale, &sk);
    if let Some(index) = selected {
        let (rx, ry, rw, rh) = widgets::popup_row_rect(popup, index, scale);
        icons::push_check(
            quads,
            rx + rw - s(16.0),
            ry + rh * 0.5,
            scale,
            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
        );
    }
}

/// Option labels for the floating dropdown; returns shell brand-icon draw
/// requests like [`draw_text`]. Must run AFTER `push_popup_quads`'s quads are
/// painted so the labels sit on top of the popup plate.
pub(super) fn draw_popup_text(
    view: &SettingsView,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) -> Vec<(String, (f32, f32, f32, f32))> {
    let mut icon_draws = Vec::new();
    let Some(dropdown) = view.dropdown else { return icon_draws };
    let s = |v: f32| v * scale;
    let sk = view.theme.skin();
    let language = view.language;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let geometry = settings_geometry(size, scale, view.area, view.scroll, view.hidden_hosts.len());
    // 背景色浮层：hex 草稿（或占位提示）画进输入框，色板格无文字。
    if dropdown == SettingsDropdown::BackgroundColor {
        if view.section != NebulaSettingsSection::Appearance {
            return icon_draws;
        }
        let popup = background_color_popup(&geometry, scale);
        let (hx, hy, hw, hh) = popup.hex;
        let ty = hy + (hh - cell_h) / 2.0;
        if view.bg_hex_input.is_empty() {
            r.draw_chrome_text(size, hx + s(12.0), ty, sk.ink_dim, "#RRGGBB", gc);
        } else {
            r.draw_chrome_text(size, hx + s(12.0), ty, sk.ink, &view.bg_hex_input, gc);
        }
        // 输入框右侧给一个动作提示（回车应用）。
        let hint = language.pick("回车应用", "Enter applies");
        let hint_cols: usize = hint.chars().map(|c| c.width().unwrap_or(0)).sum();
        let hint_x = hx + hw - s(12.0) - hint_cols as f32 * cell_w;
        if hint_x > hx + s(12.0) + 9.0 * cell_w {
            r.draw_chrome_text(size, hint_x, ty, sk.ink_dim, hint, gc);
        }
        return icon_draws;
    }
    let (_, py, _, ph) = geometry.popup;
    let Some((anchor, count)) = dropdown_anchor(
        &geometry,
        view.section,
        dropdown,
        view.shells.len(),
        view.fonts.len() + 1,
        scale,
    ) else {
        return icon_draws;
    };
    let popup =
        widgets::combobox_popup_rect(anchor, count, scale, geometry.content_top, py + ph - s(6.0));
    let selected = dropdown_selected_index(view, dropdown);
    for index in 0..count {
        let (rx, ry, rw, rh) = widgets::popup_row_rect(popup, index, scale);
        let ty = ry + (rh - cell_h) / 2.0;
        // Shell rows lead with the brand icon; every other list is text-only.
        let mut text_x = rx + s(12.0);
        let label: String = match dropdown {
            SettingsDropdown::Shell => {
                let Some((id, name, program)) = view.shells.get(index) else { continue };
                icon_draws
                    .push((id.clone(), (rx + s(8.0), ry + (rh - s(24.0)) / 2.0, s(24.0), s(24.0))));
                text_x = rx + s(40.0);
                if program.is_empty() { name.clone() } else { format!("{name}  ·  {program}") }
            },
            SettingsDropdown::Font => match view.fonts.get(index) {
                Some(family) => family.clone(),
                None => language.pick("＋  导入字体…", "+  Import font...").to_owned(),
            },
            SettingsDropdown::BackgroundFit => {
                background_image_fit_label(BACKGROUND_FIT_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::BackgroundAlignment => {
                background_image_alignment_label(BACKGROUND_ALIGNMENT_OPTIONS[index], language)
                    .to_owned()
            },
            SettingsDropdown::Language => {
                language_label(LANGUAGE_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::Accept => accept_label(ACCEPT_OPTIONS[index], language).to_owned(),
            SettingsDropdown::CursorShape => {
                cursor_shape_label(CURSOR_SHAPE_OPTIONS[index], language).to_owned()
            },
            // 上方特判提前返回；此臂只为 match 完备。
            SettingsDropdown::BackgroundColor => continue,
        };
        let import_row =
            matches!(dropdown, SettingsDropdown::Font) && view.fonts.get(index).is_none();
        let color = if selected == Some(index) || import_row { sk.accent } else { sk.ink };
        let max_chars =
            (((rx + rw - s(28.0)) - text_x).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let label = truncate_tab_label(&label, max_chars);
        r.draw_chrome_text(size, text_x, ty, color, &label, gc);
    }
    icon_draws
}

/// Draw a chrome title at `mult`× the terminal font size. Rasterized at the
/// REAL target size (`draw_doc_text`), not GPU-stretched from the base atlas —
/// stretching is what made every modal title fuzzy with ragged edges. The
/// title still grows down and to the right from the (x, y) top-left anchor.
fn draw_big_text(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    _scale: f32,
    x: f32,
    y: f32,
    mult: f32,
    ink: Rgb,
    text: &str,
) {
    r.draw_doc_text(size, x, y, mult, ink, nebula_terminal::term::cell::Flags::empty(), text, gc);
}

/// A group heading inside the content pane: clearly larger than row labels
/// (strict size hierarchy: page title 1.6× > group 1.2× > rows 1.0×) and in
/// the strong ink. One helper so every group shares one size/rhythm.
fn section_title(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    x: f32,
    y: f32,
    text: &str,
) {
    draw_big_text(r, gc, size, scale, x, y, 1.2, sk.ink_strong, text);
}

/// Draw a settings row: a left-aligned label and a right-aligned, truncated
/// value, both vertically centered. Labels are single-line by design — any
/// explanation must fit the label itself (rows with obvious semantics carry
/// no description at all). Inks come from the active theme's [`Skin`].
#[allow(clippy::too_many_arguments)]
fn row_label(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    k: &str,
    v: &str,
    value_ink: Rgb,
) {
    row_label_with_right_inset(r, gc, size, scale, sk, (rx, ry, rw, rh), k, v, value_ink, 0.0);
}

#[allow(clippy::too_many_arguments)]
fn row_label_with_right_inset(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    k: &str,
    v: &str,
    value_ink: Rgb,
    right_inset: f32,
) {
    let s = |val: f32| val * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let ty = ry + (rh - cell_h) / 2.0;
    r.draw_chrome_text(size, rx + s(16.0), ty, sk.ink, k, gc);
    let value_left = rx + rw * 0.42;
    let value_right = rx + rw - s(16.0) - right_inset;
    let max_chars = ((value_right - value_left).max(cell_w) / cell_w).floor().max(1.0) as usize;
    let value = truncate_tab_label(v, max_chars);
    let value_cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
    let vx = value_right - value_cols as f32 * cell_w;
    r.draw_chrome_text(size, vx.max(value_left), ty, value_ink, &value, gc);
}

/// Draw the Settings tab's text labels on top of its quads.
pub(super) fn draw_text(
    view: &SettingsView,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) -> Vec<(String, (f32, f32, f32, f32))> {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let sk = view.theme.skin();
    let language = view.language;

    let geometry = settings_geometry(size, scale, view.area, view.scroll, view.hidden_hosts.len());
    // Kept for parity with [`draw_popup_text`]'s shell icons; the base page
    // currently stages no icon draws of its own.
    let icon_draws = Vec::new();
    let (px, py, _pw, ph) = geometry.popup;
    let (content_x, content_y, _content_w, _) = geometry.content;
    // Text has no scissor, so unlike the quad pass (which cuts quads at the
    // viewport edges) a text block is drawn only when it fits ENTIRELY inside
    // the viewport — a glyph must never cross the header hairline.
    let clip_top = geometry.content_top;
    let clip_bot = py + ph - s(6.0);
    let visible = |ry: f32, rh: f32| ry >= clip_top && ry + rh <= clip_bot;
    // 通用 combobox 的当前值：控件框内左对齐，截断在 chevron 井之前。
    let combobox_value = |r: &mut Renderer,
                          gc: &mut GlyphCache,
                          row: (f32, f32, f32, f32),
                          value: &str,
                          ink: Rgb| {
        let rect = widgets::combobox_rect(row, scale);
        let tx = widgets::combobox_text_x(rect, scale);
        let right = widgets::combobox_text_right(rect, scale);
        let max_chars = ((right - tx).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let value = truncate_tab_label(value, max_chars);
        r.draw_chrome_text(size, tx, rect.1 + (rect.3 - cell_h) / 2.0, ink, &value, gc);
    };
    // Group titles hang 42px above their first row (title + 16px gap) and
    // scroll with it.
    let group_y = |row_y: f32| row_y - s(42.0);
    let title_h = s(26.0);

    // Brand title in the sidebar header. Drawn large via the scaled-glyph path
    // so it anchors the panel instead of reading as just another row label.
    draw_big_text(
        r,
        gc,
        size,
        scale,
        px + s(24.0),
        py + s(22.0),
        1.5,
        sk.ink_strong,
        language.pick("Nebula 设置", "Nebula Settings"),
    );
    {
        // Center the reset label inside its ghost button.
        let (rx, ry, rw, rh) = geometry.reset;
        let label = language.pick("恢复默认设置", "Restore defaults");
        let cols: usize = label.chars().map(|c| c.width().unwrap_or(0)).sum();
        let tx = rx + (rw - cols as f32 * cell_w) / 2.0;
        r.draw_chrome_text(size, tx, ry + (rh - cell_h) / 2.0, sk.ink_dim, label, gc);
    }
    let section = view.section;
    // Sidebar navigation labels — only the two wired-up sections.
    for (nav_section, nx, ny, _nw, nh) in geometry.nav {
        let active = nav_section == section;
        let hovered = view.hover == SettingsHit::Nav(nav_section);
        r.draw_chrome_text(
            size,
            nx + s(18.0),
            ny + (nh - cell_h) / 2.0,
            if active {
                sk.accent
            } else if hovered {
                sk.ink
            } else {
                sk.ink_dim
            },
            nav_section.label(view.language),
            gc,
        );
    }
    // Content header: the big section title alone. (No subtitle — the nav
    // label + title already say everything; the old dim sentence only added
    // noise under the heading.)
    draw_big_text(
        r,
        gc,
        size,
        scale,
        content_x + s(24.0),
        content_y + s(20.0),
        1.6,
        sk.ink_strong,
        section.label(view.language),
    );

    match section {
        NebulaSettingsSection::Appearance => {
            // Live preview: sample lines in the CURRENT font/size on the
            // CURRENT terminal colors; the demo cursor quad shares this
            // layout via `preview_line_y`.
            {
                let (vx, vy, _, vh) = geometry.preview;
                if visible(group_y(vy), title_h) {
                    section_title(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        content_x + s(24.0),
                        group_y(vy),
                        language.pick("预览", "Preview"),
                    );
                }
                if visible(vy, vh) {
                    let fg = view.preview_fg;
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 0.0, scale),
                        fg,
                        "user@nebula ~ $ nebula --version",
                        gc,
                    );
                    let sample = format!(
                        "Nebula Terminal · {} · {:.0}px",
                        view.font_family, view.font_size_px
                    );
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 1.0, scale),
                        fg,
                        &sample,
                        gc,
                    );
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 2.0, scale),
                        fg,
                        "❯",
                        gc,
                    );
                }
            }
            let cards_y = geometry.options[0].2;
            if visible(group_y(cards_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    content_x + s(24.0),
                    group_y(cards_y),
                    language.pick("主题", "Themes"),
                );
            }
            for (theme, ox, oy, ow, oh) in geometry.options {
                let selected = theme == view.theme;
                let hovered = view.hover == SettingsHit::Theme(theme);
                // The label rides the card's 2px hover lift (quads do the
                // same), and hides only when IT would cross the viewport edge
                // — a half-clipped card keeps its fully-visible label.
                let lift = if hovered && !selected { s(2.0) } else { 0.0 };
                let text_y = oy + oh + s(12.0) - lift;
                if !visible(text_y, cell_h) {
                    continue;
                }
                let card_label = theme.short_label();
                r.draw_chrome_text(
                    size,
                    ox + (ow - card_label.chars().count() as f32 * cell_w) / 2.0,
                    text_y,
                    if selected {
                        sk.accent
                    } else if hovered {
                        sk.ink
                    } else {
                        sk.ink_dim
                    },
                    card_label,
                    gc,
                );
            }
            let (st_x, st_y, _, st_h) = geometry.system_theme;
            if visible(group_y(st_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    st_x,
                    group_y(st_y),
                    language.pick("主题模式", "Theme mode"),
                );
            }
            if visible(st_y, st_h) {
                r.draw_chrome_text(
                    size,
                    st_x + s(16.0),
                    st_y + (st_h - cell_h) / 2.0,
                    sk.ink,
                    language.pick("跟随系统明暗模式", "Follow system appearance"),
                    gc,
                );
            }
            let (bg_x, bg_y, _, bg_h) = geometry.background;
            if visible(group_y(bg_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    bg_x,
                    group_y(bg_y),
                    language.pick("自定义背景", "Custom background"),
                );
            }
            if visible(bg_y, bg_h) {
                let background_v = view
                    .background
                    .map(format_hex_rgb)
                    .unwrap_or_else(|| language.pick("主题默认", "Theme default").to_owned());
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background,
                    language.pick("背景色", "Background color"),
                    &background_v,
                    sk.accent,
                );
            }
            let (img_x, img_y, _, img_h) = geometry.background_image;
            let _ = img_x;
            if visible(img_y, img_h) {
                let image_v = view
                    .background_image
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_else(|| language.pick("未设置", "Not set").to_owned());
                row_label_with_right_inset(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image,
                    language.pick("背景图片", "Background image"),
                    &image_v,
                    sk.accent,
                    if view.background_image.is_some() { s(48.0) } else { 0.0 },
                );
                if view.background_image.is_some() {
                    let (cx, cy, cw, ch) = geometry.background_image_clear;
                    r.draw_chrome_text(
                        size,
                        cx + (cw - cell_w) / 2.0,
                        cy + (ch - cell_h) / 2.0,
                        if view.hover == SettingsHit::BackgroundImageClear {
                            sk.ink
                        } else {
                            sk.ink_dim
                        },
                        "↶",
                        gc,
                    );
                }
            }
            let (_, fit_y, _, fit_h) = geometry.background_image_fit;
            if visible(fit_y, fit_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_fit,
                    language.pick("背景图像拉伸模式", "Background image stretch mode"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.background_image_fit,
                    background_image_fit_label(view.background_image_fit, language),
                    sk.accent,
                );
            }
            let (_, align_y, _, align_h) = geometry.background_image_alignment;
            if visible(align_y, align_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_alignment,
                    language.pick("背景图像对齐", "Background image alignment"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.background_image_alignment,
                    background_image_alignment_label(view.background_image_alignment, language),
                    sk.accent,
                );
            }
            let (_, image_opacity_y, _, image_opacity_h) = geometry.background_image_opacity_row;
            if visible(image_opacity_y, image_opacity_h) {
                r.draw_chrome_text(
                    size,
                    geometry.background_image_opacity_row.0 + s(16.0),
                    image_opacity_y + (image_opacity_h - cell_h) / 2.0,
                    sk.ink,
                    language.pick("背景图像不透明度", "Background image opacity"),
                    gc,
                );
                let image_opacity_v = format!("{:.0}%", view.background_image_opacity * 100.0);
                let image_opacity_cols: usize =
                    image_opacity_v.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    geometry.background_image_opacity_slider.0
                        - s(10.0)
                        - image_opacity_cols as f32 * cell_w,
                    image_opacity_y + (image_opacity_h - cell_h) / 2.0,
                    sk.accent,
                    &image_opacity_v,
                    gc,
                );
            }
            let (_, cover_y, _, cover_h) = geometry.background_image_cover_chrome;
            if visible(cover_y, cover_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_cover_chrome,
                    language.pick(
                        "将背景图扩展到标题栏和侧边栏",
                        "Extend background image into title bar and sidebar",
                    ),
                    "",
                    sk.ink,
                );
            }
            // ---- 光标组 ----
            let (cs_x, cs_y, _, cs_h) = geometry.cursor_shape_row;
            if visible(group_y(cs_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    cs_x,
                    group_y(cs_y),
                    language.pick("光标", "Cursor"),
                );
            }
            if visible(cs_y, cs_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cursor_shape_row,
                    language.pick("光标形状", "Cursor shape"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.cursor_shape_row,
                    cursor_shape_label(view.cursor_shape, language),
                    sk.accent,
                );
            }
            let (_, blink_y, _, blink_h) = geometry.cursor_blink_row;
            if visible(blink_y, blink_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cursor_blink_row,
                    language.pick("光标闪烁", "Cursor blinking"),
                    "",
                    sk.ink,
                );
            }
            let (or_x, or_y, _, or_h) = geometry.opacity_row;
            let (lr_x, lr_y, _, lr_h) = geometry.language_row;
            if visible(group_y(lr_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    content_x + s(24.0),
                    group_y(lr_y),
                    language.pick("界面", "Interface"),
                );
            }
            if visible(lr_y, lr_h) {
                r.draw_chrome_text(
                    size,
                    lr_x + s(16.0),
                    lr_y + (lr_h - cell_h) / 2.0,
                    sk.ink,
                    language.pick("语言", "Language"),
                    gc,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.language_row,
                    language_label(view.language_preference, language),
                    sk.accent,
                );
            }
            if visible(or_y, or_h) {
                r.draw_chrome_text(
                    size,
                    or_x + s(16.0),
                    or_y + (or_h - cell_h) / 2.0,
                    sk.ink,
                    language.pick("终端正文不透明度", "Terminal content opacity"),
                    gc,
                );
                let opacity_v = format!("{:.0}%", view.opacity * 100.0);
                let opacity_cols: usize = opacity_v.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    geometry.opacity_slider.0 - s(10.0) - opacity_cols as f32 * cell_w,
                    or_y + (or_h - cell_h) / 2.0,
                    sk.accent,
                    &opacity_v,
                    gc,
                );
            }
        },
        NebulaSettingsSection::Profiles => {
            // Rows carry single, self-explanatory Chinese labels — the old
            // second-line descriptions overflowed the 44px rows and collided
            // with the next group's title.
            let (sh_x, sh_y, _, sh_h) = geometry.shell;
            if visible(group_y(sh_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    sh_x,
                    group_y(sh_y),
                    language.pick("终端", "Terminal"),
                );
            }
            if visible(sh_y, sh_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.shell,
                    language.pick("默认 Shell", "Default shell"),
                    "",
                    sk.ink,
                );
                combobox_value(r, gc, geometry.shell, &view.shell_label, sk.accent);
            }
            if visible(geometry.startup_directory.1, geometry.startup_directory.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.startup_directory,
                    language.pick("启动目录", "Startup directory"),
                    "",
                    sk.ink,
                );

                let (dx, dy, dw, dh) = geometry.startup_directory;
                // 与"默认 Shell / 终端字体"同一右对齐基线；有清除按钮时向左避让。
                let value_left = dx + dw * 0.42;
                let value_right = if view.startup_directory.is_some() {
                    geometry.startup_directory_clear.0 - s(12.0)
                } else {
                    dx + dw - s(16.0)
                };
                let max_chars = ((value_right - value_left).max(cell_w) / cell_w).floor() as usize;
                let value = view
                    .startup_directory
                    .as_deref()
                    .unwrap_or_else(|| language.pick("继承当前目录", "Inherit current directory"));
                let value = truncate_tab_label(value, max_chars.max(1));
                let value_cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
                let value_x = (value_right - value_cols as f32 * cell_w).max(value_left);

                r.draw_chrome_text(
                    size,
                    value_x,
                    dy + (dh - cell_h) / 2.0,
                    if view.startup_directory.is_some() { sk.accent } else { sk.ink_dim },
                    &value,
                    gc,
                );

                if view.startup_directory.is_some() {
                    let (cx, cy, cw, ch) = geometry.startup_directory_clear;
                    let clear = language.pick("清除", "Clear");
                    let clear_cols: usize =
                        clear.chars().map(|character| character.width().unwrap_or(0)).sum();
                    r.draw_chrome_text(
                        size,
                        cx + (cw - clear_cols as f32 * cell_w) / 2.0,
                        cy + (ch - cell_h) / 2.0,
                        sk.accent,
                        clear,
                        gc,
                    );
                }
            }
            if visible(geometry.font.1, geometry.font.3) {
                let font_value = view.font_notice.as_deref().unwrap_or(&view.font_family);
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.font,
                    language.pick("终端字体", "Terminal font"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.font,
                    font_value,
                    if view.font_notice.is_some() { sk.ink_dim } else { sk.accent },
                );
            }
            if visible(geometry.font_size_row.1, geometry.font_size_row.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.font_size_row,
                    language.pick("终端字号（Ctrl+滚轮缩放）", "Font size (Ctrl+wheel zooms)"),
                    "",
                    sk.ink,
                );
                let (value_box, _, _) = widgets::spinner_rects(geometry.font_size_row, scale);
                let value = format!("{:.0}", view.font_size_px);
                let cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    value_box.0 + (value_box.2 - cols as f32 * cell_w) / 2.0,
                    value_box.1 + (value_box.3 - cell_h) / 2.0,
                    sk.ink,
                    &value,
                    gc,
                );
            }
            // Boolean rows: the switch (drawn in `push_quads`) carries the
            // state; no "On/Off" string next to it.
            if visible(geometry.fetch.1, geometry.fetch.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.fetch,
                    language.pick("启动欢迎信息", "Startup welcome"),
                    "",
                    sk.ink,
                );
            }
            if visible(geometry.powerline.1, geometry.powerline.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.powerline,
                    language.pick("Powerline 提示符", "Powerline prompt"),
                    "",
                    sk.ink,
                );
            }

            let (gh_x, gh_y, _, gh_h) = geometry.ghost;
            if visible(group_y(gh_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    gh_x,
                    group_y(gh_y),
                    language.pick("补全", "Completion"),
                );
            }
            if visible(gh_y, gh_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.ghost,
                    language.pick("历史补全灰字", "History ghost text"),
                    "",
                    sk.ink,
                );
            }
            if visible(geometry.accept.1, geometry.accept.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.accept,
                    language.pick("补全接受键", "Completion accept key"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.accept,
                    accept_label(view.accept, language),
                    sk.accent,
                );
            }

            let (ocx, ocy, _ocw, och) = geometry.open_config_file;
            if visible(group_y(ocy), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ocx,
                    group_y(ocy),
                    language.pick("配置文件", "Configuration"),
                );
            }
            if visible(ocy, och) {
                r.draw_chrome_text(
                    size,
                    ocx + s(16.0),
                    ocy + (och - cell_h) / 2.0,
                    sk.accent,
                    language.pick("打开配置文件", "Open configuration file"),
                    gc,
                );
            }

            if geometry.hidden_host_count > 0 {
                let (hx, hy, hw, hh) = geometry.hidden_host_row0;
                if visible(group_y(hy), title_h) {
                    section_title(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        hx,
                        group_y(hy),
                        language.pick(
                            "已隐藏 SSH 主机 · 密码不会恢复",
                            "Hidden SSH hosts · passwords are not restored",
                        ),
                    );
                }
                for (index, host) in view.hidden_hosts.iter().enumerate() {
                    let rect = (hx, hy + index as f32 * hh, hw, hh);
                    if visible(rect.1, rect.3) {
                        row_label(
                            r,
                            gc,
                            size,
                            scale,
                            &sk,
                            rect,
                            host,
                            language.pick("恢复", "Restore"),
                            sk.accent,
                        );
                    }
                }
            }
        },
        NebulaSettingsSection::Interaction => {
            let (ix, iy, _, ih) = geometry.copy_on_select;
            if visible(group_y(iy), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ix,
                    group_y(iy),
                    language.pick("剪贴板", "Clipboard"),
                );
            }
            if visible(iy, ih) {
                // The switch (drawn in `push_quads`) carries the state; the
                // label spells out the OFF fallback so both modes are clear.
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.copy_on_select,
                    language.pick(
                        "自动将所选内容复制到剪贴板（关闭时右键复制 / 粘贴）",
                        "Copy selection to clipboard (off: right-click copies / pastes)",
                    ),
                    "",
                    sk.ink,
                );
            }
        },
        NebulaSettingsSection::Keymap => {
            let (kx, ky, kw, kh) = geometry.keymap_row0;
            if visible(group_y(ky), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    kx,
                    group_y(ky),
                    language.pick(
                        "快捷键（可在配置文件 [[keyboard.bindings]] 中自定义）",
                        "Shortcuts (customize in [[keyboard.bindings]])",
                    ),
                );
            }
            for (i, (zh_label, en_label, combo)) in KEYMAP_ROWS.iter().enumerate() {
                let rect = (kx, ky + i as f32 * geometry.keymap_row_h, kw, kh);
                if visible(rect.1, rect.3) {
                    row_label(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        rect,
                        view.language.pick(zh_label, en_label),
                        combo,
                        sk.ink_dim,
                    );
                }
            }
        },
        NebulaSettingsSection::Advanced => {
            let (ax, ay, _, ah) = geometry.keep_session;
            if visible(group_y(ay), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ax,
                    group_y(ay),
                    language.pick("会话", "Sessions"),
                );
            }
            if visible(ay, ah) {
                // The switch (drawn in `push_quads`) carries the state; the
                // label says what closing a window keeps alive while it is ON.
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.keep_session,
                    language.pick(
                        "关闭窗口后保留会话（后台驻留，可恢复对话）",
                        "Keep sessions after closing the window (resident and restorable)",
                    ),
                    "",
                    sk.ink,
                );
            }
        },
    }
    icon_draws
}

#[cfg(test)]
mod opacity_tests {
    use super::opacity_from_pointer;

    #[test]
    fn slider_pointer_maps_to_clamped_fraction() {
        let slider = (100.0, 20.0, 200.0, 36.0);
        assert_eq!(opacity_from_pointer(50.0, slider), 0.0);
        assert_eq!(opacity_from_pointer(100.0, slider), 0.0);
        assert_eq!(opacity_from_pointer(200.0, slider), 0.5);
        assert_eq!(opacity_from_pointer(300.0, slider), 1.0);
        assert_eq!(opacity_from_pointer(350.0, slider), 1.0);
    }
}
