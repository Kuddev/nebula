//! GPUI `Keystroke` → VT 字节序列编码。
//!
//! 只编码控制键与带修饰组合；普通可打印字符（含中文 IME 提交文本）走
//! `EntityInputHandler::replace_text_in_range`，避免同一按键被编码两次。

#[cfg(windows)]
mod win32;

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
    crate::input::terminal_input::use_win32_input_mode(*mode)
}

/// 子进程是否请求过 kitty 键盘协议（三位标志任一）。kitty 是线上合同，
/// 压过 DECSET 9001——口径同旧壳 `input/terminal_input.rs`。
fn kitty_keyboard_active(mode: &TermMode) -> bool {
    mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES,
    )
}

/// 子进程要过 kitty 键盘标志时，Esc 必须是 `CSI 27u`，不是裸 `\x1b`。
/// 口径同旧壳 `kitty_disambiguate_escape_is_csi_27_u`。
fn kitty_escape(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    if !kitty_keyboard_active(mode) || ks.key.as_str() != "escape" {
        return None;
    }
    let param = modifier_param(ks);
    Some(if param == 1 { b"\x1b[27u".to_vec() } else { format!("\x1b[27;{param}u").into_bytes() })
}

/// 修饰回车由对端编辑器解释，不能当成普通 shell 提交。传统 VT 无法区分
/// Shift/Ctrl+Enter，仍沿用回车提交；Win32 和 kitty 则保留其修饰信息。
pub(super) fn preserves_enter_modifiers(ks: &Keystroke, mode: &TermMode) -> bool {
    let mods = &ks.modifiers;
    let modified = mods.shift || mods.control || mods.alt;
    ks.key == "enter"
        && ((kitty_keyboard_active(mode) && (modified || mods.platform))
            || (cfg!(windows) && use_win32_input_mode(mode) && modified))
}

/// Opt-in diagnostics for Enter only; printable input and prompt text are never logged.
pub(super) fn trace_enter(ks: &Keystroke, mode: &TermMode, bytes: &[u8]) {
    if ks.key != "enter" {
        return;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("NEBULA_TRACE_ENTER").is_ok_and(|value| value == "1"))
    {
        return;
    }
    let mods = &ks.modifiers;
    log::info!(
        target: "nebula::input",
        "Enter shift={} ctrl={} alt={} super={} mode={mode:?} bytes={bytes:02x?}",
        mods.shift, mods.control, mods.alt, mods.platform,
    );
}

fn kitty_sequence(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    use crate::input::terminal_input::{KeyInput, build_sequence};
    use winit::event::ElementState;
    use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};

    let (logical_key, key_without_modifiers) = if ks.key == "enter" {
        // 只有编码全部按键时，裸 Enter 才改用 CSI u。其余 kitty 模式下
        // 保留 CR；带修饰的 Enter 复用共享编码器，覆盖 Shift/Ctrl/Alt。
        if !kitty_keyboard_active(mode)
            || !(preserves_enter_modifiers(ks, mode)
                || mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC))
        {
            return None;
        }
        (Key::Named(NamedKey::Enter), Key::Named(NamedKey::Enter))
    } else {
        if !mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC)
            || !(ks.modifiers.control || ks.modifiers.alt)
        {
            return None;
        }
        let base = if ks.key == "space" {
            ' '
        } else {
            let mut characters = ks.key.chars();
            match (characters.next(), characters.next()) {
                (Some(character), None) if character.is_ascii_graphic() => {
                    character.to_ascii_lowercase()
                },
                _ => return None,
            }
        };
        let character = if ks.modifiers.shift {
            ks.key_char
                .as_deref()
                .filter(|text| text.len() == 1 && text.as_bytes()[0].is_ascii_graphic())
                .and_then(|text| text.chars().next())
                .unwrap_or_else(|| base.to_ascii_uppercase())
        } else {
            base
        };
        (Key::Character(character.to_string().into()), Key::Character(base.to_string().into()))
    };
    let input = KeyInput {
        logical_key,
        state: ElementState::Pressed,
        location: KeyLocation::Standard,
        repeat: false,
        key_without_modifiers,
        text_with_all_modifiers: None,
        #[cfg(windows)]
        raw: winit::platform::windows::RawKeyEventInfo {
            virtual_key: 0,
            scan_code: 0,
            repeat_count: 1,
            is_extended: false,
            unicode_char: 0,
            control_key_state: 0,
        },
    };
    let mut modifiers = ModifiersState::empty();
    modifiers.set(ModifiersState::SHIFT, ks.modifiers.shift);
    modifiers.set(ModifiersState::ALT, ks.modifiers.alt);
    modifiers.set(ModifiersState::CONTROL, ks.modifiers.control);
    modifiers.set(ModifiersState::SUPER, ks.modifiers.platform);
    Some(build_sequence(&input, modifiers, *mode))
}

/// 返回 `None` 表示这次按键不由编码器处理（交给 IME/文本输入路径）。
pub fn encode(ks: &Keystroke, mode: &TermMode) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;

    #[cfg(windows)]
    if mods.control
        && mods.alt
        && ks
            .key_char
            .as_deref()
            .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control))
    {
        return None;
    }

    // ConPTY 是带 Win32 input mode 标志创建的（见 `tty::windows::conpty`），
    // 子进程一发 DECSET 9001 就切到这份记录格式。旧壳在 `build_sequence`
    // 开头做同样的分流；GPUI 此前一直只发传统 VT，于是裸 `\x1b` 要由
    // OpenConsole 的翻译层反译成 KEY_EVENT_RECORD，而它会丢掉 uChar=0 的
    // VK_ESCAPE——读字节流的应用（Claude Code）因此收不到 Esc。
    #[cfg(windows)]
    if use_win32_input_mode(mode) && win32::win32_encodes_keystroke(ks) {
        if let Some(record) = win32::win32_press_and_release(ks) {
            return Some(record);
        }
    }

    if let Some(bytes) = kitty_escape(ks, mode) {
        return Some(bytes);
    }
    if let Some(bytes) = kitty_sequence(ks, mode) {
        return Some(bytes);
    }

    match ks.key.as_str() {
        "enter" => {
            return Some(if mods.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() });
        },
        "backspace" => {
            // kitty 合同下带 Ctrl 的 Backspace 走 CSI u（127 = kitty 的 Backspace
            // keysym）：pi 等 kitty 应用以此区分 ctrl+backspace 与普通退格；
            // 裸 \x08 会被当成单字符退格，删不了词。
            if kitty_keyboard_active(mode) && mods.control {
                let param = modifier_param(ks);
                return Some(format!("\x1b[127;{param}u").into_bytes());
            }
            // 传统 VT 路径：Ctrl+Backspace 出 \x17（Ctrl+W）；托管 PowerShell
            // 把 Ctrl+w 绑成了 BackwardKillWord（tty/windows/mod.rs）。
            let byte: u8 = if mods.control { 0x17 } else { 0x7f };
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
        assert_eq!(encode(&keystroke("space"), &mode), None);
        // Ctrl+C 仍走记录，不能为了 IME 把快捷键也放掉。
        let mut ctrl_c = keystroke("c");
        ctrl_c.modifiers.control = true;
        assert!(encode(&ctrl_c, &mode).is_some());
        assert!(encode(&keystroke("escape"), &mode).is_some());
    }

    /// 普通空格与字母使用同一文本输入路径：英语布局最终提交 `" "`，IME
    /// 则先消费它完成候选词选择。keydown 抢先编码会让搜狗直接漏出空格。
    #[test]
    fn unmodified_space_stays_on_the_text_input_path() {
        assert_eq!(encode(&keystroke("space"), &TermMode::default()), None);
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
        assert_eq!(win32::virtual_key_of("f99"), None);
        assert_eq!(win32::virtual_key_of("capslock"), None);
        // 方向键在 win32 模式下仍要出记录（它们有确定的 VK）。
        assert!(win32::win32_input_record(&keystroke("up"), true).is_some());
    }

    /// 9001 记录路径与真实控制台同构：Ctrl+Backspace 的记录 Uc=DEL（0x7f），
    /// 与旧壳 winit 期一致（`ctrl_space_and_ctrl_backspace_report_real_key_event_chars`）。
    /// PSReadLine 原生绑定 Ctrl+Backspace=BackwardKillWord 按 0x7f 命中。
    #[cfg(windows)]
    #[test]
    fn ctrl_backspace_record_carries_del_like_a_real_console() {
        let mut ks = keystroke("backspace");
        ks.modifiers.control = true;
        assert_eq!(
            win32::win32_press_and_release(&ks),
            Some(b"\x1b[8;14;127;1;8;1_\x1b[8;14;127;0;8;1_".to_vec())
        );
    }

    /// kitty 键盘协议下 Ctrl+Backspace 必须是 CSI u（127 是 kitty 的
    /// Backspace keysym，修饰参数 5=Ctrl）。pi 靠它识别 ctrl+backspace；
    /// 发裸 `\x08` 会被当成普通退格，只删一个字符。
    #[test]
    fn kitty_ctrl_backspace_is_csi_u() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        let mut ks = keystroke("backspace");
        ks.modifiers.control = true;
        assert_eq!(encode(&ks, &mode), Some(b"\x1b[127;5u".to_vec()));
    }

    /// kitty 的修饰参数约定（shift=1、alt=2、ctrl=4 从 1 起算）在
    /// backspace 上同样成立。
    #[test]
    fn kitty_ctrl_alt_backspace_reports_both_modifiers() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        let mut ks = keystroke("backspace");
        ks.modifiers.control = true;
        ks.modifiers.alt = true;
        assert_eq!(encode(&ks, &mode), Some(b"\x1b[127;7u".to_vec()));
    }

    /// 传统 VT 路径 Ctrl+Backspace 出 `\x17`（Ctrl+W），托管 PowerShell
    /// 把它绑成 BackwardKillWord；kitty 未开时不走 CSI u。
    #[test]
    fn legacy_ctrl_backspace_emits_ctrl_w() {
        let mode = TermMode::default();
        let mut ks = keystroke("backspace");
        ks.modifiers.control = true;
        assert_eq!(encode(&ks, &mode), Some(b"\x17".to_vec()));
    }

    /// 普通退格语义不变：两种路径都还是单个删除字节。
    #[test]
    fn plain_backspace_stays_a_single_delete_byte() {
        let mode = TermMode::default();
        assert_eq!(encode(&keystroke("backspace"), &mode), Some(b"\x7f".to_vec()));
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(encode(&keystroke("backspace"), &kitty), Some(b"\x7f".to_vec()));
    }

    #[test]
    fn legacy_enter_chords_keep_the_existing_cr_encoding() {
        for mode in
            [TermMode::default(), TermMode::REPORT_ALTERNATE_KEYS, TermMode::REPORT_ASSOCIATED_TEXT]
        {
            for (shift, control, alt) in [
                (false, false, false),
                (true, false, false),
                (false, true, false),
                (false, false, true),
                (true, true, true),
            ] {
                let mut key = keystroke("enter");
                key.modifiers.shift = shift;
                key.modifiers.control = control;
                key.modifiers.alt = alt;
                let expected: &[u8] = if alt { b"\x1b\r" } else { b"\r" };
                assert_eq!(encode(&key, &mode).as_deref(), Some(expected));
                assert!(!preserves_enter_modifiers(&key, &mode));
            }
        }
    }

    #[test]
    fn kitty_enter_preserves_each_modifier_and_bare_enter_compatibility() {
        for kitty in [
            TermMode::DISAMBIGUATE_ESC_CODES,
            TermMode::REPORT_EVENT_TYPES,
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            pi_keyboard_mode(),
        ] {
            for mode in [kitty, kitty | TermMode::WIN32_INPUT_MODE] {
                for (shift, control, alt, platform, parameter) in [
                    (false, false, false, false, 1),
                    (true, false, false, false, 2),
                    (false, false, true, false, 3),
                    (true, false, true, false, 4),
                    (false, true, false, false, 5),
                    (true, true, false, false, 6),
                    (false, true, true, false, 7),
                    (true, true, true, false, 8),
                    (false, false, false, true, 9),
                ] {
                    let mut key = keystroke("enter");
                    key.modifiers.shift = shift;
                    key.modifiers.control = control;
                    key.modifiers.alt = alt;
                    key.modifiers.platform = platform;
                    // Even an OS-provided CR/LF cannot erase kitty's key identity.
                    for text in [None, Some("\r"), Some("\n")] {
                        key.key_char = text.map(str::to_owned);
                        let expected = if parameter != 1 {
                            format!("\x1b[13;{parameter}u").into_bytes()
                        } else if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
                            b"\x1b[13u".to_vec()
                        } else {
                            b"\r".to_vec()
                        };
                        assert_eq!(encode(&key, &mode), Some(expected), "{key:?} {mode:?}");
                        assert_eq!(preserves_enter_modifiers(&key, &mode), parameter != 1);
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn win32_enter_keeps_native_characters_and_modifier_bits_in_both_records() {
        let mode = TermMode::WIN32_INPUT_MODE;
        for (shift, control, alt, text, character, flags) in [
            (false, false, false, None, 13, 0),
            (true, false, false, None, 13, 16),
            (true, false, false, Some("\r"), 13, 16),
            (false, true, false, None, 10, 8),
            (false, true, false, Some("\n"), 10, 8),
            (false, false, true, Some("\r"), 13, 2),
            (true, true, false, Some("\n"), 10, 24),
        ] {
            let mut key = keystroke("enter");
            key.modifiers.shift = shift;
            key.modifiers.control = control;
            key.modifiers.alt = alt;
            key.key_char = text.map(str::to_owned);
            let encoded = String::from_utf8(encode(&key, &mode).unwrap()).unwrap();
            let records: Vec<Vec<u16>> = encoded
                .split_inclusive('_')
                .map(|record| {
                    record
                        .strip_prefix("\x1b[")
                        .unwrap()
                        .strip_suffix('_')
                        .unwrap()
                        .split(';')
                        .map(|field| field.parse().unwrap())
                        .collect()
                })
                .collect();
            assert_eq!(records.len(), 2, "{encoded:?}");
            for (index, record) in records.iter().enumerate() {
                assert_eq!(record.len(), 6);
                assert_eq!(record[0], 13, "VK_RETURN");
                assert_ne!(record[1], 0, "native scan code");
                assert_eq!(record[2], character, "keep WM_CHAR and the CR fallback");
                assert_eq!(record[3], u16::from(index == 0));
                assert_eq!(record[4], flags, "Shift/Ctrl/Alt are carried in Cs");
                assert_eq!(record[5], 1);
            }
            assert_eq!(preserves_enter_modifiers(&key, &mode), flags != 0);
        }
    }

    #[test]
    fn only_enter_uses_modified_enter_tracking() {
        for name in ["v", "tab", "escape", "backspace"] {
            let mut key = keystroke(name);
            key.modifiers.shift = true;
            assert!(!preserves_enter_modifiers(&key, &pi_keyboard_mode()));
        }
    }

    #[test]
    fn ctrl_j_keeps_its_newline_identity_across_protocol_negotiation() {
        let mut key = keystroke("j");
        key.modifiers.control = true;
        assert_eq!(encode(&key, &TermMode::default()), Some(b"\n".to_vec()));
        assert!(!preserves_enter_modifiers(&key, &TermMode::default()));
        for mode in [pi_keyboard_mode(), pi_keyboard_mode() | TermMode::WIN32_INPUT_MODE] {
            assert_eq!(encode(&key, &mode), Some(b"\x1b[106;5u".to_vec()));
            assert!(!preserves_enter_modifiers(&key, &mode));
        }
    }

    fn pi_keyboard_mode() -> TermMode {
        TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES
            | TermMode::REPORT_ALTERNATE_KEYS
    }

    #[test]
    fn kitty_alt_v_uses_csi_u_with_or_without_platform_text() {
        let mut key = keystroke("v");
        key.modifiers.alt = true;
        for mode in [
            TermMode::DISAMBIGUATE_ESC_CODES,
            pi_keyboard_mode(),
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            pi_keyboard_mode() | TermMode::WIN32_INPUT_MODE,
        ] {
            for text in [None, Some("v")] {
                key.key_char = text.map(str::to_owned);
                assert_eq!(encode(&key, &mode), Some(b"\x1b[118;3u".to_vec()));
            }
        }
    }

    #[test]
    fn kitty_ascii_chords_preserve_control_alt_and_shift() {
        for (control, alt, shift, parameter) in [
            (false, true, false, 3),
            (true, false, false, 5),
            (true, true, false, 7),
            (false, true, true, 4),
            (true, false, true, 6),
            (true, true, true, 8),
        ] {
            let mut key = keystroke("v");
            key.modifiers.control = control;
            key.modifiers.alt = alt;
            key.modifiers.shift = shift;
            assert_eq!(
                encode(&key, &TermMode::DISAMBIGUATE_ESC_CODES),
                Some(format!("\x1b[118;{parameter}u").into_bytes())
            );
            let base = if shift { "118:86" } else { "118" };
            assert_eq!(
                encode(&key, &pi_keyboard_mode()),
                Some(format!("\x1b[{base};{parameter}u").into_bytes())
            );
        }
    }

    #[test]
    fn kitty_shifted_ascii_uses_the_unshifted_codepoint_and_reported_alternate() {
        let mut key = keystroke("1");
        key.modifiers.control = true;
        key.modifiers.shift = true;
        key.key_char = Some("!".to_owned());
        assert_eq!(encode(&key, &TermMode::DISAMBIGUATE_ESC_CODES), Some(b"\x1b[49;6u".to_vec()));
        assert_eq!(encode(&key, &pi_keyboard_mode()), Some(b"\x1b[49:33;6u".to_vec()));
    }

    #[test]
    fn kitty_modified_space_and_ascii_punctuation_use_csi_u() {
        for (name, codepoint) in [("space", 32), ("[", 91), ("/", 47)] {
            let mut key = keystroke(name);
            key.modifiers.alt = true;
            assert_eq!(
                encode(&key, &pi_keyboard_mode()),
                Some(format!("\x1b[{codepoint};3u").into_bytes())
            );
        }
    }

    #[test]
    fn alt_v_keeps_legacy_encoding_without_disambiguation() {
        let mut key = keystroke("v");
        key.modifiers.alt = true;
        for mode in
            [TermMode::default(), TermMode::REPORT_ALTERNATE_KEYS, TermMode::REPORT_EVENT_TYPES]
        {
            assert_eq!(encode(&key, &mode), Some(b"\x1bv".to_vec()));
        }
        key.modifiers.control = true;
        assert_eq!(encode(&key, &TermMode::default()), Some(b"\x1b\x16".to_vec()));
    }

    #[test]
    fn pi_flags_leave_plain_and_shifted_text_on_the_ime_path() {
        assert!(!pi_keyboard_mode().contains(TermMode::REPORT_ALL_KEYS_AS_ESC));
        for name in ["v", "1", "space", "\u{4e2d}"] {
            let mut key = keystroke(name);
            assert_eq!(encode(&key, &pi_keyboard_mode()), None);
            key.modifiers.shift = true;
            assert_eq!(encode(&key, &pi_keyboard_mode()), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn altgr_text_is_not_encoded_as_a_control_alt_shortcut() {
        let mut key = keystroke("q");
        key.modifiers.control = true;
        key.modifiers.alt = true;
        for text in ["@", "\u{20ac}"] {
            key.key_char = Some(text.to_owned());
            for mode in [
                TermMode::default(),
                TermMode::WIN32_INPUT_MODE,
                pi_keyboard_mode(),
                pi_keyboard_mode() | TermMode::WIN32_INPUT_MODE,
            ] {
                assert_eq!(encode(&key, &mode), None);
            }
        }
    }

    #[derive(Clone, Default)]
    struct KeyboardReplyRecorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl nebula_terminal::event::EventListener for KeyboardReplyRecorder {
        fn send_event(&self, event: nebula_terminal::event::Event) {
            if let nebula_terminal::event::Event::PtyWrite(reply) = event {
                self.0.borrow_mut().push(reply);
            }
        }
    }

    #[test]
    fn antigravity_startup_queries_applied_flags_and_preserves_newline_chords() {
        use nebula_terminal::term::Config;
        use nebula_terminal::vte::ansi::Processor;

        let size = super::super::session::GridSize { columns: 80, screen_lines: 24 };
        let recorder = KeyboardReplyRecorder::default();
        let mut term = nebula_terminal::Term::new(
            Config { kitty_keyboard: true, ..Config::default() },
            &size,
            recorder.clone(),
        );
        let mut parser: Processor = Processor::new();
        // Captured from AGY 1.1.7's outer ConPTY stream before authentication.
        // The host's 9001 mode must not override the later Kitty negotiation.
        parser.advance(&mut term, b"\x1b[?9001h\x1b[=0;1u\x1b[=1;1u\x1b[?u");
        assert!(term.mode().contains(TermMode::WIN32_INPUT_MODE));
        assert_eq!(
            *term.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL,
            TermMode::DISAMBIGUATE_ESC_CODES
        );
        for (name, shift, control, expected) in [
            ("enter", false, false, "\r"),
            ("enter", true, false, "\x1b[13;2u"),
            ("enter", false, true, "\x1b[13;5u"),
            ("j", false, true, "\x1b[106;5u"),
        ] {
            let mut key = keystroke(name);
            key.modifiers.shift = shift;
            key.modifiers.control = control;
            assert_eq!(encode(&key, term.mode()), Some(expected.as_bytes().to_vec()));
        }

        // A later reset must restore the legacy path without a stale reply.
        parser.advance(&mut term, b"\x1b[=0;1u\x1b[?u\x1b[?9001l");
        assert_eq!(recorder.0.borrow().as_slice(), ["\x1b[?1u", "\x1b[?0u"]);
        assert_eq!(encode(&keystroke("enter"), term.mode()), Some(b"\r".to_vec()));
        let mut ctrl_j = keystroke("j");
        ctrl_j.modifiers.control = true;
        assert_eq!(encode(&ctrl_j, term.mode()), Some(b"\n".to_vec()));
    }

    #[test]
    fn codex_keyboard_negotiation_preserves_enter_chords_until_reset() {
        use nebula_terminal::term::Config;
        use nebula_terminal::vte::ansi::Processor;

        let size = super::super::session::GridSize { columns: 80, screen_lines: 24 };
        let recorder = KeyboardReplyRecorder::default();
        let mut term = nebula_terminal::Term::new(
            Config { kitty_keyboard: true, ..Config::default() },
            &size,
            recorder.clone(),
        );
        let mut parser: Processor = Processor::new();
        let mut shift_enter = keystroke("enter");
        shift_enter.modifiers.shift = true;
        let mut ctrl_enter = keystroke("enter");
        ctrl_enter.modifiers.control = true;

        assert_eq!(encode(&shift_enter, term.mode()), Some(b"\r".to_vec()));
        // Codex 0.153.4 requests disambiguation, event types and alternate keys.
        parser.advance(&mut term, b"\x1b[>7u\x1b[?u");
        assert_eq!(*term.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL, pi_keyboard_mode());
        assert_eq!(encode(&shift_enter, term.mode()), Some(b"\x1b[13;2u".to_vec()));
        assert_eq!(encode(&ctrl_enter, term.mode()), Some(b"\x1b[13;5u".to_vec()));
        assert_eq!(encode(&keystroke("enter"), term.mode()), Some(b"\r".to_vec()));

        parser.advance(&mut term, b"\x1b[<u\x1b[?u");
        assert_eq!(encode(&shift_enter, term.mode()), Some(b"\r".to_vec()));
        assert!(!preserves_enter_modifiers(&shift_enter, term.mode()));
        parser.advance(&mut term, b"\x1b[>7u\x1b[=0u\x1b[?u");
        assert_eq!(encode(&ctrl_enter, term.mode()), Some(b"\r".to_vec()));
        assert_eq!(recorder.0.borrow().as_slice(), ["\x1b[?7u", "\x1b[?0u", "\x1b[?0u"]);
    }

    #[test]
    fn pi_keyboard_negotiation_controls_alt_v_and_pop_restores_legacy() {
        use nebula_terminal::term::Config;
        use nebula_terminal::vte::ansi::Processor;

        let recorder = KeyboardReplyRecorder::default();
        let size = super::super::session::GridSize { columns: 80, screen_lines: 24 };
        let mut term = nebula_terminal::Term::new(
            Config { kitty_keyboard: true, ..Config::default() },
            &size,
            recorder.clone(),
        );
        let mut parser: Processor = Processor::new();
        let mut key = keystroke("v");
        key.modifiers.alt = true;

        parser.advance(&mut term, b"\x1b[?u");
        assert_eq!(encode(&key, term.mode()), Some(b"\x1bv".to_vec()));
        parser.advance(&mut term, b"\x1b[>7u\x1b[?u");
        assert_eq!(*term.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL, pi_keyboard_mode());
        assert_eq!(encode(&key, term.mode()), Some(b"\x1b[118;3u".to_vec()));
        parser.advance(&mut term, b"\x1b[>0u\x1b[?u");
        assert_eq!(encode(&key, term.mode()), Some(b"\x1bv".to_vec()));
        parser.advance(&mut term, b"\x1b[<u\x1b[?u");
        assert_eq!(encode(&key, term.mode()), Some(b"\x1b[118;3u".to_vec()));
        parser.advance(&mut term, b"\x1b[<u\x1b[?u");
        assert_eq!(encode(&key, term.mode()), Some(b"\x1bv".to_vec()));
        assert_eq!(
            recorder.0.borrow().as_slice(),
            ["\x1b[?0u", "\x1b[?7u", "\x1b[?0u", "\x1b[?7u", "\x1b[?0u"]
        );
    }
}
