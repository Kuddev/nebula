//! Four quiet built-in palettes, shared by terminal and chrome adapters.
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
    pub fn fresh_palette(self) -> Option<FreshPalette> {
        let (shell, surface, accent, foreground, muted, is_light) = match self {
            Self::BreezeLight => (0xeaf0f6, 0xf9fbff, 0x306592, 0x26384a, 0x576c80, true),
            Self::BreezeDark => (0x18232e, 0x202e3b, 0x91bcdf, 0xe0eaf3, 0xa5b6c7, false),
            Self::MintLight => (0xeaf2ee, 0xf8fcf9, 0x28745e, 0x263c33, 0x587365, true),
            Self::MintDark => (0x192a26, 0x213730, 0x8bcbb3, 0xdfeee7, 0xa6bfb2, false),
            _ => return None,
        };
        Some(FreshPalette {
            shell: rgb(shell),
            surface: rgb(surface),
            accent: rgb(accent),
            foreground: rgb(foreground),
            muted: rgb(muted),
            is_light,
        })
    }
}

const fn rgb(value: u32) -> Rgb8 {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

pub(crate) fn fresh_terminal(name: ThemeName) -> TermTheme {
    let palette = name.fresh_palette().expect("fresh theme");
    let ansi = if palette.is_light {
        [
            0x26384a, 0xa34248, 0x28745e, 0x88621f, 0x306592, 0x77569b, 0x247782, 0x526375,
            0x667788, 0xb13d48, 0x247052, 0x826013, 0x295e9b, 0x80529a, 0x19717d, 0x34485c,
        ]
    } else {
        [
            0x263b49, 0xe49a9d, 0x9bc5a6, 0xe0c38b, 0x91bcdf, 0xc5addd, 0x91cecf, 0xdfeaf1,
            0x90a5b5, 0xf0b2b3, 0xb1dcc0, 0xeed4a5, 0xb0d1ed, 0xd9c4ed, 0xb0e1dc, 0xf2f7fa,
        ]
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
