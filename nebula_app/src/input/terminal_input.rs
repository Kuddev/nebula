//! Platform input encoders kept outside the general keyboard dispatcher.
//!
//! The dispatcher decides whether an event belongs to the terminal or to the
//! Nebula chrome. This module owns the wire format for platform terminal modes;
//! keeping that contract isolated makes adding a Linux or macOS raw backend a
//! local change instead of another branch in `keyboard.rs`.
//!
//! Encoders consume [`KeyInput`] — Nebula's keyboard-facts contract
//! (docs/future_planning.md「长期方案裁定」第 2 层) — never a winit event
//! directly. Winit events cannot be constructed in tests, so every modifier
//! chord would otherwise be unverifiable; with the contract in between, the
//! encoder is a pure function over values a test can spell out from recorded
//! real-keyboard facts.

use std::borrow::Cow;
#[cfg(target_os = "windows")]
use std::fmt::Write;

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, SmolStr};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
#[cfg(target_os = "windows")]
use winit::platform::windows::{KeyEventExtWindows, RawKeyEventInfo};

use nebula_terminal::term::TermMode;

use crate::runtime_api::{RuntimeKey, RuntimeKeyModifiers};

/// Keyboard facts for one key transition, captured once from the platform
/// event and consumed by every encoder.
///
/// Field values mirror what the OS actually reported (winit fork's
/// `RawKeyEventInfo` carries the Win32 `KEY_EVENT_RECORD` side). Tests build
/// this struct literally from recorded facts — see the fact table in the
/// tests below; keep new entries sourced from a real capture (VK probe,
/// `scripts/win32_input_matrix.ps1`) rather than guessed.
pub(crate) struct KeyInput {
    pub logical_key: Key,
    pub state: ElementState,
    pub location: KeyLocation,
    pub repeat: bool,
    /// The logical key with every modifier stripped (kitty alternate-key
    /// reporting needs the unshifted base, e.g. `1` for `!`).
    pub key_without_modifiers: Key,
    /// The OS text path with modifiers applied (WM_CHAR / ToUnicode). `None`
    /// on key-up and on keys that produce no text.
    pub text_with_all_modifiers: Option<SmolStr>,
    /// Native Win32 identity captured by the winit fork in one pass.
    #[cfg(target_os = "windows")]
    pub raw: RawKeyEventInfo,
}

impl From<&KeyEvent> for KeyInput {
    fn from(key: &KeyEvent) -> Self {
        Self {
            logical_key: key.logical_key.clone(),
            state: key.state,
            location: key.location,
            repeat: key.repeat,
            key_without_modifiers: key.key_without_modifiers(),
            text_with_all_modifiers: key.text_with_all_modifiers().map(SmolStr::new),
            #[cfg(target_os = "windows")]
            raw: KeyEventExtWindows::raw_key_event(key),
        }
    }
}

/// Build one or more ConPTY Win32 input-mode records for a native key event.
///
/// Dead keys intentionally return `None`; their eventual composition is
/// delivered through Winit's OS-resolved text/IME path. Every other key keeps
/// its native Windows identity. Printable keys additionally carry the UTF-16
/// code units emitted by WM_CHAR, matching KEY_EVENT_RECORD semantics without
/// reconstructing VKEYs from text.
#[inline]
pub(crate) fn build_win32_input_sequence(input: &KeyInput) -> Option<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        if uses_composition_path(&input.logical_key) {
            return None;
        }

        let raw = input.raw;
        let text = input.text_with_all_modifiers.as_deref();
        let mut sequence = String::with_capacity(48);
        match input.logical_key.as_ref() {
            Key::Character(_) | Key::Named(NamedKey::Space) => {
                let mut wrote_text = false;
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    for unicode_char in text.encode_utf16() {
                        append_raw_key_event(&mut sequence, raw, input.state, unicode_char);
                        wrote_text = true;
                    }
                }

                if !wrote_text {
                    append_raw_key_event(&mut sequence, raw, input.state, raw.unicode_char);
                }
            },
            _ => {
                // 控制键必须携带真实 KEY_EVENT_RECORD 的字符值(Esc=0x1B、
                // Enter=0x0D、Tab=0x09、Backspace=0x08,Ctrl 变体以 WM_CHAR
                // 为准):OpenConsole 1.22 的 VT 翻译层会丢弃 uChar=0 的
                // VK_ESCAPE,字节流读者(Claude Code 等)将收不到 Esc。
                // 修饰键与功能键保持 0,与真实键盘一致。
                let unicode_char = text
                    .and_then(single_utf16_code_unit)
                    .unwrap_or_else(|| control_char_fallback(&input.logical_key));
                append_raw_key_event(&mut sequence, raw, input.state, unicode_char);
            },
        }
        return Some(sequence.into_bytes());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = input;
        None
    }
}

#[cfg(target_os = "windows")]
#[inline]
fn uses_composition_path(key: &Key) -> bool {
    matches!(key, Key::Dead(_))
}

#[cfg(target_os = "windows")]
#[inline]
fn single_utf16_code_unit(text: &str) -> Option<u16> {
    let mut units = text.encode_utf16();
    let first = units.next()?;
    units.next().is_none().then_some(first)
}

/// Key-up 事件没有 WM_CHAR 文本,按无修饰基础值补齐,使 up 记录与真实
/// 键盘的 KEY_EVENT_RECORD 形状一致。
#[cfg(target_os = "windows")]
#[inline]
fn control_char_fallback(key: &Key) -> u16 {
    match key {
        Key::Named(NamedKey::Escape) => 0x1b,
        Key::Named(NamedKey::Enter) => b'\r' as u16,
        Key::Named(NamedKey::Tab) => b'\t' as u16,
        Key::Named(NamedKey::Backspace) => 0x08,
        _ => 0,
    }
}

#[cfg(target_os = "windows")]
#[inline]
fn append_raw_key_event(
    sequence: &mut String,
    raw: RawKeyEventInfo,
    state: ElementState,
    unicode_char: u16,
) {
    // ConPTY Win32 input mode 的字段顺序是 CSI Vk;Sc;Uc;Kd;Cs;Rc_。
    let key_down = u8::from(state == ElementState::Pressed);
    let repeat_count = raw.repeat_count.max(1);
    let _ = write!(
        sequence,
        "\x1b[{};{};{};{};{};{}_",
        raw.virtual_key, raw.scan_code, unicode_char, key_down, raw.control_key_state, repeat_count
    );
}

/// Build a key's keyboard escape sequence based on the given `input`, `mods`,
/// and `mode` — the kitty/legacy VT encoder shared by every platform.
///
/// The key sequences for `APP_KEYPAD` and alike are handled inside the bindings.
#[inline(never)]
pub(crate) fn build_sequence(input: &KeyInput, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
    if use_win32_input_mode(mode) {
        if let Some(sequence) = build_win32_input_sequence(input) {
            return sequence;
        }
    }
    let mut modifiers = mods.into();

    let kitty_seq = mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES,
    );

    let kitty_encode_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    // The default parameter is 1, so we can omit it.
    let kitty_event_type = mode.contains(TermMode::REPORT_EVENT_TYPES)
        && (input.repeat || input.state == ElementState::Released);

    let context =
        SequenceBuilder { mode, modifiers, kitty_seq, kitty_encode_all, kitty_event_type };

    let associated_text = input.text_with_all_modifiers.as_deref().filter(|text| {
        mode.contains(TermMode::REPORT_ASSOCIATED_TEXT)
            && input.state != ElementState::Released
            && !text.is_empty()
            && !is_control_character(text)
    });

    let sequence_base = context
        .try_build_numpad(input)
        .or_else(|| context.try_build_named_kitty(input))
        .or_else(|| context.try_build_named_normal(input, associated_text.is_some()))
        .or_else(|| context.try_build_control_char_or_mod(input, &mut modifiers))
        .or_else(|| context.try_build_textual(input, associated_text));

    let (payload, terminator) = match sequence_base {
        Some(SequenceBase { payload, terminator }) => (payload, terminator),
        _ => return Vec::new(),
    };

    let mut payload = format!("\x1b[{payload}");

    // Add modifiers information.
    if kitty_event_type || !modifiers.is_empty() || associated_text.is_some() {
        payload.push_str(&format!(";{}", modifiers.encode_esc_sequence()));
    }

    // Push event type.
    if kitty_event_type {
        payload.push(':');
        let event_type = match input.state {
            _ if input.repeat => '2',
            ElementState::Pressed => '1',
            ElementState::Released => '3',
        };
        payload.push(event_type);
    }

    if let Some(text) = associated_text {
        let mut codepoints = text.chars().map(u32::from);
        if let Some(codepoint) = codepoints.next() {
            payload.push_str(&format!(";{codepoint}"));
        }
        for codepoint in codepoints {
            payload.push_str(&format!(":{codepoint}"));
        }
    }

    payload.push(terminator.encode_esc_sequence());

    payload.into_bytes()
}

/// Encode one API-level named key from normalized facts. This path deliberately
/// excludes printable text and synthesizes a complete press/release pair only
/// for protocols that report key state.
pub(crate) fn build_runtime_sequence(
    key: RuntimeKey,
    modifiers: RuntimeKeyModifiers,
    repeat: u16,
    mode: TermMode,
) -> Vec<u8> {
    let mods = runtime_modifiers(modifiers);
    let mut result = Vec::new();
    for _ in 0..repeat {
        if use_win32_input_mode(mode) {
            #[cfg(target_os = "windows")]
            {
                let pressed = runtime_key_input(key, modifiers, ElementState::Pressed);
                let released = runtime_key_input(key, modifiers, ElementState::Released);
                result.extend(build_sequence(&pressed, mods, mode));
                result.extend(build_sequence(&released, mods, mode));
                continue;
            }
        }

        if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
            let pressed = runtime_key_input(key, modifiers, ElementState::Pressed);
            result.extend(build_sequence(&pressed, mods, mode));
            if mode.contains(TermMode::REPORT_EVENT_TYPES) {
                let released = runtime_key_input(key, modifiers, ElementState::Released);
                result.extend(build_sequence(&released, mods, mode));
            }
            continue;
        }

        result.extend(runtime_legacy_sequence(key, modifiers, mode));
    }
    result
}

fn runtime_modifiers(modifiers: RuntimeKeyModifiers) -> ModifiersState {
    let mut state = ModifiersState::empty();
    state.set(ModifiersState::SHIFT, modifiers.shift);
    state.set(ModifiersState::ALT, modifiers.alt);
    state.set(ModifiersState::CONTROL, modifiers.control);
    state
}

fn runtime_key_input(
    key: RuntimeKey,
    modifiers: RuntimeKeyModifiers,
    state: ElementState,
) -> KeyInput {
    let logical_key = runtime_logical_key(key);
    KeyInput {
        logical_key: logical_key.clone(),
        state,
        location: KeyLocation::Standard,
        repeat: false,
        key_without_modifiers: logical_key,
        text_with_all_modifiers: key.letter().and_then(|letter| {
            modifiers.control.then(|| SmolStr::new(((letter as u8 - b'a' + 1) as char).to_string()))
        }),
        #[cfg(target_os = "windows")]
        raw: runtime_raw_key(key, modifiers),
    }
}

fn runtime_logical_key(key: RuntimeKey) -> Key {
    let named = match key {
        RuntimeKey::Escape => NamedKey::Escape,
        RuntimeKey::Enter => NamedKey::Enter,
        RuntimeKey::Tab => NamedKey::Tab,
        RuntimeKey::Backspace => NamedKey::Backspace,
        RuntimeKey::Up => NamedKey::ArrowUp,
        RuntimeKey::Down => NamedKey::ArrowDown,
        RuntimeKey::Left => NamedKey::ArrowLeft,
        RuntimeKey::Right => NamedKey::ArrowRight,
        RuntimeKey::Home => NamedKey::Home,
        RuntimeKey::End => NamedKey::End,
        RuntimeKey::Insert => NamedKey::Insert,
        RuntimeKey::Delete => NamedKey::Delete,
        RuntimeKey::PageUp => NamedKey::PageUp,
        RuntimeKey::PageDown => NamedKey::PageDown,
        RuntimeKey::F1 => NamedKey::F1,
        RuntimeKey::F2 => NamedKey::F2,
        RuntimeKey::F3 => NamedKey::F3,
        RuntimeKey::F4 => NamedKey::F4,
        RuntimeKey::F5 => NamedKey::F5,
        RuntimeKey::F6 => NamedKey::F6,
        RuntimeKey::F7 => NamedKey::F7,
        RuntimeKey::F8 => NamedKey::F8,
        RuntimeKey::F9 => NamedKey::F9,
        RuntimeKey::F10 => NamedKey::F10,
        RuntimeKey::F11 => NamedKey::F11,
        RuntimeKey::F12 => NamedKey::F12,
        letter => return Key::Character(SmolStr::new(letter.as_str())),
    };
    Key::Named(named)
}

#[cfg(target_os = "windows")]
fn runtime_raw_key(key: RuntimeKey, modifiers: RuntimeKeyModifiers) -> RawKeyEventInfo {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MAPVK_VK_TO_VSC, MapVirtualKeyW, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1,
        VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
    };

    let virtual_key = match key {
        RuntimeKey::Escape => VK_ESCAPE,
        RuntimeKey::Enter => VK_RETURN,
        RuntimeKey::Tab => VK_TAB,
        RuntimeKey::Backspace => VK_BACK,
        RuntimeKey::Up => VK_UP,
        RuntimeKey::Down => VK_DOWN,
        RuntimeKey::Left => VK_LEFT,
        RuntimeKey::Right => VK_RIGHT,
        RuntimeKey::Home => VK_HOME,
        RuntimeKey::End => VK_END,
        RuntimeKey::Insert => VK_INSERT,
        RuntimeKey::Delete => VK_DELETE,
        RuntimeKey::PageUp => VK_PRIOR,
        RuntimeKey::PageDown => VK_NEXT,
        RuntimeKey::F1
        | RuntimeKey::F2
        | RuntimeKey::F3
        | RuntimeKey::F4
        | RuntimeKey::F5
        | RuntimeKey::F6
        | RuntimeKey::F7
        | RuntimeKey::F8
        | RuntimeKey::F9
        | RuntimeKey::F10
        | RuntimeKey::F11
        | RuntimeKey::F12 => VK_F1 + runtime_function_index(key) - 1,
        letter => letter.as_str().as_bytes()[0].to_ascii_uppercase() as u16,
    };
    let scan_code = unsafe { MapVirtualKeyW(u32::from(virtual_key), MAPVK_VK_TO_VSC) } as u8;
    let mut control_key_state = 0;
    if modifiers.shift {
        control_key_state |= 0x0010;
    }
    if modifiers.alt {
        control_key_state |= 0x0002;
    }
    if modifiers.control {
        control_key_state |= 0x0008;
    }
    let unicode_char = match key {
        RuntimeKey::Escape => 0x1b,
        RuntimeKey::Enter => b'\r' as u16,
        RuntimeKey::Tab => b'\t' as u16,
        RuntimeKey::Backspace => 0x08,
        letter if modifiers.control => {
            letter.letter().map_or(0, |value| u16::from(value as u8 - b'a' + 1))
        },
        _ => 0,
    };
    RawKeyEventInfo {
        virtual_key,
        scan_code,
        repeat_count: 1,
        is_extended: false,
        unicode_char,
        control_key_state,
    }
}

fn runtime_function_index(key: RuntimeKey) -> u16 {
    match key {
        RuntimeKey::F1 => 1,
        RuntimeKey::F2 => 2,
        RuntimeKey::F3 => 3,
        RuntimeKey::F4 => 4,
        RuntimeKey::F5 => 5,
        RuntimeKey::F6 => 6,
        RuntimeKey::F7 => 7,
        RuntimeKey::F8 => 8,
        RuntimeKey::F9 => 9,
        RuntimeKey::F10 => 10,
        RuntimeKey::F11 => 11,
        RuntimeKey::F12 => 12,
        _ => 0,
    }
}

fn runtime_legacy_sequence(
    key: RuntimeKey,
    modifiers: RuntimeKeyModifiers,
    mode: TermMode,
) -> Vec<u8> {
    let param = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control);
    let cursor = |letter: char| {
        if param != 1 {
            format!("\x1b[1;{param}{letter}").into_bytes()
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |code: u8| {
        if param == 1 {
            format!("\x1b[{code}~").into_bytes()
        } else {
            format!("\x1b[{code};{param}~").into_bytes()
        }
    };
    match key {
        RuntimeKey::Escape => b"\x1b".to_vec(),
        RuntimeKey::Enter => {
            if modifiers.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        },
        RuntimeKey::Tab => {
            if modifiers.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        },
        RuntimeKey::Backspace => {
            let value = if modifiers.control { 0x08 } else { 0x7f };
            if modifiers.alt { vec![0x1b, value] } else { vec![value] }
        },
        RuntimeKey::Up => cursor('A'),
        RuntimeKey::Down => cursor('B'),
        RuntimeKey::Right => cursor('C'),
        RuntimeKey::Left => cursor('D'),
        RuntimeKey::Home => cursor('H'),
        RuntimeKey::End => cursor('F'),
        RuntimeKey::Insert => tilde(2),
        RuntimeKey::Delete => tilde(3),
        RuntimeKey::PageUp => tilde(5),
        RuntimeKey::PageDown => tilde(6),
        RuntimeKey::F1 | RuntimeKey::F2 | RuntimeKey::F3 | RuntimeKey::F4 => {
            let letter = (b'P' + runtime_function_index(key) as u8 - 1) as char;
            if param == 1 {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[1;{param}{letter}").into_bytes()
            }
        },
        RuntimeKey::F5 => tilde(15),
        RuntimeKey::F6 => tilde(17),
        RuntimeKey::F7 => tilde(18),
        RuntimeKey::F8 => tilde(19),
        RuntimeKey::F9 => tilde(20),
        RuntimeKey::F10 => tilde(21),
        RuntimeKey::F11 => tilde(23),
        RuntimeKey::F12 => tilde(24),
        letter => {
            let value = letter.letter().expect("letter variant") as u8 - b'a' + 1;
            if modifiers.alt { vec![0x1b, value] } else { vec![value] }
        },
    }
}

/// Select protocol precedence for ConPTY's native input mode. Child-requested
/// Win32 records are the fallback, not a competing encoder. Once the child has
/// requested Kitty keyboard flags, those flags describe the wire contract and
/// must take precedence over DECSET 9001.
#[inline]
pub(crate) fn use_win32_input_mode(mode: TermMode) -> bool {
    mode.contains(TermMode::WIN32_INPUT_MODE) && !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
}

/// Synthesize key-up sequences for modifiers still held when the window loses
/// focus. Their real key-ups go to whichever window is focused next, so any
/// protocol stream that reported the key-down (Win32 records, kitty
/// `REPORT_ALL_KEYS_AS_ESC`) would leave the application believing the
/// modifier is held forever — worst case, the first plain `c` typed after an
/// Alt+Tab round-trip is read as Ctrl+C and kills a running task. Windows
/// Terminal synthesizes the same ups on focus loss.
///
/// Only modifiers are synthesized: a stranded ordinary key merely stops
/// repeating, while a stranded modifier rewrites the meaning of every later
/// keystroke. Left-side variants are assumed (`ModifiersState` carries no
/// sidedness); a held right-side modifier under kitty keeps its distinct
/// keysym and is a recorded, accepted deviation.
pub(crate) fn build_focus_loss_key_ups(mods: ModifiersState, mode: TermMode) -> Vec<u8> {
    // Bare modifiers only ever reach the wire through Win32 records or kitty
    // ALL_KEYS, and releases additionally require REPORT_EVENT_TYPES (the
    // same gate `key_release` applies to real events). Anywhere else the
    // down was never sent, so there is nothing to release.
    let kitty_release =
        mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES);
    if !use_win32_input_mode(mode) && !kitty_release {
        return Vec::new();
    }

    // Left-side KEY_EVENT_RECORD facts (VK, scan code, dwControlKeyState
    // bit) — captured values, same sourcing rule as the fact tables below.
    const HELD_ORDER: [(ModifiersState, NamedKey, u16, u16, u32); 4] = [
        (ModifiersState::SHIFT, NamedKey::Shift, 0x10, 0x2a, 0x10),
        (ModifiersState::CONTROL, NamedKey::Control, 0x11, 0x1d, 0x08),
        (ModifiersState::ALT, NamedKey::Alt, 0x12, 0x38, 0x02),
        (ModifiersState::SUPER, NamedKey::Super, 0x5b, 0x5b, 0),
    ];

    #[cfg(target_os = "windows")]
    let mut control_key_state: u32 =
        HELD_ORDER.iter().filter(|(flag, ..)| mods.contains(*flag)).map(|(.., bit)| bit).sum();

    let mut bytes = Vec::new();
    let mut remaining = mods;
    for (flag, named, _vk, _sc, _bit) in HELD_ORDER {
        if !mods.contains(flag) {
            continue;
        }

        // A real up event reports the state left after its own release: the
        // key's own bit is already clear in its KEY_EVENT_RECORD, and kitty
        // applies modifier keysyms to themselves (`build_sequence` clears the
        // flag internally for the key being released).
        #[cfg(target_os = "windows")]
        {
            control_key_state &= !_bit;
        }

        let input = KeyInput {
            logical_key: Key::Named(named),
            state: ElementState::Released,
            location: KeyLocation::Left,
            repeat: false,
            key_without_modifiers: Key::Named(named),
            text_with_all_modifiers: None,
            #[cfg(target_os = "windows")]
            raw: RawKeyEventInfo {
                virtual_key: _vk,
                scan_code: _sc as u8,
                repeat_count: 1,
                is_extended: false,
                unicode_char: 0,
                control_key_state,
            },
        };
        bytes.extend(build_sequence(&input, remaining, mode));
        remaining &= !flag;
    }
    bytes
}

/// Helper to build escape sequence payloads from [`KeyInput`].
struct SequenceBuilder {
    mode: TermMode,
    /// The emitted sequence should follow the kitty keyboard protocol.
    kitty_seq: bool,
    /// Encode all the keys according to the protocol.
    kitty_encode_all: bool,
    /// Report event types.
    kitty_event_type: bool,
    modifiers: SequenceModifiers,
}

impl SequenceBuilder {
    /// Try building sequence from the event's emitting text.
    fn try_build_textual(
        &self,
        input: &KeyInput,
        associated_text: Option<&str>,
    ) -> Option<SequenceBase> {
        let character = match input.logical_key.as_ref() {
            Key::Character(character) if self.kitty_seq => character,
            _ => return None,
        };

        if character.chars().count() == 1 {
            let shift = self.modifiers.contains(SequenceModifiers::SHIFT);

            let ch = character.chars().next().unwrap();
            let unshifted_ch = if shift { ch.to_lowercase().next().unwrap() } else { ch };

            let alternate_key_code = u32::from(ch);
            let mut unicode_key_code = u32::from(unshifted_ch);

            // Try to get the base for keys which change based on modifier, like `1` for `!`.
            //
            // However it should only be performed when `SHIFT` is pressed.
            if shift && alternate_key_code == unicode_key_code {
                if let Key::Character(unmodded) = input.key_without_modifiers.as_ref() {
                    unicode_key_code = u32::from(unmodded.chars().next().unwrap_or(unshifted_ch));
                }
            }

            // NOTE: Base layouts are ignored, since winit doesn't expose this information
            // yet.
            let payload = if self.mode.contains(TermMode::REPORT_ALTERNATE_KEYS)
                && alternate_key_code != unicode_key_code
            {
                format!("{unicode_key_code}:{alternate_key_code}")
            } else {
                unicode_key_code.to_string()
            };

            Some(SequenceBase::new(payload.into(), SequenceTerminator::Kitty))
        } else if self.kitty_encode_all && associated_text.is_some() {
            // Fallback when need to report text, but we don't have any key associated with this
            // text.
            Some(SequenceBase::new("0".into(), SequenceTerminator::Kitty))
        } else {
            None
        }
    }

    /// Try building from numpad key.
    ///
    /// `None` is returned when the key is neither known nor numpad.
    fn try_build_numpad(&self, input: &KeyInput) -> Option<SequenceBase> {
        if !self.kitty_seq || input.location != KeyLocation::Numpad {
            return None;
        }

        let base = match input.logical_key.as_ref() {
            Key::Character("0") => "57399",
            Key::Character("1") => "57400",
            Key::Character("2") => "57401",
            Key::Character("3") => "57402",
            Key::Character("4") => "57403",
            Key::Character("5") => "57404",
            Key::Character("6") => "57405",
            Key::Character("7") => "57406",
            Key::Character("8") => "57407",
            Key::Character("9") => "57408",
            Key::Character(".") => "57409",
            Key::Character("/") => "57410",
            Key::Character("*") => "57411",
            Key::Character("-") => "57412",
            Key::Character("+") => "57413",
            Key::Character("=") => "57415",
            Key::Named(named) => match named {
                NamedKey::Enter => "57414",
                NamedKey::ArrowLeft => "57417",
                NamedKey::ArrowRight => "57418",
                NamedKey::ArrowUp => "57419",
                NamedKey::ArrowDown => "57420",
                NamedKey::PageUp => "57421",
                NamedKey::PageDown => "57422",
                NamedKey::Home => "57423",
                NamedKey::End => "57424",
                NamedKey::Insert => "57425",
                NamedKey::Delete => "57426",
                _ => return None,
            },
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
    }

    /// Try building from [`NamedKey`] using the kitty keyboard protocol encoding
    /// for functional keys.
    fn try_build_named_kitty(&self, input: &KeyInput) -> Option<SequenceBase> {
        let named = match input.logical_key {
            Key::Named(named) if self.kitty_seq => named,
            _ => return None,
        };

        let (base, terminator) = match named {
            // F3 in kitty protocol diverges from nebula's terminfo.
            NamedKey::F3 => ("13", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("57376", SequenceTerminator::Kitty),
            NamedKey::F14 => ("57377", SequenceTerminator::Kitty),
            NamedKey::F15 => ("57378", SequenceTerminator::Kitty),
            NamedKey::F16 => ("57379", SequenceTerminator::Kitty),
            NamedKey::F17 => ("57380", SequenceTerminator::Kitty),
            NamedKey::F18 => ("57381", SequenceTerminator::Kitty),
            NamedKey::F19 => ("57382", SequenceTerminator::Kitty),
            NamedKey::F20 => ("57383", SequenceTerminator::Kitty),
            NamedKey::F21 => ("57384", SequenceTerminator::Kitty),
            NamedKey::F22 => ("57385", SequenceTerminator::Kitty),
            NamedKey::F23 => ("57386", SequenceTerminator::Kitty),
            NamedKey::F24 => ("57387", SequenceTerminator::Kitty),
            NamedKey::F25 => ("57388", SequenceTerminator::Kitty),
            NamedKey::F26 => ("57389", SequenceTerminator::Kitty),
            NamedKey::F27 => ("57390", SequenceTerminator::Kitty),
            NamedKey::F28 => ("57391", SequenceTerminator::Kitty),
            NamedKey::F29 => ("57392", SequenceTerminator::Kitty),
            NamedKey::F30 => ("57393", SequenceTerminator::Kitty),
            NamedKey::F31 => ("57394", SequenceTerminator::Kitty),
            NamedKey::F32 => ("57395", SequenceTerminator::Kitty),
            NamedKey::F33 => ("57396", SequenceTerminator::Kitty),
            NamedKey::F34 => ("57397", SequenceTerminator::Kitty),
            NamedKey::F35 => ("57398", SequenceTerminator::Kitty),
            NamedKey::ScrollLock => ("57359", SequenceTerminator::Kitty),
            NamedKey::PrintScreen => ("57361", SequenceTerminator::Kitty),
            NamedKey::Pause => ("57362", SequenceTerminator::Kitty),
            NamedKey::ContextMenu => ("57363", SequenceTerminator::Kitty),
            NamedKey::MediaPlay => ("57428", SequenceTerminator::Kitty),
            NamedKey::MediaPause => ("57429", SequenceTerminator::Kitty),
            NamedKey::MediaPlayPause => ("57430", SequenceTerminator::Kitty),
            NamedKey::MediaStop => ("57432", SequenceTerminator::Kitty),
            NamedKey::MediaFastForward => ("57433", SequenceTerminator::Kitty),
            NamedKey::MediaRewind => ("57434", SequenceTerminator::Kitty),
            NamedKey::MediaTrackNext => ("57435", SequenceTerminator::Kitty),
            NamedKey::MediaTrackPrevious => ("57436", SequenceTerminator::Kitty),
            NamedKey::MediaRecord => ("57437", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeDown => ("57438", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeUp => ("57439", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeMute => ("57440", SequenceTerminator::Kitty),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building from [`NamedKey`].
    fn try_build_named_normal(
        &self,
        input: &KeyInput,
        has_associated_text: bool,
    ) -> Option<SequenceBase> {
        let named = match input.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        // The default parameter is 1, so we can omit it.
        let one_based =
            if self.modifiers.is_empty() && !self.kitty_event_type && !has_associated_text {
                ""
            } else {
                "1"
            };
        let (base, terminator) = match named {
            NamedKey::PageUp => ("5", SequenceTerminator::Normal('~')),
            NamedKey::PageDown => ("6", SequenceTerminator::Normal('~')),
            NamedKey::Insert => ("2", SequenceTerminator::Normal('~')),
            NamedKey::Delete => ("3", SequenceTerminator::Normal('~')),
            NamedKey::Home => (one_based, SequenceTerminator::Normal('H')),
            NamedKey::End => (one_based, SequenceTerminator::Normal('F')),
            NamedKey::ArrowLeft => (one_based, SequenceTerminator::Normal('D')),
            NamedKey::ArrowRight => (one_based, SequenceTerminator::Normal('C')),
            NamedKey::ArrowUp => (one_based, SequenceTerminator::Normal('A')),
            NamedKey::ArrowDown => (one_based, SequenceTerminator::Normal('B')),
            NamedKey::F1 => (one_based, SequenceTerminator::Normal('P')),
            NamedKey::F2 => (one_based, SequenceTerminator::Normal('Q')),
            NamedKey::F3 => (one_based, SequenceTerminator::Normal('R')),
            NamedKey::F4 => (one_based, SequenceTerminator::Normal('S')),
            NamedKey::F5 => ("15", SequenceTerminator::Normal('~')),
            NamedKey::F6 => ("17", SequenceTerminator::Normal('~')),
            NamedKey::F7 => ("18", SequenceTerminator::Normal('~')),
            NamedKey::F8 => ("19", SequenceTerminator::Normal('~')),
            NamedKey::F9 => ("20", SequenceTerminator::Normal('~')),
            NamedKey::F10 => ("21", SequenceTerminator::Normal('~')),
            NamedKey::F11 => ("23", SequenceTerminator::Normal('~')),
            NamedKey::F12 => ("24", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("25", SequenceTerminator::Normal('~')),
            NamedKey::F14 => ("26", SequenceTerminator::Normal('~')),
            NamedKey::F15 => ("28", SequenceTerminator::Normal('~')),
            NamedKey::F16 => ("29", SequenceTerminator::Normal('~')),
            NamedKey::F17 => ("31", SequenceTerminator::Normal('~')),
            NamedKey::F18 => ("32", SequenceTerminator::Normal('~')),
            NamedKey::F19 => ("33", SequenceTerminator::Normal('~')),
            NamedKey::F20 => ("34", SequenceTerminator::Normal('~')),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building escape from control characters (e.g. Enter) and modifiers.
    fn try_build_control_char_or_mod(
        &self,
        input: &KeyInput,
        mods: &mut SequenceModifiers,
    ) -> Option<SequenceBase> {
        if !self.kitty_encode_all && !self.kitty_seq {
            return None;
        }

        let named = match input.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        let base = match named {
            NamedKey::Tab => "9",
            NamedKey::Enter => "13",
            NamedKey::Escape => "27",
            NamedKey::Space => "32",
            NamedKey::Backspace => "127",
            _ => "",
        };

        // Fail when the key is not a named control character and the active mode prohibits us
        // from encoding modifier keys.
        if !self.kitty_encode_all && base.is_empty() {
            return None;
        }

        let base = match (named, input.location) {
            (NamedKey::Shift, KeyLocation::Left) => "57441",
            (NamedKey::Control, KeyLocation::Left) => "57442",
            (NamedKey::Alt, KeyLocation::Left) => "57443",
            (NamedKey::Super, KeyLocation::Left) => "57444",
            (NamedKey::Hyper, KeyLocation::Left) => "57445",
            (NamedKey::Meta, KeyLocation::Left) => "57446",
            (NamedKey::Shift, _) => "57447",
            (NamedKey::Control, _) => "57448",
            (NamedKey::Alt, _) => "57449",
            (NamedKey::Super, _) => "57450",
            (NamedKey::Hyper, _) => "57451",
            (NamedKey::Meta, _) => "57452",
            (NamedKey::CapsLock, _) => "57358",
            (NamedKey::NumLock, _) => "57360",
            _ => base,
        };

        // NOTE: Kitty's protocol mandates that the modifier state is applied before
        // key press, however winit sends them after the key press, so for modifiers
        // itself apply the state based on keysyms and not the _actual_ modifiers
        // state, which is how kitty is doing so and what is suggested in such case.
        let press = input.state.is_pressed();
        match named {
            NamedKey::Shift => mods.set(SequenceModifiers::SHIFT, press),
            NamedKey::Control => mods.set(SequenceModifiers::CONTROL, press),
            NamedKey::Alt => mods.set(SequenceModifiers::ALT, press),
            NamedKey::Super => mods.set(SequenceModifiers::SUPER, press),
            _ => (),
        }

        if base.is_empty() {
            None
        } else {
            Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
        }
    }
}

struct SequenceBase {
    /// The base of the payload, which is the `number` and optionally an alt base from the kitty
    /// spec.
    payload: Cow<'static, str>,
    terminator: SequenceTerminator,
}

impl SequenceBase {
    fn new(payload: Cow<'static, str>, terminator: SequenceTerminator) -> Self {
        Self { payload, terminator }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceTerminator {
    /// The normal key esc sequence terminator defined by xterm/dec.
    Normal(char),
    /// The terminator is for kitty escape sequence.
    Kitty,
}

impl SequenceTerminator {
    fn encode_esc_sequence(self) -> char {
        match self {
            SequenceTerminator::Normal(char) => char,
            SequenceTerminator::Kitty => 'u',
        }
    }
}

bitflags::bitflags! {
    /// The modifiers encoding for escape sequence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SequenceModifiers : u8 {
        const SHIFT   = 0b0000_0001;
        const ALT     = 0b0000_0010;
        const CONTROL = 0b0000_0100;
        const SUPER   = 0b0000_1000;
        // NOTE: Kitty protocol defines additional modifiers to what is present here, like
        // Capslock, but it's not a modifier as per winit.
    }
}

impl SequenceModifiers {
    /// Get the value which should be passed to escape sequence.
    pub fn encode_esc_sequence(self) -> u8 {
        self.bits() + 1
    }
}

impl From<ModifiersState> for SequenceModifiers {
    fn from(mods: ModifiersState) -> Self {
        let mut modifiers = Self::empty();
        modifiers.set(Self::SHIFT, mods.shift_key());
        modifiers.set(Self::ALT, mods.alt_key());
        modifiers.set(Self::CONTROL, mods.control_key());
        modifiers.set(Self::SUPER, mods.super_key());
        modifiers
    }
}

/// Check whether the `text` is `0x7f`, `C0` or `C1` control code.
fn is_control_character(text: &str) -> bool {
    // 0x7f (DEL) is included here since it has a dedicated control code (`^?`) which generally
    // does not match the reported text (`^H`), despite not technically being part of C0 or C1.
    let codepoint = text.bytes().next().unwrap();
    text.len() == 1 && (codepoint < 0x20 || (0x7f..=0x9f).contains(&codepoint))
}

/// Platform-independent tests: the kitty/legacy encoder consumes only the
/// portable fields of [`KeyInput`], so these compile and run on every OS —
/// the visible form of the "one cross-platform encoder" contract.
#[cfg(test)]
mod vt_tests {
    use super::*;

    fn input(logical_key: Key, state: ElementState, text: Option<&str>) -> KeyInput {
        KeyInput {
            key_without_modifiers: logical_key.clone(),
            logical_key,
            state,
            location: KeyLocation::Standard,
            repeat: false,
            text_with_all_modifiers: text.map(SmolStr::new),
            // Inert native identity: the VT encoder under test never reads it.
            #[cfg(target_os = "windows")]
            raw: RawKeyEventInfo {
                virtual_key: 0,
                scan_code: 0,
                repeat_count: 1,
                is_extended: false,
                unicode_char: 0,
                control_key_state: 0,
            },
        }
    }

    #[test]
    fn win32_input_mode_is_used_when_no_kitty_flags_are_active() {
        assert!(use_win32_input_mode(TermMode::WIN32_INPUT_MODE));
    }

    #[test]
    fn kitty_input_takes_precedence_over_win32_input_mode() {
        let mode = TermMode::WIN32_INPUT_MODE | TermMode::DISAMBIGUATE_ESC_CODES;
        assert!(!use_win32_input_mode(mode));
    }

    #[test]
    fn unrelated_modes_do_not_enable_win32_input() {
        assert!(!use_win32_input_mode(TermMode::APP_CURSOR));
    }

    #[test]
    fn kitty_disambiguate_escape_is_csi_27_u() {
        let esc = input(Key::Named(NamedKey::Escape), ElementState::Pressed, Some("\x1b"));
        let bytes = build_sequence(&esc, ModifiersState::empty(), TermMode::DISAMBIGUATE_ESC_CODES);
        assert_eq!(bytes, b"\x1b[27u");
    }

    #[test]
    fn kitty_shift_enter_reports_the_shift_modifier() {
        let enter = input(Key::Named(NamedKey::Enter), ElementState::Pressed, Some("\r"));
        let bytes = build_sequence(&enter, ModifiersState::SHIFT, TermMode::DISAMBIGUATE_ESC_CODES);
        assert_eq!(bytes, b"\x1b[13;2u");
    }

    #[test]
    fn kitty_ctrl_character_encodes_codepoint_and_modifier() {
        let key = input(Key::Character("a".into()), ElementState::Pressed, Some("\x01"));
        let bytes = build_sequence(&key, ModifiersState::CONTROL, TermMode::DISAMBIGUATE_ESC_CODES);
        assert_eq!(bytes, b"\x1b[97;5u");
    }

    #[test]
    fn legacy_arrow_and_function_keys_keep_xterm_encoding() {
        let up = input(Key::Named(NamedKey::ArrowUp), ElementState::Pressed, None);
        assert_eq!(build_sequence(&up, ModifiersState::empty(), TermMode::empty()), b"\x1b[A");

        let f5 = input(Key::Named(NamedKey::F5), ElementState::Pressed, None);
        assert_eq!(build_sequence(&f5, ModifiersState::SHIFT, TermMode::empty()), b"\x1b[15;2~");
    }

    #[test]
    fn runtime_named_keys_follow_active_cursor_mode_and_repeat() {
        let plain = build_runtime_sequence(
            RuntimeKey::Up,
            RuntimeKeyModifiers::default(),
            2,
            TermMode::empty(),
        );
        assert_eq!(plain, b"\x1b[A\x1b[A");

        let application = build_runtime_sequence(
            RuntimeKey::Up,
            RuntimeKeyModifiers::default(),
            1,
            TermMode::APP_CURSOR,
        );
        assert_eq!(application, b"\x1bOA");
    }

    #[test]
    fn runtime_enter_follows_active_keyboard_protocol() {
        let modifiers = RuntimeKeyModifiers::default();
        assert_eq!(
            build_runtime_sequence(RuntimeKey::Enter, modifiers, 1, TermMode::empty()),
            b"\r"
        );
        assert_eq!(
            build_runtime_sequence(
                RuntimeKey::Enter,
                modifiers,
                1,
                TermMode::DISAMBIGUATE_ESC_CODES,
            ),
            b"\x1b[13u"
        );
    }

    #[test]
    fn runtime_ctrl_letter_uses_c0_or_kitty_without_printable_text() {
        let modifiers = RuntimeKeyModifiers { control: true, ..Default::default() };
        assert_eq!(
            build_runtime_sequence(RuntimeKey::C, modifiers, 1, TermMode::empty()),
            vec![0x03]
        );

        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
        assert_eq!(
            build_runtime_sequence(RuntimeKey::C, modifiers, 1, mode),
            b"\x1b[99;5u\x1b[99;5:3u"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn runtime_win32_key_is_a_complete_press_and_release_pair() {
        let bytes = build_runtime_sequence(
            RuntimeKey::Escape,
            RuntimeKeyModifiers::default(),
            1,
            TermMode::WIN32_INPUT_MODE,
        );
        let text = String::from_utf8(bytes).unwrap();
        let records: Vec<_> = text.split_inclusive('_').collect();
        assert_eq!(records.len(), 2);
        assert!(records[0].starts_with("\x1b[27;"));
        assert!(records[0].contains(";27;1;0;1_"));
        assert!(records[1].contains(";27;0;0;1_"));
    }

    #[test]
    fn kitty_event_types_mark_release() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
        let release = input(Key::Character("a".into()), ElementState::Released, None);
        assert_eq!(build_sequence(&release, ModifiersState::empty(), mode), b"\x1b[97;1:3u");
    }

    #[test]
    fn kitty_left_shift_uses_its_dedicated_keysym() {
        let mut shift = input(Key::Named(NamedKey::Shift), ElementState::Pressed, None);
        shift.location = KeyLocation::Left;
        let bytes =
            build_sequence(&shift, ModifiersState::empty(), TermMode::REPORT_ALL_KEYS_AS_ESC);
        // The modifier applies to itself on press, per the kitty spec.
        assert_eq!(bytes, b"\x1b[57441;2u");
    }

    #[test]
    fn kitty_associated_text_appends_codepoints() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ASSOCIATED_TEXT;
        let key = input(Key::Character("a".into()), ElementState::Pressed, Some("a"));
        assert_eq!(build_sequence(&key, ModifiersState::empty(), mode), b"\x1b[97;1;97u");
    }

    #[test]
    fn focus_loss_synthesizes_nothing_when_releases_are_unreportable() {
        let held = ModifiersState::CONTROL | ModifiersState::SHIFT;
        // Legacy VT never encodes bare modifiers.
        assert!(build_focus_loss_key_ups(held, TermMode::empty()).is_empty());
        // Kitty without REPORT_EVENT_TYPES never reports releases; a
        // synthetic one would be a protocol violation.
        assert!(build_focus_loss_key_ups(held, TermMode::REPORT_ALL_KEYS_AS_ESC).is_empty());
        // Disambiguate-only sessions never saw the modifier go down.
        assert!(
            build_focus_loss_key_ups(
                held,
                TermMode::WIN32_INPUT_MODE | TermMode::DISAMBIGUATE_ESC_CODES
            )
            .is_empty()
        );
        // Nothing held, nothing to release.
        let kitty_full = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES;
        assert!(build_focus_loss_key_ups(ModifiersState::empty(), kitty_full).is_empty());
    }

    #[test]
    fn focus_loss_releases_held_modifiers_in_kitty_mode() {
        let mode = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES;
        // Ctrl alone: left-control keysym, empty remaining modifiers, release
        // event type.
        assert_eq!(build_focus_loss_key_ups(ModifiersState::CONTROL, mode), b"\x1b[57442;1:3u");
        // Shift+Ctrl: shift releases first while control is still held (;5),
        // then control releases with nothing remaining (;1).
        assert_eq!(
            build_focus_loss_key_ups(ModifiersState::SHIFT | ModifiersState::CONTROL, mode),
            b"\x1b[57441;5:3u\x1b[57442;1:3u"
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    // KEY_EVENT_RECORD dwControlKeyState bits.
    const SHIFT: u32 = 0x10;
    const LCTRL: u32 = 0x08;
    const RALT: u32 = 0x01;

    /// Fact-table constructor. Values must come from a real capture (VK
    /// probe / matrix baseline / KEY_EVENT_RECORD docs), never guesses.
    fn input(
        logical_key: Key,
        state: ElementState,
        text: Option<&str>,
        (vk, sc, uc, cs): (u16, u8, u16, u32),
    ) -> KeyInput {
        KeyInput {
            key_without_modifiers: logical_key.clone(),
            logical_key,
            state,
            location: KeyLocation::Standard,
            repeat: false,
            text_with_all_modifiers: text.map(SmolStr::new),
            raw: RawKeyEventInfo {
                virtual_key: vk,
                scan_code: sc,
                repeat_count: 1,
                is_extended: false,
                unicode_char: uc,
                control_key_state: cs,
            },
        }
    }

    fn encoded(input: &KeyInput) -> Vec<u8> {
        build_win32_input_sequence(input).expect("expected a win32 record")
    }

    #[test]
    fn focus_loss_releases_held_modifiers_as_win32_records() {
        // Shift releases first with its own bit already clear and control
        // still held (Cs=8); control follows with nothing left (Cs=0).
        assert_eq!(
            build_focus_loss_key_ups(
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                TermMode::WIN32_INPUT_MODE
            ),
            b"\x1b[16;42;0;0;8;1_\x1b[17;29;0;0;0;1_"
        );
        // The Windows key exists as a record but has no dwControlKeyState bit.
        assert_eq!(
            build_focus_loss_key_ups(ModifiersState::SUPER, TermMode::WIN32_INPUT_MODE),
            b"\x1b[91;91;0;0;0;1_"
        );
    }

    #[test]
    fn only_dead_keys_stay_on_the_composition_path() {
        assert!(!uses_composition_path(&Key::Character("/".into())));
        assert!(!uses_composition_path(&Key::Character("【】".into())));
        assert!(uses_composition_path(&Key::Dead(Some('\''))));
        assert!(!uses_composition_path(&Key::Named(NamedKey::Space)));
    }

    #[test]
    fn escape_carries_its_control_character_both_directions() {
        // Down has WM_CHAR text; up has none and falls back to the table.
        let down =
            input(Key::Named(NamedKey::Escape), ElementState::Pressed, Some("\x1b"), (27, 1, 0, 0));
        assert_eq!(encoded(&down), b"\x1b[27;1;27;1;0;1_");

        let up = input(Key::Named(NamedKey::Escape), ElementState::Released, None, (27, 1, 0, 0));
        assert_eq!(encoded(&up), b"\x1b[27;1;27;0;0;1_");
    }

    #[test]
    fn shift_enter_and_ctrl_enter_keep_their_wm_char_values() {
        // Shift+Enter: WM_CHAR stays CR; the shift lives in dwControlKeyState.
        let shift_enter = input(
            Key::Named(NamedKey::Enter),
            ElementState::Pressed,
            Some("\r"),
            (13, 28, 0, SHIFT),
        );
        assert_eq!(encoded(&shift_enter), b"\x1b[13;28;13;1;16;1_");

        // Ctrl+Enter: WM_CHAR is LF — the fallback table must NOT override it.
        let ctrl_enter = input(
            Key::Named(NamedKey::Enter),
            ElementState::Pressed,
            Some("\n"),
            (13, 28, 0, LCTRL),
        );
        assert_eq!(encoded(&ctrl_enter), b"\x1b[13;28;10;1;8;1_");
    }

    #[test]
    fn ctrl_space_and_ctrl_backspace_report_real_key_event_chars() {
        // Ctrl+Space: WM_CHAR is a plain space; NUL synthesis is the host's
        // translation-side job, not the record's.
        let ctrl_space = input(
            Key::Named(NamedKey::Space),
            ElementState::Pressed,
            Some(" "),
            (32, 57, 32, LCTRL),
        );
        assert_eq!(encoded(&ctrl_space), b"\x1b[32;57;32;1;8;1_");

        // Ctrl+Backspace: WM_CHAR is DEL (0x7f), not BS.
        let ctrl_backspace = input(
            Key::Named(NamedKey::Backspace),
            ElementState::Pressed,
            Some("\x7f"),
            (8, 14, 0, LCTRL),
        );
        assert_eq!(encoded(&ctrl_backspace), b"\x1b[8;14;127;1;8;1_");
    }

    #[test]
    fn altgr_character_keeps_native_vkey_and_composed_char() {
        // German layout AltGr+Q = '@': VKEY stays Q, control state carries
        // RIGHT_ALT|LEFT_CTRL, the record's char is the composed '@'.
        let at = input(
            Key::Character("@".into()),
            ElementState::Pressed,
            Some("@"),
            (81, 16, 64, RALT | LCTRL),
        );
        assert_eq!(encoded(&at), b"\x1b[81;16;64;1;9;1_");
    }

    #[test]
    fn bare_modifiers_and_function_keys_stay_char_less() {
        let shift_down =
            input(Key::Named(NamedKey::Shift), ElementState::Pressed, None, (16, 42, 0, SHIFT));
        assert_eq!(encoded(&shift_down), b"\x1b[16;42;0;1;16;1_");

        let f5 = input(Key::Named(NamedKey::F5), ElementState::Pressed, None, (116, 63, 0, 0));
        assert_eq!(encoded(&f5), b"\x1b[116;63;0;1;0;1_");
    }

    #[test]
    fn surrogate_pair_text_emits_one_record_per_utf16_unit() {
        let emoji =
            input(Key::Character("😀".into()), ElementState::Pressed, Some("😀"), (0, 0, 0, 0));
        assert_eq!(encoded(&emoji), b"\x1b[0;0;55357;1;0;1_\x1b[0;0;56832;1;0;1_");
    }

    #[test]
    fn control_keys_fall_back_to_their_key_event_character() {
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::Escape)), 0x1b);
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::Enter)), 0x0d);
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::Tab)), 0x09);
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::Backspace)), 0x08);
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::ArrowUp)), 0);
        assert_eq!(control_char_fallback(&Key::Named(NamedKey::Shift)), 0);
    }

    #[test]
    fn repeat_count_zero_is_normalized_to_protocol_default() {
        let raw = RawKeyEventInfo {
            virtual_key: 0x25,
            scan_code: 0x4b,
            repeat_count: 0,
            is_extended: true,
            unicode_char: 0,
            control_key_state: 0x100,
        };

        let mut sequence = String::new();
        append_raw_key_event(&mut sequence, raw, ElementState::Released, 0);
        assert_eq!(sequence.as_bytes(), b"\x1b[37;75;0;0;256;1_");
    }
}
