//! 正式 GPUI 产品仍会复用少量与 OpenGL 无关的显示值类型。
//!
//! 这里刻意不暴露旧 `Renderer`、GL shader 或 crossfont 栅格器；这些类型
//! 只是配置和布局合同，供 GPUI 与 `legacy-shell` 各自的绘制后端消费。

pub mod image {
    /// Wallpaper sizing modes.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub enum BackgroundImageFit {
        Fill,
        Uniform,
        #[default]
        UniformToFill,
        None,
    }

    impl BackgroundImageFit {
        pub fn parse(value: &str) -> Option<Self> {
            match value.trim().to_ascii_lowercase().as_str() {
                "fill" | "stretch" => Some(Self::Fill),
                "uniform" | "contain" => Some(Self::Uniform),
                "uniform_to_fill" | "uniformtofill" | "cover" => Some(Self::UniformToFill),
                "none" | "native" => Some(Self::None),
                _ => None,
            }
        }

        pub const fn settings_value(self) -> &'static str {
            match self {
                Self::Fill => "fill",
                Self::Uniform => "uniform",
                Self::UniformToFill => "uniform_to_fill",
                Self::None => "none",
            }
        }
    }

    /// Anchor used when the fitted wallpaper is larger or smaller than the window.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub enum BackgroundImageAlignment {
        TopLeft,
        Top,
        TopRight,
        Left,
        #[default]
        Center,
        Right,
        BottomLeft,
        Bottom,
        BottomRight,
    }

    impl BackgroundImageAlignment {
        pub fn parse(value: &str) -> Option<Self> {
            match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
                "top-left" => Some(Self::TopLeft),
                "top" => Some(Self::Top),
                "top-right" => Some(Self::TopRight),
                "left" => Some(Self::Left),
                "center" | "centre" => Some(Self::Center),
                "right" => Some(Self::Right),
                "bottom-left" => Some(Self::BottomLeft),
                "bottom" => Some(Self::Bottom),
                "bottom-right" => Some(Self::BottomRight),
                _ => None,
            }
        }

        pub const fn settings_value(self) -> &'static str {
            match self {
                Self::TopLeft => "top_left",
                Self::Top => "top",
                Self::TopRight => "top_right",
                Self::Left => "left",
                Self::Center => "center",
                Self::Right => "right",
                Self::BottomLeft => "bottom_left",
                Self::Bottom => "bottom",
                Self::BottomRight => "bottom_right",
            }
        }

        pub const fn factors(self) -> (f32, f32) {
            match self {
                Self::TopLeft => (0.0, 0.0),
                Self::Top => (0.5, 0.0),
                Self::TopRight => (1.0, 0.0),
                Self::Left => (0.0, 0.5),
                Self::Center => (0.5, 0.5),
                Self::Right => (1.0, 0.5),
                Self::BottomLeft => (0.0, 1.0),
                Self::Bottom => (0.5, 1.0),
                Self::BottomRight => (1.0, 1.0),
            }
        }
    }
}

pub mod ui {
    /// Straight-alpha color shared by configuration and GPUI adapters.
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct Rgba {
        pub r: u8,
        pub g: u8,
        pub b: u8,
        pub a: u8,
    }

    impl Rgba {
        pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
            Self { r, g, b, a }
        }

        pub fn with_alpha(self, alpha: f32) -> Self {
            Self { a: (alpha.clamp(0.0, 1.0) * 255.0) as u8, ..self }
        }
    }
}
