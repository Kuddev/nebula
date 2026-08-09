//! 浮层列表组件：命令面板、⌘K shell 选择器、Ctrl+Shift+O 会话快跳、下拉
//! 浮层等「面板 + 搜索 + 行列表」类浮层共用的行几何与状态皮肤。
//!
//! 收敛之前，这些配方在 `command_palette.rs`、`settings.rs` 与
//! `widgets.rs` 里各写了一份：行选中/hover 的药丸、图标瓦片（发丝环 +
//! 卡片底）、身份 chip（「默认」徽标 / AI 来源 chip）、页脚提示带、查询
//! 光标梁——同一像素语言、五处手写。本模块是它们的唯一来源；新的浮层
//! 面板直接调这里，不再手写 quad 序列（见 `ui/mod.rs` 的层契约）。
//!
//! 契约与本层其他模块一致：纯几何 + quad 输出，不持有 Display 状态、
//! 不做 hover 决策；文字由文字 pass 用同一份 rect 画。

use super::surface;
use super::theme::Skin;
use super::tokens::radius;
use crate::renderer::ui::{Rgba, UiQuad};

/// `(x, y, width, height)`，物理像素。
pub type Rect = (f32, f32, f32, f32);

/// 一行的交互状态。调用方决定状态，本层只翻译成像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowState {
    Idle,
    Hover,
    /// 键盘/指针当前选中行。
    Selected,
}

/// 满宽行态（launcher / picker 卡片列表）：默认全透明露出面板底，hover 一层
/// 静底，选中用中性 `hover_strong`——强调色预算留给真正需要它的地方
/// （2026-07-29 用户裁定：列表行不画卡片、不上强调色）。
pub(crate) fn push_row_state(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    radius_px: f32,
    sk: &Skin,
    state: RowState,
) {
    let fill = match state {
        RowState::Idle => return,
        RowState::Hover => sk.hover,
        RowState::Selected => sk.hover_strong,
    };
    quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, radius_px, fill));
}

/// 内凹选项行态（命令列表 / combobox 下拉）：行 rect 是整行命中区，绘制时
/// 上下各让 2px 呼吸缝（hover 与选中同几何——迁移前命令面板的选中药丸从
/// `ry` 起画、比 hover 高 2px，是无人察觉的不对称，此处一并归一）。选中走
/// `accent_soft`，可选配左缘 2px 强调梁（命令面板用它标注键盘选中行；下拉
/// 的勾选列已有 ✓，不再加梁）。
pub(crate) fn push_option_row(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    scale: f32,
    sk: &Skin,
    state: RowState,
    beam: bool,
) {
    let s = |v: f32| v * scale;
    let (rx, ry, rw, rh) = rect;
    let fill = match state {
        RowState::Idle => return,
        RowState::Hover => sk.hover,
        RowState::Selected => sk.accent_soft,
    };
    quads.push(UiQuad::solid(rx, ry + s(2.0), rw, rh - s(4.0), radius::CONTROL * scale, fill));
    if beam && state == RowState::Selected {
        let accent = Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255);
        quads.push(UiQuad::solid(rx, ry + s(5.0), s(2.0), rh - s(14.0), s(1.0), accent));
    }
}

/// 图标瓦片：发丝环 + 卡片底。品牌图标与回落字形的视觉重量由瓦片统一，
/// 不再取决于素材是否自带底色。`size_px` 已含缩放。
pub(crate) fn icon_tile_rect(left_x: f32, row: Rect, size_px: f32) -> Rect {
    (left_x, row.1 + (row.3 - size_px) * 0.5, size_px, size_px)
}

pub(crate) fn push_icon_tile(quads: &mut Vec<UiQuad>, rect: Rect, scale: f32, sk: &Skin) {
    surface::push_stroke(quads, rect, radius::ICON_TILE * scale, scale, sk.hairline);
    quads.push(UiQuad::solid(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        radius::ICON_TILE * scale,
        sk.card,
    ));
}

/// 身份 chip 的几何：右缘对齐、行内垂直居中。`text_cols` 是 chip 文字的
/// 终端列宽（调用方用 unicode 宽度算好），宽度含左右各 6px 内衬。
pub(crate) fn identity_chip_rect(
    right_x: f32,
    row: Rect,
    text_cols: f32,
    cell_w: f32,
    scale: f32,
) -> Rect {
    let s = |v: f32| v * scale;
    let w = text_cols * cell_w + s(12.0);
    (right_x - w, row.1 + s(7.0), w, row.3 - s(14.0))
}

/// 身份 chip 的底：发丝环 + 静底。「默认」徽标、AI 会话来源 chip 同宗——
/// 标注身份，不与选中态抢强调色。文字由文字 pass 按同一 rect 画。
pub(crate) fn push_identity_chip(quads: &mut Vec<UiQuad>, rect: Rect, scale: f32, sk: &Skin) {
    let corner = radius::CHIP * scale;
    surface::push_stroke(quads, rect, corner, scale, sk.hairline);
    quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, corner, sk.surface));
}

/// 页脚提示带：面板底部一条静底，键帽提示由文字 pass 落在上面。
pub(crate) fn push_footer_band(quads: &mut Vec<UiQuad>, rect: Rect, sk: &Skin) {
    quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, 0.0, sk.surface));
}

/// 查询输入的文本光标：细梁 quad 而不是 `▏` 字形。字形光标占满一个 cell，
/// 空态 placeholder 必须右让一格、首字符落下时又跳回来；细梁不占列宽，
/// placeholder 与真实输入落在同一 x 上，输入过程没有位移。
pub(crate) fn push_query_caret(
    quads: &mut Vec<UiQuad>,
    x: f32,
    input: Rect,
    cell_h: f32,
    scale: f32,
    sk: &Skin,
) {
    if !super::caret::is_on() {
        return;
    }
    quads.push(UiQuad::solid(
        x,
        super::widgets::centered_y(input.1, input.3, cell_h),
        1.5 * scale,
        cell_h,
        0.0,
        Rgba::new(sk.ink_strong.r, sk.ink_strong.g, sk.ink_strong.b, 255),
    ));
}

/// 查询全选高亮带：`accent_soft` 衬底，几何随文字列宽走。
pub(crate) fn push_selection_band(
    quads: &mut Vec<UiQuad>,
    text_x: f32,
    input: Rect,
    text_cols: f32,
    cell_w: f32,
    scale: f32,
    sk: &Skin,
) {
    let s = |v: f32| v * scale;
    let width = (text_cols * cell_w).min((input.0 + input.2 - text_x).max(0.0));
    quads.push(UiQuad::solid(
        text_x - s(2.0),
        input.1 + s(7.0),
        width + s(4.0),
        input.3 - s(14.0),
        radius::CHIP * scale,
        sk.accent_soft,
    ));
}
