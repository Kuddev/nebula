//! 终端调色板：把 vte 的 `Color` 解析成 GPUI 的 `Rgba`。
//!
//! 解析顺序与 nebula_app 一致：OSC 4/10/11 的运行时覆盖（`Term::colors()`）
//! 优先，其次是这里的默认调色板。

use gpui::Rgba;
use nebula_terminal::term::color::Colors;
use nebula_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// 默认前景色（暗色主题下的正文灰）。
pub const FOREGROUND: Rgba = rgb8(0xd6, 0xda, 0xe2);
/// 默认背景色，与 `ui::theme` 的窗口背景同族但独立，避免主题层反向依赖终端。
pub const BACKGROUND: Rgba = rgb8(0x11, 0x14, 0x1a);
/// 光标颜色。
pub const CURSOR: Rgba = rgb8(0x4f, 0xd6, 0x9c);
/// 选区背景色。
pub const SELECTION: Rgba = Rgba { r: 0.24, g: 0.45, b: 0.38, a: 0.55 };

const fn rgb8(r: u8, g: u8, b: u8) -> Rgba {
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
}

/// ANSI 0-15 的默认值（VS Code 终端默认配色，可读性经过广泛验证）。
const ANSI16: [Rgba; 16] = [
    rgb8(0x1e, 0x22, 0x28), // black
    rgb8(0xcd, 0x31, 0x31), // red
    rgb8(0x0d, 0xbc, 0x79), // green
    rgb8(0xe5, 0xe5, 0x10), // yellow
    rgb8(0x24, 0x72, 0xc8), // blue
    rgb8(0xbc, 0x3f, 0xbc), // magenta
    rgb8(0x11, 0xa8, 0xcd), // cyan
    rgb8(0xe5, 0xe5, 0xe5), // white
    rgb8(0x66, 0x66, 0x66), // bright black
    rgb8(0xf1, 0x4c, 0x4c), // bright red
    rgb8(0x23, 0xd1, 0x8b), // bright green
    rgb8(0xf5, 0xf5, 0x43), // bright yellow
    rgb8(0x3b, 0x8e, 0xea), // bright blue
    rgb8(0xd6, 0x70, 0xd6), // bright magenta
    rgb8(0x29, 0xb8, 0xdb), // bright cyan
    rgb8(0xff, 0xff, 0xff), // bright white
];

pub fn from_ansi_rgb(rgb: Rgb) -> Rgba {
    rgb8(rgb.r, rgb.g, rgb.b)
}

fn dim(color: Rgba) -> Rgba {
    Rgba { r: color.r * 0.66, g: color.g * 0.66, b: color.b * 0.66, a: color.a }
}

fn indexed_default(index: u8) -> Rgba {
    match index {
        0..=15 => ANSI16[index as usize],
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

fn named_default(named: NamedColor) -> Rgba {
    use NamedColor::*;
    match named {
        Foreground | BrightForeground => FOREGROUND,
        Background => BACKGROUND,
        Cursor => CURSOR,
        DimForeground => dim(FOREGROUND),
        DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan | DimWhite => {
            dim(ANSI16[named as usize - NamedColor::DimBlack as usize])
        },
        // 0-15 直接落在 ANSI16 表内。
        other => ANSI16[(other as usize).min(15)],
    }
}

/// 解析单元格颜色。`bold` 让 0-7 号命名色提亮为 8-15（经典终端行为）。
pub fn resolve(color: Color, overrides: &Colors, bold: bool) -> Rgba {
    match color {
        Color::Spec(rgb) => from_ansi_rgb(rgb),
        Color::Indexed(index) => {
            let index = if bold && index < 8 { index + 8 } else { index };
            overrides[index as usize].map(from_ansi_rgb).unwrap_or_else(|| indexed_default(index))
        },
        Color::Named(named) => {
            let named = if bold && (named as usize) < 8 {
                // SAFETY-free 提亮：0-7 与 8-15 一一对应。
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
            overrides[named].map(from_ansi_rgb).unwrap_or_else(|| named_default(named))
        },
    }
}

/// OSC 4/10/11 颜色查询（`Event::ColorRequest`）的回答值。
pub fn query_reply(index: usize, overrides: &Colors) -> Rgb {
    if let Some(rgb) = overrides[index] {
        return rgb;
    }
    let rgba = match index {
        0..=255 => indexed_default(index as u8),
        _ => match index_to_named(index) {
            Some(named) => named_default(named),
            None => FOREGROUND,
        },
    };
    Rgb { r: (rgba.r * 255.0) as u8, g: (rgba.g * 255.0) as u8, b: (rgba.b * 255.0) as u8 }
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
