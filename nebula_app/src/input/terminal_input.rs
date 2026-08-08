//! Platform input encoders kept outside the general keyboard dispatcher.
//!
//! The dispatcher decides whether an event belongs to the terminal or to the
//! Nebula chrome. This module owns the wire format for platform terminal modes;
//! keeping that contract isolated makes adding a Linux or macOS raw backend a
//! local change instead of another branch in `keyboard.rs`.

#[cfg(target_os = "windows")]
use std::fmt::Write;

#[cfg(target_os = "windows")]
use winit::event::ElementState;
use winit::event::KeyEvent;
#[cfg(target_os = "windows")]
use winit::keyboard::{Key, NamedKey};
#[cfg(target_os = "windows")]
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// Build one or more ConPTY Win32 input-mode records for a native key event.
///
/// Dead keys intentionally return `None`; their eventual composition is
/// delivered through Winit's OS-resolved text/IME path. Every other key keeps
/// its native Windows identity. Printable keys additionally carry the UTF-16
/// code units emitted by WM_CHAR, matching KEY_EVENT_RECORD semantics without
/// reconstructing VKEYs from text.
#[inline]
pub(crate) fn build_win32_input_sequence(key: &KeyEvent) -> Option<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        if uses_composition_path(&key.logical_key) {
            return None;
        }

        let raw = winit::platform::windows::KeyEventExtWindows::raw_key_event(key);
        let mut sequence = String::with_capacity(48);
        match key.logical_key.as_ref() {
            Key::Character(_) | Key::Named(NamedKey::Space) => {
                let mut wrote_text = false;
                if let Some(text) = key.text_with_all_modifiers().filter(|text| !text.is_empty()) {
                    for unicode_char in text.encode_utf16() {
                        append_raw_key_event(&mut sequence, raw, key.state, unicode_char);
                        wrote_text = true;
                    }
                }

                if !wrote_text {
                    append_raw_key_event(&mut sequence, raw, key.state, raw.unicode_char);
                }
            },
            _ => append_raw_key_event(&mut sequence, raw, key.state, 0),
        }
        return Some(sequence.into_bytes());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = key;
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
fn append_raw_key_event(
    sequence: &mut String,
    raw: winit::platform::windows::RawKeyEventInfo,
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

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use winit::platform::windows::RawKeyEventInfo;

    #[test]
    fn only_dead_keys_stay_on_the_composition_path() {
        assert!(!uses_composition_path(&Key::Character("/".into())));
        assert!(!uses_composition_path(&Key::Character("【】".into())));
        assert!(uses_composition_path(&Key::Dead(Some('\''))));
        assert!(!uses_composition_path(&Key::Named(NamedKey::Space)));
    }

    #[test]
    fn functional_key_uses_the_captured_native_identity() {
        let raw = RawKeyEventInfo {
            virtual_key: 0x0d,
            scan_code: 0x1c,
            repeat_count: 1,
            is_extended: false,
            unicode_char: 0,
            control_key_state: 0x10,
        };

        let mut sequence = String::new();
        append_raw_key_event(&mut sequence, raw, ElementState::Pressed, 0);
        assert_eq!(sequence.as_bytes(), b"\x1b[13;28;0;1;16;1_");
    }

    #[test]
    fn printable_key_keeps_native_identity_and_unicode_char() {
        let raw = RawKeyEventInfo {
            virtual_key: 0xbf,
            scan_code: 0x35,
            repeat_count: 1,
            is_extended: false,
            unicode_char: b'/' as u16,
            control_key_state: 0,
        };

        let mut sequence = String::new();
        append_raw_key_event(&mut sequence, raw, ElementState::Pressed, raw.unicode_char);
        assert_eq!(sequence.as_bytes(), b"\x1b[191;53;47;1;0;1_");
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
