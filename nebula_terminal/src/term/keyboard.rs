//! Conversion between terminal flags and the keyboard protocol reported to applications.

use super::TermMode;
use crate::vte::ansi::KeyboardModes;

const KEYBOARD_FLAGS: [(KeyboardModes, TermMode); 5] = [
    (KeyboardModes::DISAMBIGUATE_ESC_CODES, TermMode::DISAMBIGUATE_ESC_CODES),
    (KeyboardModes::REPORT_EVENT_TYPES, TermMode::REPORT_EVENT_TYPES),
    (KeyboardModes::REPORT_ALTERNATE_KEYS, TermMode::REPORT_ALTERNATE_KEYS),
    (KeyboardModes::REPORT_ALL_KEYS_AS_ESC, TermMode::REPORT_ALL_KEYS_AS_ESC),
    (KeyboardModes::REPORT_ASSOCIATED_TEXT, TermMode::REPORT_ASSOCIATED_TEXT),
];

impl From<KeyboardModes> for TermMode {
    fn from(value: KeyboardModes) -> Self {
        let mut mode = Self::empty();
        for (keyboard_flag, terminal_flag) in KEYBOARD_FLAGS {
            mode.set(terminal_flag, value.contains(keyboard_flag));
        }
        mode
    }
}

impl From<TermMode> for KeyboardModes {
    fn from(value: TermMode) -> Self {
        let mut mode = Self::empty();
        for (keyboard_flag, terminal_flag) in KEYBOARD_FLAGS {
            mode.set(keyboard_flag, value.contains(terminal_flag));
        }
        mode
    }
}
