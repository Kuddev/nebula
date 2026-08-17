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

/// ConPTY Win32 input mode 是否当家。
///
/// 子进程请求的 Win32 记录是**回落**协议，不是与 kitty 并列的第二编码器：
/// 一旦子进程要过 kitty 键盘标志，那份标志就是线上合同，压过 DECSET 9001
/// （口径逐字同旧壳 `input::terminal_input::use_win32_input_mode`）。
fn use_win32_input_mode(mode: &TermMode) -> bool {
    mode.contains(TermMode::WIN32_INPUT_MODE) && !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
}

/// 无修饰的字母/数字必须交给 IME / `TranslateMessage`，不能编进 PTY。
///
/// GPUI 的 Windows 后端：`on_key_down` 一旦 `stop_propagation`，就不会再
/// `TranslateMessage`。IME 组字（微软拼音）是 TranslateMessage 喂进去的；
/// 把 `n`/`i` 编成 KEY_EVENT_RECORD 等于把拼音当英文写进 shell，中文永远
/// 起不来。旧壳对应合同是 `keyboard.rs`：`ime.preedit()` 期间直接 return。
#[cfg(windows)]
fn win32_encodes_keystroke(ks: &Keystroke) -> bool {
    if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
        return true;
    }
    let key = ks.key.as_str();
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => false,
        _ => true,
    }
}

/// GPUI 键名 → Win32 虚拟键码。
///
/// 旧壳从 winit fork 的 `RawKeyEventInfo` 直接拿到系统报的 VK；GPUI 的
/// `Keystroke` 只有键名，所以这里按名字反查。表只覆盖**编码器会处理的键**
/// （控制键、方向、功能键）——可打印字符在 GPUI 走 IME 管道，不经这里。
#[cfg(windows)]
fn virtual_key_of(key: &str) -> Option<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT,
        VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
    };

    let vk = match key {
        "escape" => VK_ESCAPE,
        "enter" => VK_RETURN,
        "tab" => VK_TAB,
        "backspace" => VK_BACK,
        "space" => VK_SPACE,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "insert" => VK_INSERT,
        "delete" => VK_DELETE,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        // F1..F24 在 VK 表里连号。
        key if key.starts_with('f') => {
            let index: u16 = key[1..].parse().ok()?;
            if !(1..=24).contains(&index) {
                return None;
            }
            VK_F1 + index - 1
        },
        // 单个字符（Ctrl+C 一族）：ASCII 字母数字的 VK 就是它的大写码点。
        key => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            if !c.is_ascii_alphanumeric() {
                return None;
            }
            c.to_ascii_uppercase() as u16
        },
    };
    Some(vk)
}

/// 控制键必须携带真实 `KEY_EVENT_RECORD` 的字符值（Esc=0x1B、Enter=0x0D、
/// Tab=0x09、Backspace=0x08）：OpenConsole 1.22 的 VT 翻译层会丢弃 uChar=0
/// 的 VK_ESCAPE，于是读字节流的那类应用（Claude Code）收不到 Esc。修饰键与
/// 功能键保持 0，与真实键盘一致。逐条同旧壳 `control_char_fallback`。
#[cfg(windows)]
fn unicode_char_of(ks: &Keystroke) -> u16 {
    // 平台已经判出文本的（含 Ctrl 变体）以它为准，与 WM_CHAR 语义一致。
    // `key_char` 若是 NUL，当作没文本：真实键盘的 Esc 不会写出 U+0000。
    if let Some(text) = ks.key_char.as_deref() {
        let mut units = text.encode_utf16();
        if let (Some(first), None) = (units.next(), units.next()) {
            if first != 0 {
                return first;
            }
        }
    }
    match ks.key.as_str() {
        "escape" => 0x1b,
        "enter" => b'\r' as u16,
        "tab" => b'\t' as u16,
        "backspace" => 0x08,
        "space" => b' ' as u16,
        _ => 0,
    }
}

/// 一条 ConPTY Win32 input 记录：`CSI Vk;Sc;Uc;Kd;Cs;Rc_`。
///
/// 认不出 VK 的键返回 `None`，调用方回落到传统 VT 编码——宁可少一条记录，
/// 也不要编一个 Vk=0 的假记录，那会让子进程读到一个不存在的键。
#[cfg(windows)]
fn win32_input_record(ks: &Keystroke, key_down: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};

    const SHIFT_PRESSED: u32 = 0x0010;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;

    let vk = virtual_key_of(ks.key.as_str())?;
    // 扫描码问系统要，不硬编码：非 US 布局与笔记本键盘上这张表并不通用。
    let scan_code = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) };
    let mut control_key_state = 0u32;
    if ks.modifiers.shift {
        control_key_state |= SHIFT_PRESSED;
    }
    if ks.modifiers.alt {
        control_key_state |= LEFT_ALT_PRESSED;
    }
    if ks.modifiers.control {
        control_key_state |= LEFT_CTRL_PRESSED;
    }
    let key_down = u8::from(key_down);
    Some(
        format!(
            "\x1b[{};{};{};{key_down};{};1_",
            vk,
            scan_code,
            unicode_char_of(ks),
            control_key_state
        )
        .into_bytes(),
    )
}

/// GPUI 的终端视图只接到 key-down。旧壳对 9001 会再写一条 Kd=0 的抬起
/// （`keyboard.rs` 的 `key_release` + `escape_carries_its_control_character_both_directions`）。
/// 真实键盘也是 down+up：Codex 按 VK 看按下就够了，Claude Code / Ink 吃的是
/// OpenConsole 翻译出的字节流，缺抬起时 Esc 常常一个字节都到不了。
#[cfg(windows)]
fn win32_press_and_release(ks: &Keystroke) -> Option<Vec<u8>> {
    let mut sequence = win32_input_record(ks, true)?;
    sequence.extend(win32_input_record(ks, false)?);
    Some(sequence)
}

/// 子进程要过 kitty 键盘标志时，Esc 必须是 `CSI 27u`，不是裸 `\x1b`。
/// 口径同旧壳 `kitty_disambiguate_escape_is_csi_27_u`。
fn kitty_escape(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    let kitty = mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES,
    );
    if !kitty || ks.key.as_str() != "escape" {
        return None;
    }
    let param = modifier_param(ks);
    Some(if param == 1 { b"\x1b[27u".to_vec() } else { format!("\x1b[27;{param}u").into_bytes() })
}

/// 返回 `None` 表示这次按键不由编码器处理（交给 IME/文本输入路径）。
pub fn encode(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;

    // ConPTY 是带 Win32 input mode 标志创建的（见 `tty::windows::conpty`），
    // 子进程一发 DECSET 9001 就切到这份记录格式。旧壳在 `build_sequence`
    // 开头做同样的分流；GPUI 此前一直只发传统 VT，于是裸 `\x1b` 要由
    // OpenConsole 的翻译层反译成 KEY_EVENT_RECORD，而它会丢掉 uChar=0 的
    // VK_ESCAPE——读字节流的应用（Claude Code）因此收不到 Esc。
    #[cfg(windows)]
    if use_win32_input_mode(mode) && win32_encodes_keystroke(ks) {
        if let Some(record) = win32_press_and_release(ks) {
            return Some(record);
        }
    }

    if let Some(bytes) = kitty_escape(ks, mode) {
        return Some(bytes);
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str) -> Keystroke {
        Keystroke { modifiers: gpui::Modifiers::default(), key: key.to_owned(), key_char: None }
    }

    /// 传统 VT 路径（子进程没要过 DECSET 9001）：Esc 就是裸 `\x1b`。
    #[test]
    fn escape_stays_a_bare_byte_on_the_legacy_vt_path() {
        let mode = TermMode::default();
        assert_eq!(encode(&keystroke("escape"), &mode), Some(b"\x1b".to_vec()));
    }

    /// Win32 input mode 下 Esc 必须走 `CSI Vk;Sc;Uc;Kd;Cs;Rc_`，且 Uc 是
    /// 真实的 0x1B——填 0 会被 OpenConsole 1.22 的翻译层丢掉，读字节流的
    /// 应用（Claude Code）就永远收不到 Esc。GPUI 没有 key-up 通道，所以
    /// 一次按键要成对写出 down+up，形状对齐旧壳真实键盘。
    #[cfg(windows)]
    #[test]
    fn escape_becomes_a_win32_record_carrying_its_real_char() {
        let bytes = encode(&keystroke("escape"), &TermMode::WIN32_INPUT_MODE)
            .expect("win32 模式下 Esc 必须编码");
        let text = String::from_utf8(bytes).expect("记录是 ASCII");
        let records: Vec<&str> = text.split_inclusive('_').filter(|s| !s.is_empty()).collect();
        assert_eq!(records.len(), 2, "一次 Esc 必须是 down+up 两条记录：{text:?}");

        let parse = |record: &str| -> Vec<String> {
            let body =
                record.strip_prefix("\x1b[").and_then(|s| s.strip_suffix('_')).expect("CSI … _");
            body.split(';').map(str::to_owned).collect()
        };
        let down = parse(records[0]);
        let up = parse(records[1]);
        assert_eq!(down.len(), 6, "字段数固定为 Vk;Sc;Uc;Kd;Cs;Rc：{text:?}");
        assert_eq!(down[0], "27", "VK_ESCAPE");
        // 扫描码问系统要（布局相关），只断言它不是 0——0 意味着查询失败。
        assert_ne!(down[1], "0", "扫描码不能为 0：{text:?}");
        assert_eq!(down[2], "27", "uChar 必须是真实的 0x1B，不是 0");
        assert_eq!(down[3], "1", "按下");
        assert_eq!(down[4], "0", "无修饰");
        assert_eq!(down[5], "1", "重复次数");
        assert_eq!(&up[..3], &down[..3], "抬起与按下的 Vk/Sc/Uc 必须一致");
        assert_eq!(up[3], "0", "抬起");
        assert_eq!(up[4], down[4]);
        assert_eq!(up[5], "1");
    }

    /// 无修饰字母必须交给 IME：Win32 模式也不能把拼音编进 PTY，否则 GPUI
    /// 当按键已处理、不再 TranslateMessage，中文组字起不来。
    #[cfg(windows)]
    #[test]
    fn unmodified_letters_stay_on_the_ime_path_in_win32_mode() {
        let mode = TermMode::WIN32_INPUT_MODE;
        assert_eq!(encode(&keystroke("n"), &mode), None);
        assert_eq!(encode(&keystroke("a"), &mode), None);
        assert_eq!(encode(&keystroke("1"), &mode), None);
        // Ctrl+C 仍走记录，不能为了 IME 把快捷键也放掉。
        let mut ctrl_c = keystroke("c");
        ctrl_c.modifiers.control = true;
        assert!(encode(&ctrl_c, &mode).is_some());
        assert!(encode(&keystroke("escape"), &mode).is_some());
    }

    /// kitty 键盘标志是子进程明确要过的线上合同，压过 DECSET 9001
    /// （口径同旧壳 `use_win32_input_mode` / `kitty_disambiguate_escape_is_csi_27_u`）。
    #[test]
    fn kitty_flags_take_precedence_over_win32_records() {
        let mode = TermMode::WIN32_INPUT_MODE | TermMode::DISAMBIGUATE_ESC_CODES;
        assert!(!use_win32_input_mode(&mode));
        assert_eq!(encode(&keystroke("escape"), &mode), Some(b"\x1b[27u".to_vec()));
    }

    /// 认不出 VK 的键回落传统 VT，而不是编一条 Vk=0 的假记录。
    #[cfg(windows)]
    #[test]
    fn unknown_keys_fall_back_instead_of_forging_a_record() {
        assert_eq!(virtual_key_of("f99"), None);
        assert_eq!(virtual_key_of("capslock"), None);
        // 方向键在 win32 模式下仍要出记录（它们有确定的 VK）。
        assert!(win32_input_record(&keystroke("up"), true).is_some());
    }
}
