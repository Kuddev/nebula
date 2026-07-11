#![allow(clippy::enum_glob_use)]

use std::fmt::{self, Debug, Display};

use bitflags::bitflags;
use serde::de::{self, Error as SerdeError, MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use std::rc::Rc;
use toml::Value as SerdeValue;
use winit::event::MouseButton;
use winit::keyboard::{
    Key, KeyCode, KeyLocation as WinitKeyLocation, ModifiersState, NamedKey, PhysicalKey,
};
use winit::platform::scancode::PhysicalKeyExtScancode;

use nebula_config_derive::{ConfigDeserialize, SerdeReplace};

use nebula_terminal::term::TermMode;
use nebula_terminal::vi_mode::ViMotion;

use crate::config::ui_config::{Hint, Program, StringVisitor};

/// Describes a state and action to take in that state.
///
/// This is the shared component of `MouseBinding` and `KeyBinding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding<T> {
    /// Modifier keys required to activate binding.
    pub mods: ModifiersState,

    /// String to send to PTY if mods and mode match.
    pub action: Action,

    /// Binding mode required to activate binding.
    pub mode: BindingMode,

    /// Excluded binding modes where the binding won't be activated.
    pub notmode: BindingMode,

    /// This property is used as part of the trigger detection code.
    ///
    /// For example, this might be a key like "G", or a mouse button.
    pub trigger: T,
}

/// Bindings that are triggered by a keyboard key.
pub type KeyBinding = Binding<BindingKey>;

/// Bindings that are triggered by a mouse event.
pub type MouseBinding = Binding<MouseEvent>;

impl<T: Eq> Binding<T> {
    #[inline]
    pub fn is_triggered_by(&self, mode: BindingMode, mods: ModifiersState, input: &T) -> bool {
        // Check input first since bindings are stored in one big list. This is
        // the most likely item to fail so prioritizing it here allows more
        // checks to be short circuited.
        self.trigger == *input
            && self.mods == mods
            && mode.contains(self.mode)
            && !mode.intersects(self.notmode)
    }

    #[inline]
    pub fn triggers_match(&self, binding: &Binding<T>) -> bool {
        // Check the binding's key and modifiers.
        if self.trigger != binding.trigger || self.mods != binding.mods {
            return false;
        }

        let selfmode = if self.mode.is_empty() { BindingMode::all() } else { self.mode };
        let bindingmode = if binding.mode.is_empty() { BindingMode::all() } else { binding.mode };

        if !selfmode.intersects(bindingmode) {
            return false;
        }

        // The bindings are never active at the same time when the required modes of one binding
        // are part of the forbidden bindings of the other.
        if self.mode.intersects(binding.notmode) || binding.mode.intersects(self.notmode) {
            return false;
        }

        true
    }
}

#[derive(ConfigDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write an escape sequence.
    #[config(skip)]
    Esc(String),

    /// Run given command.
    #[config(skip)]
    Command(Program),

    /// Regex keyboard hints.
    #[config(skip)]
    Hint(Rc<Hint>),

    /// Move vi mode cursor.
    #[config(skip)]
    ViMotion(ViMotion),

    /// Perform vi mode action.
    #[config(skip)]
    Vi(ViAction),

    /// Perform search mode action.
    #[config(skip)]
    Search(SearchAction),

    /// Perform mouse binding exclusive action.
    #[config(skip)]
    Mouse(MouseAction),

    /// Paste contents of system clipboard.
    Paste,

    /// Store current selection into clipboard.
    Copy,

    /// Store current selection into selection buffer.
    CopySelection,

    /// Paste contents of selection buffer.
    PasteSelection,

    /// Increase font size.
    IncreaseFontSize,

    /// Decrease font size.
    DecreaseFontSize,

    /// Reset font size to the config value.
    ResetFontSize,

    /// Scroll exactly one page up.
    ScrollPageUp,

    /// Scroll exactly one page down.
    ScrollPageDown,

    /// Scroll half a page up.
    ScrollHalfPageUp,

    /// Scroll half a page down.
    ScrollHalfPageDown,

    /// Scroll one line up.
    ScrollLineUp,

    /// Scroll one line down.
    ScrollLineDown,

    /// Scroll all the way to the top.
    ScrollToTop,

    /// Scroll all the way to the bottom.
    ScrollToBottom,

    /// Clear the display buffer(s) to remove history.
    ClearHistory,

    /// Hide the Nebula window.
    Hide,

    /// Hide all windows other than Nebula on macOS.
    HideOtherApplications,

    /// Minimize the Nebula window.
    Minimize,

    /// Quit Nebula.
    Quit,

    /// Clear warning and error notices.
    ClearLogNotice,

    /// Spawn a new instance of Nebula.
    SpawnNewInstance,

    /// Select next tab.
    SelectNextTab,

    /// Select previous tab.
    SelectPreviousTab,

    /// Select the first tab.
    SelectTab1,

    /// Select the second tab.
    SelectTab2,

    /// Select the third tab.
    SelectTab3,

    /// Select the fourth tab.
    SelectTab4,

    /// Select the fifth tab.
    SelectTab5,

    /// Select the sixth tab.
    SelectTab6,

    /// Select the seventh tab.
    SelectTab7,

    /// Select the eighth tab.
    SelectTab8,

    /// Select the ninth tab.
    SelectTab9,

    /// Select the last tab.
    SelectLastTab,

    /// Create a new Nebula window.
    CreateNewWindow,

    /// Create new window in a tab.
    CreateNewTab,

    /// Toggle fullscreen.
    ToggleFullscreen,

    /// Toggle maximized.
    ToggleMaximized,

    /// Toggle simple fullscreen on macOS.
    ToggleSimpleFullscreen,

    /// Clear active selection.
    ClearSelection,

    /// Toggle vi mode.
    ToggleViMode,

    /// Allow receiving char input.
    ReceiveChar,

    /// Start a forward buffer search.
    SearchForward,

    /// Start a backward buffer search.
    SearchBackward,

    /// No action.
    None,
}

impl From<&'static str> for Action {
    fn from(s: &'static str) -> Action {
        Action::Esc(s.into())
    }
}

impl From<ViAction> for Action {
    fn from(action: ViAction) -> Self {
        Self::Vi(action)
    }
}

impl From<ViMotion> for Action {
    fn from(motion: ViMotion) -> Self {
        Self::ViMotion(motion)
    }
}

impl From<SearchAction> for Action {
    fn from(action: SearchAction) -> Self {
        Self::Search(action)
    }
}

impl From<MouseAction> for Action {
    fn from(action: MouseAction) -> Self {
        Self::Mouse(action)
    }
}

/// Display trait used for error logging.
impl Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::ViMotion(motion) => motion.fmt(f),
            Action::Vi(action) => action.fmt(f),
            Action::Mouse(action) => action.fmt(f),
            _ => write!(f, "{self:?}"),
        }
    }
}

/// Vi mode specific actions.
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum ViAction {
    /// Toggle normal vi selection.
    ToggleNormalSelection,
    /// Toggle line vi selection.
    ToggleLineSelection,
    /// Toggle block vi selection.
    ToggleBlockSelection,
    /// Toggle semantic vi selection.
    ToggleSemanticSelection,
    /// Jump to the beginning of the next match.
    SearchNext,
    /// Jump to the beginning of the previous match.
    SearchPrevious,
    /// Jump to the next start of a match to the left of the origin.
    SearchStart,
    /// Jump to the next end of a match to the right of the origin.
    SearchEnd,
    /// Launch the URL below the vi mode cursor.
    Open,
    /// Centers the screen around the vi mode cursor.
    CenterAroundViCursor,
    /// Search forward within the current line.
    InlineSearchForward,
    /// Search backward within the current line.
    InlineSearchBackward,
    /// Search forward within the current line, stopping just short of the character.
    InlineSearchForwardShort,
    /// Search backward within the current line, stopping just short of the character.
    InlineSearchBackwardShort,
    /// Jump to the next inline search match.
    InlineSearchNext,
    /// Jump to the previous inline search match.
    InlineSearchPrevious,
    /// Search forward for selection or word under the cursor.
    SemanticSearchForward,
    /// Search backward for selection or word under the cursor.
    SemanticSearchBackward,
}

/// Search mode specific actions.
#[allow(clippy::enum_variant_names)]
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum SearchAction {
    /// Move the focus to the next search match.
    SearchFocusNext,
    /// Move the focus to the previous search match.
    SearchFocusPrevious,
    /// Confirm the active search.
    SearchConfirm,
    /// Cancel the active search.
    SearchCancel,
    /// Reset the search regex.
    SearchClear,
    /// Delete the last word in the search regex.
    SearchDeleteWord,
    /// Go to the previous regex in the search history.
    SearchHistoryPrevious,
    /// Go to the next regex in the search history.
    SearchHistoryNext,
}

/// Mouse binding specific actions.
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum MouseAction {
    /// Expand the selection to the current mouse cursor position.
    ExpandSelection,
}

/// Mouse binding specific events.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum MouseEvent {
    Button(MouseButton),
    WheelUp,
    WheelDown,
}

mod defaults;
pub use defaults::{default_key_bindings, default_mouse_bindings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingKey {
    Scancode(PhysicalKey),
    Keycode { key: Key, location: KeyLocation },
}

/// Key location for matching bindings.
#[derive(Debug, Clone, Copy, Eq)]
pub enum KeyLocation {
    /// The key is in its standard position.
    Standard,
    /// The key is on the numeric pad.
    Numpad,
    /// The key could be anywhere on the keyboard.
    Any,
}

impl From<WinitKeyLocation> for KeyLocation {
    fn from(value: WinitKeyLocation) -> Self {
        match value {
            WinitKeyLocation::Standard => KeyLocation::Standard,
            WinitKeyLocation::Left => KeyLocation::Any,
            WinitKeyLocation::Right => KeyLocation::Any,
            WinitKeyLocation::Numpad => KeyLocation::Numpad,
        }
    }
}

impl PartialEq for KeyLocation {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (_, KeyLocation::Any)
                | (KeyLocation::Any, _)
                | (KeyLocation::Standard, KeyLocation::Standard)
                | (KeyLocation::Numpad, KeyLocation::Numpad)
        )
    }
}

impl<'a> Deserialize<'a> for BindingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let value = SerdeValue::deserialize(deserializer)?;
        match u32::deserialize(value.clone()) {
            Ok(scancode) => Ok(BindingKey::Scancode(PhysicalKey::from_scancode(scancode))),
            Err(_) => {
                let keycode = String::deserialize(value.clone()).map_err(D::Error::custom)?;
                let (key, location) = if keycode.chars().count() == 1 {
                    (Key::Character(keycode.to_lowercase().into()), KeyLocation::Any)
                } else {
                    // Translate legacy winit codes into their modern counterparts.
                    match keycode.as_str() {
                        "Back" => (Key::Named(NamedKey::Backspace), KeyLocation::Any),
                        "Up" => (Key::Named(NamedKey::ArrowUp), KeyLocation::Any),
                        "Down" => (Key::Named(NamedKey::ArrowDown), KeyLocation::Any),
                        "Left" => (Key::Named(NamedKey::ArrowLeft), KeyLocation::Any),
                        "Right" => (Key::Named(NamedKey::ArrowRight), KeyLocation::Any),
                        "At" => (Key::Character("@".into()), KeyLocation::Any),
                        "Colon" => (Key::Character(":".into()), KeyLocation::Any),
                        "Period" => (Key::Character(".".into()), KeyLocation::Any),
                        "LBracket" => (Key::Character("[".into()), KeyLocation::Any),
                        "RBracket" => (Key::Character("]".into()), KeyLocation::Any),
                        "Semicolon" => (Key::Character(";".into()), KeyLocation::Any),
                        "Backslash" => (Key::Character("\\".into()), KeyLocation::Any),

                        // The keys which has alternative on numeric pad.
                        "Enter" => (Key::Named(NamedKey::Enter), KeyLocation::Standard),
                        "Return" => (Key::Named(NamedKey::Enter), KeyLocation::Standard),
                        "Plus" => (Key::Character("+".into()), KeyLocation::Standard),
                        "Comma" => (Key::Character(",".into()), KeyLocation::Standard),
                        "Slash" => (Key::Character("/".into()), KeyLocation::Standard),
                        "Equals" => (Key::Character("=".into()), KeyLocation::Standard),
                        "Minus" => (Key::Character("-".into()), KeyLocation::Standard),
                        "Asterisk" => (Key::Character("*".into()), KeyLocation::Standard),
                        "Key1" => (Key::Character("1".into()), KeyLocation::Standard),
                        "Key2" => (Key::Character("2".into()), KeyLocation::Standard),
                        "Key3" => (Key::Character("3".into()), KeyLocation::Standard),
                        "Key4" => (Key::Character("4".into()), KeyLocation::Standard),
                        "Key5" => (Key::Character("5".into()), KeyLocation::Standard),
                        "Key6" => (Key::Character("6".into()), KeyLocation::Standard),
                        "Key7" => (Key::Character("7".into()), KeyLocation::Standard),
                        "Key8" => (Key::Character("8".into()), KeyLocation::Standard),
                        "Key9" => (Key::Character("9".into()), KeyLocation::Standard),
                        "Key0" => (Key::Character("0".into()), KeyLocation::Standard),

                        // Special case numpad.
                        "NumpadEnter" => (Key::Named(NamedKey::Enter), KeyLocation::Numpad),
                        "NumpadAdd" => (Key::Character("+".into()), KeyLocation::Numpad),
                        "NumpadComma" => (Key::Character(",".into()), KeyLocation::Numpad),
                        "NumpadDecimal" => (Key::Character(".".into()), KeyLocation::Numpad),
                        "NumpadDivide" => (Key::Character("/".into()), KeyLocation::Numpad),
                        "NumpadEquals" => (Key::Character("=".into()), KeyLocation::Numpad),
                        "NumpadSubtract" => (Key::Character("-".into()), KeyLocation::Numpad),
                        "NumpadMultiply" => (Key::Character("*".into()), KeyLocation::Numpad),
                        "Numpad1" => (Key::Character("1".into()), KeyLocation::Numpad),
                        "Numpad2" => (Key::Character("2".into()), KeyLocation::Numpad),
                        "Numpad3" => (Key::Character("3".into()), KeyLocation::Numpad),
                        "Numpad4" => (Key::Character("4".into()), KeyLocation::Numpad),
                        "Numpad5" => (Key::Character("5".into()), KeyLocation::Numpad),
                        "Numpad6" => (Key::Character("6".into()), KeyLocation::Numpad),
                        "Numpad7" => (Key::Character("7".into()), KeyLocation::Numpad),
                        "Numpad8" => (Key::Character("8".into()), KeyLocation::Numpad),
                        "Numpad9" => (Key::Character("9".into()), KeyLocation::Numpad),
                        "Numpad0" => (Key::Character("0".into()), KeyLocation::Numpad),
                        _ if keycode.starts_with("Dead") => {
                            (Key::deserialize(value).map_err(D::Error::custom)?, KeyLocation::Any)
                        },
                        _ => (
                            Key::Named(NamedKey::deserialize(value).map_err(D::Error::custom)?),
                            KeyLocation::Any,
                        ),
                    }
                };

                Ok(BindingKey::Keycode { key, location })
            },
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModeWrapper {
    pub mode: BindingMode,
    pub not_mode: BindingMode,
}

bitflags! {
    /// Modes available for key bindings.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BindingMode: u8 {
        const APP_CURSOR             = 0b0000_0001;
        const APP_KEYPAD             = 0b0000_0010;
        const ALT_SCREEN             = 0b0000_0100;
        const VI                     = 0b0000_1000;
        const SEARCH                 = 0b0001_0000;
        const DISAMBIGUATE_ESC_CODES = 0b0010_0000;
        const REPORT_ALL_KEYS_AS_ESC = 0b0100_0000;
    }
}

impl BindingMode {
    pub fn new(mode: &TermMode, search: bool) -> BindingMode {
        let mut binding_mode = BindingMode::empty();
        binding_mode.set(BindingMode::APP_CURSOR, mode.contains(TermMode::APP_CURSOR));
        binding_mode.set(BindingMode::APP_KEYPAD, mode.contains(TermMode::APP_KEYPAD));
        binding_mode.set(BindingMode::ALT_SCREEN, mode.contains(TermMode::ALT_SCREEN));
        binding_mode.set(BindingMode::VI, mode.contains(TermMode::VI));
        binding_mode.set(BindingMode::SEARCH, search);
        binding_mode.set(
            BindingMode::DISAMBIGUATE_ESC_CODES,
            mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
        );
        binding_mode.set(
            BindingMode::REPORT_ALL_KEYS_AS_ESC,
            mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
        );
        binding_mode
    }
}

impl Default for ModeWrapper {
    fn default() -> Self {
        Self { mode: BindingMode::empty(), not_mode: BindingMode::empty() }
    }
}

impl<'a> Deserialize<'a> for ModeWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        struct ModeVisitor;

        impl Visitor<'_> for ModeVisitor {
            type Value = ModeWrapper;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a combination of AppCursor | AppKeypad | Alt | Vi, possibly with negation (~)",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<ModeWrapper, E>
            where
                E: de::Error,
            {
                let mut res =
                    ModeWrapper { mode: BindingMode::empty(), not_mode: BindingMode::empty() };

                for modifier in value.split('|') {
                    match modifier.trim().to_lowercase().as_str() {
                        "appcursor" => res.mode |= BindingMode::APP_CURSOR,
                        "~appcursor" => res.not_mode |= BindingMode::APP_CURSOR,
                        "appkeypad" => res.mode |= BindingMode::APP_KEYPAD,
                        "~appkeypad" => res.not_mode |= BindingMode::APP_KEYPAD,
                        "alt" => res.mode |= BindingMode::ALT_SCREEN,
                        "~alt" => res.not_mode |= BindingMode::ALT_SCREEN,
                        "vi" => res.mode |= BindingMode::VI,
                        "~vi" => res.not_mode |= BindingMode::VI,
                        "search" => res.mode |= BindingMode::SEARCH,
                        "~search" => res.not_mode |= BindingMode::SEARCH,
                        _ => return Err(E::invalid_value(Unexpected::Str(modifier), &self)),
                    }
                }

                Ok(res)
            }
        }
        deserializer.deserialize_str(ModeVisitor)
    }
}

impl<'a> Deserialize<'a> for MouseEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        struct MouseButtonVisitor;

        impl Visitor<'_> for MouseButtonVisitor {
            type Value = MouseEvent;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "Left, Right, Middle, Back, Forward, WheelUp, WheelDown, or a number from 0 \
                     to 65536",
                )
            }

            fn visit_i64<E>(self, value: i64) -> Result<MouseEvent, E>
            where
                E: de::Error,
            {
                match value {
                    0..=65536 => Ok(MouseEvent::Button(MouseButton::Other(value as u16))),
                    _ => Err(E::invalid_value(Unexpected::Signed(value), &self)),
                }
            }

            fn visit_u64<E>(self, value: u64) -> Result<MouseEvent, E>
            where
                E: de::Error,
            {
                match value {
                    0..=65536 => Ok(MouseEvent::Button(MouseButton::Other(value as u16))),
                    _ => Err(E::invalid_value(Unexpected::Unsigned(value), &self)),
                }
            }

            fn visit_str<E>(self, value: &str) -> Result<MouseEvent, E>
            where
                E: de::Error,
            {
                match value {
                    "Left" => Ok(MouseEvent::Button(MouseButton::Left)),
                    "Right" => Ok(MouseEvent::Button(MouseButton::Right)),
                    "Middle" => Ok(MouseEvent::Button(MouseButton::Middle)),
                    "Back" => Ok(MouseEvent::Button(MouseButton::Back)),
                    "Forward" => Ok(MouseEvent::Button(MouseButton::Forward)),
                    "WheelUp" => Ok(MouseEvent::WheelUp),
                    "WheelDown" => Ok(MouseEvent::WheelDown),
                    _ => Err(E::invalid_value(Unexpected::Str(value), &self)),
                }
            }
        }

        deserializer.deserialize_any(MouseButtonVisitor)
    }
}

/// Bindings are deserialized into a `RawBinding` before being parsed as a
/// `KeyBinding` or `MouseBinding`.
#[derive(PartialEq, Eq)]
struct RawBinding {
    key: Option<BindingKey>,
    mouse: Option<MouseEvent>,
    mods: ModifiersState,
    mode: BindingMode,
    notmode: BindingMode,
    action: Action,
}

impl RawBinding {
    fn into_mouse_binding(self) -> Result<MouseBinding, Box<Self>> {
        if let Some(mouse) = self.mouse {
            Ok(Binding {
                trigger: mouse,
                mods: self.mods,
                action: self.action,
                mode: self.mode,
                notmode: self.notmode,
            })
        } else {
            Err(Box::new(self))
        }
    }

    fn into_key_binding(self) -> Result<KeyBinding, Box<Self>> {
        if let Some(key) = self.key {
            Ok(KeyBinding {
                trigger: key,
                mods: self.mods,
                action: self.action,
                mode: self.mode,
                notmode: self.notmode,
            })
        } else {
            Err(Box::new(self))
        }
    }
}

impl<'a> Deserialize<'a> for RawBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        const FIELDS: &[&str] = &["key", "mods", "mode", "action", "chars", "mouse", "command"];

        enum Field {
            Key,
            Mods,
            Mode,
            Action,
            Chars,
            Mouse,
            Command,
        }

        impl<'a> Deserialize<'a> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Field, D::Error>
            where
                D: Deserializer<'a>,
            {
                struct FieldVisitor;

                impl Visitor<'_> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("binding fields")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Field, E>
                    where
                        E: de::Error,
                    {
                        match value.to_ascii_lowercase().as_str() {
                            "key" => Ok(Field::Key),
                            "mods" => Ok(Field::Mods),
                            "mode" => Ok(Field::Mode),
                            "action" => Ok(Field::Action),
                            "chars" => Ok(Field::Chars),
                            "mouse" => Ok(Field::Mouse),
                            "command" => Ok(Field::Command),
                            _ => Err(E::unknown_field(value, FIELDS)),
                        }
                    }
                }

                deserializer.deserialize_str(FieldVisitor)
            }
        }

        struct RawBindingVisitor;
        impl<'a> Visitor<'a> for RawBindingVisitor {
            type Value = RawBinding;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("binding specification")
            }

            fn visit_map<V>(self, mut map: V) -> Result<RawBinding, V::Error>
            where
                V: MapAccess<'a>,
            {
                let mut mods: Option<ModifiersState> = None;
                let mut key: Option<BindingKey> = None;
                let mut chars: Option<String> = None;
                let mut action: Option<Action> = None;
                let mut mode: Option<BindingMode> = None;
                let mut not_mode: Option<BindingMode> = None;
                let mut mouse: Option<MouseEvent> = None;
                let mut command: Option<Program> = None;

                use de::Error;

                while let Some(struct_key) = map.next_key::<Field>()? {
                    match struct_key {
                        Field::Key => {
                            if key.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("key"));
                            }

                            let value = map.next_value::<SerdeValue>()?;
                            match value.as_integer() {
                                Some(scancode) => match u32::try_from(scancode) {
                                    Ok(scancode) => {
                                        key = Some(BindingKey::Scancode(KeyCode::from_scancode(
                                            scancode,
                                        )))
                                    },
                                    Err(_) => {
                                        return Err(<V::Error as Error>::custom(format!(
                                            "Invalid key binding, scancode is too big: {scancode}"
                                        )));
                                    },
                                },
                                None => {
                                    key = Some(
                                        BindingKey::deserialize(value).map_err(V::Error::custom)?,
                                    )
                                },
                            }
                        },
                        Field::Mods => {
                            if mods.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mods"));
                            }

                            mods = Some(map.next_value::<ModsWrapper>()?.into_inner());
                        },
                        Field::Mode => {
                            if mode.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mode"));
                            }

                            let mode_deserializer = map.next_value::<ModeWrapper>()?;
                            mode = Some(mode_deserializer.mode);
                            not_mode = Some(mode_deserializer.not_mode);
                        },
                        Field::Action => {
                            if action.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("action"));
                            }

                            let value = map.next_value::<SerdeValue>()?;

                            action = if let Ok(vi_action) = ViAction::deserialize(value.clone()) {
                                Some(vi_action.into())
                            } else if let Ok(vi_motion) = SerdeViMotion::deserialize(value.clone())
                            {
                                Some(vi_motion.0.into())
                            } else if let Ok(search_action) =
                                SearchAction::deserialize(value.clone())
                            {
                                Some(search_action.into())
                            } else if let Ok(mouse_action) = MouseAction::deserialize(value.clone())
                            {
                                Some(mouse_action.into())
                            } else {
                                match Action::deserialize(value.clone()).map_err(V::Error::custom) {
                                    Ok(action) => Some(action),
                                    Err(err) => {
                                        let value = match value {
                                            SerdeValue::String(string) => string,
                                            _ => return Err(err),
                                        };
                                        return Err(V::Error::custom(format!(
                                            "unknown keyboard action `{value}`"
                                        )));
                                    },
                                }
                            };
                        },
                        Field::Chars => {
                            if chars.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("chars"));
                            }

                            chars = Some(map.next_value()?);
                        },
                        Field::Mouse => {
                            if mouse.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mouse"));
                            }

                            mouse = Some(map.next_value::<MouseEvent>()?);
                        },
                        Field::Command => {
                            if command.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("command"));
                            }

                            command = Some(map.next_value::<Program>()?);
                        },
                    }
                }

                let mode = mode.unwrap_or_else(BindingMode::empty);
                let not_mode = not_mode.unwrap_or_else(BindingMode::empty);
                let mods = mods.unwrap_or_default();

                let action = match (action, chars, command) {
                    (Some(action @ Action::ViMotion(_)), None, None)
                    | (Some(action @ Action::Vi(_)), None, None) => action,
                    (Some(action @ Action::Search(_)), None, None) => action,
                    (Some(action @ Action::Mouse(_)), None, None) => {
                        if mouse.is_none() {
                            return Err(V::Error::custom(format!(
                                "action `{action}` is only available for mouse bindings",
                            )));
                        }
                        action
                    },
                    (Some(action), None, None) => action,
                    (None, Some(chars), None) => Action::Esc(chars),
                    (None, None, Some(cmd)) => Action::Command(cmd),
                    _ => {
                        return Err(V::Error::custom(
                            "must specify exactly one of chars, action or command",
                        ));
                    },
                };

                if mouse.is_none() && key.is_none() {
                    return Err(V::Error::custom("bindings require mouse button or key"));
                }

                Ok(RawBinding { mode, notmode: not_mode, action, key, mouse, mods })
            }
        }

        deserializer.deserialize_struct("RawBinding", FIELDS, RawBindingVisitor)
    }
}

impl<'a> Deserialize<'a> for MouseBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let raw = RawBinding::deserialize(deserializer)?;
        raw.into_mouse_binding()
            .map_err(|_| D::Error::custom("expected mouse binding, got key binding"))
    }
}

impl<'a> Deserialize<'a> for KeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let raw = RawBinding::deserialize(deserializer)?;
        raw.into_key_binding()
            .map_err(|_| D::Error::custom("expected key binding, got mouse binding"))
    }
}

#[derive(SerdeReplace, Debug, Copy, Clone, Eq, PartialEq)]
pub struct SerdeViMotion(ViMotion);

impl<'de> Deserialize<'de> for SerdeViMotion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(StringVisitor)?;
        ViMotion::deserialize(SerdeValue::String(value))
            .map(SerdeViMotion)
            .map_err(de::Error::custom)
    }
}

/// Newtype for implementing deserialize on winit Mods.
///
/// Our deserialize impl wouldn't be covered by a derive(Deserialize); see the
/// impl below.
#[derive(SerdeReplace, Debug, Copy, Clone, Hash, Default, Eq, PartialEq)]
pub struct ModsWrapper(pub ModifiersState);

impl ModsWrapper {
    pub fn into_inner(self) -> ModifiersState {
        self.0
    }
}

impl<'a> de::Deserialize<'a> for ModsWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'a>,
    {
        struct ModsVisitor;

        impl Visitor<'_> for ModsVisitor {
            type Value = ModsWrapper;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("None or a subset of Shift|Control|Super|Command|Alt|Option")
            }

            fn visit_str<E>(self, value: &str) -> Result<ModsWrapper, E>
            where
                E: de::Error,
            {
                let mut res = ModifiersState::empty();
                for modifier in value.split('|') {
                    match modifier.trim().to_lowercase().as_str() {
                        "command" | "super" => res.insert(ModifiersState::SUPER),
                        "shift" => res.insert(ModifiersState::SHIFT),
                        "alt" | "option" => res.insert(ModifiersState::ALT),
                        "control" => res.insert(ModifiersState::CONTROL),
                        "none" => (),
                        _ => return Err(E::invalid_value(Unexpected::Str(modifier), &self)),
                    }
                }

                Ok(ModsWrapper(res))
            }
        }

        deserializer.deserialize_str(ModsVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use winit::keyboard::ModifiersState;

    type MockBinding = Binding<usize>;

    impl Default for MockBinding {
        fn default() -> Self {
            Self {
                mods: Default::default(),
                action: Action::None,
                mode: BindingMode::empty(),
                notmode: BindingMode::empty(),
                trigger: Default::default(),
            }
        }
    }

    #[test]
    fn binding_matches_itself() {
        let binding = MockBinding::default();
        let identical_binding = MockBinding::default();

        assert!(binding.triggers_match(&identical_binding));
        assert!(identical_binding.triggers_match(&binding));
    }

    #[test]
    fn binding_matches_different_action() {
        let binding = MockBinding::default();
        let different_action =
            MockBinding { action: Action::ClearHistory, ..MockBinding::default() };

        assert!(binding.triggers_match(&different_action));
        assert!(different_action.triggers_match(&binding));
    }

    #[test]
    fn mods_binding_requires_strict_match() {
        let superset_mods = MockBinding { mods: ModifiersState::all(), ..MockBinding::default() };
        let subset_mods = MockBinding { mods: ModifiersState::ALT, ..MockBinding::default() };

        assert!(!superset_mods.triggers_match(&subset_mods));
        assert!(!subset_mods.triggers_match(&superset_mods));
    }

    #[test]
    fn binding_matches_identical_mode() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_without_mode_matches_any_mode() {
        let b1 = MockBinding::default();
        let b2 = MockBinding {
            mode: BindingMode::APP_KEYPAD,
            notmode: BindingMode::ALT_SCREEN,
            ..MockBinding::default()
        };

        assert!(b1.triggers_match(&b2));
    }

    #[test]
    fn binding_with_mode_matches_empty_mode() {
        let b1 = MockBinding {
            mode: BindingMode::APP_KEYPAD,
            notmode: BindingMode::ALT_SCREEN,
            ..MockBinding::default()
        };
        let b2 = MockBinding::default();

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_modes() {
        let b1 = MockBinding {
            mode: BindingMode::ALT_SCREEN | BindingMode::APP_KEYPAD,
            ..MockBinding::default()
        };
        let b2 = MockBinding { mode: BindingMode::APP_KEYPAD, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_partial_intersection() {
        let b1 = MockBinding {
            mode: BindingMode::ALT_SCREEN | BindingMode::APP_KEYPAD,
            ..MockBinding::default()
        };
        let b2 = MockBinding {
            mode: BindingMode::APP_KEYPAD | BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_mismatches_notmode() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { notmode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        assert!(!b1.triggers_match(&b2));
        assert!(!b2.triggers_match(&b1));
    }

    #[test]
    fn binding_mismatches_unrelated() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { mode: BindingMode::APP_KEYPAD, ..MockBinding::default() };

        assert!(!b1.triggers_match(&b2));
        assert!(!b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_notmodes() {
        let subset_notmodes = MockBinding {
            notmode: BindingMode::VI | BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };
        let superset_notmodes =
            MockBinding { notmode: BindingMode::APP_CURSOR, ..MockBinding::default() };

        assert!(subset_notmodes.triggers_match(&superset_notmodes));
        assert!(superset_notmodes.triggers_match(&subset_notmodes));
    }

    #[test]
    fn binding_matches_mode_notmode() {
        let b1 = MockBinding {
            mode: BindingMode::VI,
            notmode: BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };
        let b2 = MockBinding { notmode: BindingMode::APP_CURSOR, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_trigger_input() {
        let binding = MockBinding { trigger: 13, ..MockBinding::default() };

        let mods = binding.mods;
        let mode = binding.mode;

        assert!(binding.is_triggered_by(mode, mods, &13));
        assert!(!binding.is_triggered_by(mode, mods, &32));
    }

    #[test]
    fn binding_trigger_mods() {
        let binding = MockBinding {
            mods: ModifiersState::ALT | ModifiersState::SUPER,
            ..MockBinding::default()
        };

        let superset_mods = ModifiersState::all();
        let subset_mods = ModifiersState::empty();

        let t = binding.trigger;
        let mode = binding.mode;

        assert!(binding.is_triggered_by(mode, binding.mods, &t));
        assert!(!binding.is_triggered_by(mode, superset_mods, &t));
        assert!(!binding.is_triggered_by(mode, subset_mods, &t));
    }

    #[test]
    fn binding_trigger_modes() {
        let binding = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        let t = binding.trigger;
        let mods = binding.mods;

        assert!(!binding.is_triggered_by(BindingMode::VI, mods, &t));
        assert!(binding.is_triggered_by(BindingMode::ALT_SCREEN, mods, &t));
        assert!(binding.is_triggered_by(BindingMode::ALT_SCREEN | BindingMode::VI, mods, &t));
    }

    #[test]
    fn binding_trigger_notmodes() {
        let binding = MockBinding { notmode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        let t = binding.trigger;
        let mods = binding.mods;

        assert!(binding.is_triggered_by(BindingMode::VI, mods, &t));
        assert!(!binding.is_triggered_by(BindingMode::ALT_SCREEN, mods, &t));
        assert!(!binding.is_triggered_by(BindingMode::ALT_SCREEN | BindingMode::VI, mods, &t));
    }
}
