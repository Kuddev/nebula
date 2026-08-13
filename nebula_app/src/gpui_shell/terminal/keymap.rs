//! GPUI `Keystroke` → VT 字节序列编码。
//!
//! 只编码控制键与带修饰组合；普通可打印字符（含中文 IME 提交文本）走
//! `EntityInputHandler::replace_text_in_range`，避免同一按键被编码两次。

use gpui::Keystroke;
use nebula_terminal::term::TermMode;

/// xterm 修饰参数：1 + shift(1) + alt(2) + ctrl(4)。
fn modifier_param(ks: &Keystroke) -> u8 {
    let mut param = 1;
    if ks.modifiers.shift {
        param += 1;
    }
    if ks.modifiers.alt {
        param += 2;
    }
    if ks.modifiers.control {
        param += 4;
    }
    param
}

fn cursor_key(ks: &Keystroke, mode: &TermMode, letter: char) -> Vec<u8> {
    let param = modifier_param(ks);
    if param != 1 {
        format!("\x1b[1;{param}{letter}").into_bytes()
    } else if mode.contains(TermMode::APP_CURSOR) {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn tilde_key(ks: &Keystroke, code: u8) -> Vec<u8> {
    let param = modifier_param(ks);
    if param != 1 {
        format!("\x1b[{code};{param}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

fn function_key(ks: &Keystroke, index: u8) -> Option<Vec<u8>> {
    let param = modifier_param(ks);
    match index {
        // F1-F4 走 SS3（无修饰）或 CSI 1;m（带修饰）。
        1..=4 => {
            let letter = (b'P' + index - 1) as char;
            Some(if param == 1 {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[1;{param}{letter}").into_bytes()
            })
        },
        5 => Some(tilde_key(ks, 15)),
        6 => Some(tilde_key(ks, 17)),
        7 => Some(tilde_key(ks, 18)),
        8 => Some(tilde_key(ks, 19)),
        9 => Some(tilde_key(ks, 20)),
        10 => Some(tilde_key(ks, 21)),
        11 => Some(tilde_key(ks, 23)),
        12 => Some(tilde_key(ks, 24)),
        _ => None,
    }
}

/// Ctrl+字符 的 C0 映射。
fn ctrl_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
        '@' | ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

/// 返回 `None` 表示这次按键不由编码器处理（交给 IME/文本输入路径）。
pub fn encode(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;

    match ks.key.as_str() {
        "enter" => {
            return Some(if mods.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() });
        },
        "backspace" => {
            let byte: u8 = if mods.control { 0x08 } else { 0x7f };
            return Some(if mods.alt { vec![0x1b, byte] } else { vec![byte] });
        },
        "tab" => {
            return Some(if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() });
        },
        "escape" => return Some(b"\x1b".to_vec()),
        "up" => return Some(cursor_key(ks, mode, 'A')),
        "down" => return Some(cursor_key(ks, mode, 'B')),
        "right" => return Some(cursor_key(ks, mode, 'C')),
        "left" => return Some(cursor_key(ks, mode, 'D')),
        "home" => return Some(cursor_key(ks, mode, 'H')),
        "end" => return Some(cursor_key(ks, mode, 'F')),
        "insert" => return Some(tilde_key(ks, 2)),
        "delete" => return Some(tilde_key(ks, 3)),
        "pageup" => return Some(tilde_key(ks, 5)),
        "pagedown" => return Some(tilde_key(ks, 6)),
        key if key.len() == 2 && key.starts_with('f') => {
            if let Ok(n) = key[1..].parse::<u8>() {
                return function_key(ks, n);
            }
        },
        key if key.len() == 3 && key.starts_with('f') => {
            if let Ok(n) = key[1..].parse::<u8>() {
                return function_key(ks, n);
            }
        },
        _ => {},
    }

    // Ctrl 组合：定位到基础字符后映射 C0。
    if mods.control {
        let base = if ks.key.as_str() == "space" {
            Some(' ')
        } else {
            let mut chars = ks.key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        };
        if let Some(byte) = base.and_then(ctrl_char) {
            return Some(if mods.alt { vec![0x1b, byte] } else { vec![byte] });
        }
    }

    // Alt+可打印字符：ESC 前缀（Windows 上 AltGr 会同时置 alt+control，
    // key_char 存在说明平台已判定它产生文本，交给文本路径）。
    if mods.alt && !mods.control {
        if let Some(text) = ks.key_char.as_deref().filter(|t| !t.is_empty()) {
            let mut bytes = vec![0x1b];
            bytes.extend_from_slice(text.as_bytes());
            return Some(bytes);
        }
        let mut chars = ks.key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let mut bytes = vec![0x1b];
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            return Some(bytes);
        }
    }

    if ks.key.as_str() == "space" && !mods.control {
        return Some(b" ".to_vec());
    }

    None
}
