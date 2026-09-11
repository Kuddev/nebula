//! Keyboard protocol flag conversion and application to the active terminal mode.

use log::trace;

use super::{Term, TermMode};
use crate::vte::ansi::{KeyboardModes, KeyboardModesApplyBehavior};

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

impl<T> Term<T> {
    #[inline]
    pub(super) fn set_keyboard_mode(&mut self, mode: TermMode, apply: KeyboardModesApplyBehavior) {
        let active_mode = self.mode & TermMode::KITTY_KEYBOARD_PROTOCOL;
        self.mode &= !TermMode::KITTY_KEYBOARD_PROTOCOL;
        let new_mode = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => active_mode.union(mode),
            KeyboardModesApplyBehavior::Difference => active_mode.difference(mode),
        };
        trace!("Setting keyboard mode to {new_mode:?}");
        self.mode |= new_mode;
    }
}
