//! Built-in default key & mouse bindings.
//!
//! Split out of the parent `bindings` module to keep `bindings.rs` focused on
//! the binding *types* and (de)serialization. The `bindings!` / `trigger!`
//! macros are only used to build these default tables, so they live here too.

use winit::event::MouseButton;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use nebula_terminal::vi_mode::ViMotion;

use super::{
    Action, BindingKey, BindingMode, KeyBinding, KeyLocation, MouseAction, MouseBinding,
    MouseEvent, SearchAction, ViAction,
};

macro_rules! bindings {
    (
        $ty:ident;
        $(
            $key:tt$(::$button:ident)?
            $(=>$location:expr)?
            $(,$mods:expr)*
            $(,+$mode:expr)*
            $(,~$notmode:expr)*
            ;$action:expr
        );*
        $(;)*
    ) => {{
        let mut v = Vec::new();

        $(
            let mut _mods = ModifiersState::empty();
            $(_mods = $mods;)*
            let mut _mode = BindingMode::empty();
            $(_mode.insert($mode);)*
            let mut _notmode = BindingMode::empty();
            $(_notmode.insert($notmode);)*

            v.push($ty {
                trigger: trigger!($ty, $key$(::$button)?, $($location)?),
                mods: _mods,
                mode: _mode,
                notmode: _notmode,
                action: $action.into(),
            });
        )*

        v
    }};
}

macro_rules! trigger {
    (KeyBinding, $key:literal, $location:expr) => {{ BindingKey::Keycode { key: Key::Character($key.into()), location: $location } }};
    (KeyBinding, $key:literal,) => {{ BindingKey::Keycode { key: Key::Character($key.into()), location: KeyLocation::Any } }};
    (KeyBinding, $key:ident, $location:expr) => {{ BindingKey::Keycode { key: Key::Named(NamedKey::$key), location: $location } }};
    (KeyBinding, $key:ident,) => {{ BindingKey::Keycode { key: Key::Named(NamedKey::$key), location: KeyLocation::Any } }};
    (MouseBinding, MouseButton::$button:ident,) => {{ MouseEvent::Button(MouseButton::$button) }};
    (MouseBinding, MouseEvent::$event:ident,) => {{ MouseEvent::$event }};
}

pub fn default_mouse_bindings() -> Vec<MouseBinding> {
    bindings!(
        MouseBinding;
        MouseButton::Right;                            MouseAction::ExpandSelection;
        MouseButton::Right,   ModifiersState::CONTROL; MouseAction::ExpandSelection;
        MouseButton::Middle, ~BindingMode::VI;         Action::PasteSelection;
    )
}

// NOTE: key sequences which are not present here, like F5-F20, PageUp/PageDown codes are
// built on the fly in input/keyboard.rs.
pub fn default_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = bindings!(
        KeyBinding;
        Copy; Action::Copy;
        Copy,  +BindingMode::VI; Action::ClearSelection;
        Paste, ~BindingMode::VI; Action::Paste;
        Paste, +BindingMode::VI, +BindingMode::SEARCH; Action::Paste;
        "l",       ModifiersState::CONTROL; Action::ClearLogNotice;
        "l",       ModifiersState::CONTROL; Action::ReceiveChar;
        Home,      ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollToTop;
        End,       ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollToBottom;
        PageUp,    ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollPageUp;
        PageDown,  ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollPageDown;
        // App cursor mode.
        Home,       +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOH".into());
        End,        +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOF".into());
        ArrowUp,    +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOA".into());
        ArrowDown,  +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOB".into());
        ArrowRight, +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOC".into());
        ArrowLeft,  +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOD".into());
        // Legacy keys handling which can't be automatically encoded.
        F1,         ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1bOP".into());
        F2,         ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1bOQ".into());
        F3,         ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1bOR".into());
        F4,         ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1bOS".into());
        Tab,       ModifiersState::SHIFT,   ~BindingMode::VI,   ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1b[Z".into());
        Tab,       ModifiersState::SHIFT | ModifiersState::ALT, ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1b\x1b[Z".into());
        Backspace, ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC; Action::Esc("\x7f".into());
        // Ctrl+Backspace 删除前一段/词。这里发送 Ctrl+W，避免依赖各平台
        // 对“带修饰键 Backspace”的非标准转义序列解析。
        Backspace, ModifiersState::CONTROL, ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x17".into());
        Backspace, ModifiersState::ALT,     ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x1b\x7f".into());
        Backspace, ModifiersState::SHIFT,   ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\x7f".into());
        Enter => KeyLocation::Numpad, ~BindingMode::VI, ~BindingMode::SEARCH, ~BindingMode::REPORT_ALL_KEYS_AS_ESC, ~BindingMode::DISAMBIGUATE_ESC_CODES; Action::Esc("\n".into());
        // Vi mode.
        Space, ModifiersState::SHIFT | ModifiersState::CONTROL, ~BindingMode::SEARCH; Action::ToggleViMode;
        Space, ModifiersState::SHIFT | ModifiersState::CONTROL, +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollToBottom;
        Escape,                             +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        "i",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::ToggleViMode;
        "i",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollToBottom;
        "c",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ToggleViMode;
        "y",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollLineUp;
        "e",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollLineDown;
        "g",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollToTop;
        "g",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollToBottom;
        "b",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollPageUp;
        "f",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollPageDown;
        "u",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollHalfPageUp;
        "d",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; Action::ScrollHalfPageDown;
        "y",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::Copy;
        "y",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        "/",                                +BindingMode::VI, ~BindingMode::SEARCH; Action::SearchForward;
        "?",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; Action::SearchBackward;
        "y",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::ToggleNormalSelection;
        "y",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Last;
        "y",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; Action::Copy;
        "y",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        "v",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::ToggleNormalSelection;
        "v",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::ToggleLineSelection;
        "v",      ModifiersState::CONTROL,  +BindingMode::VI, ~BindingMode::SEARCH; ViAction::ToggleBlockSelection;
        "v",      ModifiersState::ALT,      +BindingMode::VI, ~BindingMode::SEARCH; ViAction::ToggleSemanticSelection;
        "n",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::SearchNext;
        "n",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::SearchPrevious;
        Enter,                              +BindingMode::VI, ~BindingMode::SEARCH; ViAction::Open;
        "z",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::CenterAroundViCursor;
        "f",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchForward;
        "f",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchBackward;
        "t",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchForwardShort;
        "t",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchBackwardShort;
        ";",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchNext;
        ",",                                +BindingMode::VI, ~BindingMode::SEARCH; ViAction::InlineSearchPrevious;
        "*",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::SemanticSearchForward;
        "#",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViAction::SemanticSearchBackward;
        "k",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Up;
        "j",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Down;
        "h",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Left;
        "l",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Right;
        ArrowUp,                            +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Up;
        ArrowDown,                          +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Down;
        ArrowLeft,                          +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Left;
        ArrowRight,                         +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Right;
        "0",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::First;
        "$",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Last;
        Home,                               +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::First;
        End,                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Last;
        "^",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::FirstOccupied;
        "h",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::High;
        "m",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Middle;
        "l",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Low;
        "b",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::SemanticLeft;
        "w",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::SemanticRight;
        "e",                                +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::SemanticRightEnd;
        "b",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::WordLeft;
        "w",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::WordRight;
        "e",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::WordRightEnd;
        "%",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::Bracket;
        "{",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::ParagraphUp;
        "}",      ModifiersState::SHIFT,    +BindingMode::VI, ~BindingMode::SEARCH; ViMotion::ParagraphDown;
        Enter,                              +BindingMode::VI, +BindingMode::SEARCH; SearchAction::SearchConfirm;
        // Plain search.
        Escape,                             +BindingMode::SEARCH; SearchAction::SearchCancel;
        "c",      ModifiersState::CONTROL,  +BindingMode::SEARCH; SearchAction::SearchCancel;
        "u",      ModifiersState::CONTROL,  +BindingMode::SEARCH; SearchAction::SearchClear;
        "w",      ModifiersState::CONTROL,  +BindingMode::SEARCH; SearchAction::SearchDeleteWord;
        "p",      ModifiersState::CONTROL,  +BindingMode::SEARCH; SearchAction::SearchHistoryPrevious;
        "n",      ModifiersState::CONTROL,  +BindingMode::SEARCH; SearchAction::SearchHistoryNext;
        ArrowUp,                            +BindingMode::SEARCH; SearchAction::SearchHistoryPrevious;
        ArrowDown,                          +BindingMode::SEARCH; SearchAction::SearchHistoryNext;
        Enter,                              +BindingMode::SEARCH, ~BindingMode::VI; SearchAction::SearchFocusNext;
        Enter, ModifiersState::SHIFT,       +BindingMode::SEARCH, ~BindingMode::VI; SearchAction::SearchFocusPrevious;
    );

    bindings.extend(platform_key_bindings());

    bindings
}

#[cfg(not(any(target_os = "macos", test)))]
fn common_keybindings() -> Vec<KeyBinding> {
    bindings!(
        KeyBinding;
        "v",    ModifiersState::CONTROL | ModifiersState::SHIFT, ~BindingMode::VI;                       Action::Paste;
        "v",    ModifiersState::CONTROL | ModifiersState::SHIFT, +BindingMode::VI, +BindingMode::SEARCH; Action::Paste;
        // Windows/Linux convention: plain Ctrl+V pastes too (matches PowerShell
        // PSReadLine and Windows Terminal). Suppressed in Nebula's vi scroll
        // mode so Ctrl+V there keeps its navigation meaning.
        "v",    ModifiersState::CONTROL,                          ~BindingMode::VI;                       Action::Paste;
        "v",    ModifiersState::CONTROL,                          +BindingMode::VI, +BindingMode::SEARCH; Action::Paste;
        "f",    ModifiersState::CONTROL | ModifiersState::SHIFT, ~BindingMode::SEARCH;                   Action::SearchForward;
        "b",    ModifiersState::CONTROL | ModifiersState::SHIFT, ~BindingMode::SEARCH;                   Action::SearchBackward;
        Insert, ModifiersState::SHIFT,                           ~BindingMode::VI;                       Action::PasteSelection;
        "c",    ModifiersState::CONTROL | ModifiersState::SHIFT;                                         Action::Copy;
        "c",    ModifiersState::CONTROL | ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        "0",    ModifiersState::CONTROL;                                                                 Action::ResetFontSize;
        "=",    ModifiersState::CONTROL;                                                                 Action::IncreaseFontSize;
        "+",    ModifiersState::CONTROL;                                                                 Action::IncreaseFontSize;
        "-",    ModifiersState::CONTROL;                                                                 Action::DecreaseFontSize;
        "+" => KeyLocation::Numpad, ModifiersState::CONTROL;                                             Action::IncreaseFontSize;
        "-" => KeyLocation::Numpad, ModifiersState::CONTROL;                                             Action::DecreaseFontSize;
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows", test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    common_keybindings()
}

#[cfg(all(target_os = "windows", not(test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = bindings!(
        KeyBinding;
        Enter, ModifiersState::ALT; Action::ToggleFullscreen;
    );
    bindings.extend(common_keybindings());
    bindings
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    bindings!(
        KeyBinding;
        Insert, ModifiersState::SHIFT, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[2;2~".into());
        // Tabbing api.
        "t",    ModifiersState::SUPER;                                         Action::CreateNewTab;
        "]",    ModifiersState::SUPER | ModifiersState::SHIFT;                 Action::SelectNextTab;
        "[",    ModifiersState::SUPER | ModifiersState::SHIFT;                 Action::SelectPreviousTab;
        Tab,    ModifiersState::SUPER;                                         Action::SelectNextTab;
        Tab,    ModifiersState::SUPER | ModifiersState::SHIFT;                 Action::SelectPreviousTab;
        "1",    ModifiersState::SUPER;                                         Action::SelectTab1;
        "2",    ModifiersState::SUPER;                                         Action::SelectTab2;
        "3",    ModifiersState::SUPER;                                         Action::SelectTab3;
        "4",    ModifiersState::SUPER;                                         Action::SelectTab4;
        "5",    ModifiersState::SUPER;                                         Action::SelectTab5;
        "6",    ModifiersState::SUPER;                                         Action::SelectTab6;
        "7",    ModifiersState::SUPER;                                         Action::SelectTab7;
        "8",    ModifiersState::SUPER;                                         Action::SelectTab8;
        "9",    ModifiersState::SUPER;                                         Action::SelectLastTab;
        "0",    ModifiersState::SUPER;                                         Action::ResetFontSize;
        "=",    ModifiersState::SUPER;                                         Action::IncreaseFontSize;
        "+",    ModifiersState::SUPER;                                         Action::IncreaseFontSize;
        "-",    ModifiersState::SUPER;                                         Action::DecreaseFontSize;
        "k",    ModifiersState::SUPER, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x0c".into());
        "k",    ModifiersState::SUPER, ~BindingMode::VI, ~BindingMode::SEARCH; Action::ClearHistory;
        "v",    ModifiersState::SUPER, ~BindingMode::VI;                       Action::Paste;
        "v",    ModifiersState::SUPER, +BindingMode::VI, +BindingMode::SEARCH; Action::Paste;
        "n",    ModifiersState::SUPER;                                         Action::CreateNewWindow;
        "f",    ModifiersState::CONTROL | ModifiersState::SUPER;               Action::ToggleFullscreen;
        "c",    ModifiersState::SUPER;                                         Action::Copy;
        "c",    ModifiersState::SUPER, +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        "h",    ModifiersState::SUPER;                                         Action::Hide;
        "h",    ModifiersState::SUPER   | ModifiersState::ALT;                 Action::HideOtherApplications;
        "m",    ModifiersState::SUPER;                                         Action::Minimize;
        "q",    ModifiersState::SUPER;                                         Action::Quit;
        "w",    ModifiersState::SUPER;                                         Action::Quit;
        "f",    ModifiersState::SUPER, ~BindingMode::SEARCH;                   Action::SearchForward;
        "b",    ModifiersState::SUPER, ~BindingMode::SEARCH;                   Action::SearchBackward;
        "+" => KeyLocation::Numpad, ModifiersState::SUPER;                     Action::IncreaseFontSize;
        "-" => KeyLocation::Numpad, ModifiersState::SUPER;                     Action::DecreaseFontSize;
    )
}

// Don't return any bindings for tests since they are commented-out by default.
#[cfg(test)]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    vec![]
}
