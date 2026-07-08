//! Top-left settings modal: a centered floating panel for Nebula's runtime
//! appearance and completion settings.
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
//! `NebulaTheme::palette`, `AcceptKey::label`, …) via `super::` — no visibility
//! churn needed in `mod.rs`.

use unicode_width::UnicodeWidthChar;

use crate::config::UiConfig;
use crate::display::color::Rgb;
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use super::theme::Skin;
use super::{
    AcceptKey, NebulaShell, NebulaTheme, SizeInfo, chrome_settings_button_rect, contains_rect,
    nebula_data_dir, truncate_tab_label,
};

// Visual language: one flat panel color, one hairline, three text grays, ONE
// accent — hierarchy comes from typography and spacing. Every color is a
// [`Skin`] token from `display::theme` (single source of truth), so the modal
// flips correctly between the light and dark theme families.

/// Sidebar sections of the settings panel. Deliberately small: only sections
/// with real functionality behind them are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NebulaSettingsSection {
    /// Themes, custom colors, wallpaper and window opacity.
    #[default]
    Appearance,
    /// Completion behaviour plus the raw `nebula_settings.txt` config file.
    Profiles,
}

impl NebulaSettingsSection {
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "外观",
            Self::Profiles => "配置文件",
        }
    }
}

/// Hit result for the top-left Nebula settings affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    None,
    Toggle,
    Panel,
    Dismiss,
    Nav(NebulaSettingsSection),
    Theme(NebulaTheme),
    GhostToggle,
    AcceptCycle,
    ShellCycle,
    FetchToggle,
    PowerlineToggle,
    OpacityDown,
    OpacityUp,
    BackgroundColor,
    BackgroundImage,
    OpenConfigFile,
    Reset,
}

// ---- runtime settings store (`Nebula/nebula_settings.txt`) ----

pub(super) struct NebulaRuntimeSettings {
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    pub(super) shell: NebulaShell,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    pub(super) opacity: f32,
    pub(super) background: Option<Rgb>,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    /// Chrome theme. Persisted so a restart keeps the chosen look AND the
    /// powerline bridge file gets rewritten with the right name on boot
    /// (it used to be reset to the default theme every launch).
    pub(super) theme: NebulaTheme,
}

/// Load runtime UI settings from `Nebula/nebula_settings.txt`; defaults when
/// absent. Format is one `key=value` per line so power users can edit it while
/// the graphical settings page catches up.
pub(super) fn nebula_settings_load(config: &UiConfig) -> NebulaRuntimeSettings {
    let path = nebula_data_dir().join("nebula_settings.txt");
    let mut settings = NebulaRuntimeSettings {
        ghost: true,
        accept: AcceptKey::Both,
        shell: NebulaShell::PowerShell,
        // Off by default: the welcome screen pipes a whole script through the
        // fresh shell and repaints on resize — real startup-latency cost on
        // the critical path (user ruling: startup speed outranks the art).
        fetch: false,
        powerline: true,
        opacity: config.window_opacity(),
        background: None,
        background_image: None,
        background_image_opacity: 0.38,
        theme: NebulaTheme::default(),
    };
    if let Ok(data) = std::fs::read_to_string(path) {
        for line in data.lines() {
            match line.split_once('=') {
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
                    if let Some(shell) = NebulaShell::from_settings(v) {
                        settings.shell = shell;
                    }
                },
                Some(("fetch", v)) => settings.fetch = parse_bool(v, true),
                Some(("powerline", v)) => settings.powerline = parse_bool(v, true),
                Some(("opacity", v)) => {
                    if let Ok(opacity) = v.trim().parse::<f32>() {
                        settings.opacity = opacity.clamp(0.35, 1.0);
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

fn parse_hex_rgb(value: &str) -> Option<Rgb> {
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
    let shell = settings.shell.settings_value();
    let theme = settings.theme.prompt_name();
    let path = nebula_data_dir().join("nebula_settings.txt");
    let _ = std::fs::write(
        path,
        format!(
            "theme={theme}\nghost={}\naccept={accept}\nshell={shell}\nfetch={}\npowerline={}\nopacity={:.2}\nbackground={background}\nbackground_image={background_image}\nbackground_image_opacity={:.2}\n",
            settings.ghost as u8,
            settings.fetch as u8,
            settings.powerline as u8,
            settings.opacity,
            settings.background_image_opacity
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
    nav: [(NebulaSettingsSection, f32, f32, f32, f32); 2],
    options: [(NebulaTheme, f32, f32, f32, f32); 7],
    shell: (f32, f32, f32, f32),
    fetch: (f32, f32, f32, f32),
    powerline: (f32, f32, f32, f32),
    ghost: (f32, f32, f32, f32),
    accept: (f32, f32, f32, f32),
    open_config_file: (f32, f32, f32, f32),
    /// Full-width "窗口透明度" row (the frame); the stepper buttons below sit
    /// inside it.
    opacity_row: (f32, f32, f32, f32),
    opacity_down: (f32, f32, f32, f32),
    opacity_up: (f32, f32, f32, f32),
    background: (f32, f32, f32, f32),
    background_image: (f32, f32, f32, f32),
    reset: (f32, f32, f32, f32),
    /// Top edge of the scrollable content viewport (just below the fixed
    /// header band); everything above it never scrolls.
    content_top: f32,
    /// Total designed content height per section (scaled px, measured from
    /// `content_top`). `max_scroll = (height - viewport).max(0)`.
    appearance_h: f32,
    profiles_h: f32,
}

/// Scrollable-content viewport height for the settings modal.
fn settings_viewport_h(popup_h: f32, scale_factor: f32) -> f32 {
    popup_h - 72.0 * scale_factor
}

/// Max scroll offset for `section` at the current window size. The input
/// layer clamps its accumulated wheel delta with this.
pub(super) fn settings_max_scroll(
    size_info: &SizeInfo,
    scale_factor: f32,
    section: NebulaSettingsSection,
) -> f32 {
    let geometry = settings_geometry(size_info, scale_factor, 0.0);
    let (_, _, _, ph) = geometry.popup;
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
    };
    (content_h - settings_viewport_h(ph, scale_factor)).max(0.0)
}

fn settings_geometry(size_info: &SizeInfo, scale_factor: f32, scroll: f32) -> SettingsGeometry {
    let s = |v: f32| v * scale_factor;
    let w = size_info.width();
    let h = size_info.height();
    let margin = s(8.0);
    let bar_h = s(40.0);
    let gear = chrome_settings_button_rect(size_info, scale_factor);

    // Floating, centered modal — fixed design size clamped to the window so it
    // never fills the whole screen, sized to hold the 4+3 theme card grid plus
    // the compact appearance/profile controls (no large vertical gaps).
    let popup_w = s(1000.0).min(w - 2.0 * margin);
    let popup_h = s(640.0).min(h - 2.0 * bar_h - 2.0 * margin);
    let popup_x = ((w - popup_w) * 0.5).max(margin);
    let popup_y = ((h - popup_h) * 0.5).max(bar_h + margin);
    let sidebar_w = s(240.0).min(popup_w * 0.30);
    let sidebar = (popup_x, popup_y, sidebar_w, popup_h);
    let content_x = popup_x + sidebar_w + s(16.0);
    let content_w = (popup_w - sidebar_w - s(16.0)).max(s(520.0));
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

    // Theme cards flow as a 4 + 3 grid with the design sheet's 20px gaps:
    // slot i sits at column i%4 on row i/4. The row pitch reserves a strip
    // under each card for its label (12px gap + text + 20px grid row gap).
    let card_gap = s(20.0);
    let card_w = ((content_w - s(48.0) - 3.0 * card_gap) / 4.0).clamp(s(88.0), s(170.0));
    let card_h = s(64.0);
    let card_y0 = 146.0;
    let card_x = content_x + s(24.0);
    let card_row_pitch = card_h + s(48.0);
    let card = |i: f32| card_x + (i % 4.0) * (card_w + card_gap);
    let card_slot_y = |i: f32| at(card_y0) + (i / 4.0).floor() * card_row_pitch;

    let row_x = content_x + s(24.0);
    let row_w = content_w - s(48.0);
    let row_h = s(ROW_H);

    // Appearance: cards (146..322 design px + label strip), colors group,
    // interface group. The card block spans 2 grid rows of `64 + 48` design
    // px each (card + label strip, matching `card_row_pitch` above).
    let color_y0 = card_y0 + 2.0 * (64.0 + 48.0) + GROUP_ADVANCE;
    let iface_y0 = color_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let appearance_h = s(iface_y0 + ROW_H + 32.0 - 72.0);
    // Opacity row controls, right-aligned with NO overlap: value · − · slider · +.
    let opacity_row = (row_x, at(iface_y0), row_w, row_h);
    let opacity_down = (row_x + row_w - s(224.0), at(iface_y0) + s(4.0), s(40.0), s(36.0));
    let opacity_up = (row_x + row_w - s(46.0), at(iface_y0) + s(4.0), s(40.0), s(36.0));

    // Sidebar navigation rows. Only the two wired-up sections are listed; the
    // rects line up with the active-row highlight drawn while rendering.
    // 4px between items — same breathing as the design sheet's nav menu.
    let nav_x = popup_x + s(24.0);
    let nav_w = sidebar_w - s(48.0);
    let nav_h = s(44.0);
    let nav_gap = s(4.0);
    let nav_y0 = popup_y + s(88.0);
    let nav = [
        (NebulaSettingsSection::Appearance, nav_x, nav_y0, nav_w, nav_h),
        (NebulaSettingsSection::Profiles, nav_x, nav_y0 + nav_h + nav_gap, nav_w, nav_h),
    ];

    // Profiles: three groups — terminal (3 rows), completion (2), config file.
    let shell_y0 = 146.0;
    let ghost_y0 = shell_y0 + 3.0 * ROW_H + GROUP_ADVANCE;
    let open_y0 = ghost_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let profiles_h = s(open_y0 + ROW_H + 32.0 - 72.0);

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
        background: (row_x, at(color_y0), row_w, row_h),
        background_image: (row_x, at(color_y0 + ROW_H), row_w, row_h),
        opacity_row,
        opacity_down,
        opacity_up,
        shell: (row_x, at(shell_y0), row_w, row_h),
        fetch: (row_x, at(shell_y0 + ROW_H), row_w, row_h),
        powerline: (row_x, at(shell_y0 + 2.0 * ROW_H), row_w, row_h),
        ghost: (row_x, at(ghost_y0), row_w, row_h),
        accept: (row_x, at(ghost_y0 + ROW_H), row_w, row_h),
        open_config_file: (row_x, at(open_y0), row_w, row_h),
        reset: (popup_x + popup_w - s(170.0), popup_y + s(24.0), s(150.0), s(42.0)),
        content_top,
        appearance_h,
        profiles_h,
    }
}

/// Hit-test the top-left settings button and its popup. `scroll` must be the
/// same offset the renderer used, so hits land on what the user actually sees;
/// rows scrolled out of the content viewport don't respond.
pub fn settings_hit(
    size_info: &SizeInfo,
    scale_factor: f32,
    x: f32,
    y: f32,
    popup_open: bool,
    section: NebulaSettingsSection,
    scroll: f32,
) -> SettingsHit {
    let geometry = settings_geometry(size_info, scale_factor, scroll);

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
                if contains_rect(geometry.background, x, y) {
                    return SettingsHit::BackgroundColor;
                }
                if contains_rect(geometry.background_image, x, y) {
                    return SettingsHit::BackgroundImage;
                }
                if contains_rect(geometry.opacity_down, x, y) {
                    return SettingsHit::OpacityDown;
                }
                if contains_rect(geometry.opacity_up, x, y) {
                    return SettingsHit::OpacityUp;
                }
            },
            NebulaSettingsSection::Profiles => {
                if contains_rect(geometry.shell, x, y) {
                    return SettingsHit::ShellCycle;
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
            },
        }
    }

    if contains_rect(geometry.popup, x, y) { SettingsHit::Panel } else { SettingsHit::Dismiss }
}

// ---- rendering ----

/// A per-frame snapshot of the display state the settings render reads. Owns its
/// data (notably the wallpaper path) so the caller can hand it in by reference
/// while still borrowing `&mut renderer` for [`draw_text`].
pub(super) struct SettingsView {
    pub(super) section: NebulaSettingsSection,
    pub(super) hover: SettingsHit,
    pub(super) theme: NebulaTheme,
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    pub(super) shell: NebulaShell,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    pub(super) opacity: f32,
    pub(super) background: Option<Rgb>,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    /// Content scroll offset in scaled px (0 = top). Owned by `Display`,
    /// clamped there against [`settings_max_scroll`].
    pub(super) scroll: f32,
}

/// Push the settings modal's background quads (dim veil, flat neutral panel,
/// sidebar separator, nav indicator, rows, theme cards and controls). Caller
/// guards on the panel being open.
pub(super) fn push_quads(view: &SettingsView, quads: &mut Vec<UiQuad>, size: &SizeInfo, scale: f32) {
    let s = |v: f32| v * scale;
    let w = size.width();
    let h = size.height();
    let sk = view.theme.skin();

    let geometry = settings_geometry(size, scale, view.scroll);
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

    // Heavily dim the whole window behind the modal. The panel is now opaque,
    // so the veil's only job is to push the terminal far back — a strong, near
    // -black wash reads as "modal focus" instead of the old faint glass tint.
    quads.push(UiQuad::solid(0.0, 0.0, w, h, 0.0, Rgba::new(0, 0, 0, 205)));

    // Diffuse shadow: one wide, soft glow under the panel sells the Z-depth
    // (the softer and wider, the higher the panel appears to float).
    quads.push(UiQuad::glow(
        px - s(44.0),
        py - s(26.0),
        pw + s(88.0),
        ph + s(96.0),
        Rgba::new(0, 0, 0, 180),
    ));

    // Hairline edge + near-opaque themed panel (two-layer stroke: 1px larger
    // quad underneath), then a 1px top edge-light — the lit bevel.
    quads.push(UiQuad::solid(px - s(1.0), py - s(1.0), pw + s(2.0), ph + s(2.0), s(13.0), sk.hairline));
    quads.push(UiQuad::solid(px, py, pw, ph, s(12.0), sk.panel));
    quads.push(UiQuad::solid(
        px + s(12.0),
        py,
        pw - s(24.0),
        s(1.0),
        0.0,
        // Top bevel: a subtle white lift on dark themes; on light panels a
        // faint dark line reads as the lit edge instead.
        if sk.is_light { Rgba::new(0, 0, 0, 24) } else { Rgba::new(255, 255, 255, 20) },
    ));

    let (side_x, side_y, side_w, side_h) = geometry.sidebar;

    // Sidebar: no fill of its own — just a hairline separator on its right
    // edge. Hierarchy comes from the nav rows, not a competing surface.
    quads.push(UiQuad::solid(side_x + side_w - s(1.0), side_y + s(16.0), s(1.0), side_h - s(32.0), 0.0, sk.hairline));

    // Header separator under the panel title row.
    quads.push(UiQuad::solid(px + s(1.0), py + header_h, pw - s(2.0), s(1.0), 0.0, sk.hairline));

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
        quads.push(UiQuad::solid(rx - s(1.0), ry - s(1.0), rw + s(2.0), rh + s(2.0), s(9.0), sk.hairline));
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
        clip(quads, UiQuad::solid(gx - s(1.0), gy - s(1.0), gw + s(2.0), gh + s(2.0), s(9.0), sk.hairline));
        clip(quads, UiQuad::solid(gx, gy, gw, gh, s(8.0), sk.panel));
        for i in 1..rows {
            clip(quads, UiQuad::solid(gx + s(1.0), gy + i as f32 * row_h, gw - s(2.0), s(1.0), 0.0, sk.hairline));
        }
    };
    let row_hover = |quads: &mut Vec<UiQuad>, rect: (f32, f32, f32, f32), hovered: bool| {
        if hovered {
            let (rx, ry, rw, rh) = rect;
            clip(quads, UiQuad::solid(rx + s(2.0), ry + s(2.0), rw - s(4.0), rh - s(4.0), s(6.0), sk.hover));
        }
    };

    // A toggle switch at the right edge of `rect`: pill track + round thumb.
    // `on` fills the track with the accent; off stays a muted gray.
    let toggle = |quads: &mut Vec<UiQuad>, rect: (f32, f32, f32, f32), on: bool| {
        let (rx, ry, rw, rh) = rect;
        let tw = s(38.0);
        let th = s(20.0);
        let tx = rx + rw - s(16.0) - tw;
        let ty = ry + (rh - th) / 2.0;
        clip(quads, UiQuad::solid(tx, ty, tw, th, th / 2.0, if on { Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255) } else { sk.track_off }));
        let knob = th - s(6.0);
        let kx = if on { tx + tw - s(3.0) - knob } else { tx + s(3.0) };
        let kcol = if on { sk.knob_on } else { sk.knob_off };
        clip(quads, UiQuad::solid(kx, ty + s(3.0), knob, knob, knob / 2.0, kcol));
    };

    match section {
        NebulaSettingsSection::Appearance => {
            // Theme cards ARE the swatches: each card is filled with its own
            // theme's real panel color. Selection = accent ring + halo; hover
            // = the card lifts 2px and glows softly (the design sheet's
            // floating-card hover) — no wash, so the swatch color stays true.
            for (theme, ox, oy, ow, oh) in geometry.options {
                let selected = theme == view.theme;
                let hovered = view.hover == SettingsHit::Theme(theme);
                let lift = if hovered && !selected { s(2.0) } else { 0.0 };
                let oy = oy - lift;
                let stroke = if selected { Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255) } else { sk.hairline };
                let stroke_w = if selected { s(2.0) } else { s(1.0) };
                if selected {
                    // Selected card glows softly: the accent ring plus a
                    // diffuse halo, per the design sheet's lit-control look.
                    clip(quads, UiQuad::glow(
                        ox - s(14.0),
                        oy - s(14.0),
                        ow + s(28.0),
                        oh + s(28.0),
                        Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 66),
                    ));
                } else if hovered {
                    // Hover halo: same shape, fainter — enough 辉光 to read
                    // as "lit up" without competing with the selected card.
                    clip(quads, UiQuad::glow(
                        ox - s(12.0),
                        oy - s(10.0),
                        ow + s(24.0),
                        oh + s(26.0),
                        Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 38),
                    ));
                }
                clip(quads, UiQuad::solid(
                    ox - stroke_w,
                    oy - stroke_w,
                    ow + 2.0 * stroke_w,
                    oh + 2.0 * stroke_w,
                    s(9.0),
                    stroke,
                ));
                let mut card_bg = theme.palette().panel;
                card_bg.a = 255;
                clip(quads, UiQuad::solid(ox, oy, ow, oh, s(8.0), card_bg));
            }

            // 自定义背景: one 2-row frame. 界面: one single-row frame.
            group_frame(quads, geometry.background, 2);
            row_hover(quads, geometry.background, view.hover == SettingsHit::BackgroundColor);
            row_hover(quads, geometry.background_image, view.hover == SettingsHit::BackgroundImage);
            group_frame(quads, geometry.opacity_row, 1);

            // Opacity controls inside the row: ghost stepper buttons flanking
            // a muted rail with a flat accent fill and a round thumb.
            for (hit, rect) in [
                (SettingsHit::OpacityDown, geometry.opacity_down),
                (SettingsHit::OpacityUp, geometry.opacity_up),
            ] {
                let (bx, by, bw, bh) = rect;
                clip(quads, UiQuad::solid(bx - s(1.0), by - s(1.0), bw + s(2.0), bh + s(2.0), s(7.0), sk.hairline));
                clip(quads, UiQuad::solid(bx, by, bw, bh, s(6.0), sk.panel));
                if view.hover == hit {
                    clip(quads, UiQuad::solid(bx, by, bw, bh, s(6.0), sk.hover));
                }
            }
            {
                let (down_x, down_y, _, down_h) = geometry.opacity_down;
                let (up_x, _, _, _) = geometry.opacity_up;
                let track_x = down_x + s(56.0);
                let track_w = (up_x - track_x - s(12.0)).max(s(90.0));
                let track_y = down_y + down_h / 2.0 - s(2.0);
                let frac = ((view.opacity - 0.35) / 0.65).clamp(0.0, 1.0);
                clip(quads, UiQuad::solid(track_x, track_y, track_w, s(4.0), s(2.0), sk.track_off));
                clip(quads, UiQuad::solid(track_x, track_y, track_w * frac, s(4.0), s(2.0), Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255)));
                clip(quads, UiQuad::solid(
                    track_x + track_w * frac - s(6.0),
                    track_y - s(4.0),
                    s(12.0),
                    s(12.0),
                    s(6.0),
                    sk.knob_off,
                ));
            }
        },
        NebulaSettingsSection::Profiles => {
            // 终端: 3-row frame; 补全: 2-row frame; 配置文件: 1-row frame.
            group_frame(quads, geometry.shell, 3);
            group_frame(quads, geometry.ghost, 2);
            group_frame(quads, geometry.open_config_file, 1);
            for (hit, rect) in [
                (SettingsHit::ShellCycle, geometry.shell),
                (SettingsHit::FetchToggle, geometry.fetch),
                (SettingsHit::PowerlineToggle, geometry.powerline),
                (SettingsHit::GhostToggle, geometry.ghost),
                (SettingsHit::AcceptCycle, geometry.accept),
                (SettingsHit::OpenConfigFile, geometry.open_config_file),
            ] {
                row_hover(quads, rect, view.hover == hit);
            }
            // Boolean rows render a real switch instead of an "On/Off" string.
            for (rect, on) in [
                (geometry.fetch, view.fetch),
                (geometry.powerline, view.powerline),
                (geometry.ghost, view.ghost),
            ] {
                toggle(quads, rect, on);
            }
        },
    }

    // Overlay scrollbar on the content viewport's right edge, only when the
    // section actually overflows (same style as the pane scrollbar: thin
    // rounded thumb, no track).
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
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

/// Draw a chrome title at `mult`× the terminal font size. The renderer scales
/// glyph geometry about the (x, y) top-left anchor, so the title grows down and
/// to the right from that point — callers position by the top-left corner.
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
    r.draw_chrome_text_scaled(size, x, y, mult, ink, text, gc);
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
    let s = |val: f32| val * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let ty = ry + (rh - cell_h) / 2.0;
    r.draw_chrome_text(size, rx + s(16.0), ty, sk.ink, k, gc);
    let value_left = rx + rw * 0.42;
    let max_chars = ((rx + rw - s(16.0) - value_left).max(cell_w) / cell_w).floor().max(1.0) as usize;
    let value = truncate_tab_label(v, max_chars);
    let value_cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
    let vx = rx + rw - s(16.0) - value_cols as f32 * cell_w;
    r.draw_chrome_text(size, vx.max(value_left), ty, value_ink, &value, gc);
}

/// Draw the settings modal's text labels on top of its quads. Caller guards on
/// the panel being open.
pub(super) fn draw_text(
    view: &SettingsView,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let sk = view.theme.skin();

    let geometry = settings_geometry(size, scale, view.scroll);
    let (px, py, _pw, ph) = geometry.popup;
    let (content_x, content_y, _content_w, _) = geometry.content;
    // Text has no scissor, so unlike the quad pass (which cuts quads at the
    // viewport edges) a text block is drawn only when it fits ENTIRELY inside
    // the viewport — a glyph must never cross the header hairline.
    let clip_top = geometry.content_top;
    let clip_bot = py + ph - s(6.0);
    let visible = |ry: f32, rh: f32| ry >= clip_top && ry + rh <= clip_bot;
    // Group titles hang 42px above their first row (title + 16px gap) and
    // scroll with it.
    let group_y = |row_y: f32| row_y - s(42.0);
    let title_h = s(26.0);

    // Brand title in the sidebar header. Drawn large via the scaled-glyph path
    // so it anchors the panel instead of reading as just another row label.
    draw_big_text(r, gc, size, scale, px + s(24.0), py + s(22.0), 1.5, sk.ink_strong, "Nebula 设置");
    {
        // Center the reset label inside its ghost button.
        let (rx, ry, rw, rh) = geometry.reset;
        let label = "恢复默认设置";
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
            nav_section.label(),
            gc,
        );
    }
    // Content header: the big section title alone. (No subtitle — the nav
    // label + title already say everything; the old dim sentence only added
    // noise under the heading.)
    draw_big_text(r, gc, size, scale, content_x + s(24.0), content_y + s(20.0), 1.6, sk.ink_strong, section.label());

    match section {
        NebulaSettingsSection::Appearance => {
            let cards_y = geometry.options[0].2;
            if visible(group_y(cards_y), title_h) {
                section_title(r, gc, size, scale, &sk, content_x + s(24.0), group_y(cards_y), "主题");
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
            let (bg_x, bg_y, _, bg_h) = geometry.background;
            if visible(group_y(bg_y), title_h) {
                section_title(r, gc, size, scale, &sk, bg_x, group_y(bg_y), "自定义背景");
            }
            if visible(bg_y, bg_h) {
                let background_v =
                    view.background.map(format_hex_rgb).unwrap_or_else(|| "主题默认".to_owned());
                row_label(r, gc, size, scale, &sk, geometry.background, "背景色", &background_v, sk.accent);
            }
            let (img_x, img_y, _, img_h) = geometry.background_image;
            let _ = img_x;
            if visible(img_y, img_h) {
                let image_v = view
                    .background_image
                    .as_deref()
                    .map(|path| format!("{path} · {:.0}%", view.background_image_opacity * 100.0))
                    .unwrap_or_else(|| "未设置".to_owned());
                row_label(r, gc, size, scale, &sk, geometry.background_image, "背景图片", &image_v, sk.accent);
            }
            let (or_x, or_y, _, or_h) = geometry.opacity_row;
            if visible(group_y(or_y), title_h) {
                section_title(r, gc, size, scale, &sk, content_x + s(24.0), group_y(or_y), "界面");
            }
            if visible(or_y, or_h) {
                r.draw_chrome_text(size, or_x + s(16.0), or_y + (or_h - cell_h) / 2.0, sk.ink, "窗口透明度", gc);
                // Center the fullwidth −/＋ glyphs inside their stepper buttons.
                for (rect, glyph) in [(geometry.opacity_down, "－"), (geometry.opacity_up, "＋")] {
                    let (bx, by, bw, bh) = rect;
                    r.draw_chrome_text(
                        size,
                        bx + (bw - 2.0 * cell_w) / 2.0,
                        by + (bh - cell_h) / 2.0,
                        sk.ink,
                        glyph,
                        gc,
                    );
                }
                let opacity_v = format!("{:.0}%", view.opacity * 100.0);
                let opacity_cols: usize = opacity_v.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    geometry.opacity_down.0 - s(14.0) - opacity_cols as f32 * cell_w,
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
                section_title(r, gc, size, scale, &sk, sh_x, group_y(sh_y), "终端");
            }
            if visible(sh_y, sh_h) {
                row_label(r, gc, size, scale, &sk, geometry.shell, "默认 Shell", view.shell.label(), sk.accent);
            }
            // Boolean rows: the switch (drawn in `push_quads`) carries the
            // state; no "On/Off" string next to it.
            if visible(geometry.fetch.1, geometry.fetch.3) {
                row_label(r, gc, size, scale, &sk, geometry.fetch, "启动欢迎信息", "", sk.ink);
            }
            if visible(geometry.powerline.1, geometry.powerline.3) {
                row_label(r, gc, size, scale, &sk, geometry.powerline, "Powerline 提示符", "", sk.ink);
            }

            let (gh_x, gh_y, _, gh_h) = geometry.ghost;
            if visible(group_y(gh_y), title_h) {
                section_title(r, gc, size, scale, &sk, gh_x, group_y(gh_y), "补全");
            }
            if visible(gh_y, gh_h) {
                row_label(r, gc, size, scale, &sk, geometry.ghost, "历史补全灰字", "", sk.ink);
            }
            if visible(geometry.accept.1, geometry.accept.3) {
                row_label(r, gc, size, scale, &sk, geometry.accept, "补全接受键", view.accept.label(), sk.accent);
            }

            let (ocx, ocy, _ocw, och) = geometry.open_config_file;
            if visible(group_y(ocy), title_h) {
                section_title(r, gc, size, scale, &sk, ocx, group_y(ocy), "配置文件");
            }
            if visible(ocy, och) {
                r.draw_chrome_text(size, ocx + s(16.0), ocy + (och - cell_h) / 2.0, sk.accent, "打开配置文件", gc);
            }
        },
    }
}
