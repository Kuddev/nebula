//! 键帽规范：所有展示键位的地方共用一颗 chip——hairline 圈边 +
//! panel/surface 叠底、小圆角；不再各处自造键帽样式，也不叠加额外装饰。
//!
//! # 为什么是**一整串**而不是逐键拆分
//!
//! 2026-07-29 用户裁定，推翻了本文件的上一版。逐键 chip 是 macOS 的形式：
//! 那里修饰键是 `⌘ ⇧ ⌥` 单字符图形，一符号一方块天然成立、宽度还整齐。
//! Windows 上修饰键是 `Ctrl` `Shift` `Alt` 这种**单词**，拆开会得到一排
//! 宽度参差的方块，画面反而更碎。
//!
//! 更重要的是心智迁移：Windows 用户的既有参照系（VS Code、Windows
//! Terminal、Office）一律把组合键显示成 `Ctrl+Shift+P` 一整串。跟随它，
//! 用户把已有的识别习惯整体搬过来，学习成本为零；自造形式则要他们先解析
//! 一遍"这几个方块是一个组合还是三个选项"。
//!
//! `layout_combo` 是几何唯一来源：quad pass 与 text pass 各调一次拿到
//! 相同矩形（同 settings 键位行 quad/text 共用几何的既有约定）。

use unicode_width::UnicodeWidthChar;

use super::theme::Skin;
use super::tokens::radius;
use crate::renderer::ui::UiQuad;

/// Chip height in logical px（与 palette 底栏既有键帽一致）。
pub const KEY_H: f32 = 20.0;

/// chip 内的左右呼吸缝。整串比单键长，边距太小会显得字挤在框上。
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

/// Lay out a `Ctrl+Shift+P` style combo as one chip, right-aligned so it ends
/// at `right_x`, vertically centered on `center_y`.
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
    // 规范化：丢掉空段后用 "+" 重新连接。畸形输入（末尾多一个 "+"、
    // 连续 "++"）因此得到稳定结果，与上一版逐键实现的取舍保持一致。
    let label = combo.split('+').filter(|k| !k.is_empty()).collect::<Vec<_>>().join("+");
    let width = cols(&label) as f32 * cell_w + s(PAD_X) * 2.0;
    let x = right_x - width;
    ComboChips { chips: vec![(x, width, label)], bounds: (x, key_y, width, key_h) }
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
    fn combo_is_one_chip_carrying_the_whole_string() {
        // Windows 心智：`Ctrl+K` 是一颗键帽，不是两颗方块加一个加号。
        let combo = layout_combo("Ctrl+K", 500.0, 100.0, 10.0, 1.0);
        assert_eq!(combo.chips.len(), 1);
        let (x, w, ref label) = combo.chips[0];
        assert_eq!(label, "Ctrl+K");
        assert!((x + w - 500.0).abs() < 0.01, "chip 右缘落在 right_x");
        assert_eq!(w, 6.0 * 10.0 + PAD_X * 2.0, "宽 = 整串列宽 + 左右呼吸缝");
        let (bx, _, bw, bh) = combo.bounds;
        assert!((bx + bw - 500.0).abs() < 0.01);
        assert_eq!(bh, KEY_H);
    }

    #[test]
    fn three_key_combo_stays_a_single_chip() {
        let combo = layout_combo("Ctrl+Shift+P", 400.0, 50.0, 8.0, 1.0);
        assert_eq!(combo.chips.len(), 1);
        assert_eq!(combo.chips[0].2, "Ctrl+Shift+P");
    }

    #[test]
    fn malformed_input_normalizes_instead_of_drawing_empty_keys() {
        // 末尾多一个 "+"、连续 "++" 都不该画出空键帽。
        for raw in ["Ctrl+", "+Ctrl", "Ctrl++K"] {
            let combo = layout_combo(raw, 300.0, 50.0, 8.0, 1.0);
            assert_eq!(combo.chips.len(), 1);
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
