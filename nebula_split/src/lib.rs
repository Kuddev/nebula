//! Nebula 分屏规则的共享权威实现：布局树、几何切割、分隔条拖拽与方向聚焦。
//!
//! 从旧壳 `nebula_app/src/window_context/split.rs` 逐字对照移植成纯数据 +
//! 纯函数——切割数学（floor/钳制次序）、拖拽比例曲线（关闭边距/预览钉边/
//! 提交量化）与最近邻导航（垂直漂移 4 倍惩罚）以旧壳行为为权威，任何 UI
//! 框架类型都不出现在这里。
//!
//! 同步事实（记录在案）：旧壳 `split.rs` 是体内既有实现，按 P4 裁定冻结
//! 保留；新 UI 一律读本 crate。改规则时两处同步，直到 P3 接入完成。
//!
//! 坐标系：屏幕像素、左上原点。`Rect` 是 pane 的真实分配矩形；旧壳把它
//! 换算成整窗 `SizeInfo` + 四边 padding 属于壳内投影细节，不进合同。

use std::mem;

/// 可见分隔条厚度（逻辑像素；使用处乘以 scale 后取整，至少 1 物理像素）。
pub const DIVIDER_GAP: f32 = 2.0;
/// 分隔条命中扩边（逻辑像素）：视觉线很细，抓取目标刻意加宽。
pub const HIT_SLOP: f32 = 8.0;
/// 松手时原始比例越过该边距即关闭被挤压的一侧
/// （`< margin` 关第一个孩子，`> 1 - margin` 关第二个）。
pub const CLOSE_MARGIN: f32 = 0.06;
/// 提交时比例的硬钳制带：任何一侧都不小于总宽的 10%。
pub const RATIO_CLAMP: (f32, f32) = (0.10, 0.90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    LeftRight,
    TopBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitNav {
    Left,
    Right,
    Up,
    Down,
}

/// 屏幕矩形，左上原点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

/// 一条分隔条的身份：屏幕矩形 + 所属 Split 节点的树路径
/// （`false` = 第一个孩子，`true` = 第二个），以及该节点铺陈孩子的内容矩形。
#[derive(Debug, Clone)]
pub struct Divider {
    pub rect: Rect,
    pub direction: SplitDirection,
    pub path: Vec<bool>,
    pub viewport: Rect,
}

/// 从树上摘除叶子的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome<T> {
    /// 目标不在这棵树上。
    NotFound,
    /// 目标是唯一叶子；调用方应关闭整个 tab。
    WasRoot,
    /// 已摘除且父节点塌缩；焦点应移交给幸存子树的首叶。
    Collapsed(T),
}

/// 分屏布局树：叶子是 pane（泛型 id），内部节点是带比例的二分。
#[derive(Debug, Clone, PartialEq)]
pub enum SplitTree<T> {
    Leaf(T),
    Split {
        direction: SplitDirection,
        /// 第一个孩子的占比（提交值）。
        ratio: f32,
        /// 拖拽中的预览比例；PTY 尺寸跟随提交值直到松手。
        preview_ratio: Option<f32>,
        dragging: bool,
        first: Box<SplitTree<T>>,
        second: Box<SplitTree<T>>,
    },
}

/// 一次几何铺陈的输出：每个 pane 的矩形 + 所有分隔条。
#[derive(Debug, Clone)]
pub struct SplitLayout<T> {
    pub panes: Vec<(T, Rect)>,
    pub dividers: Vec<Divider>,
}

impl<T: Copy + Eq> SplitTree<T> {
    pub fn leaf(id: T) -> Self {
        SplitTree::Leaf(id)
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, SplitTree::Leaf(_))
    }

    /// 深度优先收集全部叶子。
    pub fn leaves(&self) -> Vec<T> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<T>) {
        match self {
            SplitTree::Leaf(id) => out.push(*id),
            SplitTree::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            },
        }
    }

    /// 最左/最上的叶子。
    pub fn first_leaf(&self) -> T {
        match self {
            SplitTree::Leaf(id) => *id,
            SplitTree::Split { first, .. } => first.first_leaf(),
        }
    }

    pub fn contains(&self, id: T) -> bool {
        match self {
            SplitTree::Leaf(leaf) => *leaf == id,
            SplitTree::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// 把 `target` 叶子替换成 `[target, new]` 的 Split。找到返回 `true`。
    pub fn split_leaf(
        &mut self,
        target: T,
        new: T,
        direction: SplitDirection,
        ratio: f32,
    ) -> bool {
        match self {
            SplitTree::Leaf(id) if *id == target => {
                let old = *id;
                *self = SplitTree::Split {
                    direction,
                    ratio,
                    preview_ratio: None,
                    dragging: false,
                    first: Box::new(SplitTree::Leaf(old)),
                    second: Box::new(SplitTree::Leaf(new)),
                };
                true
            },
            SplitTree::Leaf(_) => false,
            SplitTree::Split { first, second, .. } => {
                first.split_leaf(target, new, direction, ratio)
                    || second.split_leaf(target, new, direction, ratio)
            },
        }
    }

    /// 摘除 `target` 叶子并塌缩其父节点，兄弟子树接管腾出的空间。
    pub fn remove_leaf(&mut self, target: T) -> RemoveOutcome<T> {
        if let SplitTree::Leaf(id) = self {
            return if *id == target { RemoveOutcome::WasRoot } else { RemoveOutcome::NotFound };
        }

        if let SplitTree::Split { first, second, .. } = self {
            // 直接孩子命中 → 塌缩为兄弟。占位叶子随 *self 覆盖一起丢弃。
            if matches!(first.as_ref(), SplitTree::Leaf(id) if *id == target) {
                let survivor = mem::replace(second.as_mut(), SplitTree::Leaf(target));
                let focus = survivor.first_leaf();
                *self = survivor;
                return RemoveOutcome::Collapsed(focus);
            }
            if matches!(second.as_ref(), SplitTree::Leaf(id) if *id == target) {
                let survivor = mem::replace(first.as_mut(), SplitTree::Leaf(target));
                let focus = survivor.first_leaf();
                *self = survivor;
                return RemoveOutcome::Collapsed(focus);
            }
            return match first.remove_leaf(target) {
                RemoveOutcome::NotFound => second.remove_leaf(target),
                other => other,
            };
        }

        RemoveOutcome::NotFound
    }

    /// 按根路径寻址节点（`false` = 第一个孩子，`true` = 第二个）。
    pub fn node_mut(&mut self, path: &[bool]) -> Option<&mut SplitTree<T>> {
        let mut node = self;
        for &second in path {
            match node {
                SplitTree::Split { first, second: s, .. } => {
                    node = if second { s.as_mut() } else { first.as_mut() };
                },
                SplitTree::Leaf(_) => return None,
            }
        }
        Some(node)
    }

    /// 把树铺陈进 `viewport`：每个叶子得到自己的矩形，每个 Split 产出一条
    /// 分隔条。`use_preview` 时 Split 使用拖拽预览比例（分隔条随指针走），
    /// 否则使用提交比例（pane 内容保持稳定）。
    ///
    /// 切割数学与旧壳逐字一致：可用长度 = 总长 - 分隔条，第一段先 floor
    /// 再双向钳制到"至少一个单元格"，第二段吃掉余数。
    pub fn layout(
        &self,
        viewport: Rect,
        cell_w: f32,
        cell_h: f32,
        divider: f32,
        use_preview: bool,
    ) -> SplitLayout<T> {
        let mut out = SplitLayout { panes: Vec::new(), dividers: Vec::new() };
        let mut path = Vec::new();
        self.collect_rects(viewport, cell_w, cell_h, divider, use_preview, &mut path, &mut out);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_rects(
        &self,
        vp: Rect,
        cell_w: f32,
        cell_h: f32,
        divider: f32,
        use_preview: bool,
        path: &mut Vec<bool>,
        out: &mut SplitLayout<T>,
    ) {
        match self {
            SplitTree::Leaf(id) => out.panes.push((*id, vp)),
            SplitTree::Split { direction, ratio, preview_ratio, first, second, .. } => {
                let r = if use_preview { preview_ratio.unwrap_or(*ratio) } else { *ratio };
                match direction {
                    SplitDirection::LeftRight => {
                        let usable = (vp.w - divider).max(cell_w);
                        let first_w =
                            (usable * r).floor().max(cell_w).min((usable - cell_w).max(cell_w));
                        let second_w = (usable - first_w).max(cell_w);
                        out.dividers.push(Divider {
                            rect: Rect::new(vp.x + first_w, vp.y, divider, vp.h),
                            direction: *direction,
                            path: path.clone(),
                            viewport: vp,
                        });
                        path.push(false);
                        first.collect_rects(
                            Rect::new(vp.x, vp.y, first_w, vp.h),
                            cell_w,
                            cell_h,
                            divider,
                            use_preview,
                            path,
                            out,
                        );
                        path.pop();
                        path.push(true);
                        second.collect_rects(
                            Rect::new(vp.x + first_w + divider, vp.y, second_w, vp.h),
                            cell_w,
                            cell_h,
                            divider,
                            use_preview,
                            path,
                            out,
                        );
                        path.pop();
                    },
                    SplitDirection::TopBottom => {
                        let usable = (vp.h - divider).max(cell_h);
                        let first_h =
                            (usable * r).floor().max(cell_h).min((usable - cell_h).max(cell_h));
                        let second_h = (usable - first_h).max(cell_h);
                        out.dividers.push(Divider {
                            rect: Rect::new(vp.x, vp.y + first_h, vp.w, divider),
                            direction: *direction,
                            path: path.clone(),
                            viewport: vp,
                        });
                        path.push(false);
                        first.collect_rects(
                            Rect::new(vp.x, vp.y, vp.w, first_h),
                            cell_w,
                            cell_h,
                            divider,
                            use_preview,
                            path,
                            out,
                        );
                        path.pop();
                        path.push(true);
                        second.collect_rects(
                            Rect::new(vp.x, vp.y + first_h + divider, vp.w, second_h),
                            cell_w,
                            cell_h,
                            divider,
                            use_preview,
                            path,
                            out,
                        );
                        path.pop();
                    },
                }
            },
        }
    }
}

/// `(x, y)` 命中的分隔条（含扩边）。`slop` 已按 scale 换算。
pub fn hit_divider<'a>(dividers: &'a [Divider], x: f32, y: f32, slop: f32) -> Option<&'a Divider> {
    dividers.iter().find(|d| {
        x >= d.rect.x - slop
            && x <= d.rect.x + d.rect.w + slop
            && y >= d.rect.y - slop
            && y <= d.rect.y + d.rect.h + slop
    })
}

/// 指针位置 → 本次拖拽的原始比例，**不钳制**。刻意不按整格量化：预览必须
/// 逐像素跟手才顺滑，比例在提交时才吸附网格。超出 `0..1` 表示指针越过了
/// pane 边缘（拖拽关闭领域）。
pub fn drag_ratio(
    direction: SplitDirection,
    viewport: Rect,
    divider: f32,
    cell: f32,
    x: f32,
    y: f32,
) -> f32 {
    let (pos, origin, extent) = match direction {
        SplitDirection::LeftRight => (x, viewport.x, viewport.w),
        SplitDirection::TopBottom => (y, viewport.y, viewport.h),
    };
    let usable = (extent - divider).max(cell);
    (pos - origin - divider * 0.5) / usable
}

/// 原始拖拽比例 → 预览比例：常规带内跟手；关闭区钉死在边缘，让 pane 可见地
/// 塌下去，示意"松手即关"。
pub fn preview_ratio(raw: f32) -> f32 {
    if raw < CLOSE_MARGIN {
        0.02
    } else if raw > 1.0 - CLOSE_MARGIN {
        0.98
    } else {
        raw.clamp(RATIO_CLAMP.0, RATIO_CLAMP.1)
    }
}

/// 松手时是否触发拖拽关闭：`Some(false)` 关第一个孩子，`Some(true)` 关第二个。
pub fn drag_close_target(raw: f32) -> Option<bool> {
    if raw < CLOSE_MARGIN {
        Some(false)
    } else if raw > 1.0 - CLOSE_MARGIN {
        Some(true)
    } else {
        None
    }
}

/// 提交比例：吸附到整数个单元格（pane 尺寸与字符网格对齐）再硬钳制。
pub fn commit_ratio(preview: f32, extent: f32, divider: f32, cell: f32) -> f32 {
    let cell = cell.max(1.0);
    let usable = (extent - divider).max(cell);
    (((preview * usable) / cell).round() * cell / usable).clamp(RATIO_CLAMP.0, RATIO_CLAMP.1)
}

/// 从 `focused` 出发沿 `nav` 方向找最近的 pane。"最近"偏好垂直轴上对齐的
/// pane：垂直漂移按 4 倍计入距离，尽量停留在同一行/列。
pub fn nav_target<T: Copy + Eq>(panes: &[(T, Rect)], focused: T, nav: SplitNav) -> Option<T> {
    let (_, frect) = panes.iter().find(|(id, _)| *id == focused)?;
    let (fcx, fcy) = frect.center();

    let mut best: Option<(T, f32)> = None;
    for (id, rect) in panes {
        if *id == focused {
            continue;
        }
        let (cx, cy) = rect.center();
        let dx = cx - fcx;
        let dy = cy - fcy;
        let in_direction = match nav {
            SplitNav::Left => dx < -1.0,
            SplitNav::Right => dx > 1.0,
            SplitNav::Up => dy < -1.0,
            SplitNav::Down => dy > 1.0,
        };
        if !in_direction {
            continue;
        }
        let dist = match nav {
            SplitNav::Left | SplitNav::Right => dx.abs() + dy.abs() * 4.0,
            SplitNav::Up | SplitNav::Down => dy.abs() + dx.abs() * 4.0,
        };
        if best.map_or(true, |(_, b)| dist < b) {
            best = Some((*id, dist));
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lr(ratio: f32, first: SplitTree<u32>, second: SplitTree<u32>) -> SplitTree<u32> {
        SplitTree::Split {
            direction: SplitDirection::LeftRight,
            ratio,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn tb(ratio: f32, first: SplitTree<u32>, second: SplitTree<u32>) -> SplitTree<u32> {
        SplitTree::Split {
            direction: SplitDirection::TopBottom,
            ratio,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    #[test]
    fn split_and_remove_roundtrip() {
        let mut tree = SplitTree::leaf(1u32);
        assert!(tree.split_leaf(1, 2, SplitDirection::LeftRight, 0.5));
        assert!(tree.split_leaf(2, 3, SplitDirection::TopBottom, 0.5));
        assert_eq!(tree.leaves(), vec![1, 2, 3]);
        assert_eq!(tree.first_leaf(), 1);

        // 摘掉 2：其父塌缩，焦点给幸存子树首叶 3。
        assert_eq!(tree.remove_leaf(2), RemoveOutcome::Collapsed(3));
        assert_eq!(tree.leaves(), vec![1, 3]);
        // 不存在的叶子。
        assert_eq!(tree.remove_leaf(99), RemoveOutcome::NotFound);
        // 摘到只剩一个 → WasRoot。
        assert_eq!(tree.remove_leaf(1), RemoveOutcome::Collapsed(3));
        assert_eq!(tree.remove_leaf(3), RemoveOutcome::WasRoot);
    }

    #[test]
    fn layout_splits_left_right_with_floor_and_divider() {
        let tree = lr(0.5, SplitTree::Leaf(1), SplitTree::Leaf(2));
        let out = tree.layout(Rect::new(100.0, 50.0, 803.0, 600.0), 10.0, 20.0, 3.0, false);

        // usable = 800，first = floor(400) = 400，second = 400。
        assert_eq!(out.panes.len(), 2);
        let (_, a) = out.panes[0];
        let (_, b) = out.panes[1];
        assert_eq!((a.x, a.w), (100.0, 400.0));
        assert_eq!((b.x, b.w), (503.0, 400.0));
        assert_eq!(a.h, 600.0);
        // 分隔条在两段之间。
        assert_eq!(out.dividers.len(), 1);
        let d = &out.dividers[0];
        assert_eq!((d.rect.x, d.rect.w), (500.0, 3.0));
        assert!(d.path.is_empty());
    }

    #[test]
    fn layout_clamps_each_side_to_a_cell() {
        // 极端比例：任何一侧都不小于一个单元格。
        let tree = lr(0.001, SplitTree::Leaf(1), SplitTree::Leaf(2));
        let out = tree.layout(Rect::new(0.0, 0.0, 103.0, 100.0), 10.0, 20.0, 3.0, false);
        let (_, a) = out.panes[0];
        assert_eq!(a.w, 10.0);

        let tree = lr(0.999, SplitTree::Leaf(1), SplitTree::Leaf(2));
        let out = tree.layout(Rect::new(0.0, 0.0, 103.0, 100.0), 10.0, 20.0, 3.0, false);
        let (_, b) = out.panes[1];
        assert_eq!(b.w, 10.0);
    }

    #[test]
    fn layout_nested_paths_address_dividers() {
        // [1 | [2 / 3]]
        let tree = lr(0.5, SplitTree::Leaf(1), tb(0.5, SplitTree::Leaf(2), SplitTree::Leaf(3)));
        let out = tree.layout(Rect::new(0.0, 0.0, 1003.0, 600.0), 10.0, 20.0, 3.0, false);
        assert_eq!(out.panes.len(), 3);
        assert_eq!(out.dividers.len(), 2);
        assert!(out.dividers[0].path.is_empty());
        // 嵌套 Split 是根的第二个孩子。
        assert_eq!(out.dividers[1].path, vec![true]);
        assert_eq!(out.dividers[1].direction, SplitDirection::TopBottom);
    }

    #[test]
    fn layout_preview_only_when_asked() {
        let mut tree = lr(0.5, SplitTree::Leaf(1), SplitTree::Leaf(2));
        if let SplitTree::Split { preview_ratio, .. } = &mut tree {
            *preview_ratio = Some(0.25);
        }
        let vp = Rect::new(0.0, 0.0, 803.0, 600.0);
        let committed = tree.layout(vp, 10.0, 20.0, 3.0, false);
        let preview = tree.layout(vp, 10.0, 20.0, 3.0, true);
        assert_eq!(committed.panes[0].1.w, 400.0);
        assert_eq!(preview.panes[0].1.w, 200.0);
    }

    #[test]
    fn divider_hit_uses_slop() {
        let tree = lr(0.5, SplitTree::Leaf(1), SplitTree::Leaf(2));
        let out = tree.layout(Rect::new(0.0, 0.0, 803.0, 600.0), 10.0, 20.0, 3.0, false);
        // 分隔条在 x=400..403。
        assert!(hit_divider(&out.dividers, 395.0, 300.0, 8.0).is_some());
        assert!(hit_divider(&out.dividers, 410.0, 300.0, 8.0).is_some());
        assert!(hit_divider(&out.dividers, 380.0, 300.0, 8.0).is_none());
    }

    #[test]
    fn drag_ratio_tracks_pointer_unclamped() {
        let vp = Rect::new(100.0, 0.0, 803.0, 600.0);
        // 指针在可用区中点：(501.5 - 100 - 1.5) / 800 = 0.5。
        let r = drag_ratio(SplitDirection::LeftRight, vp, 3.0, 10.0, 501.5, 0.0);
        assert!((r - 0.5).abs() < 1e-6);
        // 越过左缘 → 负值（不钳制）。
        assert!(drag_ratio(SplitDirection::LeftRight, vp, 3.0, 10.0, 50.0, 0.0) < 0.0);
    }

    #[test]
    fn preview_curve_pins_close_zone() {
        assert_eq!(preview_ratio(0.03), 0.02);
        assert_eq!(preview_ratio(0.97), 0.98);
        assert_eq!(preview_ratio(0.05), 0.02);
        // 常规带 clamp 到 0.10..0.90。
        assert_eq!(preview_ratio(0.07), 0.10);
        assert_eq!(preview_ratio(0.5), 0.5);
        assert_eq!(preview_ratio(0.93), 0.90);
    }

    #[test]
    fn drag_close_targets_squeezed_side() {
        assert_eq!(drag_close_target(0.01), Some(false));
        assert_eq!(drag_close_target(0.99), Some(true));
        assert_eq!(drag_close_target(0.5), None);
        assert_eq!(drag_close_target(CLOSE_MARGIN), None);
    }

    #[test]
    fn commit_snaps_to_whole_cells() {
        // usable = 800，cell = 10：0.437 → 349.6px → round 350 → 0.4375。
        let r = commit_ratio(0.437, 803.0, 3.0, 10.0);
        assert!((r - 0.4375).abs() < 1e-6);
        // 钳制带仍然生效。
        assert_eq!(commit_ratio(0.02, 803.0, 3.0, 10.0), 0.10);
        assert_eq!(commit_ratio(0.98, 803.0, 3.0, 10.0), 0.90);
    }

    #[test]
    fn nav_prefers_aligned_panes() {
        // 2x2 网格：1 2 / 3 4。
        let panes = vec![
            (1u32, Rect::new(0.0, 0.0, 100.0, 100.0)),
            (2, Rect::new(110.0, 0.0, 100.0, 100.0)),
            (3, Rect::new(0.0, 110.0, 100.0, 100.0)),
            (4, Rect::new(110.0, 110.0, 100.0, 100.0)),
        ];
        assert_eq!(nav_target(&panes, 1, SplitNav::Right), Some(2));
        assert_eq!(nav_target(&panes, 1, SplitNav::Down), Some(3));
        assert_eq!(nav_target(&panes, 4, SplitNav::Left), Some(3));
        assert_eq!(nav_target(&panes, 4, SplitNav::Up), Some(2));
        assert_eq!(nav_target(&panes, 1, SplitNav::Left), None);
        // 对角比同行远：从 3 往右应该到 4 而不是 2。
        assert_eq!(nav_target(&panes, 3, SplitNav::Right), Some(4));
    }

    #[test]
    fn node_mut_follows_paths() {
        let mut tree = lr(0.5, SplitTree::Leaf(1), tb(0.5, SplitTree::Leaf(2), SplitTree::Leaf(3)));
        assert!(matches!(tree.node_mut(&[]), Some(SplitTree::Split { .. })));
        assert!(matches!(tree.node_mut(&[false]), Some(SplitTree::Leaf(1))));
        assert!(matches!(tree.node_mut(&[true]), Some(SplitTree::Split { .. })));
        assert!(matches!(tree.node_mut(&[true, false]), Some(SplitTree::Leaf(2))));
        assert!(tree.node_mut(&[false, false]).is_none());
    }
}
