//! Program identity, icons, and logo preparation shared by both UI shells.

use super::command_completion::extract_program;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiLogo {
    Claude,
    OpenAi,
    OpenCode,
    Pi,
    Grok,
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
    let normalized =
        extract_program(program).unwrap_or_else(|| program.trim().to_ascii_lowercase());
    match normalized.as_str() {
        "claude" => Some(AiLogo::Claude),
        "codex" => Some(AiLogo::OpenAi),
        "opencode" => Some(AiLogo::OpenCode),
        "pi" => Some(AiLogo::Pi),
        "grok" | "grok-cli" => Some(AiLogo::Grok),
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
