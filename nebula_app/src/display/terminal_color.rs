use std::collections::HashMap;

use nebula_terminal::term::color::Colors;
use nebula_terminal::vte::ansi::Color;

use super::color::Rgb;
use super::design_tokens::terminal_feedback::FIXED_TEXT_MIN_CONTRAST;

const CACHE_ENTRY_LIMIT: usize = 4096;
const SURFACE_CHANNEL_TOLERANCE: u8 = 32;
const SURFACE_REMAP_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ColorPairKey {
    foreground: u32,
    background: u32,
    theme_foreground: u32,
    theme_background: u32,
}

/// Resolves application-owned colors against the active terminal theme.
///
/// The terminal grid keeps the exact PTY colors. Only the draw-time result is
/// cached, so changing the theme immediately recolors existing scrollback.
#[derive(Default)]
pub(super) struct TerminalColorResolver {
    cache: HashMap<ColorPairKey, Rgb>,
    surface_remaps: Vec<SurfaceRemap>,
}

#[derive(Clone, Copy, Debug)]
struct SurfaceRemap {
    source: Rgb,
    target: Rgb,
}

impl TerminalColorResolver {
    pub(super) fn resolve_background(&self, background: Rgb, fixed_background: bool) -> Rgb {
        if !fixed_background {
            return background;
        }

        self.surface_remaps
            .iter()
            .filter_map(|remap| {
                surface_distance(background, remap.source)
                    .map(|distance| (distance, remap_surface(background, *remap)))
            })
            .min_by_key(|(distance, _)| *distance)
            .map_or(background, |(_, resolved)| resolved)
    }

    pub(super) fn resolve_foreground(
        &mut self,
        foreground: Rgb,
        background: Rgb,
        fixed_foreground: bool,
        theme_foreground: Rgb,
        theme_background: Rgb,
    ) -> Rgb {
        if !fixed_foreground {
            return foreground;
        }

        let key = ColorPairKey {
            foreground: packed(foreground),
            background: packed(background),
            theme_foreground: packed(theme_foreground),
            theme_background: packed(theme_background),
        };
        if let Some(resolved) = self.cache.get(&key) {
            return *resolved;
        }

        let resolved = ensure_contrast(
            foreground,
            background,
            theme_foreground,
            theme_background,
            FIXED_TEXT_MIN_CONTRAST,
        );

        // 任意程序都能连续生成 RGB；设置上限防止恶意/动画输出撑大常驻缓存。
        if self.cache.len() >= CACHE_ENTRY_LIMIT {
            self.cache.clear();
        }
        self.cache.insert(key, resolved);
        resolved
    }

    pub(super) fn theme_changed(&mut self, old_background: Rgb, new_background: Rgb) {
        self.cache.clear();
        for remap in &mut self.surface_remaps {
            remap.target = new_background;
        }
        self.surface_remaps.retain(|remap| remap.source != new_background);
        if old_background != new_background {
            if let Some(remap) =
                self.surface_remaps.iter_mut().find(|remap| remap.source == old_background)
            {
                remap.target = new_background;
            } else {
                self.surface_remaps
                    .push(SurfaceRemap { source: old_background, target: new_background });
            }
            if self.surface_remaps.len() > SURFACE_REMAP_LIMIT {
                let excess = self.surface_remaps.len() - SURFACE_REMAP_LIMIT;
                self.surface_remaps.drain(..excess);
            }
        }
    }
}

fn surface_distance(color: Rgb, source: Rgb) -> Option<u16> {
    let dr = color.r.abs_diff(source.r);
    let dg = color.g.abs_diff(source.g);
    let db = color.b.abs_diff(source.b);
    (dr.max(dg).max(db) <= SURFACE_CHANNEL_TOLERANCE)
        .then_some(u16::from(dr) + u16::from(dg) + u16::from(db))
}

fn remap_surface(color: Rgb, remap: SurfaceRemap) -> Rgb {
    let crosses_polarity = is_light(remap.source) != is_light(remap.target);
    let channel = |value: u8, source: u8, target: u8| {
        let delta = i16::from(value) - i16::from(source);
        let delta = if crosses_polarity { -delta } else { delta };
        (i16::from(target) + delta).clamp(0, 255) as u8
    };
    Rgb::new(
        channel(color.r, remap.source.r, remap.target.r),
        channel(color.g, remap.source.g, remap.target.g),
        channel(color.b, remap.source.b, remap.target.b),
    )
}

fn is_light(color: Rgb) -> bool {
    let luminance = u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114;
    luminance >= 128_000
}

pub(super) fn is_fixed_color(color: Color, overrides: &Colors) -> bool {
    match color {
        Color::Spec(_) => true,
        Color::Named(name) => overrides[name].is_some(),
        Color::Indexed(index) => {
            overrides[index as usize].is_some()
                // 16..=23 是 Nebula 随主题重建的 powerline 动态槽位。
                || index >= 24
        },
    }
}

fn ensure_contrast(
    foreground: Rgb,
    background: Rgb,
    theme_foreground: Rgb,
    theme_background: Rgb,
    minimum: f64,
) -> Rgb {
    if foreground.contrast(*background) >= minimum {
        return foreground;
    }

    let target = if theme_foreground.contrast(*background) >= theme_background.contrast(*background)
    {
        theme_foreground
    } else {
        theme_background
    };
    binary_mix(foreground, target, |candidate| candidate.contrast(*background) >= minimum)
}

fn binary_mix(start: Rgb, end: Rgb, reached: impl Fn(Rgb) -> bool) -> Rgb {
    if !reached(end) {
        return end;
    }

    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..10 {
        let middle = (low + high) / 2.0;
        if reached(mix(start, end, middle)) {
            high = middle;
        } else {
            low = middle;
        }
    }
    mix(start, end, high)
}

fn mix(start: Rgb, end: Rgb, amount: f64) -> Rgb {
    let channel =
        |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * amount).round().clamp(0.0, 255.0) as u8;
    Rgb::new(channel(start.r, end.r), channel(start.g, end.g), channel(start.b, end.b))
}

fn packed(color: Rgb) -> u32 {
    u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)
}

#[cfg(test)]
mod tests {
    use nebula_terminal::term::color::Colors;
    use nebula_terminal::vte::ansi::{Color, NamedColor, Rgb as VteRgb};

    use super::{TerminalColorResolver, is_fixed_color};
    use crate::display::color::Rgb;
    use crate::display::design_tokens::terminal_feedback::FIXED_TEXT_MIN_CONTRAST;

    const LIGHT_FOREGROUND: Rgb = Rgb::new(36, 41, 47);
    const LIGHT_BACKGROUND: Rgb = Rgb::new(255, 255, 255);
    const DARK_FOREGROUND: Rgb = Rgb::new(228, 231, 246);
    const DARK_BACKGROUND: Rgb = Rgb::new(15, 17, 26);

    #[test]
    fn fixed_foreground_reaches_contrast_without_replacing_readable_colors() {
        let mut resolver = TerminalColorResolver::default();
        let pale = Rgb::new(245, 240, 190);
        let resolved = resolver.resolve_foreground(
            pale,
            LIGHT_BACKGROUND,
            true,
            LIGHT_FOREGROUND,
            LIGHT_BACKGROUND,
        );
        assert!(resolved.contrast(*LIGHT_BACKGROUND) >= FIXED_TEXT_MIN_CONTRAST);
        assert_ne!(resolved, pale);

        let readable = Rgb::new(154, 103, 0);
        let resolved = resolver.resolve_foreground(
            readable,
            LIGHT_BACKGROUND,
            true,
            LIGHT_FOREGROUND,
            LIGHT_BACKGROUND,
        );
        assert_eq!(resolved, readable);
    }

    #[test]
    fn application_background_is_never_recolored() {
        let mut resolver = TerminalColorResolver::default();
        let brand_surface = Rgb::new(218, 119, 87);
        let foreground = resolver.resolve_foreground(
            brand_surface,
            LIGHT_BACKGROUND,
            false,
            LIGHT_FOREGROUND,
            LIGHT_BACKGROUND,
        );
        assert_eq!(foreground, brand_surface);
    }

    #[test]
    fn old_theme_surfaces_follow_a_light_dark_switch() {
        let mut resolver = TerminalColorResolver::default();
        resolver.theme_changed(LIGHT_BACKGROUND, DARK_BACKGROUND);

        assert_eq!(
            resolver.resolve_background(Rgb::new(244, 244, 244), true),
            Rgb::new(26, 28, 37)
        );
        let brand_surface = Rgb::new(218, 119, 87);
        assert_eq!(resolver.resolve_background(brand_surface, true), brand_surface);
        assert_eq!(
            resolver.resolve_background(Rgb::new(244, 244, 244), false),
            Rgb::new(244, 244, 244)
        );
    }

    #[test]
    fn repeated_theme_switches_retarget_old_tabs_without_touching_current_output() {
        let mut resolver = TerminalColorResolver::default();
        let second_dark = Rgb::new(30, 33, 30);
        resolver.theme_changed(LIGHT_BACKGROUND, DARK_BACKGROUND);
        resolver.theme_changed(DARK_BACKGROUND, second_dark);
        assert_eq!(
            resolver.resolve_background(Rgb::new(244, 244, 244), true),
            Rgb::new(41, 44, 41)
        );

        resolver.theme_changed(second_dark, LIGHT_BACKGROUND);
        assert_eq!(
            resolver.resolve_background(Rgb::new(244, 244, 244), true),
            Rgb::new(244, 244, 244)
        );
        assert_eq!(
            resolver.resolve_background(Rgb::new(41, 44, 41), true),
            Rgb::new(244, 244, 244)
        );
    }

    #[test]
    fn same_saved_rgb_pair_recomputes_for_the_active_theme() {
        let foreground = Rgb::new(215, 220, 230);
        let light_background = LIGHT_BACKGROUND;
        let dark_background = DARK_BACKGROUND;
        let mut resolver = TerminalColorResolver::default();
        let light = resolver.resolve_foreground(
            foreground,
            light_background,
            true,
            LIGHT_FOREGROUND,
            LIGHT_BACKGROUND,
        );
        let dark = resolver.resolve_foreground(
            foreground,
            dark_background,
            true,
            DARK_FOREGROUND,
            DARK_BACKGROUND,
        );
        assert_ne!(light, dark);
        assert!(light.contrast(*light_background) >= FIXED_TEXT_MIN_CONTRAST);
        assert!(dark.contrast(*dark_background) >= FIXED_TEXT_MIN_CONTRAST);
    }

    #[test]
    fn fixed_color_detection_excludes_theme_owned_slots() {
        let mut overrides = Colors::default();
        assert!(is_fixed_color(Color::Spec(VteRgb { r: 1, g: 2, b: 3 }), &overrides));
        assert!(is_fixed_color(Color::Indexed(24), &overrides));
        assert!(!is_fixed_color(Color::Indexed(16), &overrides));
        assert!(!is_fixed_color(Color::Named(NamedColor::Red), &overrides));

        overrides[NamedColor::Red] = Some(VteRgb { r: 1, g: 2, b: 3 });
        assert!(is_fixed_color(Color::Named(NamedColor::Red), &overrides));
    }
}
