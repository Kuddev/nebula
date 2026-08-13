//! 终端调色板：把 vte 的 `Color` 解析成 GPUI 的 `Rgba`。
//!
//! 解析顺序与 nebula_app 一致：OSC 4/10/11 的运行时覆盖（`Term::colors()`）
//! 优先，其次是 `Palette`（用户 nebula.toml 覆盖 deep-space 默认值，由
//! `crate::config` 构造）。

use gpui::Rgba;
use nebula_terminal::term::color::Colors;
use nebula_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb8(r: u8, g: u8, b: u8) -> Rgba {
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
}

/// 一份终端实例的静态配色。默认值与 nebula_app 的 deep-space 主题逐字段一致
/// （`nebula_app/src/config/color.rs`），保证两个壳在同一份用户配置下同色。
#[derive(Clone)]
pub struct Palette {
    pub foreground: Rgba,
    pub background: Rgba,
    pub bright_foreground: Rgba,
    pub dim_foreground: Rgba,
    /// 块状光标的填充色（主应用 NEBULA_DEFAULT_CURSOR 的 background）。
    pub cursor: Rgba,
    /// 选区叠加色。主应用默认是反色语义；本壳以半透明叠加近似，
    /// 用户显式配置 `colors.selection.background` 时用不透明具体色。
    pub selection: Rgba,
    /// ANSI 0-15。
    pub ansi: [Rgba; 16],
    /// Dim 0-7。
    pub dim: [Rgba; 8],
    /// `colors.indexed_colors` 的 16-255 覆盖。
    pub indexed: Vec<(u8, Rgba)>,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            background: rgb8(0x08, 0x0a, 0x18),
            foreground: rgb8(0xd6, 0xda, 0xea),
            bright_foreground: rgb8(0xd6, 0xda, 0xea),
            dim_foreground: Self::dim_of(rgb8(0xd6, 0xda, 0xea)),
            cursor: rgb8(0x49, 0x4d, 0x72),
            selection: Rgba { a: 0.60, ..rgb8(0x49, 0x4d, 0x72) },
            ansi: [
                // normal
                rgb8(0x1a, 0x1d, 0x2e), // black
                rgb8(0xff, 0x6b, 0x81), // red
                rgb8(0x65, 0xe8, 0x6e), // green
                rgb8(0xf5, 0xc8, 0x4c), // yellow
                rgb8(0x38, 0xa8, 0xff), // blue
                rgb8(0xb4, 0x8c, 0xff), // magenta
                rgb8(0x4f, 0xd6, 0xe0), // cyan
                rgb8(0xd6, 0xda, 0xea), // white
                // bright
                rgb8(0x8d, 0x94, 0xaa),
                rgb8(0xff, 0x8b, 0x9d),
                rgb8(0x86, 0xf0, 0x90),
                rgb8(0xff, 0xda, 0x7a),
                rgb8(0x6c, 0xc0, 0xff),
                rgb8(0xc9, 0xa8, 0xff),
                rgb8(0x73, 0xe4, 0xec),
                rgb8(0xf2, 0xf4, 0xfb),
            ],
            dim: [
                rgb8(0x0f, 0x0f, 0x0f),
                rgb8(0x71, 0x2b, 0x2b),
                rgb8(0x5f, 0x6f, 0x3a),
                rgb8(0xa1, 0x7e, 0x4d),
                rgb8(0x45, 0x68, 0x77),
                rgb8(0x70, 0x4d, 0x68),
                rgb8(0x4d, 0x77, 0x70),
                rgb8(0x8e, 0x8e, 0x8e),
            ],
            indexed: Vec::new(),
        }
    }
}

impl Palette {
    /// 主应用的 dimming 系数。
    pub fn dim_of(color: Rgba) -> Rgba {
        Rgba { r: color.r * 0.66, g: color.g * 0.66, b: color.b * 0.66, a: color.a }
    }

    fn indexed_default(&self, index: u8) -> Rgba {
        if index >= 16 {
            if let Some((_, color)) = self.indexed.iter().find(|(i, _)| *i == index) {
                return *color;
            }
        }
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let idx = index as u16 - 16;
                let comp = |c: u16| -> u8 { if c == 0 { 0 } else { (c * 40 + 55) as u8 } };
                rgb8(comp(idx / 36), comp(idx / 6 % 6), comp(idx % 6))
            },
            232..=255 => {
                let v = 8 + 10 * (index - 232);
                rgb8(v, v, v)
            },
        }
    }

    fn named_default(&self, named: NamedColor) -> Rgba {
        use NamedColor::*;
        match named {
            Foreground => self.foreground,
            BrightForeground => self.bright_foreground,
            DimForeground => self.dim_foreground,
            Background => self.background,
            Cursor => self.cursor,
            DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan
            | DimWhite => self.dim[named as usize - NamedColor::DimBlack as usize],
            // 0-15 直接落在 ansi 表内。
            other => self.ansi[(other as usize).min(15)],
        }
    }

    /// 解析单元格颜色。`bold` 让 0-7 号命名色提亮为 8-15（经典终端行为）。
    pub fn resolve(&self, color: Color, overrides: &Colors, bold: bool) -> Rgba {
        match color {
            Color::Spec(rgb) => from_ansi_rgb(rgb),
            Color::Indexed(index) => {
                let index = if bold && index < 8 { index + 8 } else { index };
                overrides[index as usize]
                    .map(from_ansi_rgb)
                    .unwrap_or_else(|| self.indexed_default(index))
            },
            Color::Named(named) => {
                let named = if bold && (named as usize) < 8 {
                    // 0-7 与 8-15 一一对应。
                    match named {
                        NamedColor::Black => NamedColor::BrightBlack,
                        NamedColor::Red => NamedColor::BrightRed,
                        NamedColor::Green => NamedColor::BrightGreen,
                        NamedColor::Yellow => NamedColor::BrightYellow,
                        NamedColor::Blue => NamedColor::BrightBlue,
                        NamedColor::Magenta => NamedColor::BrightMagenta,
                        NamedColor::Cyan => NamedColor::BrightCyan,
                        NamedColor::White => NamedColor::BrightWhite,
                        other => other,
                    }
                } else {
                    named
                };
                overrides[named].map(from_ansi_rgb).unwrap_or_else(|| self.named_default(named))
            },
        }
    }

    /// OSC 4/10/11 颜色查询（`Event::ColorRequest`）的回答值。
    pub fn query_reply(&self, index: usize, overrides: &Colors) -> Rgb {
        if let Some(rgb) = overrides[index] {
            return rgb;
        }
        let rgba = match index {
            0..=255 => self.indexed_default(index as u8),
            _ => match index_to_named(index) {
                Some(named) => self.named_default(named),
                None => self.foreground,
            },
        };
        Rgb { r: (rgba.r * 255.0) as u8, g: (rgba.g * 255.0) as u8, b: (rgba.b * 255.0) as u8 }
    }
}

pub fn from_ansi_rgb(rgb: Rgb) -> Rgba {
    rgb8(rgb.r, rgb.g, rgb.b)
}

fn index_to_named(index: usize) -> Option<NamedColor> {
    use NamedColor::*;
    // Colors 的 256..268 段与 NamedColor 的显式判别值对齐。
    [
        Foreground,
        Background,
        Cursor,
        DimBlack,
        DimRed,
        DimGreen,
        DimYellow,
        DimBlue,
        DimMagenta,
        DimCyan,
        DimWhite,
        BrightForeground,
        DimForeground,
    ]
    .into_iter()
    .find(|n| *n as usize == index)
}
