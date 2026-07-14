//! Unified native context menu for sidebar tabs and SSH destinations.
//!
//! The menu is rendered by Nebula's existing OpenGL UI pipeline. Geometry,
//! hit-testing and animation live together here so pointer targets cannot drift
//! away from the pixels the user sees at non-integer DPI scales.

use std::time::{Duration, Instant};

use super::*;

const OPEN_DURATION: Duration = Duration::from_millis(120);
const CLOSE_DURATION: Duration = Duration::from_millis(90);

// Maple Mono Normal NF CN ships the stable Codicon block below. Keep menu
// symbols on that block: newer Codicon additions such as U+EC86 are absent
// from the installed Maple version and rasterize as a missing-glyph box.
const ICON_COPY: &str = "\u{ebcc}";
const ICON_SPLIT_RIGHT: &str = "\u{eb56}";
const ICON_SPLIT_DOWN: &str = "\u{eb57}";
const ICON_EDIT: &str = "\u{ea73}";
const ICON_CLOSE: &str = "\u{ea76}";

/// Curated tab colors. `None` is the first swatch and means no light strip.
pub const TAB_COLORS: [Rgb; 7] = [
    Rgb::new(224, 108, 117),
    Rgb::new(209, 154, 102),
    Rgb::new(229, 192, 123),
    Rgb::new(152, 195, 121),
    Rgb::new(86, 182, 194),
    Rgb::new(97, 175, 239),
    Rgb::new(198, 120, 221),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Tab(usize),
    Ssh(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    DuplicateTab(usize),
    SplitTabRight(usize),
    SplitTabDown(usize),
    RenameTab(usize),
    CloseTab(usize),
    SetTabColor { index: usize, color: Option<Rgb> },
    ConnectSsh(usize),
    CopySshAddress(usize),
    EditSsh(usize),
    DeleteSsh(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuHit {
    Outside,
    Panel,
    Action(ContextMenuAction),
}

#[derive(Debug, Clone)]
pub(super) struct ContextMenu {
    target: ContextMenuTarget,
    anchor: (f32, f32),
    opened_at: Instant,
    closing_at: Option<Instant>,
    hover: Option<ContextMenuAction>,
    current_color: Option<Rgb>,
}

impl ContextMenu {
    pub(super) fn new(
        target: ContextMenuTarget,
        anchor: (f32, f32),
        current_color: Option<Rgb>,
    ) -> Self {
        Self {
            target,
            anchor,
            opened_at: Instant::now(),
            closing_at: None,
            hover: None,
            current_color,
        }
    }

    pub(super) fn begin_close(&mut self) {
        self.closing_at.get_or_insert_with(Instant::now);
        self.hover = None;
    }

    pub(super) fn set_hover(&mut self, action: Option<ContextMenuAction>) -> bool {
        if self.hover == action {
            return false;
        }
        self.hover = action;
        true
    }

    fn progress(&self) -> f32 {
        if let Some(start) = self.closing_at {
            return (1.0 - start.elapsed().as_secs_f32() / CLOSE_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
        }
        (self.opened_at.elapsed().as_secs_f32() / OPEN_DURATION.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub(super) fn finished(&self) -> bool {
        self.closing_at.is_some_and(|start| start.elapsed() >= CLOSE_DURATION)
    }

    pub(super) fn animating(&self) -> bool {
        self.closing_at.is_some() || self.opened_at.elapsed() < OPEN_DURATION
    }

    pub(super) fn interactive(&self) -> bool {
        self.closing_at.is_none()
    }
}

#[derive(Debug, Clone, Copy)]
struct Row {
    action: ContextMenuAction,
    icon: &'static str,
    label: &'static str,
    hint: &'static str,
    rect: (f32, f32, f32, f32),
    danger: bool,
}

#[derive(Debug, Clone)]
struct MenuLayout {
    panel: (f32, f32, f32, f32),
    rows: Vec<Row>,
    separators: Vec<(f32, f32, f32, f32)>,
    color_label: Option<(f32, f32)>,
    colors: Vec<(Option<Rgb>, ContextMenuAction, (f32, f32, f32, f32))>,
}

fn contains((x, y, w, h): (f32, f32, f32, f32), px: f32, py: f32) -> bool {
    px >= x && px <= x + w && py >= y && py <= y + h
}

fn layout(menu: &ContextMenu, size: SizeInfo, scale: f32, animated_y_offset: f32) -> MenuLayout {
    let s = |v: f32| v * scale;
    let width = s(252.0);
    let row_h = s(38.0);
    let pad = s(7.0);
    let sep_h = s(9.0);
    let color_h = s(62.0);
    let (row_count, separators, extra) = match menu.target {
        ContextMenuTarget::Tab(_) => (5usize, 2usize, color_h),
        ContextMenuTarget::Ssh(_) => (4usize, 1usize, 0.0),
    };
    let height = pad * 2.0 + row_count as f32 * row_h + separators as f32 * sep_h + extra;
    let margin = s(8.0);
    let mut x = menu.anchor.0;
    let mut y = menu.anchor.1 + animated_y_offset;
    if x + width > size.width() - margin {
        x = (menu.anchor.0 - width).max(margin);
    }
    if y + height > size.height() - margin {
        y = (size.height() - margin - height).max(margin);
    }
    // The anchor may itself sit inside the safety margin (a right-click on a
    // close glyph at the edge), so clamp after choosing the preferred side as
    // well. Without this final cap the menu could still overhang by the
    // anchor-to-margin distance.
    let max_x = (size.width() - margin - width).max(margin);
    let max_y = (size.height() - margin - height).max(margin);
    x = x.clamp(margin, max_x);
    y = y.clamp(margin, max_y);

    let panel = (x, y, width, height);
    let mut rows = Vec::with_capacity(row_count);
    let mut separator_rects = Vec::with_capacity(separators);
    let mut cursor_y = y + pad;
    let row = |action, icon, label, hint, y, danger| Row {
        action,
        icon,
        label,
        hint,
        rect: (x + pad, y, width - 2.0 * pad, row_h),
        danger,
    };
    let mut separator = |cursor_y: &mut f32| {
        separator_rects.push((x + s(12.0), *cursor_y + sep_h * 0.5, width - s(24.0), s(1.0)));
        *cursor_y += sep_h;
    };

    let mut color_label = None;
    let mut colors = Vec::new();
    match menu.target {
        ContextMenuTarget::Tab(index) => {
            rows.push(row(
                ContextMenuAction::DuplicateTab(index),
                ICON_COPY,
                "复制标签页",
                "",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            separator(&mut cursor_y);
            rows.push(row(
                ContextMenuAction::SplitTabRight(index),
                ICON_SPLIT_RIGHT,
                "左右分屏",
                "Ctrl+Shift+D",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            rows.push(row(
                ContextMenuAction::SplitTabDown(index),
                ICON_SPLIT_DOWN,
                "上下分屏",
                "Ctrl+Shift+S",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            separator(&mut cursor_y);
            rows.push(row(
                ContextMenuAction::RenameTab(index),
                ICON_EDIT,
                "重命名",
                "F2",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            rows.push(row(
                ContextMenuAction::CloseTab(index),
                ICON_CLOSE,
                "关闭",
                "Ctrl+Shift+W",
                cursor_y,
                true,
            ));
            cursor_y += row_h;

            color_label = Some((x + s(14.0), cursor_y + s(8.0)));
            let swatch = s(20.0);
            let gap = s(8.0);
            let start_x = x + s(14.0);
            let swatch_y = cursor_y + s(32.0);
            let all = std::iter::once(None).chain(TAB_COLORS.into_iter().map(Some));
            for (slot, color) in all.enumerate() {
                let rect = (start_x + slot as f32 * (swatch + gap), swatch_y, swatch, swatch);
                colors.push((color, ContextMenuAction::SetTabColor { index, color }, rect));
            }
        },
        ContextMenuTarget::Ssh(index) => {
            rows.push(row(
                ContextMenuAction::ConnectSsh(index),
                "\u{f489}",
                "连接",
                "Enter",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            rows.push(row(
                ContextMenuAction::CopySshAddress(index),
                ICON_COPY,
                "复制地址",
                "",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            rows.push(row(
                ContextMenuAction::EditSsh(index),
                ICON_EDIT,
                "编辑",
                "",
                cursor_y,
                false,
            ));
            cursor_y += row_h;
            separator(&mut cursor_y);
            rows.push(row(
                ContextMenuAction::DeleteSsh(index),
                "\u{ea81}",
                "删除",
                "",
                cursor_y,
                true,
            ));
        },
    }

    MenuLayout { panel, rows, separators: separator_rects, color_label, colors }
}

pub(super) fn hit_test(
    menu: &ContextMenu,
    size: SizeInfo,
    scale: f32,
    x: f32,
    y: f32,
) -> ContextMenuHit {
    let layout = layout(menu, size, scale, 0.0);
    for row in &layout.rows {
        if contains(row.rect, x, y) {
            return ContextMenuHit::Action(row.action);
        }
    }
    for (_, action, rect) in &layout.colors {
        if contains(*rect, x, y) {
            return ContextMenuHit::Action(*action);
        }
    }
    if contains(layout.panel, x, y) { ContextMenuHit::Panel } else { ContextMenuHit::Outside }
}

fn alpha(color: Rgba, opacity: f32) -> Rgba {
    Rgba::new(color.r, color.g, color.b, (color.a as f32 * opacity).round() as u8)
}

fn fade_ink(base: Rgba, ink: Rgb, opacity: f32) -> Rgb {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * opacity).round() as u8;
    Rgb::new(mix(base.r, ink.r), mix(base.g, ink.g), mix(base.b, ink.b))
}

/// Paint the menu after the chrome/logo pass and before true modal dialogs.
pub(super) fn draw(display: &mut Display) {
    let Some(menu) = display.nebula_context_menu.clone() else { return };
    if menu.finished() {
        display.nebula_context_menu = None;
        return;
    }

    let progress = menu.progress();
    let eased = 1.0 - (1.0 - progress) * (1.0 - progress) * (1.0 - progress);
    let size = display.size_info;
    let scale = display.window.scale_factor as f32;
    let s = |v: f32| v * scale;
    let layout = layout(&menu, size, scale, -s(5.0) * (1.0 - eased));
    let sk = display.nebula_theme.skin();
    let radius = s(9.0);
    let (px, py, pw, ph) = layout.panel;

    let mut quads = vec![
        UiQuad::glow(
            px - s(16.0),
            py - s(12.0),
            pw + s(32.0),
            ph + s(30.0),
            alpha(Rgba::new(0, 0, 0, 72), progress),
        ),
        UiQuad::solid(px, py, pw, ph, radius, alpha(sk.panel, progress)),
        UiQuad::solid(px, py, pw, s(1.0).max(1.0), radius, alpha(sk.hairline, progress)),
    ];
    for separator in &layout.separators {
        quads.push(UiQuad::solid(
            separator.0,
            separator.1,
            separator.2,
            separator.3,
            0.0,
            alpha(sk.hairline, progress),
        ));
    }
    for row in &layout.rows {
        if menu.hover == Some(row.action) {
            quads.push(UiQuad::solid(
                row.rect.0,
                row.rect.1,
                row.rect.2,
                row.rect.3,
                s(6.0),
                alpha(sk.hover, progress),
            ));
        }
    }

    for (color, action, rect) in &layout.colors {
        let selected = *color == menu.current_color;
        if menu.hover == Some(*action) || selected {
            let ring = if selected { sk.accent } else { sk.ink_dim };
            quads.push(UiQuad::solid(
                rect.0 - s(3.0),
                rect.1 - s(3.0),
                rect.2 + s(6.0),
                rect.3 + s(6.0),
                s(7.0),
                alpha(Rgba::new(ring.r, ring.g, ring.b, 210), progress),
            ));
        }
        let swatch = color.unwrap_or(sk.accent);
        quads.push(UiQuad::solid(
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            s(5.0),
            alpha(Rgba::new(swatch.r, swatch.g, swatch.b, 255), progress),
        ));
    }
    display.renderer.draw_ui(&size, &quads);

    // Text has no alpha channel in the glyph pipeline, so fade it toward the
    // panel color. This avoids a one-frame label pop during both directions.
    let ink = fade_ink(sk.panel, sk.ink, progress);
    let dim = fade_ink(sk.panel, sk.ink_dim, progress);
    let danger = fade_ink(sk.panel, Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b), progress);
    let cell_h = size.cell_height();
    for row in &layout.rows {
        let y = row.rect.1 + (row.rect.3 - cell_h) * 0.5;
        display.renderer.draw_chrome_text(
            &size,
            row.rect.0 + s(10.0),
            y,
            if row.danger { danger } else { dim },
            row.icon,
            &mut display.glyph_cache,
        );
        display.renderer.draw_chrome_text(
            &size,
            row.rect.0 + s(38.0),
            y,
            if row.danger { danger } else { ink },
            row.label,
            &mut display.glyph_cache,
        );
        if !row.hint.is_empty() {
            let hint_w = row.hint.chars().count() as f32 * size.cell_width();
            display.renderer.draw_chrome_text(
                &size,
                row.rect.0 + row.rect.2 - hint_w - s(10.0),
                y,
                dim,
                row.hint,
                &mut display.glyph_cache,
            );
        }
    }
    if let Some((x, y)) = layout.color_label {
        display.renderer.draw_chrome_text(&size, x, y, dim, "标签颜色", &mut display.glyph_cache);
    }
    if let Some((None, _, rect)) = layout.colors.first() {
        display.renderer.draw_chrome_text(
            &size,
            rect.0 + (rect.2 - size.cell_width()) * 0.5,
            rect.1 + (rect.3 - cell_h) * 0.5,
            fade_ink(
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
                sk.ink_on_accent,
                progress,
            ),
            "A",
            &mut display.glyph_cache,
        );
    }

    if menu.animating() {
        display.window.request_redraw();
        display.pending_update.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> SizeInfo {
        SizeInfo::new(800.0, 600.0, 9.0, 18.0, 8.0, 8.0, false)
    }

    #[test]
    fn tab_menu_stays_inside_bottom_right_window_edge() {
        let menu = ContextMenu::new(ContextMenuTarget::Tab(2), (796.0, 596.0), None);
        let layout = layout(&menu, size(), 1.0, 0.0);
        assert!(layout.panel.0 >= 8.0);
        assert!(layout.panel.1 >= 8.0);
        assert!(layout.panel.0 + layout.panel.2 <= 792.0);
        assert!(layout.panel.1 + layout.panel.3 <= 592.0);
    }

    #[test]
    fn first_tab_row_and_auto_color_have_actions() {
        let menu = ContextMenu::new(ContextMenuTarget::Tab(3), (100.0, 100.0), None);
        let layout = layout(&menu, size(), 1.0, 0.0);
        let first = layout.rows[0];
        assert_eq!(
            hit_test(
                &menu,
                size(),
                1.0,
                first.rect.0 + 2.0,
                first.rect.1 + 2.0,
            ),
            ContextMenuHit::Action(ContextMenuAction::DuplicateTab(3))
        );
        assert_eq!(layout.colors.len(), 8);
        assert_eq!(layout.colors[0].0, None);
        assert_eq!(
            layout.colors[0].1,
            ContextMenuAction::SetTabColor { index: 3, color: None }
        );
    }
}
