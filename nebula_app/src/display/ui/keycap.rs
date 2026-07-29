//! 图8 键帽规范（2026-07-29 用户裁定）：所有展示键位的地方统一为逐键
//! chip——hairline 圈边 + panel/surface 叠底、小圆角、弱墨 "+" 分隔；
//! 不再各处自造键帽样式，也不叠加额外装饰。
//!
//! `layout_combo` 是几何唯一来源：quad pass 与 text pass 各调一次拿到
//! 相同矩形（同 settings 键位行 quad/text 共用几何的既有约定）。

use unicode_width::UnicodeWidthChar;

use super::theme::Skin;
use crate::renderer::ui::UiQuad;

/// Chip height in logical px（与 palette 底栏既有键帽一致）。
pub const KEY_H: f32 = 20.0;

fn cols(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// One rendered key chip: `(x, w, key)`；`pluses` 是分隔加号的绘制 X。
pub struct ComboChips {
    pub chips: Vec<(f32, f32, String)>,
    pub pluses: Vec<f32>,
    /// Bounding box `(x, y, w, h)` across every chip.
    pub bounds: (f32, f32, f32, f32),
}

/// Lay out a "Ctrl+Shift+P" style combo as per-key chips, right-aligned so
/// the last chip ends at `right_x`, vertically centered on `center_y`.
pub fn layout_combo(
    combo: &str,
    right_x: f32,
    center_y: f32,
    cell_w: f32,
    scale: f32,
) -> ComboChips {
    let s = |v: f32| v * scale;
    let key_h = s(KEY_H);
    let key_y = center_y - key_h / 2.0;
    let keys: Vec<&str> = combo.split('+').filter(|k| !k.is_empty()).collect();
    let chip_w = |key: &str| cols(key) as f32 * cell_w + s(10.0);
    let total: f32 = keys.iter().map(|k| chip_w(k)).sum::<f32>()
        + keys.len().saturating_sub(1) as f32 * (cell_w + s(8.0));
    let mut x = right_x - total;
    let mut chips = Vec::with_capacity(keys.len());
    let mut pluses = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let w = chip_w(key);
        chips.push((x, w, (*key).to_owned()));
        x += w;
        if index + 1 < keys.len() {
            pluses.push(x + s(4.0));
            x += cell_w + s(8.0);
        }
    }
    ComboChips { chips, pluses, bounds: (right_x - total, key_y, total, key_h) }
}

/// The one chip recipe: hairline ring + panel + surface, radius 4.
pub fn push_chip(
    quads: &mut Vec<UiQuad>,
    sk: &Skin,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    quads.push(UiQuad::solid(x - s(1.0), y - s(1.0), w + s(2.0), h + s(2.0), s(5.0), sk.hairline));
    quads.push(UiQuad::solid(x, y, w, h, s(4.0), sk.panel));
    quads.push(UiQuad::solid(x, y, w, h, s(4.0), sk.surface));
}

/// Push every chip of a laid-out combo.
pub fn push_combo(quads: &mut Vec<UiQuad>, sk: &Skin, combo: &ComboChips, scale: f32) {
    let (_, y, _, h) = combo.bounds;
    for &(x, w, _) in &combo.chips {
        push_chip(quads, sk, x, y, w, h, scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_right_aligns_and_separates_keys() {
        let combo = layout_combo("Ctrl+K", 500.0, 100.0, 10.0, 1.0);
        assert_eq!(combo.chips.len(), 2);
        assert_eq!(combo.pluses.len(), 1);
        let (last_x, last_w, ref last_key) = combo.chips[1];
        assert_eq!(last_key, "K");
        assert!((last_x + last_w - 500.0).abs() < 0.01, "last chip ends at right_x");
        let (bx, _, bw, bh) = combo.bounds;
        assert!((bx + bw - 500.0).abs() < 0.01);
        assert_eq!(bh, KEY_H);
        // chips never overlap the plus separator
        let (first_x, first_w, _) = combo.chips[0];
        assert!(combo.pluses[0] >= first_x + first_w);
    }

    #[test]
    fn single_key_has_no_plus() {
        let combo = layout_combo("Esc", 300.0, 50.0, 8.0, 2.0);
        assert_eq!(combo.chips.len(), 1);
        assert!(combo.pluses.is_empty());
    }
}
