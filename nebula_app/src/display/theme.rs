//! Nebula theme system — the single source of truth for every chrome color.
//!
//! Everything visual that is NOT terminal-grid content reads from here:
//! the seven built-in themes (from the low-saturation powerline design
//! sheet: the deep-blue default plus three light/dark pairs), each theme's
//! chrome palette ([`NebulaPalette`]) and its full overlay ink set
//! ([`Skin`]). The settings modal, confirm dialogs, the command palette,
//! resize HUD, scrollbar and the tab/window chrome all pull their colors
//! from these two structs — no component keeps a private color constant, so
//! a theme switch (including light ↔ dark) restyles every surface at once.
//!
//! Design language (from the sheet): low-saturation surfaces, hierarchy by
//! brightness not borders, ONE accent per theme, semantic red reserved for
//! destructive actions. Light themes flip the whole ink set to dark-on-light
//! rather than dimming the dark inks.
//!
//! Adding a theme = one enum variant + one `palette()` arm + one `accent()`
//! arm (+ a card slot in the settings grid and a palette action). The
//! [`Skin`] derives from those automatically via `is_light`.

use crate::display::color::{List, Rgb};
use crate::renderer::ui::Rgba;
use nebula_terminal::vte::ansi::NamedColor;

/// First 256-color palette slot claimed for the powerline prompt chips
/// (16..=23: icon bg/fg, path bg/fg, branch bg/fg, time bg/fg). Chosen at the
/// very start of the 6×6×6 cube — the darkest corner, rarely load-bearing for
/// TUIs — so hijacking eight slots stays invisible in practice.
pub(crate) const POWERLINE_SLOT0: usize = 16;

/// Built-in Nebula chrome themes exposed from the settings panel — the seven
/// looks from the design sheet: the deep-blue default plus three light/dark
/// low-saturation pairs (silver/steel, limestone/coal, linen/moss).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NebulaTheme {
    Nebula,
    SilverLight,
    SteelDark,
    LimestoneLight,
    CoalDark,
    LinenLight,
    MossDark,
}

impl Default for NebulaTheme {
    fn default() -> Self {
        Self::Nebula
    }
}

impl NebulaTheme {
    /// Resolve the light/dark member of the user's chosen theme family.
    ///
    /// Nebula is an intentionally standalone dark theme. When automatic mode
    /// needs a light counterpart, Silver is the closest neutral match; the
    /// original Nebula preference is kept separately so switching back to a
    /// dark system appearance restores it instead of silently changing the
    /// user's choice to Steel.
    pub(crate) fn for_system_appearance(self, is_light: bool) -> Self {
        match (self, is_light) {
            (Self::Nebula, true) => Self::SilverLight,
            (Self::Nebula, false) => Self::Nebula,
            (Self::SilverLight | Self::SteelDark, true) => Self::SilverLight,
            (Self::SilverLight | Self::SteelDark, false) => Self::SteelDark,
            (Self::LimestoneLight | Self::CoalDark, true) => Self::LimestoneLight,
            (Self::LimestoneLight | Self::CoalDark, false) => Self::CoalDark,
            (Self::LinenLight | Self::MossDark, true) => Self::LinenLight,
            (Self::LinenLight | Self::MossDark, false) => Self::MossDark,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Nebula => "Nebula",
            Self::SilverLight => "Silver Light",
            Self::SteelDark => "Steel Dark",
            Self::LimestoneLight => "Limestone",
            Self::CoalDark => "Coal Dark",
            Self::LinenLight => "Linen Light",
            Self::MossDark => "Moss Dark",
        }
    }

    pub(crate) fn prompt_name(self) -> &'static str {
        match self {
            Self::Nebula => "Nebula",
            Self::SilverLight => "SilverLight",
            Self::SteelDark => "SteelDark",
            Self::LimestoneLight => "LimestoneLight",
            Self::CoalDark => "CoalDark",
            Self::LinenLight => "LinenLight",
            Self::MossDark => "MossDark",
        }
    }

    /// Inverse of [`prompt_name`](Self::prompt_name); used to restore the
    /// persisted theme from `nebula_settings.txt`.
    pub(crate) fn from_prompt_name(name: &str) -> Option<Self> {
        Some(match name {
            "Nebula" => Self::Nebula,
            "SilverLight" => Self::SilverLight,
            "SteelDark" => Self::SteelDark,
            "LimestoneLight" => Self::LimestoneLight,
            "CoalDark" => Self::CoalDark,
            "LinenLight" => Self::LinenLight,
            "MossDark" => Self::MossDark,
            _ => return None,
        })
    }

    /// Shorter label for theme cards so long names fit within a single card.
    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::SilverLight => "Silver",
            Self::LimestoneLight => "Limestone",
            Self::LinenLight => "Linen",
            Self::SteelDark => "Steel",
            Self::CoalDark => "Coal",
            Self::MossDark => "Moss",
            _ => self.label(),
        }
    }

    /// The theme's single accent color — selection rings, toggles-on, slider
    /// fill and the prompt caret. Kept in lock-step with the powerline `$accent`
    /// bridge (see `tty::windows`) so chrome, settings panel and shell prompt
    /// all shift together when the theme changes. Each value is chosen to
    /// contrast its own theme surface (light themes get a dark accent, dark
    /// themes a light one).
    pub(crate) fn accent(self) -> Rgb {
        match self {
            Self::Nebula => Rgb::new(82, 168, 255),
            Self::SilverLight => Rgb::new(73, 80, 87),
            Self::SteelDark => Rgb::new(148, 163, 184),
            Self::LimestoneLight => Rgb::new(88, 85, 76),
            Self::CoalDark => Rgb::new(212, 212, 212),
            Self::LinenLight => Rgb::new(95, 99, 95),
            Self::MossDark => Rgb::new(163, 179, 163),
        }
    }

    /// Rebuild the terminal color table for this theme on top of the user's
    /// configured scheme (`defaults`).
    ///
    /// Every theme moves the default background — that is what OSC 11 reports,
    /// and TUIs like Claude Code / lazygit key their light/dark mode off it.
    /// Light themes additionally replace the foreground and the ANSI-16 set
    /// with a low-saturation light scheme: the configured (dark-ground) colors
    /// are pale by design and unreadable on a pale background.
    pub(crate) fn apply_term_colors(self, colors: &mut List, defaults: &List) {
        *colors = *defaults;
        let p = self.palette();
        colors[NamedColor::Background] = p.term_bg;
        // Powerline prompt slots: the injected prompt paints its segment chips
        // with indexed colors 16..=23 instead of baked-in truecolor, so a
        // theme switch remaps the palette and every chip ALREADY PRINTED in
        // scrollback recolors instantly — indexed cells resolve the palette at
        // draw time; truecolor is frozen the moment it is printed.
        for (i, rgb) in self.powerline_colors().into_iter().enumerate() {
            colors[POWERLINE_SLOT0 + i] = rgb;
        }
        if !p.is_light {
            return;
        }

        colors[NamedColor::Foreground] = Rgb::new(36, 41, 47); // #24292f
        // GitHub Primer Light ANSI-16 (from the premium-light design sheet):
        // deep ink hues tuned for a pure-white ground. BrightWhite is a
        // gray on purpose — true white would vanish on the white terminal.
        const LIGHT_ANSI: [(NamedColor, Rgb); 16] = [
            (NamedColor::Black, Rgb::new(36, 41, 47)),        // #24292f
            (NamedColor::Red, Rgb::new(207, 34, 46)),         // #cf222e
            (NamedColor::Green, Rgb::new(26, 127, 55)),       // #1a7f37
            (NamedColor::Yellow, Rgb::new(154, 103, 0)),      // #9a6700
            (NamedColor::Blue, Rgb::new(9, 105, 218)),        // #0969da
            (NamedColor::Magenta, Rgb::new(130, 80, 223)),    // #8250df
            (NamedColor::Cyan, Rgb::new(27, 124, 131)),       // #1b7c83
            (NamedColor::White, Rgb::new(110, 119, 129)),     // #6e7781
            (NamedColor::BrightBlack, Rgb::new(87, 96, 106)), // #57606a
            (NamedColor::BrightRed, Rgb::new(164, 14, 38)),   // #a40e26
            (NamedColor::BrightGreen, Rgb::new(45, 164, 78)), // #2da44e
            (NamedColor::BrightYellow, Rgb::new(191, 135, 0)), // #bf8700
            (NamedColor::BrightBlue, Rgb::new(33, 139, 255)), // #218bff
            (NamedColor::BrightMagenta, Rgb::new(164, 117, 249)), // #a475f9
            (NamedColor::BrightCyan, Rgb::new(49, 146, 170)), // #3192aa
            (NamedColor::BrightWhite, Rgb::new(140, 149, 159)), // #8c959f
        ];
        for (name, rgb) in LIGHT_ANSI {
            colors[name] = rgb;
        }
    }

    /// Segment colors for the injected powerline prompt, published into the
    /// 256-color palette at [`POWERLINE_SLOT0`]`..+8` by [`Self::apply_term_colors`].
    /// Order: icon bg/fg, path bg/fg, branch bg/fg, time bg/fg — one flat color
    /// per chip (the old per-character truecolor gradient could never follow a
    /// theme switch retroactively, which users read as "the prompt is stuck").
    pub(crate) fn powerline_colors(self) -> [Rgb; 8] {
        match self {
            Self::Nebula => [
                Rgb::new(57, 75, 112),
                Rgb::new(192, 202, 245),
                Rgb::new(41, 52, 82),
                Rgb::new(169, 177, 214),
                Rgb::new(47, 79, 79),
                Rgb::new(139, 213, 202),
                Rgb::new(29, 33, 46),
                Rgb::new(100, 116, 139),
            ],
            Self::SilverLight => [
                Rgb::new(229, 231, 235),
                Rgb::new(55, 65, 81),
                Rgb::new(243, 244, 246),
                Rgb::new(55, 65, 81),
                Rgb::new(224, 242, 254),
                Rgb::new(3, 105, 161),
                Rgb::new(249, 250, 251),
                Rgb::new(107, 114, 128),
            ],
            Self::SteelDark => [
                Rgb::new(71, 85, 105),
                Rgb::new(241, 245, 249),
                Rgb::new(51, 65, 85),
                Rgb::new(203, 213, 225),
                Rgb::new(59, 82, 73),
                Rgb::new(163, 184, 153),
                Rgb::new(40, 44, 56),
                Rgb::new(148, 163, 184),
            ],
            Self::LimestoneLight => [
                Rgb::new(214, 211, 209),
                Rgb::new(250, 250, 249),
                Rgb::new(231, 229, 228),
                Rgb::new(68, 64, 60),
                Rgb::new(200, 198, 167),
                Rgb::new(41, 37, 36),
                Rgb::new(235, 233, 230),
                Rgb::new(163, 160, 151),
            ],
            Self::CoalDark => [
                Rgb::new(82, 82, 82),
                Rgb::new(245, 245, 245),
                Rgb::new(64, 64, 64),
                Rgb::new(212, 212, 212),
                Rgb::new(74, 79, 65),
                Rgb::new(181, 181, 166),
                Rgb::new(48, 48, 48),
                Rgb::new(115, 115, 115),
            ],
            Self::LinenLight => [
                Rgb::new(212, 212, 208),
                Rgb::new(255, 255, 255),
                Rgb::new(229, 229, 223),
                Rgb::new(63, 63, 63),
                Rgb::new(181, 196, 177),
                Rgb::new(45, 45, 45),
                Rgb::new(236, 236, 230),
                Rgb::new(176, 179, 176),
            ],
            Self::MossDark => [
                Rgb::new(75, 85, 72),
                Rgb::new(240, 253, 244),
                Rgb::new(59, 66, 56),
                Rgb::new(220, 252, 231),
                Rgb::new(60, 79, 60),
                Rgb::new(187, 247, 208),
                Rgb::new(42, 47, 42),
                Rgb::new(107, 114, 107),
            ],
        }
    }

    pub(crate) fn palette(self) -> NebulaPalette {
        match self {
            Self::Nebula => NebulaPalette {
                panel: Rgba::new(34, 38, 48, 224),
                pill: Rgba::new(43, 48, 59, 218),
                tab_stroke_l: Rgba::new(150, 157, 188, 132),
                tab_bg_l: Rgba::new(65, 72, 88, 230),
                tab_bg_r: Rgba::new(48, 54, 67, 226),
                edge_l: Rgba::new(169, 152, 188, 180),
                edge_r: Rgba::new(125, 178, 194, 180),
                edge_glow_l: Rgba::new(169, 152, 188, 24),
                glow_l: Rgba::new(169, 152, 188, 14),
                glow_r: Rgba::new(125, 178, 194, 14),
                is_light: false,
                term_bg: Rgb::new(15, 17, 26),
                shell_bg: Rgb::new(34, 38, 48),
            },
            // Cool silver — the light half of the steel pair. Chrome layers
            // follow the premium-light sheet: sidebar #f3f4f6 over app-bg
            // #f9fafb, terminal pure white for maximum contrast.
            Self::SilverLight => NebulaPalette {
                // Neutral silver, blue removed: the panel/tab surfaces sit on a
                // true-neutral gray ramp (was Tailwind's blue-leaning gray-100),
                // and the active-tab halo is a soft neutral shadow instead of a
                // blue wash — a pure-white pill lifting off a flat gray gutter.
                panel: Rgba::new(245, 245, 246, 236),
                pill: Rgba::new(233, 233, 234, 230),
                tab_stroke_l: Rgba::new(198, 198, 200, 150),
                tab_bg_l: Rgba::new(255, 255, 255, 242),
                tab_bg_r: Rgba::new(250, 250, 251, 236),
                edge_l: Rgba::new(110, 112, 116, 170),
                edge_r: Rgba::new(118, 121, 126, 180),
                edge_glow_l: Rgba::new(118, 121, 126, 18),
                // Ambient glows are OFF on light themes: a ~4% alpha radial
                // gradient over a pale backdrop lands on very few 8-bit steps,
                // and the quantization contours read as blurry gray "lines"
                // (invisible on the dark themes' deep backgrounds).
                glow_l: Rgba::new(82, 168, 255, 0),
                glow_r: Rgba::new(73, 80, 87, 0),
                is_light: true,
                // Pure white terminal on every light theme (premium-light
                // sheet): highest contrast for the Primer ANSI ink set.
                term_bg: Rgb::new(255, 255, 255),
                // Premium-light app-bg layer (#f3f4f6-ish): the white terminal
                // card floats on this neutral silver.
                shell_bg: Rgb::new(243, 244, 246),
            },
            // Warm limestone — the light half of the coal pair.
            Self::LimestoneLight => NebulaPalette {
                panel: Rgba::new(240, 239, 235, 236),
                pill: Rgba::new(231, 229, 224, 230),
                tab_stroke_l: Rgba::new(163, 160, 151, 150),
                tab_bg_l: Rgba::new(255, 255, 255, 242),
                tab_bg_r: Rgba::new(247, 246, 242, 236),
                edge_l: Rgba::new(88, 85, 76, 160),
                edge_r: Rgba::new(206, 178, 126, 190),
                edge_glow_l: Rgba::new(206, 178, 126, 20),
                // Ambient glow off on light themes (8-bit banding, see Silver).
                glow_l: Rgba::new(206, 178, 126, 0),
                glow_r: Rgba::new(88, 85, 76, 0),
                is_light: true,
                term_bg: Rgb::new(255, 255, 255),
                shell_bg: Rgb::new(240, 239, 235),
            },
            // Soft linen — the light half of the moss pair.
            Self::LinenLight => NebulaPalette {
                panel: Rgba::new(242, 242, 236, 236),
                pill: Rgba::new(233, 233, 227, 230),
                tab_stroke_l: Rgba::new(176, 179, 176, 150),
                tab_bg_l: Rgba::new(255, 255, 255, 242),
                tab_bg_r: Rgba::new(251, 251, 246, 236),
                edge_l: Rgba::new(95, 99, 95, 160),
                edge_r: Rgba::new(149, 175, 149, 190),
                edge_glow_l: Rgba::new(149, 175, 149, 20),
                // Ambient glow off on light themes (8-bit banding, see Silver).
                glow_l: Rgba::new(149, 175, 149, 0),
                glow_r: Rgba::new(95, 99, 95, 0),
                is_light: true,
                term_bg: Rgb::new(255, 255, 255),
                shell_bg: Rgb::new(242, 242, 236),
            },
            // The three dark themes from the floating-pill design sheet
            // (steel blue-gray / coal warm-gold / moss green), low-saturation
            // accents per the powerline sheet.
            Self::SteelDark => NebulaPalette {
                panel: Rgba::new(22, 24, 30, 224),
                pill: Rgba::new(30, 33, 41, 218),
                tab_stroke_l: Rgba::new(148, 163, 184, 124),
                tab_bg_l: Rgba::new(52, 58, 72, 230),
                tab_bg_r: Rgba::new(38, 43, 54, 226),
                edge_l: Rgba::new(148, 163, 184, 170),
                edge_r: Rgba::new(82, 168, 255, 168),
                edge_glow_l: Rgba::new(148, 163, 184, 20),
                glow_l: Rgba::new(148, 163, 184, 12),
                glow_r: Rgba::new(82, 168, 255, 12),
                is_light: false,
                term_bg: Rgb::new(26, 28, 36),
                shell_bg: Rgb::new(22, 24, 30),
            },
            Self::CoalDark => NebulaPalette {
                panel: Rgba::new(22, 22, 22, 224),
                pill: Rgba::new(30, 30, 30, 218),
                tab_stroke_l: Rgba::new(186, 186, 182, 120),
                tab_bg_l: Rgba::new(56, 56, 54, 230),
                tab_bg_r: Rgba::new(41, 41, 40, 226),
                edge_l: Rgba::new(206, 178, 126, 172),
                edge_r: Rgba::new(212, 212, 212, 148),
                edge_glow_l: Rgba::new(206, 178, 126, 22),
                glow_l: Rgba::new(206, 178, 126, 12),
                glow_r: Rgba::new(212, 212, 212, 12),
                is_light: false,
                term_bg: Rgb::new(23, 23, 23),
                shell_bg: Rgb::new(22, 22, 22),
            },
            Self::MossDark => NebulaPalette {
                panel: Rgba::new(25, 28, 25, 224),
                pill: Rgba::new(33, 37, 33, 218),
                tab_stroke_l: Rgba::new(163, 179, 163, 124),
                tab_bg_l: Rgba::new(54, 61, 54, 230),
                tab_bg_r: Rgba::new(40, 46, 40, 226),
                edge_l: Rgba::new(149, 175, 149, 172),
                edge_r: Rgba::new(163, 179, 163, 158),
                edge_glow_l: Rgba::new(149, 175, 149, 22),
                glow_l: Rgba::new(149, 175, 149, 12),
                glow_r: Rgba::new(163, 179, 163, 12),
                is_light: false,
                term_bg: Rgb::new(30, 33, 30),
                shell_bg: Rgb::new(25, 28, 25),
            },
        }
    }

    /// Theme-derived ink/surface tokens for every floating chrome layer.
    /// See [`Skin`] for what each token means.
    pub(crate) fn skin(self) -> Skin {
        let p = self.palette();
        let a = self.accent();
        let b = p.panel;
        let t = p.term_bg;
        if p.is_light {
            Skin {
                panel: Rgba::new(b.r, b.g, b.b, 252),
                // A white inset on light panels: the hairline carries the
                // "sunken" read, the fill stays cleaner than a gray wash.
                input: Rgba::new(255, 255, 255, 240),
                // Light panels stay flat — the gradient-to-dark trick is a
                // dark-theme depth cue and would read as dirt here.
                panel_grad_to: Rgba::new(b.r, b.g, b.b, 252),
                ink: Rgb::new(55, 65, 81),        // #374151 (ui-text-main)
                ink_dim: Rgb::new(107, 114, 128), // #6b7280 (ui-text-muted)
                ink_strong: Rgb::new(18, 22, 30),
                ink_faint: Rgb::new(148, 153, 163),
                // Light accents are dark grays — pale ink on top.
                ink_on_accent: Rgb::new(248, 250, 252),
                icon: Rgb::new(73, 80, 87),
                icon_hover: Rgb::new(18, 22, 26),
                accent: a,
                accent_soft: Rgba::new(a.r, a.g, a.b, 34),
                danger: Rgba::new(196, 74, 88, 255),
                hairline: Rgba::new(0, 0, 0, 20), // rgba(0,0,0,.08) — hairline borders
                surface: Rgba::new(0, 0, 0, 12),
                hover: Rgba::new(0, 0, 0, 20),
                hover_strong: Rgba::new(0, 0, 0, 32),
                track_off: Rgba::new(0, 0, 0, 48),
                knob_off: Rgba::new(255, 255, 255, 255),
                knob_on: Rgba::new(250, 250, 250, 255),
                scrollbar_thumb: Rgba::new(60, 66, 80, 0),
                is_light: true,
            }
        } else {
            Skin {
                panel: Rgba::new(b.r, b.g, b.b, 250),
                // Derive the inset/input surface from the terminal background
                // so it stays in-family on every dark theme (blue-black on
                // Nebula, pure gray on Coal) instead of one fixed navy.
                input: Rgba::new(t.r, t.g, t.b, 220),
                panel_grad_to: Rgba::new(t.r, t.g, t.b, 235),
                ink: Rgb::new(228, 231, 246),
                ink_dim: Rgb::new(158, 164, 188),
                ink_strong: Rgb::new(255, 255, 255),
                ink_faint: Rgb::new(118, 124, 148),
                // Dark accents are light — near-black ink on top.
                ink_on_accent: Rgb::new(12, 16, 22),
                icon: Rgb::new(205, 210, 230),
                icon_hover: Rgb::new(244, 246, 255),
                accent: a,
                accent_soft: Rgba::new(a.r, a.g, a.b, 46),
                danger: Rgba::new(196, 74, 88, 255),
                hairline: Rgba::new(255, 255, 255, 22),
                surface: Rgba::new(255, 255, 255, 11),
                hover: Rgba::new(255, 255, 255, 26),
                hover_strong: Rgba::new(255, 255, 255, 40),
                track_off: Rgba::new(255, 255, 255, 34),
                knob_off: Rgba::new(210, 214, 228, 255),
                knob_on: Rgba::new(12, 16, 22, 255),
                scrollbar_thumb: Rgba::new(180, 184, 200, 0),
                is_light: false,
            }
        }
    }
}

/// Per-theme chrome palette: the translucent panels, tab pills, edge accents
/// and glows painted by `draw_chrome`, plus the terminal background the theme
/// applies on selection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NebulaPalette {
    pub(crate) panel: Rgba,
    /// Standalone fill for inactive tab rows / the "+" pill. Currently unpainted
    /// (inactive rows sit flush on the sidebar; state is the white active pill),
    /// kept for reintroducing a per-row background without re-plumbing the palette.
    #[allow(dead_code)]
    pub(crate) pill: Rgba,
    pub(crate) tab_stroke_l: Rgba,
    pub(crate) tab_bg_l: Rgba,
    pub(crate) tab_bg_r: Rgba,
    pub(crate) edge_l: Rgba,
    pub(crate) edge_r: Rgba,
    pub(crate) edge_glow_l: Rgba,
    pub(crate) glow_l: Rgba,
    pub(crate) glow_r: Rgba,
    /// Light chrome theme: flips the chrome ink set (labels/icons) to dark
    /// text so it stays readable on the pale surfaces.
    pub(crate) is_light: bool,
    /// The theme's default terminal background, applied on selection.
    pub(crate) term_bg: Rgb,
    /// Opaque window shell base color. The whole window clears to this; the
    /// terminal renders as a rounded [`term_bg`] card floating on top, and the
    /// chrome (top bar / sidebar) melts into it. Slightly offset from `panel`'s
    /// RGB so the translucent panels still read on top of it.
    pub(crate) shell_bg: Rgb,
}

/// Theme-derived skin for every floating chrome layer: the settings modal,
/// confirm dialogs, the command palette, the resize HUD, scrollbar and the
/// chrome ink set. One struct so light themes flip EVERY overlay at once —
/// components must not keep private color constants.
///
/// Naming: `ink*` are text colors (strong > ink > dim > faint), `panel` /
/// `input` / `surface` are fills from back to front, `hover*` are transient
/// washes stacked on top, and `accent` / `danger` are the only saturated
/// voices (selection/primary vs destructive).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Skin {
    /// Near-opaque panel surface (kills the see-through bleed where the
    /// shell's own powerline used to collide with overlay labels).
    pub(crate) panel: Rgba,
    /// Inset/input surface (command palette query box and friends).
    pub(crate) input: Rgba,
    /// Far end of the tall-panel gradient (command palette). Same as `panel`
    /// on light themes — the depth cue is dark-theme only.
    pub(crate) panel_grad_to: Rgba,
    /// Primary label ink.
    pub(crate) ink: Rgb,
    /// Secondary / sub-label ink.
    pub(crate) ink_dim: Rgb,
    /// Titles, active nav row, selected list rows.
    pub(crate) ink_strong: Rgb,
    /// Placeholder / hint text (weakest voice).
    pub(crate) ink_faint: Rgb,
    /// Ink on top of an `accent`-filled control (primary buttons).
    pub(crate) ink_on_accent: Rgb,
    /// Chrome glyph icons (sidebar toggle, settings gear, tab ×, …).
    pub(crate) icon: Rgb,
    pub(crate) icon_hover: Rgb,
    /// The theme's single accent (selection ring, toggle-on, slider fill).
    pub(crate) accent: Rgb,
    /// Soft accent wash for the active nav pill and selected card fill.
    pub(crate) accent_soft: Rgba,
    /// Destructive primary actions (close-busy-pane confirm). Same on both
    /// light and dark — semantic red doesn't flip.
    pub(crate) danger: Rgba,
    /// Edges, separators and quiet control borders.
    pub(crate) hairline: Rgba,
    /// Faint lift for interactive rows and cards.
    pub(crate) surface: Rgba,
    /// Hover wash on rows and cards.
    pub(crate) hover: Rgba,
    /// Stronger hover wash for small icon targets.
    pub(crate) hover_strong: Rgba,
    /// Toggle-off track / slider rail.
    pub(crate) track_off: Rgba,
    /// Toggle knob when off / on (chosen to contrast the track).
    pub(crate) knob_off: Rgba,
    pub(crate) knob_on: Rgba,
    /// Scrollbar thumb base color; alpha applied at the call site (drag
    /// feedback brightens it).
    pub(crate) scrollbar_thumb: Rgba,
    /// True on the pale themes — lets renderers pick a brighter bevel and the
    /// right slider-thumb ink without re-deriving it from the palette.
    pub(crate) is_light: bool,
}

/// Publish the active theme for the shell prompt bridge: the powerline script
/// polls `%TEMP%\nebula_theme.txt` and recolors its segments to match. Written
/// atomically (tmp + rename) so readers never see a torn value.
pub(crate) fn write_nebula_prompt_theme(theme: NebulaTheme) {
    let dir = std::env::temp_dir();
    let path = dir.join("nebula_theme.txt");
    let tmp = dir.join(format!("nebula_theme.{}.tmp", std::process::id()));

    if std::fs::write(&tmp, theme.prompt_name()).is_ok() {
        // Windows cannot always rename over an existing file with `std::fs::rename`.
        // The prompt script treats a missing/invalid theme as Nebula, so even the
        // fallback path stays safe; the temporary file prevents readers from seeing
        // partially-written contents.
        let _ = std::fs::rename(&tmp, &path).or_else(|_| {
            let _ = std::fs::remove_file(&path);
            std::fs::rename(&tmp, &path)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::NebulaTheme;

    #[test]
    fn system_appearance_keeps_the_selected_theme_family() {
        assert_eq!(NebulaTheme::Nebula.for_system_appearance(true), NebulaTheme::SilverLight);
        assert_eq!(NebulaTheme::Nebula.for_system_appearance(false), NebulaTheme::Nebula);
        assert_eq!(NebulaTheme::SilverLight.for_system_appearance(false), NebulaTheme::SteelDark);
        assert_eq!(NebulaTheme::SteelDark.for_system_appearance(true), NebulaTheme::SilverLight);
        assert_eq!(NebulaTheme::LimestoneLight.for_system_appearance(false), NebulaTheme::CoalDark);
        assert_eq!(NebulaTheme::CoalDark.for_system_appearance(true), NebulaTheme::LimestoneLight);
        assert_eq!(NebulaTheme::LinenLight.for_system_appearance(false), NebulaTheme::MossDark);
        assert_eq!(NebulaTheme::MossDark.for_system_appearance(true), NebulaTheme::LinenLight);
    }
}
