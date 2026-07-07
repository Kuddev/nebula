use serde::{Deserialize, Deserializer, Serialize};

use nebula_config_derive::{ConfigDeserialize, SerdeReplace};

use crate::config::bindings::{self, MouseBinding};
use crate::config::ui_config;

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Mouse {
    /// Hide the mouse cursor while typing; any mouse movement shows it again.
    pub hide_when_typing: bool,
    /// In a split, move keyboard focus to whichever pane the mouse hovers over
    /// (only within the focused window). Default off.
    pub focus_follows_mouse: bool,
    #[serde(skip_serializing)]
    pub bindings: MouseBindings,
}

impl Default for Mouse {
    fn default() -> Self {
        Self {
            // On by default: the user asked for the "hide mouse while typing"
            // behavior. Set `mouse.hide_when_typing = false` to disable.
            hide_when_typing: true,
            focus_follows_mouse: false,
            bindings: MouseBindings::default(),
        }
    }
}

#[derive(SerdeReplace, Clone, Debug, PartialEq, Eq)]
pub struct MouseBindings(pub Vec<MouseBinding>);

impl Default for MouseBindings {
    fn default() -> Self {
        Self(bindings::default_mouse_bindings())
    }
}

impl<'de> Deserialize<'de> for MouseBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(ui_config::deserialize_bindings(deserializer, Self::default().0)?))
    }
}
