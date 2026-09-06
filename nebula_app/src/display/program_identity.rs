//! Program identity, icons, and logo preparation shared by both UI shells.

use super::command_completion::extract_program;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiLogo {
    Claude,
    OpenAi,
    OpenCode,
    Pi,
    Grok,
    Antigravity,
}

impl AiLogo {
    pub(crate) const ALL: [Self; 6] =
        [Self::Claude, Self::OpenAi, Self::OpenCode, Self::Pi, Self::Grok, Self::Antigravity];

    /// One source catalog for both shells. Official color assets are embedded unchanged.
    pub(crate) fn png(self, light_ink: bool) -> &'static [u8] {
        match self {
            Self::Claude => include_bytes!("../../../extra/logo/ai_claude.png"),
            Self::OpenAi => include_bytes!("../../../extra/logo/ai_openai.png"),
            Self::OpenCode => include_bytes!("../../../extra/logo/ai_opencode.png"),
            Self::Pi => include_bytes!("../../../extra/logo/ai_pi.png"),
            Self::Grok if light_ink => include_bytes!("../../../extra/logo/ai_grok_light.png"),
            Self::Grok => include_bytes!("../../../extra/logo/ai_grok_dark.png"),
            Self::Antigravity => include_bytes!("../../../extra/logo/ai_antigravity.png"),
        }
    }

    pub(crate) fn tint_pixels(self, pixels: &mut [u8], ink: [u8; 3]) {
        let preserve_luma = match self {
            Self::OpenAi | Self::Pi => false,
            // OpenCode stores a luma map: the frame is white, the inner block gray.
            Self::OpenCode => true,
            Self::Claude | Self::Grok | Self::Antigravity => return,
        };
        for pixel in pixels.chunks_exact_mut(4) {
            let luma = if preserve_luma { u16::from(pixel[0]) } else { 255 };
            for channel in 0..3 {
                pixel[channel] = (u16::from(ink[channel]) * luma / 255) as u8;
            }
        }
    }
}

pub(crate) fn prepare_ai_logo_texture(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_size: u32,
) -> (Vec<u8>, u32, u32) {
    use image::imageops::FilterType;

    let target_size = target_size.max(1);
    let source = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .expect("decoded logo dimensions must match its RGBA buffer");
    let resized = image::imageops::resize(&source, target_size, target_size, FilterType::Lanczos3);
    let pixels = resized.as_raw();
    let (mass, weighted_x, weighted_y) = pixels.chunks_exact(4).enumerate().fold(
        (0.0, 0.0, 0.0),
        |(mass, weighted_x, weighted_y), (index, pixel)| {
            let alpha = f64::from(pixel[3]);
            let x = (index as u32 % target_size) as f64 + 0.5;
            let y = (index as u32 / target_size) as f64 + 0.5;
            (mass + alpha, weighted_x + x * alpha, weighted_y + y * alpha)
        },
    );
    if mass == 0.0 {
        return (resized.into_raw(), target_size, target_size);
    }

    let center = f64::from(target_size) / 2.0;
    let shift_x = (center - weighted_x / mass).round() as i32;
    let shift_y = (center - weighted_y / mass).round() as i32;
    if shift_x == 0 && shift_y == 0 {
        return (resized.into_raw(), target_size, target_size);
    }

    let mut centered = vec![0; pixels.len()];
    for y in 0..target_size as i32 {
        for x in 0..target_size as i32 {
            let dest_x = x + shift_x;
            let dest_y = y + shift_y;
            if !(0..target_size as i32).contains(&dest_x)
                || !(0..target_size as i32).contains(&dest_y)
            {
                continue;
            }
            let source_index = ((y as u32 * target_size + x as u32) * 4) as usize;
            let dest_index = ((dest_y as u32 * target_size + dest_x as u32) * 4) as usize;
            centered[dest_index..dest_index + 4]
                .copy_from_slice(&pixels[source_index..source_index + 4]);
        }
    }
    (centered, target_size, target_size)
}

pub(crate) fn ai_logo_for_program(program: &str) -> Option<AiLogo> {
    use crate::ai_agents::AgentKind;

    let normalized =
        extract_program(program).unwrap_or_else(|| program.trim().to_ascii_lowercase());
    match AgentKind::parse(&normalized)? {
        AgentKind::Claude => Some(AiLogo::Claude),
        AgentKind::Codex => Some(AiLogo::OpenAi),
        AgentKind::OpenCode => Some(AiLogo::OpenCode),
        AgentKind::Pi => Some(AiLogo::Pi),
        AgentKind::Grok => Some(AiLogo::Grok),
        AgentKind::Antigravity => Some(AiLogo::Antigravity),
        _ => None,
    }
}

pub(crate) fn ai_logo(program: &str) -> Option<AiLogo> {
    if cfg!(any(not(feature = "png"), target_os = "macos")) {
        return None;
    }
    ai_logo_for_program(program)
}

pub(crate) fn program_icon(program: &str) -> &'static str {
    match program {
        "claude" => "\u{f0ce5}",
        "codex" => "\u{f02d8}",
        "gemini" => "\u{f0ce6}",
        "copilot" => "\u{f4b8}",
        "cursor" | "cursor-agent" => "\u{f0ec3}",
        "aider" | "goose" | "crush" | "ollama" => "\u{f06a9}",
        "opencode" => "\u{f489}",
        "pi" => "\u{f135}",
        "git" | "lazygit" => "\u{f418}",
        "vim" | "nvim" | "vi" | "hx" | "nano" => "\u{e62b}",
        "ssh" | "mosh" => "\u{f489}",
        "cargo" | "rustc" => "\u{e7a8}",
        "node" | "npm" | "pnpm" | "yarn" | "bun" | "deno" => "\u{e718}",
        "python" | "python3" | "pip" | "uv" => "\u{e73c}",
        "docker" | "podman" => "\u{e7b0}",
        _ => "\u{f04b}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let image = image::load_from_memory(bytes).unwrap().into_rgba8();
        (image.width(), image.height(), image.into_raw())
    }

    fn logo_for_command(command: &str) -> Option<AiLogo> {
        extract_program(command).as_deref().and_then(ai_logo_for_program)
    }

    #[test]
    fn running_program_identity_normalizes_grok_without_changing_fallbacks() {
        for command in [
            "grok",
            "grok-cli",
            "GROK.CMD",
            "C:\\Tools\\GROK-CLI.EXE --help",
            "/usr/local/bin/grok",
        ] {
            assert_eq!(logo_for_command(command), Some(AiLogo::Grok), "{command}");
        }

        assert_eq!(logo_for_command("codex"), Some(AiLogo::OpenAi));
        assert_eq!(logo_for_command("C:\\Tools\\CLAUDE.EXE --resume"), Some(AiLogo::Claude));
        assert_eq!(program_icon("cargo"), "\u{e7a8}");
        assert_eq!(logo_for_command("unlisted-program"), None);
        assert_eq!(program_icon("unlisted-program"), "\u{f04b}");
        assert_eq!(extract_program(""), None);
    }

    #[test]
    fn official_grok_logos_are_embedded_unchanged_and_decodable() {
        use sha2::{Digest, Sha256};

        let assets = [
            (
                AiLogo::Grok.png(false),
                [
                    0x37, 0xdd, 0xbc, 0xb6, 0xe2, 0xa7, 0xf2, 0xe4, 0xb3, 0xbe, 0x78, 0xa7, 0xd4,
                    0x12, 0x96, 0xa3, 0xbc, 0x7e, 0xdf, 0x69, 0x26, 0x36, 0x24, 0x34, 0xef, 0xc0,
                    0x0d, 0xf5, 0xa5, 0x6a, 0x35, 0x86,
                ],
            ),
            (
                AiLogo::Grok.png(true),
                [
                    0x35, 0x90, 0x56, 0xee, 0x89, 0x83, 0xcf, 0xa0, 0xba, 0x7e, 0x72, 0x79, 0x50,
                    0x78, 0xc7, 0xc0, 0xdd, 0xf6, 0xc5, 0xd7, 0xa1, 0x87, 0x04, 0x01, 0xab, 0x96,
                    0x0e, 0xd4, 0xf9, 0xdf, 0x9e, 0x53,
                ],
            ),
        ];

        for (png, expected_sha256) in assets {
            assert_eq!(Sha256::digest(png).as_slice(), expected_sha256);
            let (width, height, rgba) = decode_png(png);
            assert_eq!((width, height), (1024, 1024));
            assert!(rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }

    #[test]
    fn official_grok_logos_prepare_sharp_optically_centered_physical_textures() {
        for png in [AiLogo::Grok.png(false), AiLogo::Grok.png(true)] {
            let (width, height, rgba) = decode_png(png);
            let (rgba, width, height) = prepare_ai_logo_texture(&rgba, width, height, 18);

            assert_eq!((width, height, rgba.len()), (18, 18, 18 * 18 * 4));
            let alpha_levels = rgba
                .chunks_exact(4)
                .map(|pixel| pixel[3])
                .filter(|alpha| *alpha > 0)
                .collect::<std::collections::HashSet<_>>();
            assert!(alpha_levels.len() >= 8);

            let (mass, weighted_x, weighted_y) = rgba.chunks_exact(4).enumerate().fold(
                (0.0, 0.0, 0.0),
                |(mass, weighted_x, weighted_y), (index, pixel)| {
                    let alpha = f64::from(pixel[3]);
                    let x = (index as u32 % width) as f64 + 0.5;
                    let y = (index as u32 / width) as f64 + 0.5;
                    (mass + alpha, weighted_x + x * alpha, weighted_y + y * alpha)
                },
            );
            let center = f64::from(width) / 2.0;
            assert!((weighted_x / mass - center).abs() <= 0.5);
            assert!((weighted_y / mass - center).abs() <= 0.5);
        }
    }

    #[test]
    fn antigravity_aliases_share_the_running_program_logo() {
        for command in [
            "antigravity",
            "agy",
            "antigravity-cli",
            "AGY.CMD --help",
            "C:\\Tools\\ANTIGRAVITY-CLI.EXE",
            "/usr/local/bin/antigravity",
        ] {
            assert_eq!(ai_logo_for_program(command), Some(AiLogo::Antigravity), "{command}");
        }
        for command in ["gravity", "antigravity-helper", "agy-not-an-agent"] {
            assert_eq!(ai_logo_for_program(command), None, "{command}");
        }
    }

    #[test]
    fn official_antigravity_logo_keeps_its_bytes_and_colors_on_both_themes() {
        use sha2::{Digest, Sha256};

        let png = AiLogo::Antigravity.png(false);
        assert_eq!(png, AiLogo::Antigravity.png(true));
        assert_eq!(
            Sha256::digest(png).as_slice(),
            [
                0xe0, 0xcd, 0x08, 0xcc, 0xd1, 0x0c, 0xd8, 0xd0, 0x8c, 0xcf, 0x0b, 0xa4, 0x49, 0x82,
                0x3e, 0xe8, 0x84, 0x95, 0x82, 0x5c, 0x08, 0x41, 0x61, 0x96, 0x18, 0x10, 0x0d, 0x3a,
                0xb0, 0x89, 0xf5, 0x1e,
            ]
        );
        let (width, height, source) = decode_png(png);
        assert_eq!((width, height), (540, 540));
        assert!(source.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(source.chunks_exact(4).any(|pixel| pixel[3] == 255));
        for ink in [[236, 239, 245], [35, 40, 50]] {
            let mut rgba = source.clone();
            AiLogo::Antigravity.tint_pixels(&mut rgba, ink);
            assert_eq!(rgba, source);
            for size in [16, 18, 24, 27, 36] {
                let (pixels, actual_width, actual_height) =
                    prepare_ai_logo_texture(&rgba, width, height, size);
                assert_eq!((actual_width, actual_height), (size, size));
                assert_eq!(pixels.len(), (size * size * 4) as usize);
                assert!(
                    pixels
                        .chunks_exact(4)
                        .any(|pixel| { pixel[3] > 128 && pixel[0].abs_diff(pixel[2]) > 50 })
                );
            }
        }
    }

    #[test]
    fn shared_tint_preserves_color_assets_and_opencode_luminance() {
        let source = [255, 255, 255, 128, 128, 128, 128, 64];
        let ink = [100, 200, 240];
        for logo in [AiLogo::Claude, AiLogo::Grok, AiLogo::Antigravity] {
            let mut pixels = source;
            logo.tint_pixels(&mut pixels, ink);
            assert_eq!(pixels, source);
        }
        for logo in [AiLogo::OpenAi, AiLogo::Pi] {
            let mut pixels = source;
            logo.tint_pixels(&mut pixels, ink);
            assert_eq!(pixels, [100, 200, 240, 128, 100, 200, 240, 64]);
        }
        let mut pixels = source;
        AiLogo::OpenCode.tint_pixels(&mut pixels, ink);
        assert_eq!(pixels, [100, 200, 240, 128, 50, 100, 120, 64]);
    }
}
