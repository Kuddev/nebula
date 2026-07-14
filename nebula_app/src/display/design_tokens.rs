//! Nebula's shared UI contract: spacing, typography, motion and control state.
//!
//! Values are logical pixels/scales. Callers apply the window DPI exactly once
//! at layout time; draw, hover, focus and hit-testing must derive from the same
//! resulting rectangle.

// Token adoption is intentionally incremental: keeping the complete scale in
// one catalog prevents new local magic numbers while existing screens migrate.
#![allow(dead_code)]

pub mod space {
    pub const XXS: f32 = 4.0;
    pub const XS: f32 = 8.0;
    pub const S: f32 = 12.0;
    pub const M: f32 = 16.0;
    pub const L: f32 = 24.0;
    pub const XL: f32 = 32.0;
}

pub mod type_scale {
    /// 11px-style sidebar/group caption from the Maple base face.
    pub const SECTION_CAPTION: f32 = 0.75;
    /// Supporting/empty-state copy: subordinate without becoming unreadable.
    pub const SUPPORTING: f32 = 0.80;
    pub const BODY: f32 = 1.0;
    pub const DIALOG_TITLE: f32 = 1.35;
}

pub mod control {
    pub const ICON_BUTTON: f32 = 20.0;
    pub const MIN_HIT_TARGET: f32 = 32.0;
    /// Dense rows used by command/search pickers where several choices must
    /// remain visible without making the input visually dominate the content.
    pub const COMPACT_ROW: f32 = 38.0;
    pub const ROW: f32 = 44.0;
    pub const RADIUS: f32 = 8.0;
    pub const HAIRLINE: f32 = 1.0;
}

/// Z-axis treatment for transient surfaces rendered above normal content.
pub mod elevation {
    /// Soft edge around a floating menu/card.
    pub const FLOATING_BLUR: f32 = 12.0;
    /// A small downward bias makes the card read as lifted rather than glowing.
    pub const FLOATING_OFFSET_Y: f32 = 4.0;
    pub const FLOATING_SHADOW_ALPHA_LIGHT: u8 = 54;
    pub const FLOATING_SHADOW_ALPHA_DARK: u8 = 86;
}

/// Terminal-grid feedback colors are rendered through the cell/rect alpha
/// pipeline. Thin cursors need more opacity than a full block to remain visible.
pub mod terminal_feedback {
    /// Blend the terminal hue anchor toward theme-neutral ink before applying
    /// alpha; this preserves hue continuity without an electric saturation.
    pub const ANCHOR_NEUTRAL_MIX_LIGHT: f32 = 0.28;
    pub const ANCHOR_NEUTRAL_MIX_DARK: f32 = 0.18;
    pub const BLOCK_CURSOR_ALPHA_LIGHT: f32 = 0.20;
    pub const BLOCK_CURSOR_ALPHA_DARK: f32 = 0.30;
    pub const STROKE_CURSOR_ALPHA_LIGHT: f32 = 0.72;
    pub const STROKE_CURSOR_ALPHA_DARK: f32 = 0.82;
    pub const SELECTION_ALPHA_LIGHT: f32 = 0.15;
    pub const SELECTION_ALPHA_DARK: f32 = 0.22;
}

pub mod motion {
    use std::time::Duration;

    pub const MENU_OPEN: Duration = Duration::from_millis(120);
    pub const MENU_CLOSE: Duration = Duration::from_millis(90);
    pub const HOVER: Duration = Duration::from_millis(150);
    /// CSS-like emphasized-decelerate curve used by short reveal motion.
    pub const EMPHASIZED_DECELERATE: [f32; 4] = [0.16, 1.0, 0.3, 1.0];
}

/// Required states for every self-drawn control. Empty/loading/error describe
/// content surfaces; pointer/keyboard controls use the first five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Default,
    Hover,
    Focus,
    Active,
    Disabled,
    Empty,
    Loading,
    Error,
}

impl ControlState {
    /// Persistent/blocked states outrank transient pointer feedback.
    pub fn interactive(disabled: bool, active: bool, focused: bool, hovered: bool) -> Self {
        if disabled {
            Self::Disabled
        } else if active {
            Self::Active
        } else if focused {
            Self::Focus
        } else if hovered {
            Self::Hover
        } else {
            Self::Default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ControlState;

    #[test]
    fn persistent_control_states_outrank_hover() {
        assert_eq!(ControlState::interactive(true, true, true, true), ControlState::Disabled);
        assert_eq!(ControlState::interactive(false, true, true, true), ControlState::Active);
        assert_eq!(ControlState::interactive(false, false, true, true), ControlState::Focus);
        assert_eq!(ControlState::interactive(false, false, false, true), ControlState::Hover);
    }
}
