use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer, Serialize};

use nebula_config_derive::ConfigDeserialize;

use crate::display::color::{CellRgb, Rgb};

/// Sentinel used by the theme-following cursor path. A user/OSC override that
/// differs from this value keeps its explicit color and full opacity.
pub(crate) const NEBULA_DEFAULT_CURSOR: InvertedCellColors = InvertedCellColors {
    foreground: CellRgb::CellBackground,
    background: CellRgb::Rgb(Rgb::new(0x49, 0x4d, 0x72)),
};

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub primary: PrimaryColors,
    pub cursor: InvertedCellColors,
    pub vi_mode_cursor: InvertedCellColors,
    pub selection: InvertedCellColors,
    pub normal: NormalColors,
    pub bright: BrightColors,
    pub dim: Option<DimColors>,
    pub indexed_colors: Vec<IndexedColor>,
    pub search: SearchColors,
    pub line_indicator: LineIndicatorColors,
    pub hints: HintColors,
    pub transparent_background_colors: bool,
    pub draw_bold_text_with_bright_colors: bool,
    footer_bar: BarColors,
}

impl Default for Colors {
    fn default() -> Self {
        // 只定制默认光标，不改 InvertedCellColors::default()：该类型也被
        // selection 复用，直接改会把选区颜色一起染偏。用户显式配置的
        // [colors.cursor] 会继续覆盖这里的默认值。
        let cursor = NEBULA_DEFAULT_CURSOR;

        Self {
            primary: Default::default(),
            cursor,
            vi_mode_cursor: cursor,
            selection: Default::default(),
            normal: Default::default(),
            bright: Default::default(),
            dim: Default::default(),
            indexed_colors: Default::default(),
            search: Default::default(),
            line_indicator: Default::default(),
            hints: Default::default(),
            transparent_background_colors: Default::default(),
            draw_bold_text_with_bright_colors: Default::default(),
            footer_bar: Default::default(),
        }
    }
}

impl Colors {
    pub fn footer_bar_foreground(&self) -> Rgb {
        self.footer_bar.foreground.unwrap_or(self.primary.background)
    }

    pub fn footer_bar_background(&self) -> Rgb {
        self.footer_bar.background.unwrap_or(self.primary.foreground)
    }
}

#[derive(ConfigDeserialize, Serialize, Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct LineIndicatorColors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
}

#[derive(ConfigDeserialize, Serialize, Default, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintColors {
    pub start: HintStartColors,
    pub end: HintEndColors,
}

#[derive(ConfigDeserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintStartColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for HintStartColors {
    fn default() -> Self {
        Self {
            foreground: CellRgb::Rgb(Rgb::new(0x18, 0x18, 0x18)),
            background: CellRgb::Rgb(Rgb::new(0xf4, 0xbf, 0x75)),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintEndColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for HintEndColors {
    fn default() -> Self {
        Self {
            foreground: CellRgb::Rgb(Rgb::new(0x18, 0x18, 0x18)),
            background: CellRgb::Rgb(Rgb::new(0xac, 0x42, 0x42)),
        }
    }
}

#[derive(Deserialize, Serialize, Copy, Clone, Default, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IndexedColor {
    pub color: Rgb,

    index: ColorIndex,
}

impl IndexedColor {
    #[inline]
    pub fn index(&self) -> u8 {
        self.index.0
    }
}

#[derive(Serialize, Copy, Clone, Default, Debug, PartialEq, Eq)]
struct ColorIndex(u8);

impl<'de> Deserialize<'de> for ColorIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let index = u8::deserialize(deserializer)?;

        if index < 16 {
            Err(SerdeError::custom(
                "Config error: indexed_color's index is {}, but a value bigger than 15 was \
                 expected; ignoring setting",
            ))
        } else {
            Ok(Self(index))
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct InvertedCellColors {
    #[config(alias = "text")]
    pub foreground: CellRgb,
    #[config(alias = "cursor")]
    pub background: CellRgb,
}

impl Default for InvertedCellColors {
    fn default() -> Self {
        Self { foreground: CellRgb::CellBackground, background: CellRgb::CellForeground }
    }
}

#[derive(ConfigDeserialize, Serialize, Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct SearchColors {
    pub focused_match: FocusedMatchColors,
    pub matches: MatchColors,
}

#[derive(ConfigDeserialize, Serialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct FocusedMatchColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for FocusedMatchColors {
    fn default() -> Self {
        Self {
            background: CellRgb::Rgb(Rgb::new(0xf4, 0xbf, 0x75)),
            foreground: CellRgb::Rgb(Rgb::new(0x18, 0x18, 0x18)),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct MatchColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for MatchColors {
    fn default() -> Self {
        Self {
            background: CellRgb::Rgb(Rgb::new(0xac, 0x42, 0x42)),
            foreground: CellRgb::Rgb(Rgb::new(0x18, 0x18, 0x18)),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct BarColors {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
}

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct PrimaryColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub bright_foreground: Option<Rgb>,
    pub dim_foreground: Option<Rgb>,
}

impl Default for PrimaryColors {
    fn default() -> Self {
        // Nebula deep-space theme (sampled from the design mockup).
        PrimaryColors {
            background: Rgb::new(0x08, 0x0a, 0x18),
            foreground: Rgb::new(0xd6, 0xda, 0xea),
            bright_foreground: Default::default(),
            dim_foreground: Default::default(),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct NormalColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for NormalColors {
    fn default() -> Self {
        // Nebula deep-space theme (sampled from the design mockup).
        NormalColors {
            black: Rgb::new(0x1a, 0x1d, 0x2e),
            red: Rgb::new(0xff, 0x6b, 0x81),
            green: Rgb::new(0x65, 0xe8, 0x6e),
            yellow: Rgb::new(0xf5, 0xc8, 0x4c),
            blue: Rgb::new(0x38, 0xa8, 0xff),
            magenta: Rgb::new(0xb4, 0x8c, 0xff),
            cyan: Rgb::new(0x4f, 0xd6, 0xe0),
            white: Rgb::new(0xd6, 0xda, 0xea),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BrightColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for BrightColors {
    fn default() -> Self {
        // Nebula deep-space theme (brighter accents).
        BrightColors {
            black: Rgb::new(0x8d, 0x94, 0xaa),
            red: Rgb::new(0xff, 0x8b, 0x9d),
            green: Rgb::new(0x86, 0xf0, 0x90),
            yellow: Rgb::new(0xff, 0xda, 0x7a),
            blue: Rgb::new(0x6c, 0xc0, 0xff),
            magenta: Rgb::new(0xc9, 0xa8, 0xff),
            cyan: Rgb::new(0x73, 0xe4, 0xec),
            white: Rgb::new(0xf2, 0xf4, 0xfb),
        }
    }
}

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct DimColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for DimColors {
    fn default() -> Self {
        // Generated with builtin nebula's color dimming function.
        DimColors {
            black: Rgb::new(0x0f, 0x0f, 0x0f),
            red: Rgb::new(0x71, 0x2b, 0x2b),
            green: Rgb::new(0x5f, 0x6f, 0x3a),
            yellow: Rgb::new(0xa1, 0x7e, 0x4d),
            blue: Rgb::new(0x45, 0x68, 0x77),
            magenta: Rgb::new(0x70, 0x4d, 0x68),
            cyan: Rgb::new(0x4d, 0x77, 0x70),
            white: Rgb::new(0x8e, 0x8e, 0x8e),
        }
    }
}
