use std::time::Duration;

use serde::Serialize;

use nebula_config_derive::ConfigDeserialize;

use crate::config::ui_config::Program;
use crate::display::color::Rgb;

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BellConfig {
    /// Visual bell animation function.
    pub animation: BellAnimation,

    /// Command to run on bell.
    pub command: Option<Program>,

    /// Play an audible system sound when the bell rings (BEL / `\a`).
    ///
    /// AI CLIs ring BEL when a turn finishes or they need input, so this is
    /// the "claude/codex needs you" cue even while Nebula is focused on a
    /// different tab — the visual bell and background-tab toast never cover
    /// that case. Plays regardless of focus and is throttled in
    /// [`crate::platform::beep`] so a bell-happy program cannot machine-gun
    /// it. On by default, matching a conventional terminal; set
    /// `bell.audible = false` to silence it.
    pub audible: bool,

    /// Visual bell flash color.
    pub color: Rgb,

    /// Visual bell duration in milliseconds.
    duration: u16,
}

impl Default for BellConfig {
    fn default() -> Self {
        Self {
            color: Rgb::new(255, 255, 255),
            animation: Default::default(),
            command: Default::default(),
            audible: true,
            duration: Default::default(),
        }
    }
}

impl BellConfig {
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration as u64)
    }
}

/// `VisualBellAnimations` are modeled after a subset of CSS transitions and Robert
/// Penner's Easing Functions.
#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BellAnimation {
    // CSS animation.
    Ease,
    // CSS animation.
    EaseOut,
    // Penner animation.
    EaseOutSine,
    // Penner animation.
    EaseOutQuad,
    // Penner animation.
    EaseOutCubic,
    // Penner animation.
    EaseOutQuart,
    // Penner animation.
    EaseOutQuint,
    // Penner animation.
    EaseOutExpo,
    // Penner animation.
    EaseOutCirc,
    // Penner animation.
    #[default]
    Linear,
}
