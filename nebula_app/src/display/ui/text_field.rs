//! 自绘输入框的光标、选区与命中测试。
//!
//! # 为什么在组件层
//!
//! 在此之前每个输入框只有一个 `bool`（"整个内容是否被全选"），光标永远画在
//! 文本末尾。于是点击框内某处只能聚焦、不能定位，拖拽不能选中，方向键无处
//! 可去——这不是没接线，是**模型里根本没有"光标在哪"这个概念**。
//!
//! 这类能力是输入框的地板而不是天花板：Windows 上任何一个 `<input>` 都自带
//! 它们，用户不会认为这是功能，只会认为坏了。所以它必须住在组件层，让每个
//! 新输入框自动获得，而不是各自实现一遍——各自实现的结局必然是"地址框能拖
//! 选、密码框不能"这种说不出理由的不一致。
//!
//! # 模型
//!
//! 一个字符索引 `caret` 加一个可选的 `anchor`。选区是 `[min, max)` 的半开
//! 区间，`anchor == caret` 视作没有选区。索引单位是**字符**而不是字节，因为
//! 上层拿到的是"第几个字"；字节偏移只在真正要切 `String` 时才换算。
//!
//! 所有查询都对当前文本做 clamp。文本可能被外部改写（粘贴地址时自动拆出
//! 端口、切换认证方式时清空密码），此时旧的 caret 会越界——clamp 让它退化
//! 成"落在末尾"，而不是 panic。

use super::caret;
use super::theme::Skin;
use super::tokens::{radius, space};
use crate::renderer::ui::{Rgba, UiQuad};
use unicode_width::UnicodeWidthChar;

/// 输入框里的光标与选区。
#[derive(Debug, Clone, Default)]
pub struct TextCursor {
    caret: usize,
    anchor: Option<usize>,
}

impl TextCursor {
    /// 光标位置，已 clamp 到 `text` 的长度内。
    pub fn caret(&self, text: &str) -> usize {
        self.caret.min(char_count(text))
    }

    /// 选区的 `[start, end)`；没有选中任何字符时返回 `None`。
    pub fn range(&self, text: &str) -> Option<(usize, usize)> {
        let len = char_count(text);
        let anchor = self.anchor?.min(len);
        let caret = self.caret.min(len);
        (anchor != caret).then(|| (anchor.min(caret), anchor.max(caret)))
    }

    pub fn has_selection(&self, text: &str) -> bool {
        self.range(text).is_some()
    }

    pub fn selected_text(&self, text: &str) -> Option<String> {
        let (start, end) = self.range(text)?;
        Some(text.chars().skip(start).take(end - start).collect())
    }

    pub fn select_all(&mut self, text: &str) {
        let len = char_count(text);
        // 空串上不留锚点：否则 `has_selection` 为真而选区宽度为零，渲染会画
        // 出一条贴边的高亮，读起来像多了个看不懂的竖条。
        self.anchor = (len > 0).then_some(0);
        self.caret = len;
        caret::note_activity();
    }

    /// 单击定位：光标落在 `index`，清掉选区。
    pub fn place(&mut self, text: &str, index: usize) {
        self.caret = index.min(char_count(text));
        self.anchor = None;
        caret::note_activity();
    }

    /// 拖拽或 Shift+移动：保留锚点（没有就把当前光标当锚点），把光标拉到
    /// `index`。
    pub fn extend_to(&mut self, text: &str, index: usize) {
        let len = char_count(text);
        self.anchor.get_or_insert(self.caret.min(len));
        self.caret = index.min(len);
        caret::note_activity();
    }

    /// 光标塌到末尾。Tab 切进一个字段时用这个，而不是 `select_all`——
    /// 全选会让紧接着的一次按键把已有内容整个吃掉。
    pub fn collapse_to_end(&mut self, text: &str) {
        self.caret = char_count(text);
        self.anchor = None;
        caret::note_activity();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        caret::note_activity();
    }

    /// 展示窗口截掉前 `hidden` 个字符时的等效光标：配合截断后的展示串传给
    /// [`push_cursor`]。只做只读换算，不触发 caret 活跃节律。
    pub fn shifted(&self, hidden: usize) -> TextCursor {
        TextCursor {
            caret: self.caret.saturating_sub(hidden),
            anchor: self.anchor.map(|anchor| anchor.saturating_sub(hidden)),
        }
    }

    /// 左右移动一格。
    ///
    /// 有选区且不按 Shift 时，方向键是**塌陷到选区的那一端**而不是移动一格
    /// ——这是所有原生输入框的行为：选中一段后按 →，光标去选区右端，不是右
    /// 端再往右一个字符。
    pub fn step(&mut self, text: &str, forward: bool, extend: bool) {
        if !extend {
            if let Some((start, end)) = self.range(text) {
                self.caret = if forward { end } else { start };
                self.anchor = None;
                caret::note_activity();
                return;
            }
        }
        let len = char_count(text);
        let caret = self.caret.min(len);
        let next = if forward { (caret + 1).min(len) } else { caret.saturating_sub(1) };
        if extend {
            self.extend_to(text, next);
        } else {
            self.place(text, next);
        }
    }

    /// Home / End。
    pub fn jump(&mut self, text: &str, to_end: bool, extend: bool) {
        let index = if to_end { char_count(text) } else { 0 };
        if extend {
            self.extend_to(text, index);
        } else {
            self.place(text, index);
        }
    }

    /// 在光标处插入，先删掉选区。控制字符被丢弃——粘贴多行文本时把换行
    /// 带进单行输入框，会得到一个看不见却会被存盘的字符。
    pub fn insert(&mut self, text: &mut String, incoming: &str) {
        self.delete_selection(text);
        let filtered: String = incoming.chars().filter(|c| !c.is_control()).collect();
        if !filtered.is_empty() {
            let at = byte_index(text, self.caret.min(char_count(text)));
            let inserted = char_count(&filtered);
            text.insert_str(at, &filtered);
            self.caret = self.caret.min(char_count(text)) + inserted;
            self.anchor = None;
        }
        caret::note_activity();
    }

    /// Backspace：有选区删选区，否则删光标前一个字符。
    pub fn backspace(&mut self, text: &mut String) {
        if !self.delete_selection(text) {
            let caret = self.caret.min(char_count(text));
            if caret > 0 {
                let from = byte_index(text, caret - 1);
                let to = byte_index(text, caret);
                text.replace_range(from..to, "");
                self.caret = caret - 1;
            }
        }
        caret::note_activity();
    }

    /// Delete：有选区删选区，否则删光标后一个字符。
    pub fn delete_forward(&mut self, text: &mut String) {
        if !self.delete_selection(text) {
            let caret = self.caret.min(char_count(text));
            if caret < char_count(text) {
                let from = byte_index(text, caret);
                let to = byte_index(text, caret + 1);
                text.replace_range(from..to, "");
            }
        }
        caret::note_activity();
    }

    /// 删掉选区并把光标收到选区起点。返回是否真的删了东西，让调用方决定
    /// 要不要接着做"没有选区时"的那件事。
    fn delete_selection(&mut self, text: &mut String) -> bool {
        let Some((start, end)) = self.range(text) else {
            return false;
        };
        let from = byte_index(text, start);
        let to = byte_index(text, end);
        text.replace_range(from..to, "");
        self.caret = start;
        self.anchor = None;
        true
    }
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// 第 `index` 个字符的字节偏移；越界时给 `text.len()`，于是"在末尾插入"是
/// 自然的退化行为。
fn byte_index(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(at, _)| at)
}

/// 一段文本占多少显示列。等宽网格上一个全角字符占两列，所以不能用字符数。
pub fn columns(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(1)).sum()
}

/// 前 `index` 个字符占多少列。光标和选区的像素位置都由它算，绘制与命中因此
/// 共用同一套换算——两边各算一次必然会在全角字符上漂移。
pub fn columns_before(text: &str, index: usize) -> usize {
    text.chars().take(index).map(|c| c.width().unwrap_or(1)).sum()
}

/// 落在 `offset_x`（相对文本起点的像素）处的字符边界。
///
/// 判据是"过半进位"：点在一个字符的左半边，光标停在它**前面**；右半边，停在
/// 它后面。这是原生输入框的手感——点击的意图是"插到这里"，不是"选中这个字"。
pub fn index_at(text: &str, offset_x: f32, cell_w: f32) -> usize {
    if cell_w <= 0.0 {
        return 0;
    }
    let mut x = 0.0;
    for (index, character) in text.chars().enumerate() {
        let width = character.width().unwrap_or(1) as f32 * cell_w;
        if offset_x < x + width * 0.5 {
            return index;
        }
        x += width;
    }
    char_count(text)
}

/// 画选区高亮或光标竖线。
///
/// `display` 是**实际画在框里的那串字**：密码框传掩码，空框传空串（不要传
/// 占位文案，否则光标会跑到占位文字的末尾去）。索引与真文本一一对应，因为
/// 掩码是逐字符替换的。
///
/// `text_x` 由调用方给，因为对齐方式是调用方的事（端口居中、其余靠左）——
/// 组件层只负责让光标落在与文字同一套换算上。
#[allow(clippy::too_many_arguments)]
pub fn push_cursor(
    quads: &mut Vec<UiQuad>,
    field_y: f32,
    field_h: f32,
    text_x: f32,
    display: &str,
    cursor: &TextCursor,
    cell_w: f32,
    scale: f32,
    sk: &Skin,
) {
    let inset = space::XXS * scale;
    if let Some((start, end)) = cursor.range(display) {
        let from = text_x + columns_before(display, start) as f32 * cell_w;
        let width = (columns_before(display, end) - columns_before(display, start)) as f32 * cell_w;
        quads.push(UiQuad::solid(
            from,
            field_y + inset,
            width,
            field_h - inset * 2.0,
            radius::CHIP * scale,
            sk.accent_soft,
        ));
        return;
    }
    // 没有选区才画竖线。相位来自共享节律：打字时常亮、停手后才呼吸，
    // 且聚焦的瞬间必定是亮的。
    if !caret::is_on() {
        return;
    }
    let x = text_x + columns_before(display, cursor.caret(display)) as f32 * cell_w;
    let thickness = (1.5 * scale).max(1.0);
    quads.push(UiQuad::solid(
        x,
        field_y + inset,
        thickness,
        field_h - inset * 2.0,
        0.0,
        Rgba::new(sk.ink_strong.r, sk.ink_strong.g, sk.ink_strong.b, 235),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_lands_at_the_caret_not_the_end() {
        let mut text = "abcd".to_owned();
        let mut cursor = TextCursor::default();
        cursor.place(&text, 2);
        cursor.insert(&mut text, "XY");
        assert_eq!(text, "abXYcd");
        // 光标跟着插入的内容走，接着打字才会续在后面。
        assert_eq!(cursor.caret(&text), 4);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut text = "root@example.com".to_owned();
        let mut cursor = TextCursor::default();
        cursor.place(&text, 0);
        cursor.extend_to(&text, 4);
        assert_eq!(cursor.selected_text(&text).as_deref(), Some("root"));
        cursor.insert(&mut text, "admin");
        assert_eq!(text, "admin@example.com");
        assert!(!cursor.has_selection(&text));
    }

    #[test]
    fn backspace_eats_the_selection_before_the_character() {
        let mut text = "abcdef".to_owned();
        let mut cursor = TextCursor::default();
        cursor.place(&text, 1);
        cursor.extend_to(&text, 4);
        cursor.backspace(&mut text);
        assert_eq!(text, "aef");
        assert_eq!(cursor.caret(&text), 1);
        // 选区没了，这次才轮到删单个字符。
        cursor.backspace(&mut text);
        assert_eq!(text, "ef");
    }

    #[test]
    fn arrow_collapses_a_selection_to_its_edge() {
        let text = "abcdef".to_owned();
        let mut cursor = TextCursor::default();
        cursor.place(&text, 1);
        cursor.extend_to(&text, 4);
        cursor.step(&text, true, false);
        assert_eq!(cursor.caret(&text), 4, "按右应该跳到选区右端，不是右端再右一格");
        cursor.place(&text, 1);
        cursor.extend_to(&text, 4);
        cursor.step(&text, false, false);
        assert_eq!(cursor.caret(&text), 1);
    }

    #[test]
    fn shift_arrow_keeps_extending_from_the_anchor() {
        let text = "abcdef".to_owned();
        let mut cursor = TextCursor::default();
        cursor.place(&text, 3);
        cursor.step(&text, false, true);
        cursor.step(&text, false, true);
        assert_eq!(cursor.range(&text), Some((1, 3)));
    }

    #[test]
    fn a_stale_caret_degrades_to_the_end_instead_of_panicking() {
        let mut text = "192.168.1.1:2222".to_owned();
        let mut cursor = TextCursor::default();
        cursor.collapse_to_end(&text);
        // 粘贴地址后上层把端口拆走了，文本比光标记得的短。
        text = "192.168.1.1".to_owned();
        assert_eq!(cursor.caret(&text), char_count(&text));
        cursor.insert(&mut text, "x");
        assert_eq!(text, "192.168.1.1x");
    }

    #[test]
    fn clicking_picks_the_nearer_character_boundary() {
        let text = "abcd";
        // 一格宽 10：点在第 2 格的左半 → 停在它前面（索引 1）。
        assert_eq!(index_at(text, 11.0, 10.0), 1);
        // 右半 → 停在它后面（索引 2）。
        assert_eq!(index_at(text, 19.0, 10.0), 2);
        // 点在文字右边的空白里 → 落到末尾。
        assert_eq!(index_at(text, 999.0, 10.0), 4);
        assert_eq!(index_at(text, -5.0, 10.0), 0);
    }

    #[test]
    fn wide_characters_advance_two_columns() {
        let text = "生产数据库";
        assert_eq!(columns(text), 10);
        assert_eq!(columns_before(text, 2), 4);
        // 点在第三个字的左半边，光标要停在它前面而不是被算成第 5 个字符。
        assert_eq!(index_at(text, 41.0, 10.0), 2);
    }

    #[test]
    fn paste_strips_control_characters() {
        let mut text = String::new();
        let mut cursor = TextCursor::default();
        cursor.insert(&mut text, "dev@host\r\n");
        assert_eq!(text, "dev@host");
    }
}
