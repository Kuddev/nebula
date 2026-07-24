//! Central opacity policy for the terminal surface and persistent chrome.
//!
//! Terminal opacity is an exact user preference and — by explicit user ruling
//! (2026-07-23) — the WHOLE window follows it as one body: shell backdrop,
//! title bar, sidebar and the borders around the terminal card all share the
//! same alpha, so adjusting the slider never splits the frame from its
//! contents. Text/icons stay opaque, which is what keeps the controls
//! recoverable even at 0%. Modals keep a readability floor: a confirm dialog
//! must stay legible over any wallpaper.

use crate::renderer::ui::Rgba;

pub(crate) const MODAL_OPACITY_FLOOR: f32 = 0.92;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SurfaceOpacityPolicy {
    pub terminal: f32,
    pub chrome: f32,
    pub modal: f32,
}

impl SurfaceOpacityPolicy {
    pub(crate) fn new(user_opacity: f32) -> Self {
        let terminal = user_opacity.clamp(0.0, 1.0);
        Self { terminal, chrome: terminal, modal: terminal.max(MODAL_OPACITY_FLOOR) }
    }

    /// Preserve the theme token's authored alpha while applying a surface
    /// multiplier. This avoids turning intentionally soft panels fully opaque
    /// when the user's terminal opacity is 100%.
    pub(crate) fn chrome_color(self, color: Rgba) -> Rgba {
        let alpha = (color.a as f32 * self.chrome).round().clamp(0.0, 255.0) as u8;
        Rgba::new(color.r, color.g, color.b, alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::{MODAL_OPACITY_FLOOR, SurfaceOpacityPolicy};

    #[test]
    fn whole_window_follows_user_while_modals_keep_readability_floor() {
        let policy = SurfaceOpacityPolicy::new(0.0);
        assert_eq!(policy.terminal, 0.0);
        assert_eq!(policy.chrome, 0.0);
        assert_eq!(policy.modal, MODAL_OPACITY_FLOOR);
    }

    #[test]
    fn fully_opaque_preference_keeps_every_surface_opaque() {
        let policy = SurfaceOpacityPolicy::new(1.0);
        assert_eq!(policy.terminal, 1.0);
        assert_eq!(policy.chrome, 1.0);
        assert_eq!(policy.modal, 1.0);
    }
}
