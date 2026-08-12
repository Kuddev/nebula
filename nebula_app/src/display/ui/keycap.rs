//! 键帽规范：所有展示键位的地方共用一颗 chip——hairline 圈边 +
//! panel/surface 叠底、小圆角；不再各处自造键帽样式，也不叠加额外装饰。
//!
//! 组合键按物理按键拆成相邻的小键帽。每颗键帽都有独立的表面、底边和
//! 呼吸缝，长组合键因此仍然可扫读，不会变成一块写满文字的灰色胶囊。
//!
//! `layout_combo` 是几何唯一来源：quad pass 与 text pass 各调一次拿到
//! 相同矩形（同 settings 键位行 quad/text 共用几何的既有约定）。

use unicode_width::UnicodeWidthChar;

use super::theme::Skin;
use super::tokens::radius;
use crate::renderer::ui::UiQuad;

/// Chip height in logical px（与 palette 底栏既有键帽一致）。
pub const KEY_H: f32 = 20.0;

/// 每颗键帽内的左右呼吸缝。单键也要留出足够边距，避免文字贴住轮廓。
const PAD_X: f32 = 7.0;

fn cols(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// One rendered key chip: `(x, w, text)`。
///
/// 仍是 `Vec` 而不是单个字段：调用方按序列消费，未来若某个平台要回到逐键
/// 形式（例如 macOS 版），只需换掉本文件的布局函数，绘制端不动。
pub struct ComboChips {
    pub chips: Vec<(f32, f32, String)>,
    /// Bounding box `(x, y, w, h)` across every chip.
    pub bounds: (f32, f32, f32, f32),
}

/// Lay out a `Ctrl+Shift+P` style combo as separate keycaps, right-aligned so
/// the final cap ends at `right_x`, vertically centered on `center_y`.
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
    // 丢掉空段，避免配置里的末尾/连续 `+` 生成空键帽；加号是键帽之间的
    // 视觉连接，不占任何一颗键帽的文字宽度。
    let labels: Vec<&str> = combo.split('+').filter(|key| !key.is_empty()).collect();
    let gap = s(4.0);
    let widths: Vec<f32> =
        labels.iter().map(|label| cols(label) as f32 * cell_w + s(PAD_X) * 2.0).collect();
    let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    let mut x = right_x - total;
    let chips = labels
        .into_iter()
        .zip(widths)
        .map(|(label, width)| {
            let chip = (x, width, label.to_owned());
            x += width + gap;
            chip
        })
        .collect();
    ComboChips { chips, bounds: (right_x - total, key_y, total, key_h) }
}

/// The one chip recipe: hairline ring + panel + surface, chip radius.
pub fn push_chip(quads: &mut Vec<UiQuad>, sk: &Skin, x: f32, y: f32, w: f32, h: f32, scale: f32) {
    push_chip_with_hover(quads, sk, x, y, w, h, scale, false);
}

/// Same chip recipe with a quiet hover wash. The geometry is intentionally
/// identical in both states so a shortcut chip never shifts under the pointer.
pub fn push_chip_with_hover(
    quads: &mut Vec<UiQuad>,
    sk: &Skin,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
    hovered: bool,
) {
    push_chip_toned(quads, sk, x, y, w, h, scale, hovered, false);
}

/// 完整键帽配方（2026-08-09 对齐原型 .kbd）：surface 底 + **底边 1.5px
/// 实体感**替代四边描边——四边环在 20px 高度上糊成灰块，只留底边才读得出
/// 「可按」。`danger` 是冲突键帽：danger 13% 底 + 文字侧配 danger 墨。
/// 回滚（四边 hairline 环旧配方）：
/// super::surface::push_stroke(quads, (x, y, w, h), corner, scale, sk.hairline);
#[allow(clippy::too_many_arguments)]
pub fn push_chip_toned(
    quads: &mut Vec<UiQuad>,
    sk: &Skin,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
    hovered: bool,
    danger: bool,
) {
    let corner = radius::CHIP * scale;
    quads.push(UiQuad::solid(x, y, w, h, corner, sk.panel));
    let fill = if hovered { super::surface::over(sk.hover, sk.surface) } else { sk.surface };
    quads.push(UiQuad::solid(x, y, w, h, corner, fill));
    if danger {
        // 原型 .kbd.clash：color-mix(danger 13%)。
        let tint = crate::renderer::ui::Rgba::new(sk.danger.r, sk.danger.g, sk.danger.b, 33);
        quads.push(UiQuad::solid(x, y, w, h, corner, tint));
    }
    let lip = (1.5 * scale).max(1.0);
    quads.push(UiQuad::solid(x, y + h - lip, w, lip, lip * 0.4, sk.hairline));
}

/// Push every chip of a laid-out combo.
pub fn push_combo(quads: &mut Vec<UiQuad>, sk: &Skin, combo: &ComboChips, scale: f32) {
    push_combo_with_hover(quads, sk, combo, scale, false);
}

/// Push a combo whose surface follows a transient overlay's alpha. Context
/// menus animate their panel and labels together; fading the three keycap
/// layers here prevents the shortcut caps from popping in a frame early.
pub fn push_combo_with_progress(
    quads: &mut Vec<UiQuad>,
    sk: &Skin,
    combo: &ComboChips,
    scale: f32,
    progress: f32,
) {
    let mut faded = *sk;
    faded.panel = super::surface::fade(faded.panel, progress);
    faded.surface = super::surface::fade(faded.surface, progress);
    faded.hairline = super::surface::fade(faded.hairline, progress);
    push_combo(quads, &faded, combo, scale);
}

/// Push every chip of a laid-out combo, with a subtle hover color only.
pub fn push_combo_with_hover(
    quads: &mut Vec<UiQuad>,
    sk: &Skin,
    combo: &ComboChips,
    scale: f32,
    hovered: bool,
) {
    let (_, y, _, h) = combo.bounds;
    for &(x, w, _) in &combo.chips {
        push_chip_with_hover(quads, sk, x, y, w, h, scale, hovered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_is_split_into_keycaps_and_right_aligned() {
        let combo = layout_combo("Ctrl+K", 500.0, 100.0, 10.0, 1.0);
        assert_eq!(combo.chips.len(), 2);
        let (x, w, ref label) = combo.chips[0];
        assert_eq!(label, "Ctrl");
        assert!((combo.chips[1].0 + combo.chips[1].1 - 500.0).abs() < 0.01);
        assert!(combo.chips[1].0 > x + w, "键帽之间要有稳定呼吸缝");
        let (bx, _, bw, bh) = combo.bounds;
        assert!((bx + bw - 500.0).abs() < 0.01);
        assert_eq!(bh, KEY_H);
    }

    #[test]
    fn three_key_combo_has_three_caps() {
        let combo = layout_combo("Ctrl+Shift+P", 400.0, 50.0, 8.0, 1.0);
        assert_eq!(combo.chips.len(), 3);
        assert_eq!(
            combo.chips.iter().map(|chip| chip.2.as_str()).collect::<Vec<_>>(),
            ["Ctrl", "Shift", "P"]
        );
    }

    #[test]
    fn malformed_input_normalizes_instead_of_drawing_empty_keys() {
        // 末尾多一个 "+"、连续 "++" 都不该画出空键帽。
        for raw in ["Ctrl+", "+Ctrl", "Ctrl++K"] {
            let combo = layout_combo(raw, 300.0, 50.0, 8.0, 1.0);
            assert!(!combo.chips.is_empty());
            assert!(!combo.chips[0].2.is_empty());
            assert!(!combo.chips[0].2.ends_with('+'));
        }
    }

    #[test]
    fn single_key_needs_no_separator() {
        let combo = layout_combo("Esc", 300.0, 50.0, 8.0, 2.0);
        assert_eq!(combo.chips.len(), 1);
        assert_eq!(combo.chips[0].2, "Esc");
    }
}
