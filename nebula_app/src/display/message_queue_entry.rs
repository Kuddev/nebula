//! Bottom-docked message-queue entry for the left sidebar.
//!
//! The entry deliberately owns its geometry, quads and labels in one module.
//! Tabs and SSH rows only consume [`reserved_height`], so future queue-panel
//! work can evolve without adding another block of unrelated drawing code to
//! `chrome.rs`.

use nebula_terminal::term::cell::Flags;

use super::color::Rgb;
use super::theme::Skin;
use super::{SizeInfo, UiLanguage};
use crate::renderer::ui::{Gradient, Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

const ENTRY_HEIGHT_LOGICAL: f32 = 54.0;
const ENTRY_SIDE_INSET_LOGICAL: f32 = 14.0;
const ENTRY_BOTTOM_INSET_LOGICAL: f32 = 12.0;
const ENTRY_CONTENT_GAP_LOGICAL: f32 = 14.0;
// 自动审批与真实 Agent 事件尚未接入前，首页不展示一个只能打开空状态的
// 入口。保留完整模块和接线，后续只需切换可见性并接入真实数据，无需再次
// 改动 Tabs/SSH 的布局与命中合同。
const ENTRY_VISIBLE: bool = false;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MessageQueueEntry {
    /// Total actionable messages. Real event sources will update this in the
    /// next integration stage; zero is intentionally honest in the meantime.
    pub(crate) pending: usize,
    pub(crate) high_risk: usize,
    /// Kept now so the entry's interaction contract is stable before the
    /// expanded queue panel is wired in.
    pub(crate) open: bool,
}

impl MessageQueueEntry {
    pub(crate) fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MessageQueueEntryLayout {
    pub(crate) rect: (f32, f32, f32, f32),
    icon: (f32, f32, f32, f32),
    badge: (f32, f32, f32, f32),
    chevron: (f32, f32, f32, f32),
}

/// Sidebar height unavailable to the elastic Tabs / SSH row allocator.
pub(crate) fn reserved_height(scale: f32) -> f32 {
    reserved_height_for(scale, ENTRY_VISIBLE)
}

fn reserved_height_for(scale: f32, visible: bool) -> f32 {
    if visible {
        (ENTRY_HEIGHT_LOGICAL + ENTRY_BOTTOM_INSET_LOGICAL + ENTRY_CONTENT_GAP_LOGICAL) * scale
    } else {
        0.0
    }
}

/// Derive every visible and interactive rectangle from the sidebar panel.
pub(crate) fn layout(panel: (f32, f32, f32, f32), scale: f32) -> MessageQueueEntryLayout {
    layout_for(panel, scale, ENTRY_VISIBLE)
}

fn layout_for(panel: (f32, f32, f32, f32), scale: f32, visible: bool) -> MessageQueueEntryLayout {
    if !visible {
        return MessageQueueEntryLayout::default();
    }

    let (panel_x, panel_y, panel_w, panel_h) = panel;
    if panel_w <= 0.0 || panel_h <= 0.0 {
        return MessageQueueEntryLayout::default();
    }

    let inset = ENTRY_SIDE_INSET_LOGICAL * scale;
    let bottom = ENTRY_BOTTOM_INSET_LOGICAL * scale;
    let height = ENTRY_HEIGHT_LOGICAL * scale;
    let width = (panel_w - inset * 2.0).max(0.0);
    let rect = (panel_x + inset, panel_y + panel_h - bottom - height, width, height);
    if rect.2 <= 0.0 || rect.1 < panel_y {
        return MessageQueueEntryLayout::default();
    }

    let icon_size = 30.0 * scale;
    let icon = (rect.0 + 8.0 * scale, rect.1 + (rect.3 - icon_size) * 0.5, icon_size, icon_size);
    let badge_size = 17.0 * scale;
    let badge =
        (icon.0 + icon.2 - badge_size * 0.54, icon.1 - badge_size * 0.32, badge_size, badge_size);
    let chevron_size = 28.0 * scale;
    let chevron = (
        rect.0 + rect.2 - 8.0 * scale - chevron_size,
        rect.1 + (rect.3 - chevron_size) * 0.5,
        chevron_size,
        chevron_size,
    );

    MessageQueueEntryLayout { rect, icon, badge, chevron }
}

#[inline]
pub(crate) fn contains(entry_layout: MessageQueueEntryLayout, x: f32, y: f32) -> bool {
    let (rx, ry, rw, rh) = entry_layout.rect;
    rw > 0.0 && rh > 0.0 && x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
}

fn push_segment(
    quads: &mut Vec<UiQuad>,
    from: (f32, f32),
    to: (f32, f32),
    thickness: f32,
    color: Rgba,
) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let len = dx.hypot(dy);
    if len <= f32::EPSILON {
        return;
    }
    let px = -dy / len * thickness * 0.5;
    let py = dx / len * thickness * 0.5;
    quads.push(UiQuad::poly(
        [
            [from.0 - px, from.1 - py],
            [from.0 + px, from.1 + py],
            [to.0 - px, to.1 - py],
            [to.0 + px, to.1 + py],
        ],
        color,
        color,
        Gradient::None,
    ));
}

pub(crate) fn push_quads(
    quads: &mut Vec<UiQuad>,
    entry_layout: MessageQueueEntryLayout,
    state: MessageQueueEntry,
    skin: Skin,
    scale: f32,
    hovered: bool,
) {
    let (x, y, width, height) = entry_layout.rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let radius = 10.0 * scale;
    quads.push(UiQuad::solid(x, y, width, height, radius, skin.hairline));
    let inner = 1.0_f32.max(scale);
    let fill = if hovered || state.open { skin.hover } else { skin.surface };
    quads.push(UiQuad::gradient(
        x + inner,
        y + inner,
        (width - inner * 2.0).max(0.0),
        (height - inner * 2.0).max(0.0),
        (radius - inner).max(0.0),
        fill,
        skin.panel,
        Gradient::Axis([0.8, 0.45]),
    ));

    let (ix, iy, iw, ih) = entry_layout.icon;
    quads.push(UiQuad::solid(ix, iy, iw, ih, 8.0 * scale, skin.accent_soft));
    // Three short lanes read as a queue without relying on a private-use glyph.
    let lane_color = Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, 225);
    let lane_x = ix + 9.0 * scale;
    let lane_w = 12.0 * scale;
    let lane_h = (1.5 * scale).max(1.0);
    for offset in [10.0_f32, 15.0, 20.0] {
        quads.push(UiQuad::solid(
            lane_x,
            iy + offset * scale,
            lane_w,
            lane_h,
            lane_h * 0.5,
            lane_color,
        ));
    }

    if state.pending > 0 {
        let (bx, by, bw, bh) = entry_layout.badge;
        quads.push(UiQuad::solid(bx, by, bw, bh, bw * 0.5, skin.danger));
    }

    let (cx, cy, cw, ch) = entry_layout.chevron;
    quads.push(UiQuad::solid(
        cx,
        cy,
        cw,
        ch,
        7.0 * scale,
        if hovered { skin.hover_strong } else { skin.input },
    ));
    let center_x = cx + cw * 0.5;
    let center_y = cy + ch * 0.5;
    let arm = 4.0 * scale;
    let stroke = (1.4 * scale).max(1.0);
    let icon_color = Rgba::new(skin.icon.r, skin.icon.g, skin.icon.b, 235);
    if state.open {
        push_segment(
            quads,
            (center_x - arm, center_y + arm * 0.5),
            (center_x, center_y - arm * 0.5),
            stroke,
            icon_color,
        );
        push_segment(
            quads,
            (center_x, center_y - arm * 0.5),
            (center_x + arm, center_y + arm * 0.5),
            stroke,
            icon_color,
        );
    } else {
        push_segment(
            quads,
            (center_x - arm * 0.5, center_y - arm),
            (center_x + arm * 0.5, center_y),
            stroke,
            icon_color,
        );
        push_segment(
            quads,
            (center_x + arm * 0.5, center_y),
            (center_x - arm * 0.5, center_y + arm),
            stroke,
            icon_color,
        );
    }
}

fn summary_text(state: MessageQueueEntry, language: UiLanguage) -> String {
    if state.pending == 0 {
        return language.pick("暂无待处理", "Nothing pending").to_owned();
    }

    let regular = state.pending.saturating_sub(state.high_risk);
    match language {
        UiLanguage::ZhCn => format!("{} 个高风险 · {} 个待处理", state.high_risk, regular),
        UiLanguage::EnUs => format!("{} high risk · {} pending", state.high_risk, regular),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    size: &SizeInfo,
    entry_layout: MessageQueueEntryLayout,
    state: MessageQueueEntry,
    skin: Skin,
    language: UiLanguage,
    scale: f32,
) {
    let (_x, y, width, height) = entry_layout.rect;
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let text_x = entry_layout.icon.0 + entry_layout.icon.2 + 9.0 * scale;
    renderer.draw_doc_text_tracked(
        size,
        text_x,
        y + 8.0 * scale,
        0.78,
        0.0,
        skin.ink_strong,
        Flags::BOLD,
        language.pick("消息队列", "Message queue"),
        glyph_cache,
    );
    let summary = summary_text(state, language);
    renderer.draw_doc_text_tracked(
        size,
        text_x,
        y + 29.0 * scale,
        0.66,
        0.0,
        skin.ink_dim,
        Flags::empty(),
        &summary,
        glyph_cache,
    );

    if state.pending > 0 {
        let label = state.pending.min(99).to_string();
        let (bx, by, bw, bh) = entry_layout.badge;
        let digit_w = size.cell_width() * 0.62 * label.len() as f32;
        renderer.draw_doc_text_tracked(
            size,
            bx + (bw - digit_w) * 0.5,
            by + (bh - size.cell_height() * 0.62) * 0.5,
            0.62,
            0.0,
            Rgb::new(255, 255, 255),
            Flags::BOLD,
            &label,
            glyph_cache,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MessageQueueEntry, MessageQueueEntryLayout, layout, layout_for, reserved_height,
        reserved_height_for, summary_text,
    };
    use crate::display::UiLanguage;

    #[test]
    fn entry_is_bottom_docked_and_reserves_its_own_band() {
        let panel = (8.0, 48.0, 210.0, 744.0);
        let entry = layout_for(panel, 1.0, true);

        assert_eq!(entry.rect, (22.0, 726.0, 182.0, 54.0));
        assert_eq!(reserved_height_for(1.0, true), 80.0);
        assert!(entry.rect.1 >= panel.1);
        assert!(entry.rect.1 + entry.rect.3 < panel.1 + panel.3);
    }

    #[test]
    fn homepage_entry_is_hidden_until_real_queue_events_are_connected() {
        assert_eq!(layout((8.0, 48.0, 210.0, 744.0), 1.0), MessageQueueEntryLayout::default());
        assert_eq!(reserved_height(1.0), 0.0);
    }

    #[test]
    fn empty_and_actionable_summaries_never_invent_messages() {
        assert_eq!(summary_text(MessageQueueEntry::default(), UiLanguage::ZhCn), "暂无待处理");
        assert_eq!(
            summary_text(
                MessageQueueEntry { pending: 3, high_risk: 2, open: false },
                UiLanguage::EnUs,
            ),
            "2 high risk · 1 pending"
        );
    }
}
