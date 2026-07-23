use std::borrow::Cow;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::{cmp, mem};

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::{Dimensions, Indexed};
use nebula_terminal::index::{Column, Line, Point};
use nebula_terminal::selection::SelectionRange;
use nebula_terminal::term::cell::{Cell, Flags, Hyperlink};
use nebula_terminal::term::search::{Match, RegexSearch};
use nebula_terminal::term::{self, RenderableContent as TerminalContent, Term, TermMode};
use nebula_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use crate::config::UiConfig;
use crate::config::color::{InvertedCellColors, NEBULA_DEFAULT_CURSOR};
use crate::display::color::{CellRgb, DIM_FACTOR, List, Rgb};
use crate::display::design_tokens::terminal_feedback;
use crate::display::hint::{self, HintState};
use crate::display::terminal_color::{TerminalColorResolver, is_fixed_color};
use crate::display::{Display, SizeInfo};
use crate::event::SearchState;

/// Minimum contrast between a fixed cursor color and the cell's background.
pub const MIN_CURSOR_CONTRAST: f64 = 1.5;

/// Renderable terminal content.
///
/// This provides the terminal cursor and an iterator over all non-empty cells.
pub struct RenderableContent<'a> {
    terminal_content: TerminalContent<'a>,
    cursor: RenderableCursor,
    cursor_shape: CursorShape,
    cursor_point: Point<usize>,
    search: Option<HintMatches<'a>>,
    hint: Option<Hint<'a>>,
    config: &'a UiConfig,
    colors: &'a List,
    focused_match: Option<&'a Match>,
    size: &'a SizeInfo,
    theme_anchor: Rgb,
    theme_foreground: Rgb,
    theme_background: Rgb,
    color_resolver: &'a mut TerminalColorResolver,
    theme_is_light: bool,
    themed_selection: bool,
}

impl<'a> RenderableContent<'a> {
    pub fn new<T: EventListener>(
        config: &'a UiConfig,
        display: &'a mut Display,
        term: &'a Term<T>,
        search_state: &'a mut SearchState,
        size: &'a SizeInfo,
    ) -> Self {
        let search = search_state.dfas().map(|dfas| HintMatches::visible_regex_matches(term, dfas));
        let focused_match = search_state.focused_match();
        let terminal_content = term.renderable_content();
        let theme_is_light = display.nebula_theme.palette().is_light;
        let theme_foreground = display.colors[NamedColor::Foreground];
        let theme_background = display.colors[NamedColor::Background];
        let neutral_mix = if theme_is_light {
            terminal_feedback::ANCHOR_NEUTRAL_MIX_LIGHT
        } else {
            terminal_feedback::ANCHOR_NEUTRAL_MIX_DARK
        };
        let theme_anchor = mix_rgb(
            display.colors[NamedColor::Magenta],
            display.nebula_theme.skin().ink_dim,
            neutral_mix,
        );
        let themed_selection = config.colors.selection == InvertedCellColors::default();

        // Find terminal cursor shape.
        let cursor_shape = if terminal_content.cursor.shape == CursorShape::Hidden
            || display.cursor_hidden
            || search_state.regex().is_some()
            || display.ime.preedit().is_some()
        {
            CursorShape::Hidden
        } else if !term.is_focused && config.cursor.unfocused_hollow {
            CursorShape::HollowBlock
        } else {
            terminal_content.cursor.shape
        };

        // Convert terminal cursor point to viewport position.
        let cursor_point = terminal_content.cursor.point;
        let display_offset = terminal_content.display_offset;
        let cursor_point = term::point_to_viewport(display_offset, cursor_point).unwrap();

        let hint = if display.hint_state.active() {
            display.hint_state.update_matches(term);
            Some(Hint::from(&display.hint_state))
        } else {
            None
        };

        Self {
            colors: &display.colors,
            size,
            cursor: RenderableCursor::new_hidden(),
            terminal_content,
            focused_match,
            cursor_shape,
            cursor_point,
            search,
            config,
            hint,
            theme_anchor,
            theme_foreground,
            theme_background,
            color_resolver: &mut display.terminal_color_resolver,
            theme_is_light,
            themed_selection,
        }
    }

    /// Viewport offset.
    pub fn display_offset(&self) -> usize {
        self.terminal_content.display_offset
    }

    /// Get the terminal cursor.
    pub fn cursor(mut self) -> RenderableCursor {
        // Assure this function is only called after the iterator has been drained.
        debug_assert!(self.next().is_none());

        self.cursor
    }

    /// Get the RGB value for a color index.
    pub fn color(&self, color: usize) -> Rgb {
        self.terminal_content.colors[color].map(Rgb).unwrap_or(self.colors[color])
    }

    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.terminal_content.selection
    }

    /// Assemble the information required to render the terminal cursor.
    fn renderable_cursor(&mut self, cell: &RenderableCell) -> RenderableCursor {
        // Cursor colors.
        let color = if self.terminal_content.mode.contains(TermMode::VI) {
            self.config.colors.vi_mode_cursor
        } else {
            self.config.colors.cursor
        };
        let osc_cursor = self.terminal_content.colors[NamedColor::Cursor].map(|c| Rgb(c));
        // 默认光标属于宿主主题；应用的 OSC 12 不应把它重新固定成黑/白。
        // 用户显式配置光标后才退出主题跟随，并继续尊重 OSC 覆盖。
        let follows_theme = cursor_follows_theme(color);
        let (cursor_color, text_color, opacity) = if follows_theme {
            themed_cursor_style(self.cursor_shape, self.theme_is_light, self.theme_anchor, cell.fg)
        } else {
            let cursor_color = osc_cursor.map_or(color.background, CellRgb::Rgb);
            let text_color = color.foreground;
            let insufficient_contrast = (!matches!(cursor_color, CellRgb::Rgb(_))
                || !matches!(text_color, CellRgb::Rgb(_)))
                && cell.fg.contrast(*cell.bg) < MIN_CURSOR_CONTRAST;
            let mut text_color = text_color.color(cell.fg, cell.bg);
            let mut cursor_color = cursor_color.color(cell.fg, cell.bg);
            if insufficient_contrast {
                cursor_color = self.config.colors.primary.foreground;
                text_color = self.config.colors.primary.background;
            }
            (cursor_color, text_color, 1.0)
        };

        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            NonZeroU32::new(2).unwrap()
        } else {
            NonZeroU32::new(1).unwrap()
        };
        RenderableCursor {
            width,
            shape: self.cursor_shape,
            point: self.cursor_point,
            cursor_color,
            text_color,
            opacity,
        }
    }
}

fn cursor_follows_theme(color: InvertedCellColors) -> bool {
    color == NEBULA_DEFAULT_CURSOR
}

fn themed_cursor_style(
    shape: CursorShape,
    theme_is_light: bool,
    theme_anchor: Rgb,
    cell_foreground: Rgb,
) -> (Rgb, Rgb, f32) {
    let opacity = if shape == CursorShape::Block {
        if theme_is_light {
            terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT
        } else {
            terminal_feedback::BLOCK_CURSOR_ALPHA_DARK
        }
    } else if theme_is_light {
        terminal_feedback::STROKE_CURSOR_ALPHA_LIGHT
    } else {
        terminal_feedback::STROKE_CURSOR_ALPHA_DARK
    };
    // 保留原来的浅色半透明光标；默认主题光标的 OSC 12 已在调用前被忽略。
    (theme_anchor, cell_foreground, opacity)
}

impl Iterator for RenderableContent<'_> {
    type Item = RenderableCell;

    /// Gets the next renderable cell.
    ///
    /// Skips empty (background) cells and applies any flags to the cell state
    /// (eg. invert fg and bg colors).
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let cell = self.terminal_content.display_iter.next()?;
            let mut cell = RenderableCell::new(self, cell);

            if self.cursor_point == cell.point {
                // Store the cursor which should be rendered.
                self.cursor = self.renderable_cursor(&cell);
                if self.cursor.shape == CursorShape::Block {
                    if self.cursor.opacity < 1.0 {
                        (cell.bg, cell.bg_alpha) = composite_overlay(
                            self.cursor.cursor_color,
                            self.cursor.opacity,
                            cell.bg,
                            cell.bg_alpha,
                        );
                    } else {
                        cell.fg = self.cursor.text_color;
                        cell.bg = self.cursor.cursor_color;
                        cell.bg_alpha = 1.0;
                    }
                }

                return Some(cell);
            } else if !cell.is_empty() && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                // Skip empty cells and wide char spacers.
                return Some(cell);
            }
        }
    }
}

/// Cell ready for rendering.
#[derive(Clone, Debug)]
pub struct RenderableCell {
    pub character: char,
    pub point: Point<usize>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub underline: Rgb,
    pub flags: Flags,
    pub extra: Option<Box<RenderableCellExtra>>,
}

/// Extra storage with rarely present fields for [`RenderableCell`], to reduce the cell size we
/// pass around.
#[derive(Clone, Debug)]
pub struct RenderableCellExtra {
    pub zerowidth: Option<Vec<char>>,
    pub hyperlink: Option<Hyperlink>,
}

impl RenderableCell {
    fn new(content: &mut RenderableContent<'_>, cell: Indexed<&Cell>) -> Self {
        let overrides = content.terminal_content.colors;
        let mut fixed_bg = is_fixed_color(cell.bg, overrides);

        // Lookup RGB values.
        let (mut fg, mut fixed_fg) = Self::compute_fg_rgb(content, cell.fg, cell.flags);
        let mut bg = Self::compute_bg_rgb(content, cell.bg);

        let mut bg_alpha = if cell.flags.contains(Flags::INVERSE) {
            mem::swap(&mut fg, &mut bg);
            mem::swap(&mut fixed_fg, &mut fixed_bg);
            1.0
        } else {
            Self::compute_bg_alpha(content.config, cell.bg)
        };
        // 块绘图、框线和图标的颜色表达图形本身，不是正文对比度。
        // 仅让与旧主题底色相邻的连续表面跟随主题，普通 ANSI 色和图形色保持原值。
        bg =
            content.color_resolver.resolve_background(bg, fixed_bg && !is_terminal_graphic(cell.c));
        fg = content.color_resolver.resolve_foreground(
            fg,
            bg,
            fixed_fg && !is_terminal_graphic(cell.c),
            content.theme_foreground,
            content.theme_background,
        );
        if is_application_cursor_cell(
            content.terminal_content.cursor.shape,
            content.terminal_content.cursor.point,
            cell.point,
            cell.c,
            cell.flags,
            fixed_bg,
        ) {
            let opacity = if content.theme_is_light {
                terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT
            } else {
                terminal_feedback::BLOCK_CURSOR_ALPHA_DARK
            };
            // 反色空格的 `bg` 此时就是应用写入的黑色光标本身；以它为
            // 合成底色只会得到深色。宿主光标应叠在真实主题终端底色上。
            let cursor_base = if cell.c == ' ' { content.theme_background } else { bg };
            let (cursor, _) = composite_overlay(content.theme_anchor, opacity, cursor_base, 1.0);
            if cell.c == ' ' {
                bg = cursor;
                bg_alpha = 1.0;
            } else {
                fg = cursor;
            }
        }

        let is_selected = content.terminal_content.selection.is_some_and(|selection| {
            selection.contains_cell(
                &cell,
                content.terminal_content.cursor.point,
                content.cursor_shape,
            )
        });

        let display_offset = content.terminal_content.display_offset;
        let viewport_start = Point::new(Line(-(display_offset as i32)), Column(0));
        let colors = &content.config.colors;
        let mut character = cell.c;
        let mut flags = cell.flags;

        let num_cols = content.size.columns();
        if let Some((c, is_first)) = content
            .hint
            .as_mut()
            .and_then(|hint| hint.advance(viewport_start, num_cols, cell.point))
        {
            if is_first {
                let (config_fg, config_bg) =
                    (colors.hints.start.foreground, colors.hints.start.background);
                Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);
            } else if c.is_some() {
                let (config_fg, config_bg) =
                    (colors.hints.end.foreground, colors.hints.end.background);
                Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);
            } else {
                flags.insert(Flags::UNDERLINE);
            }

            character = c.unwrap_or(character);
        } else if is_selected {
            if content.themed_selection {
                let opacity = if content.theme_is_light {
                    terminal_feedback::SELECTION_ALPHA_LIGHT
                } else {
                    terminal_feedback::SELECTION_ALPHA_DARK
                };
                (bg, bg_alpha) = composite_overlay(content.theme_anchor, opacity, bg, bg_alpha);
            } else {
                let config_fg = colors.selection.foreground;
                let config_bg = colors.selection.background;
                Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);
            }

            if fg == bg && !cell.flags.contains(Flags::HIDDEN) {
                // Reveal inversed text when fg/bg is the same.
                fg = content.color(NamedColor::Background as usize);
                bg = content.color(NamedColor::Foreground as usize);
                bg_alpha = 1.0;
            }
        } else if content.search.as_mut().is_some_and(|search| search.advance(cell.point)) {
            let focused = content.focused_match.is_some_and(|fm| fm.contains(&cell.point));
            let (config_fg, config_bg) = if focused {
                (colors.search.focused_match.foreground, colors.search.focused_match.background)
            } else {
                (colors.search.matches.foreground, colors.search.matches.background)
            };
            Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);
        }

        // Apply transparency to all renderable cells if `transparent_background_colors` is set
        if bg_alpha > 0. && content.config.colors.transparent_background_colors {
            bg_alpha = content.config.window_opacity();
        }

        // Convert cell point to viewport position.
        let cell_point = cell.point;
        let point = term::point_to_viewport(display_offset, cell_point).unwrap();

        let underline = cell.underline_color().map_or(fg, |underline| {
            let (underline_rgb, fixed_underline) = Self::compute_fg_rgb(content, underline, flags);
            content.color_resolver.resolve_foreground(
                underline_rgb,
                bg,
                fixed_underline && !is_terminal_graphic(cell.c),
                content.theme_foreground,
                content.theme_background,
            )
        });

        let zerowidth = cell.zerowidth();
        let hyperlink = cell.hyperlink();

        let extra = (zerowidth.is_some() || hyperlink.is_some()).then(|| {
            Box::new(RenderableCellExtra {
                zerowidth: zerowidth.map(|zerowidth| zerowidth.to_vec()),
                hyperlink,
            })
        });

        RenderableCell { flags, character, bg_alpha, point, fg, bg, underline, extra }
    }

    /// Check if cell contains any renderable content.
    fn is_empty(&self) -> bool {
        self.bg_alpha == 0.
            && self.character == ' '
            && self.extra.is_none()
            && !self.flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
    }

    /// Apply [`CellRgb`] colors to the cell's colors.
    fn compute_cell_rgb(
        cell_fg: &mut Rgb,
        cell_bg: &mut Rgb,
        bg_alpha: &mut f32,
        fg: CellRgb,
        bg: CellRgb,
    ) {
        let old_fg = mem::replace(cell_fg, fg.color(*cell_fg, *cell_bg));
        *cell_bg = bg.color(old_fg, *cell_bg);

        if bg != CellRgb::CellBackground {
            *bg_alpha = 1.0;
        }
    }

    /// Get the RGB color from a cell's foreground color.
    fn compute_fg_rgb(content: &RenderableContent<'_>, fg: Color, flags: Flags) -> (Rgb, bool) {
        let config = &content.config;
        match fg {
            Color::Spec(rgb) => {
                let rgb = match flags & Flags::DIM {
                    Flags::DIM => Rgb::from(rgb) * DIM_FACTOR,
                    _ => rgb.into(),
                };
                (rgb, true)
            },
            Color::Named(ansi) => {
                let index = match (
                    config.colors.draw_bold_text_with_bright_colors,
                    flags & Flags::DIM_BOLD,
                ) {
                    // If no bright foreground is set, treat it like the BOLD flag doesn't exist.
                    (_, Flags::DIM_BOLD)
                        if ansi == NamedColor::Foreground
                            && config.colors.primary.bright_foreground.is_none() =>
                    {
                        NamedColor::DimForeground as usize
                    },
                    // Draw bold text in bright colors *and* contains bold flag.
                    (true, Flags::BOLD) => ansi.to_bright() as usize,
                    // Cell is marked as dim and not bold.
                    (_, Flags::DIM) | (false, Flags::DIM_BOLD) => ansi.to_dim() as usize,
                    // None of the above, keep original color..
                    _ => ansi as usize,
                };
                (content.color(index), content.terminal_content.colors[index].is_some())
            },
            Color::Indexed(idx) => {
                let idx = match (
                    config.colors.draw_bold_text_with_bright_colors,
                    flags & Flags::DIM_BOLD,
                    idx,
                ) {
                    (true, Flags::BOLD, 0..=7) => idx as usize + 8,
                    (false, Flags::DIM, 8..=15) => idx as usize - 8,
                    (false, Flags::DIM, 0..=7) => NamedColor::DimBlack as usize + idx as usize,
                    _ => idx as usize,
                };

                let fixed = content.terminal_content.colors[idx].is_some() || idx >= 24;
                (content.color(idx), fixed)
            },
        }
    }

    /// Get the RGB color from a cell's background color.
    #[inline]
    fn compute_bg_rgb(content: &RenderableContent<'_>, bg: Color) -> Rgb {
        match bg {
            Color::Spec(rgb) => rgb.into(),
            Color::Named(ansi) => content.color(ansi as usize),
            Color::Indexed(idx) => content.color(idx as usize),
        }
    }

    /// Compute background alpha based on cell's original color.
    ///
    /// Since an RGB color matching the background should not be transparent, this is computed
    /// using the named input color, rather than checking the RGB of the background after its color
    /// is computed.
    #[inline]
    fn compute_bg_alpha(config: &UiConfig, bg: Color) -> f32 {
        if bg == Color::Named(NamedColor::Background) {
            0.
        } else if config.colors.transparent_background_colors {
            config.window_opacity()
        } else {
            1.
        }
    }
}

/// Terminal graphics carry semantic/brand color in their glyph pixels; treating
/// them as prose changes icons into a different image on light themes.
fn is_terminal_graphic(character: char) -> bool {
    matches!(
        character,
        '\u{2500}'..='\u{27bf}'
            | '\u{2800}'..='\u{28ff}'
            | '\u{e000}'..='\u{f8ff}'
            | '\u{1f300}'..='\u{1faff}'
            | '\u{1fb00}'..='\u{1fbff}'
    )
}

fn is_application_cursor_cell(
    terminal_cursor_shape: CursorShape,
    terminal_cursor_point: Point,
    cell_point: Point,
    character: char,
    flags: Flags,
    fixed_background: bool,
) -> bool {
    terminal_cursor_shape == CursorShape::Hidden
        && terminal_cursor_point == cell_point
        && (matches!(character, '\u{2580}'..='\u{259f}')
            || (character == ' ' && (flags.contains(Flags::INVERSE) || fixed_background)))
}

/// Alpha-compose a theme overlay over the cell's existing background. Keeping
/// this in the render model preserves explicit ANSI cell backgrounds while
/// still allowing the default transparent terminal surface/image to show.
fn composite_overlay(overlay: Rgb, overlay_alpha: f32, base: Rgb, base_alpha: f32) -> (Rgb, f32) {
    let oa = overlay_alpha.clamp(0.0, 1.0);
    let ba = base_alpha.clamp(0.0, 1.0);
    let out_a = oa + ba * (1.0 - oa);
    if out_a <= f32::EPSILON {
        return (base, 0.0);
    }
    let channel =
        |o: u8, b: u8| ((o as f32 * oa + b as f32 * ba * (1.0 - oa)) / out_a).round() as u8;
    (
        Rgb::new(
            channel(overlay.r, base.r),
            channel(overlay.g, base.g),
            channel(overlay.b, base.b),
        ),
        out_a,
    )
}

fn mix_rgb(color: Rgb, neutral: Rgb, neutral_amount: f32) -> Rgb {
    let t = neutral_amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Rgb::new(channel(color.r, neutral.r), channel(color.g, neutral.g), channel(color.b, neutral.b))
}

/// Cursor storing all information relevant for rendering.
#[derive(Debug, PartialEq, Copy, Clone)]
pub struct RenderableCursor {
    shape: CursorShape,
    cursor_color: Rgb,
    text_color: Rgb,
    width: NonZeroU32,
    point: Point<usize>,
    opacity: f32,
}

impl RenderableCursor {
    fn new_hidden() -> Self {
        let shape = CursorShape::Hidden;
        let cursor_color = Rgb::default();
        let text_color = Rgb::default();
        let width = NonZeroU32::new(1).unwrap();
        let point = Point::default();
        Self { shape, cursor_color, text_color, width, point, opacity: 0.0 }
    }
}

impl RenderableCursor {
    pub fn new(
        point: Point<usize>,
        shape: CursorShape,
        cursor_color: Rgb,
        width: NonZeroU32,
    ) -> Self {
        Self { shape, cursor_color, text_color: cursor_color, width, point, opacity: 1.0 }
    }

    pub fn color(&self) -> Rgb {
        self.cursor_color
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn shape(&self) -> CursorShape {
        self.shape
    }

    pub fn width(&self) -> NonZeroU32 {
        self.width
    }

    pub fn point(&self) -> Point<usize> {
        self.point
    }
}

#[cfg(test)]
mod tests {
    use nebula_terminal::vte::ansi::CursorShape;

    use super::{
        composite_overlay, cursor_follows_theme, is_application_cursor_cell, is_terminal_graphic,
        mix_rgb, themed_cursor_style,
    };
    use crate::config::color::NEBULA_DEFAULT_CURSOR;
    use crate::display::color::Rgb;
    use crate::display::design_tokens::terminal_feedback;
    use nebula_terminal::index::{Column, Line, Point};
    use nebula_terminal::term::cell::Flags;

    #[test]
    fn translucent_overlay_preserves_transparent_surface() {
        let purple = Rgb::new(130, 80, 223);
        let (color, alpha) = composite_overlay(purple, 0.20, Rgb::new(255, 255, 255), 0.0);
        assert_eq!(color, purple);
        assert!((alpha - 0.20).abs() < f32::EPSILON);
    }

    #[test]
    fn translucent_overlay_composites_over_opaque_ansi_background() {
        let (color, alpha) =
            composite_overlay(Rgb::new(130, 80, 223), 0.20, Rgb::new(255, 255, 255), 1.0);
        assert_eq!(color, Rgb::new(230, 220, 249));
        assert!((alpha - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn theme_anchor_is_softened_toward_neutral_ink() {
        let softened = mix_rgb(Rgb::new(130, 80, 223), Rgb::new(107, 114, 128), 0.28);
        assert_eq!(softened, Rgb::new(124, 90, 196));
    }

    #[test]
    fn terminal_graphics_keep_their_application_colors() {
        assert!(is_terminal_graphic('█'));
        assert!(is_terminal_graphic('─'));
        assert!(is_terminal_graphic('\u{e0b0}'));
        assert!(is_terminal_graphic('🦀'));
        assert!(!is_terminal_graphic('A'));
        assert!(!is_terminal_graphic('数'));
        assert!(!is_terminal_graphic('$'));
    }

    #[test]
    fn light_block_cursor_keeps_the_original_translucent_theme_style() {
        let anchor = Rgb::new(124, 90, 196);
        let foreground = Rgb::new(36, 41, 47);
        let style = themed_cursor_style(CursorShape::Block, true, anchor, foreground);

        assert_eq!(style.0, anchor);
        assert_eq!(style.1, foreground);
        assert_eq!(style.2, terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT);
        assert!(cursor_follows_theme(NEBULA_DEFAULT_CURSOR));
    }

    #[test]
    fn hidden_terminal_cursor_only_recolors_its_own_fake_block() {
        let cursor = Point::new(Line(4), Column(7));
        assert!(is_application_cursor_cell(
            CursorShape::Hidden,
            cursor,
            cursor,
            '█',
            Flags::empty(),
            false,
        ));
        assert!(is_application_cursor_cell(
            CursorShape::Hidden,
            cursor,
            cursor,
            ' ',
            Flags::INVERSE,
            false,
        ));
        assert!(!is_application_cursor_cell(
            CursorShape::Block,
            cursor,
            cursor,
            '█',
            Flags::empty(),
            false,
        ));
        assert!(!is_application_cursor_cell(
            CursorShape::Hidden,
            cursor,
            Point::new(Line(4), Column(6)),
            '█',
            Flags::empty(),
            false,
        ));

        let (light_cursor, _) = composite_overlay(
            Rgb::new(124, 90, 196),
            terminal_feedback::BLOCK_CURSOR_ALPHA_LIGHT,
            Rgb::new(255, 255, 255),
            1.0,
        );
        assert_eq!(light_cursor, Rgb::new(229, 222, 243));
    }
}

/// Regex hints for keyboard shortcuts.
struct Hint<'a> {
    /// Hint matches and position.
    matches: HintMatches<'a>,

    /// Last match checked against current cell position.
    labels: &'a Vec<Vec<char>>,
}

impl Hint<'_> {
    /// Advance the hint iterator.
    ///
    /// If the point is within a hint, the keyboard shortcut character that should be displayed at
    /// this position will be returned.
    ///
    /// The tuple's [`bool`] will be `true` when the character is the first for this hint.
    ///
    /// The tuple's [`Option<char>`] will be [`None`] when the point is part of the match, but not
    /// part of the hint label.
    fn advance(
        &mut self,
        viewport_start: Point,
        num_cols: usize,
        point: Point,
    ) -> Option<(Option<char>, bool)> {
        // Check if we're within a match at all.
        if !self.matches.advance(point) {
            return None;
        }

        // Match starting position on this line; linebreaks interrupt the hint labels.
        let start = self
            .matches
            .get(self.matches.index)
            .map(|bounds| cmp::max(*bounds.start(), viewport_start))?;

        // Position within the hint label.
        let line_delta = point.line.0 - start.line.0;
        let col_delta = point.column.0 as i32 - start.column.0 as i32;
        let label_position = usize::try_from(line_delta * num_cols as i32 + col_delta).unwrap_or(0);
        let is_first = label_position == 0;

        // Hint label character.
        let hint_char = self.labels[self.matches.index]
            .get(label_position)
            .copied()
            .map(|c| (Some(c), is_first))
            .unwrap_or((None, false));

        Some(hint_char)
    }
}

impl<'a> From<&'a HintState> for Hint<'a> {
    fn from(hint_state: &'a HintState) -> Self {
        let matches = HintMatches::new(hint_state.matches());
        Self { labels: hint_state.labels(), matches }
    }
}

/// Visible hint match tracking.
#[derive(Default)]
struct HintMatches<'a> {
    /// All visible matches.
    matches: Cow<'a, [Match]>,

    /// Index of the last match checked.
    index: usize,
}

impl<'a> HintMatches<'a> {
    /// Create new renderable matches iterator..
    fn new(matches: impl Into<Cow<'a, [Match]>>) -> Self {
        Self { matches: matches.into(), index: 0 }
    }

    /// Create from regex matches on term visible part.
    fn visible_regex_matches<T>(term: &Term<T>, dfas: &mut RegexSearch) -> Self {
        let matches = hint::visible_regex_match_iter(term, dfas).collect::<Vec<_>>();
        Self::new(matches)
    }

    /// Advance the regex tracker to the next point.
    ///
    /// This will return `true` if the point passed is part of a regex match.
    fn advance(&mut self, point: Point) -> bool {
        while let Some(bounds) = self.get(self.index) {
            if bounds.start() > &point {
                break;
            } else if bounds.end() < &point {
                self.index += 1;
            } else {
                return true;
            }
        }
        false
    }
}

impl Deref for HintMatches<'_> {
    type Target = [Match];

    fn deref(&self) -> &Self::Target {
        self.matches.deref()
    }
}
