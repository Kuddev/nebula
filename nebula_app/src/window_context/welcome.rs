//! Nebula 新 tab 欢迎页。
//!
//! Windows 上渲染彩色字符 logo + fastfetch 风格的信息卡（通过一个缓存的
//! PowerShell 脚本秒出）；非 Windows 上回退到 `fastfetch`/`uname`。全部是
//! 无状态纯函数，与 [`WindowContext`](super::WindowContext) 完全解耦——
//! 唯一对外入口是 [`nebula_fastfetch_intro_command`]。

#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use crate::display::NebulaShell;

#[cfg(windows)]
fn nebula_fastfetch_text(narrow: bool) -> String {
    let cyan = "\x1b[38;2;0;229;255m";
    let green = "\x1b[38;2;0;205;64m";
    let magenta = "\x1b[38;2;190;45;220m";
    let white = "\x1b[97m";
    let dim = "\x1b[38;2;180;186;200m";
    let icon = "\x1b[38;2;150;140;220m";
    let reset = "\x1b[0m";
    // Keycap styling for shortcut combos: grey pill with builtin powerline round
    // caps (e0b6/e0b4) so the background reads as a rounded keycap, light ink.
    let kbd = |k: &str| {
        format!(
            "\x1b[38;2;48;52;68m\u{e0b6}\x1b[48;2;48;52;68m\x1b[38;2;224;228;242m{k}\x1b[0m\x1b[38;2;48;52;68m\u{e0b4}\x1b[0m"
        )
    };

    let cwd = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_owned());
    let art = super::nebula_fetch_art::NEBULA_STAR_ART;
    let info = [
        format!("{white}Welcome to {green}Nebula Terminal{white},{reset}"),
        format!(
            "{white}a fast terminal workspace for {cyan}tabs{white}, {cyan}splits{white} and shells.{reset}"
        ),
        String::new(),
        format!("{icon}\u{f02b}{reset}  {dim}Version:{reset} {green}{}{reset}", env!("CARGO_PKG_VERSION")),
        format!("{icon}\u{f09b}{reset}  {dim}GitHub:{reset} {cyan}https://github.com/Kuddev/nebulaTerminal{reset}"),
        format!(
            "{icon}\u{f11c}{reset}  {dim}Shortcuts:{reset}  {a}{white} new tab · {b}{white} switch tabs{reset}",
            a = kbd("Ctrl+Shift+T"),
            b = kbd("Ctrl+1..5"),
        ),
        format!(
            "{icon}\u{f0db}{reset}  {dim}Split:{reset}  {a}{white} / {b}{white} split · drag to resize{reset}",
            a = kbd("Ctrl+Shift+D"),
            b = kbd("Ctrl+Shift+S"),
        ),
        format!(
            "{icon}\u{f009}{reset}  {dim}Panes:{reset}  {a}{white} focus · {b}{white} zoom · {c}{white} close{reset}",
            a = kbd("Ctrl+Alt+Arrows"),
            b = kbd("Ctrl+Shift+Enter"),
            c = kbd("Ctrl+Shift+W"),
        ),
        format!(
            "{icon}\u{f1fc}{reset}  {dim}Theme:{reset} {white}use the gear menu for chrome and completion settings{reset}"
        ),
        format!("{icon}\u{f07b}{reset}  {dim}Current Directory:{reset} {magenta}{cwd}{reset}"),
        String::new(),
        // Faint horizontal rule before the closing line, like the reference.
        format!("\x1b[38;2;58;63;84m{}{reset}", "─".repeat(56)),
        String::new(),
        format!("{icon}\u{f135}{reset}  {green}Startup Time:{reset} {white}ready{reset}"),
    ];

    let rows = art.len().max(info.len());
    let info_col = 65; // the density-mapped galaxy art is 62 visible columns wide, +3 gap
    // Disable auto-wrap (DECAWM) while the intro prints: on narrow windows a
    // long info row must clip at the right edge, not wrap into the galaxy art.
    let mut text = String::from("\x1b[?7l");
    if narrow {
        // Narrow pane (e.g. after a split): stack the info below the art
        // instead of the two-column layout, which needs ~150 columns.
        for logo in art {
            text.push_str(logo);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        for line in info.iter().filter(|l| !l.is_empty()) {
            text.push_str(line);
            text.push_str("\r\n");
        }
        text.push_str("\x1b[?7h");
        return text;
    }
    for row in 0..rows {
        if let Some(logo) = art.get(row) {
            text.push_str(logo); // pre-coloured cells, already terminated with reset
        }
        let right = info.get(row).map(String::as_str).unwrap_or("");
        if !right.is_empty() {
            // 不写填充空格，直接把光标移动到右栏；否则 logo 中间的空行会
            // 在终端选择时变成一整条空白带，看起来比设计稿宽很多。
            text.push_str(&format!("\x1b[{info_col}G"));
            text.push_str(right);
        }
        text.push_str("\r\n");
    }
    text.push_str("\x1b[?7h"); // restore auto-wrap for the shell session
    text
}

#[cfg(windows)]
fn nebula_fastfetch_script_path(narrow: bool) -> Option<std::path::PathBuf> {
    static WIDE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    static NARROW: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    let (cell, file) = if narrow {
        (&NARROW, "nebula_fastfetch_intro_narrow.ps1")
    } else {
        (&WIDE, "nebula_fastfetch_intro.ps1")
    };
    cell.get_or_init(|| {
        let path = std::env::temp_dir().join(file);
        let text = nebula_fastfetch_text(narrow).replace("\r\n", "\n").replace('\r', "\n");
            let script = format!(
                "$OutputEncoding = [System.Text.Encoding]::UTF8\r\n\
                 [Console]::OutputEncoding = [System.Text.Encoding]::UTF8\r\n\
                 [Console]::Write(@'\r\n{text}\r\n'@)\r\n"
            );

            // Windows PowerShell 5.1 会把无 BOM UTF-8 当作本地 ANSI；这里写 BOM，
            // 保证字符 logo 与 ANSI 序列在脚本文件中稳定解析。
            let mut bytes = Vec::with_capacity(3 + script.len());
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            bytes.extend_from_slice(script.as_bytes());
            std::fs::write(&path, bytes).ok()?;
            Some(path)
        })
        .clone()
}

#[cfg(windows)]
fn powershell_single_quoted_path(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Bytes to send to a freshly spawned shell so it prints the Nebula intro.
#[cfg(windows)]
pub(super) fn nebula_fastfetch_intro_command() -> Vec<u8> {
    nebula_fastfetch_intro_command_for(usize::MAX, NebulaShell::PowerShell)
}

/// Width-aware intro: the two-column layout needs ~150 columns (62-col art +
/// info at column 65); anything narrower gets the stacked layout instead.
/// Also used to *re-print* the intro after a resize reflows a pristine pane.
#[cfg(windows)]
pub(super) fn nebula_fastfetch_intro_command_for(columns: usize, shell: NebulaShell) -> Vec<u8> {
    if shell == NebulaShell::Bash {
        return b"clear; if command -v fastfetch >/dev/null 2>&1; then fastfetch; else printf '\\033[36mNebula Terminal\\033[0m\\n'; uname -a; fi\n".to_vec();
    }

    // 新 tab 必须秒出：所有系统信息在 Rust 侧一次性缓存；交给 PowerShell 的
    // 只有执行一个纯输出脚本，避免把 ANSI/Logo 当成用户输入逐行解析。
    let narrow = columns < 132;
    match nebula_fastfetch_script_path(narrow) {
        Some(path) => {
            format!("Clear-Host; & {}\r", powershell_single_quoted_path(&path)).into_bytes()
        },
        None => b"Clear-Host\r".to_vec(),
    }
}

/// Bytes to send to a freshly spawned shell so it prints the Nebula intro.
#[cfg(not(windows))]
pub(super) fn nebula_fastfetch_intro_command() -> Vec<u8> {
    b"clear; if command -v fastfetch >/dev/null 2>&1; then fastfetch; else printf '\\033[36mNebula Terminal\\033[0m\\n'; uname -a; fi\n".to_vec()
}

/// Width-aware variant; the Unix intro delegates to fastfetch, which already
/// adapts to the terminal width, so `columns` is unused there.
#[cfg(not(windows))]
pub(super) fn nebula_fastfetch_intro_command_for(
    _columns: usize,
    _shell: crate::display::NebulaShell,
) -> Vec<u8> {
    nebula_fastfetch_intro_command()
}
