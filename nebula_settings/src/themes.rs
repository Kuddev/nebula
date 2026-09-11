//! Static built-in theme catalog, shared by terminal and chrome adapters.
use crate::{ExactTermColors, Rgb8, TermTheme, ThemeName};

#[derive(Clone, Copy, Debug)]
pub struct FreshPalette {
    pub shell: Rgb8,
    pub surface: Rgb8,
    pub accent: Rgb8,
    pub foreground: Rgb8,
    pub muted: Rgb8,
    pub is_light: bool,
}

impl ThemeName {
    /// The only catalog of selectable built-ins. Retired identifiers remain readable.
    pub const BUILTIN: [Self; 13] = [
        Self::BreezeLight,
        Self::BreezeDark,
        Self::MintLight,
        Self::MintDark,
        Self::SilverLight,
        Self::Nord,
        Self::Paper,
        Self::LimestoneLight,
        Self::LinenLight,
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::GlassLight,
        Self::GlassDark,
    ];

    pub const BUILTIN_NAMES: [&'static str; 13] = {
        let mut names = [""; 13];
        let mut index = 0;
        while index < Self::BUILTIN.len() {
            names[index] = Self::BUILTIN[index].prompt_name();
            index += 1;
        }
        names
    };

    /// Retired names keep existing settings readable without reappearing in the picker.
    pub const fn available(self) -> Self {
        match self {
            Self::Nebula => Self::CatppuccinMocha,
            Self::SteelDark | Self::CoalDark => Self::Nord,
            Self::MossDark => Self::MintDark,
            other => other,
        }
    }

    pub fn fresh_palette(self) -> Option<FreshPalette> {
        let palette = self.reviewed_palette();
        Some(FreshPalette {
            shell: palette.shell,
            surface: palette.background,
            accent: palette.accent,
            foreground: palette.foreground,
            muted: palette.muted,
            is_light: matches!(
                self.available(),
                Self::BreezeLight
                    | Self::MintLight
                    | Self::SilverLight
                    | Self::Paper
                    | Self::LimestoneLight
                    | Self::LinenLight
                    | Self::CatppuccinLatte
                    | Self::GlassLight
            ),
        })
    }
}

const fn rgb(value: u32) -> Rgb8 {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

/// Exact semantic colors of the reviewed terminal HTML. These values are static;
/// color adapters must not desaturate accents or synthesize a second selected ramp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewedPalette {
    pub shell: Rgb8,
    pub background: Rgb8,
    pub foreground: Rgb8,
    pub muted: Rgb8,
    pub accent: Rgb8,
    pub selected: [u8; 4],
    pub line: [u8; 4],
    pub red: Rgb8,
    pub green: Rgb8,
    pub yellow: Rgb8,
    pub blue: Rgb8,
    pub purple: Rgb8,
    pub cyan: Rgb8,
    pub frame: Rgb8,
}

impl ReviewedPalette {
    /// CSS-style alpha composition of the reviewed secondary surface on the pane.
    pub const fn code_background(self) -> Rgb8 {
        let alpha = self.selected[3] as u32;
        let mut color = [0; 3];
        let mut index = 0;
        while index < 3 {
            color[index] = ((self.selected[index] as u32 * alpha
                + self.background[index] as u32 * (255 - alpha)
                + 127)
                / 255) as u8;
            index += 1;
        }
        color
    }
}

impl ThemeName {
    pub const fn reviewed_palette(self) -> ReviewedPalette {
        match self.available() {
            Self::BreezeLight => ReviewedPalette {
                shell: [0xea, 0xf0, 0xf6],
                background: [0xf9, 0xfb, 0xff],
                foreground: [0x26, 0x38, 0x4a],
                muted: [0x57, 0x6c, 0x80],
                accent: [0x30, 0x65, 0x92],
                selected: [0xdf, 0xe9, 0xf2, 255],
                line: [0xd4, 0xdf, 0xe9, 255],
                red: [0xa3, 0x42, 0x48],
                green: [0x28, 0x74, 0x5e],
                yellow: [0x88, 0x62, 0x1f],
                blue: [0x30, 0x65, 0x92],
                purple: [0x77, 0x56, 0x9b],
                cyan: [0x24, 0x77, 0x82],
                frame: [0x95, 0xa7, 0xb8],
            },
            Self::BreezeDark => ReviewedPalette {
                shell: [0x18, 0x23, 0x2e],
                background: [0x20, 0x2e, 0x3b],
                foreground: [0xe0, 0xea, 0xf3],
                muted: [0xa5, 0xb6, 0xc7],
                accent: [0x91, 0xbc, 0xdf],
                selected: [0x2b, 0x3c, 0x4b, 255],
                line: [0x34, 0x46, 0x54, 255],
                red: [0xe4, 0x9a, 0x9d],
                green: [0x9b, 0xc5, 0xa6],
                yellow: [0xe0, 0xc3, 0x8b],
                blue: [0x91, 0xbc, 0xdf],
                purple: [0xc5, 0xad, 0xdd],
                cyan: [0x91, 0xce, 0xcf],
                frame: [0x50, 0x65, 0x79],
            },
            Self::MintLight => ReviewedPalette {
                shell: [0xea, 0xf2, 0xee],
                background: [0xf8, 0xfc, 0xf9],
                foreground: [0x26, 0x3c, 0x33],
                muted: [0x58, 0x73, 0x65],
                accent: [0x28, 0x74, 0x5e],
                selected: [0xdc, 0xeb, 0xe2, 255],
                line: [0xd0, 0xe0, 0xd6, 255],
                red: [0xa3, 0x42, 0x48],
                green: [0x28, 0x74, 0x5e],
                yellow: [0x88, 0x62, 0x1f],
                blue: [0x30, 0x65, 0x92],
                purple: [0x77, 0x56, 0x9b],
                cyan: [0x24, 0x77, 0x82],
                frame: [0x94, 0xaa, 0x9d],
            },
            Self::MintDark => ReviewedPalette {
                shell: [0x19, 0x2a, 0x26],
                background: [0x21, 0x37, 0x30],
                foreground: [0xdf, 0xee, 0xe7],
                muted: [0xa6, 0xbf, 0xb2],
                accent: [0x8b, 0xcb, 0xb3],
                selected: [0x2d, 0x44, 0x3b, 255],
                line: [0x3a, 0x50, 0x46, 255],
                red: [0xe4, 0x9a, 0x9d],
                green: [0x9b, 0xc5, 0xa6],
                yellow: [0xe0, 0xc3, 0x8b],
                blue: [0x91, 0xbc, 0xdf],
                purple: [0xc5, 0xad, 0xdd],
                cyan: [0x91, 0xce, 0xcf],
                frame: [0x52, 0x6f, 0x62],
            },
            Self::SilverLight => ReviewedPalette {
                shell: [0xf3, 0xf4, 0xf6],
                background: [0xff, 0xff, 0xff],
                foreground: [0x24, 0x29, 0x2f],
                muted: [0x60, 0x67, 0x71],
                accent: [0x49, 0x50, 0x57],
                selected: [0xe6, 0xe8, 0xec, 255],
                line: [0xdf, 0xe2, 0xe6, 255],
                red: [0xcf, 0x22, 0x2e],
                green: [0x1a, 0x7f, 0x37],
                yellow: [0x9a, 0x67, 0x00],
                blue: [0x09, 0x69, 0xda],
                purple: [0x82, 0x50, 0xdf],
                cyan: [0x1b, 0x7c, 0x83],
                frame: [0xa7, 0xac, 0xb4],
            },
            Self::Nord => ReviewedPalette {
                shell: [0x2e, 0x34, 0x40],
                background: [0x2e, 0x34, 0x40],
                foreground: [0xe5, 0xe9, 0xf0],
                muted: [0xab, 0xb5, 0xc7],
                accent: [0x88, 0xc0, 0xd0],
                selected: [0x3b, 0x42, 0x52, 255],
                line: [0x43, 0x4c, 0x5e, 255],
                red: [0xbf, 0x61, 0x6a],
                green: [0xa3, 0xbe, 0x8c],
                yellow: [0xeb, 0xcb, 0x8b],
                blue: [0x81, 0xa1, 0xc1],
                purple: [0xb4, 0x8e, 0xad],
                cyan: [0x88, 0xc0, 0xd0],
                frame: [0x65, 0x72, 0x86],
            },
            Self::Paper => ReviewedPalette {
                shell: [0xf5, 0xf4, 0xf0],
                background: [0xfc, 0xfb, 0xf9],
                foreground: [0x1a, 0x1a, 0x1a],
                muted: [0x73, 0x73, 0x69],
                accent: [0x2b, 0x5a, 0x38],
                selected: [0xe9, 0xe8, 0xe1, 255],
                line: [0xde, 0xdd, 0xd4, 255],
                red: [0xa3, 0x3a, 0x3a],
                green: [0x2b, 0x5a, 0x38],
                yellow: [0xa8, 0x5a, 0x20],
                blue: [0x4a, 0x7a, 0x8a],
                purple: [0x4a, 0x3a, 0x6a],
                cyan: [0x3a, 0x7a, 0x6a],
                frame: [0xb2, 0xb0, 0xa3],
            },
            Self::LimestoneLight => ReviewedPalette {
                shell: [0xf0, 0xef, 0xeb],
                background: [0xff, 0xff, 0xff],
                foreground: [0x24, 0x29, 0x2f],
                muted: [0x70, 0x6c, 0x63],
                accent: [0x58, 0x55, 0x4c],
                selected: [0xe6, 0xe3, 0xdc, 255],
                line: [0xde, 0xdb, 0xd2, 255],
                red: [0xcf, 0x22, 0x2e],
                green: [0x1a, 0x7f, 0x37],
                yellow: [0x9a, 0x67, 0x00],
                blue: [0x09, 0x69, 0xda],
                purple: [0x82, 0x50, 0xdf],
                cyan: [0x1b, 0x7c, 0x83],
                frame: [0xaa, 0xa4, 0x96],
            },
            Self::LinenLight => ReviewedPalette {
                shell: [0xf2, 0xf2, 0xec],
                background: [0xff, 0xff, 0xff],
                foreground: [0x24, 0x29, 0x2f],
                muted: [0x6b, 0x70, 0x66],
                accent: [0x5f, 0x63, 0x5f],
                selected: [0xe5, 0xe7, 0xde, 255],
                line: [0xdc, 0xdf, 0xd3, 255],
                red: [0xcf, 0x22, 0x2e],
                green: [0x1a, 0x7f, 0x37],
                yellow: [0x9a, 0x67, 0x00],
                blue: [0x09, 0x69, 0xda],
                purple: [0x82, 0x50, 0xdf],
                cyan: [0x1b, 0x7c, 0x83],
                frame: [0xa4, 0xaa, 0x9b],
            },
            Self::CatppuccinMocha => ReviewedPalette {
                shell: [0x18, 0x18, 0x25],
                background: [0x1e, 0x1e, 0x2e],
                foreground: [0xcd, 0xd6, 0xf4],
                muted: [0xa6, 0xad, 0xc8],
                accent: [0xb4, 0xbe, 0xfe],
                selected: [0x31, 0x32, 0x44, 255],
                line: [0x45, 0x47, 0x5a, 255],
                red: [0xf3, 0x8b, 0xa8],
                green: [0xa6, 0xe3, 0xa1],
                yellow: [0xf9, 0xe2, 0xaf],
                blue: [0x89, 0xb4, 0xfa],
                purple: [0xcb, 0xa6, 0xf7],
                cyan: [0x94, 0xe2, 0xd5],
                frame: [0x6c, 0x70, 0x86],
            },
            Self::CatppuccinLatte => ReviewedPalette {
                shell: [0xe6, 0xe9, 0xef],
                background: [0xef, 0xf1, 0xf5],
                foreground: [0x4c, 0x4f, 0x69],
                muted: [0x5c, 0x5f, 0x77],
                accent: [0x72, 0x87, 0xfd],
                selected: [0xcc, 0xd0, 0xda, 255],
                line: [0xbc, 0xc0, 0xcc, 255],
                red: [0xd2, 0x0f, 0x39],
                green: [0x40, 0xa0, 0x2b],
                yellow: [0xdf, 0x8e, 0x1d],
                blue: [0x1e, 0x66, 0xf5],
                purple: [0x88, 0x39, 0xef],
                cyan: [0x17, 0x92, 0x99],
                frame: [0x9c, 0xa0, 0xb0],
            },
            Self::GlassLight => ReviewedPalette {
                shell: [0xe9, 0xec, 0xef],
                background: [0xef, 0xf1, 0xf5],
                foreground: [0x30, 0x30, 0x30],
                muted: [0x56, 0x61, 0x6b],
                accent: [0x74, 0x5a, 0xa5],
                selected: [0xff, 0xff, 0xff, 0x55],
                line: [0x56, 0x6b, 0x7c, 0x30],
                red: [0xa3, 0x17, 0x00],
                green: [0x0a, 0x7f, 0x3d],
                yellow: [0xaf, 0x55, 0x1d],
                blue: [0x00, 0x6c, 0xd8],
                purple: [0x58, 0x3c, 0xac],
                cyan: [0x00, 0x79, 0x8a],
                frame: [0xa4, 0xb4, 0xbd],
            },
            Self::GlassDark => ReviewedPalette {
                shell: [0x44, 0x44, 0x45],
                background: [0x40, 0x43, 0x4b],
                foreground: [0xf7, 0xf8, 0xff],
                muted: [0xc4, 0xca, 0xd6],
                accent: [0xbb, 0xc9, 0xed],
                selected: [0xff, 0xff, 0xff, 0x15],
                line: [0x6b, 0x72, 0x86, 0x40],
                red: [0xff, 0x8a, 0x8a],
                green: [0xa8, 0xd4, 0x6f],
                yellow: [0xe8, 0xc7, 0x78],
                blue: [0x8d, 0xb7, 0xff],
                purple: [0xd1, 0xa3, 0xff],
                cyan: [0x7f, 0xd6, 0xc2],
                frame: [0x7e, 0x8b, 0x9d],
            },
            _ => panic!("retired themes map to the active catalog"),
        }
    }
}

pub(crate) fn fresh_terminal(name: ThemeName) -> TermTheme {
    let palette = name.fresh_palette().expect("fresh theme");
    let ansi = match name {
        ThemeName::CatppuccinMocha => [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
        ThemeName::CatppuccinLatte => [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
        ThemeName::GlassLight => [
            0x303030, 0xa31700, 0x0a7f3d, 0xaf551d, 0x006cd8, 0x583cac, 0x00798a, 0x494949,
            0x8c8c8c, 0xa31700, 0x0a7f3d, 0xaf551d, 0x006cd8, 0x583cac, 0x00798a, 0x1c1c1c,
        ],
        ThemeName::GlassDark => [
            0x252a35, 0xff8a8a, 0xa8d46f, 0xe8c778, 0x8db7ff, 0xd1a3ff, 0x7fd6c2, 0xe3e6f0,
            0x747b8e, 0xffb0a8, 0xc8ea90, 0xf2da9a, 0xb2ccff, 0xe0c2ff, 0xa3e6d8, 0xffffff,
        ],
        _ if palette.is_light => [
            0x26384a, 0xa34248, 0x28745e, 0x88621f, 0x306592, 0x77569b, 0x247782, 0x526375,
            0x667788, 0xb13d48, 0x247052, 0x826013, 0x295e9b, 0x80529a, 0x19717d, 0x34485c,
        ],
        _ => [
            0x263b49, 0xe49a9d, 0x9bc5a6, 0xe0c38b, 0x91bcdf, 0xc5addd, 0x91cecf, 0xdfeaf1,
            0x90a5b5, 0xf0b2b3, 0xb1dcc0, 0xeed4a5, 0xb0d1ed, 0xd9c4ed, 0xb0e1dc, 0xf2f7fa,
        ],
    }
    .map(rgb);
    TermTheme {
        background: palette.surface,
        is_light: palette.is_light,
        exact: Some(ExactTermColors {
            foreground: palette.foreground,
            ansi,
            cursor: Some(palette.accent),
            cursor_text: Some(palette.surface),
            cursor_stroke: Some(palette.accent),
            selection_foreground: Some(palette.foreground),
            selection_background: Some(palette.shell),
        }),
        powerline: [
            palette.accent,
            palette.surface,
            palette.shell,
            palette.foreground,
            palette.shell,
            palette.accent,
            palette.surface,
            palette.muted,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_themes_round_trip_and_keep_readable_foreground() {
        for name in [
            ThemeName::BreezeLight,
            ThemeName::BreezeDark,
            ThemeName::MintLight,
            ThemeName::MintDark,
        ] {
            assert_eq!(ThemeName::from_prompt_name(name.prompt_name()), Some(name));
            let palette = name.fresh_palette().unwrap();
            let terminal = name.term_theme();
            assert_eq!(terminal.background, palette.surface);
            assert_eq!(terminal.is_light, palette.is_light);
            let luma = |color: Rgb8| {
                color
                    .into_iter()
                    .zip([0.2126, 0.7152, 0.0722])
                    .map(|(c, weight)| {
                        let c = f64::from(c) / 255.0;
                        (if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) })
                            * weight
                    })
                    .sum::<f64>()
            };
            let a = luma(palette.foreground);
            let b = luma(palette.surface);
            assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 7.0, "{}", name.prompt_name());
        }
    }
}
