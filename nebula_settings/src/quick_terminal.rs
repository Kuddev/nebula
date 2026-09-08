//! Persisted quick-terminal preferences; native monitor/window adapters live in the UI.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuickTerminalMode {
    #[default]
    Dedicated,
    Existing,
}

impl QuickTerminalMode {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value {
            "dedicated" => Some(Self::Dedicated),
            "existing" => Some(Self::Existing),
            _ => None,
        }
    }

    pub const fn settings_value(self) -> &'static str {
        match self {
            Self::Dedicated => "dedicated",
            Self::Existing => "existing",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuickTerminalSize {
    pub width: f32,
    pub height: f32,
}

impl QuickTerminalSize {
    pub fn new(width: f32, height: f32) -> Option<Self> {
        (width.is_finite()
            && height.is_finite()
            && width >= 320.0
            && height >= 160.0
            && width <= 16384.0
            && height <= 16384.0)
            .then_some(Self { width, height })
    }

    pub fn fit(self, width: f32, height: f32) -> Self {
        Self { width: self.width.min(width.max(1.0)), height: self.height.min(height.max(1.0)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawSettings, RuntimeSettings};

    #[test]
    fn preferences_round_trip_and_invalid_dimensions_fall_back() {
        let raw = RawSettings::from_text(
            "quick_terminal_mode=existing\nquick_terminal_width=900\nquick_terminal_height=400\n",
        );
        let settings = RuntimeSettings::from_raw(&raw);
        assert_eq!(settings.quick_terminal_mode, QuickTerminalMode::Existing);
        assert_eq!(settings.quick_terminal_size, QuickTerminalSize::new(900.0, 400.0));
        assert_eq!(QuickTerminalMode::Existing.settings_value(), "existing");
        assert!(QuickTerminalSize::new(f32::NAN, 400.0).is_none());
        assert!(QuickTerminalSize::new(0.0, 400.0).is_none());
        assert!(QuickTerminalSize::new(900.0, f32::INFINITY).is_none());
        let settings = RuntimeSettings::from_raw(&RawSettings::from_text(
            "quick_terminal_mode=unknown\nquick_terminal_width=900\n",
        ));
        assert_eq!(settings.quick_terminal_mode, QuickTerminalMode::Dedicated);
        assert_eq!(settings.quick_terminal_size, None);
        assert_eq!(
            QuickTerminalSize::new(1800.0, 900.0).unwrap().fit(1280.0, 720.0),
            QuickTerminalSize { width: 1280.0, height: 720.0 }
        );
    }
}
