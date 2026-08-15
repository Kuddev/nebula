//! GPUI 壳的会话恢复桥：共享 schema = `crate::session`（v4，与旧壳同一份
//! `session.json`、同一套版本升级/崩溃断路器语义），这里只做
//! `nebula_split::SplitTree`（运行时布局）与 `LayoutSession`（持久化树）
//! 之间的纯转换。文档/图片/设置 tab 不进会话（旧壳同合同）。

use crate::session::{AgentSession, LayoutSession, SplitAxis};
use nebula_split::{SplitDirection, SplitTree};

fn axis_of(direction: SplitDirection) -> SplitAxis {
    match direction {
        SplitDirection::LeftRight => SplitAxis::LeftRight,
        SplitDirection::TopBottom => SplitAxis::TopBottom,
    }
}

fn direction_of(axis: SplitAxis) -> SplitDirection {
    match axis {
        SplitAxis::LeftRight => SplitDirection::LeftRight,
        SplitAxis::TopBottom => SplitDirection::TopBottom,
    }
}

/// 运行时分屏树 → 持久化树。`leaf_data` 按 pane id 供叶数据（cwd + 快照
/// 瞬间的 AI 会话）；比例转 permille 整数（自动保存的变化检测与文件 diff
/// 不被 f32 序列化噪声绊倒，旧壳同因）。
pub fn layout_from_tree(
    tree: &SplitTree<u64>,
    leaf_data: &impl Fn(u64) -> (String, Option<AgentSession>),
) -> LayoutSession {
    match tree {
        SplitTree::Leaf(id) => {
            let (cwd, agent) = leaf_data(*id);
            LayoutSession::Pane { cwd, agent }
        },
        SplitTree::Split { direction, ratio, first, second, .. } => LayoutSession::Split {
            axis: axis_of(*direction),
            ratio_permille: (ratio.clamp(0.05, 0.95) * 1000.0).round() as u16,
            first: Box::new(layout_from_tree(first, leaf_data)),
            second: Box::new(layout_from_tree(second, leaf_data)),
        },
    }
}

/// 持久化树 → 运行时分屏树。叶 id 由 `alloc` 逐叶分配（DFS 序），返回的
/// id 列表与 [`LayoutSession::leaves`] 的 DFS 序一一对应——恢复注入
/// （cwd/agent → 第 i 个 pane）靠这个配对。
pub fn tree_from_layout(
    layout: &LayoutSession,
    alloc: &mut impl FnMut() -> u64,
) -> (SplitTree<u64>, Vec<u64>) {
    fn walk(
        node: &LayoutSession,
        alloc: &mut impl FnMut() -> u64,
        ids: &mut Vec<u64>,
    ) -> SplitTree<u64> {
        match node {
            LayoutSession::Pane { .. } => {
                let id = alloc();
                ids.push(id);
                SplitTree::Leaf(id)
            },
            LayoutSession::Split { axis, ratio_permille, first, second } => {
                let first = walk(first, alloc, ids);
                let second = walk(second, alloc, ids);
                SplitTree::Split {
                    direction: direction_of(*axis),
                    ratio: (f32::from(*ratio_permille) / 1000.0).clamp(0.05, 0.95),
                    preview_ratio: None,
                    dragging: false,
                    first: Box::new(first),
                    second: Box::new(second),
                }
            },
        }
    }
    let mut ids = Vec::new();
    let tree = walk(layout, alloc, &mut ids);
    (tree, ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> SplitTree<u64> {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, 2, SplitDirection::LeftRight, 0.618));
        assert!(tree.split_leaf(2, 3, SplitDirection::TopBottom, 0.5));
        tree
    }

    #[test]
    fn layout_round_trips_structure_ratio_and_leaf_order() {
        let tree = sample_tree();
        let layout = layout_from_tree(&tree, &|id| (format!("D:/pane-{id}"), None));
        assert_eq!(layout.pane_count(), 3);

        let mut next = 10u64;
        let (rebuilt, ids) = tree_from_layout(&layout, &mut || {
            next += 1;
            next
        });
        // DFS 叶序保持：源树 leaves() 与重建树 leaves()/返回 ids 一致配对。
        assert_eq!(ids, vec![11, 12, 13]);
        assert_eq!(rebuilt.leaves(), ids);
        let leaves = layout.leaves();
        assert!(matches!(
            leaves[0],
            LayoutSession::Pane { cwd, .. } if cwd == "D:/pane-1"
        ));
        assert!(matches!(
            leaves[1],
            LayoutSession::Pane { cwd, .. } if cwd == "D:/pane-2"
        ));

        // 比例经 permille 往返，误差 ≤ 0.001。
        let LayoutSession::Split { ratio_permille, .. } = &layout else {
            panic!("根应是 Split");
        };
        assert_eq!(*ratio_permille, 618);
        let SplitTree::Split { ratio, .. } = &rebuilt else { panic!("根应是 Split") };
        assert!((ratio - 0.618).abs() < 0.001);
    }

    #[test]
    fn agent_snapshot_rides_on_the_owning_leaf() {
        let tree = SplitTree::leaf(7u64);
        let layout = layout_from_tree(&tree, &|_| {
            (
                "D:/work".to_owned(),
                Some(AgentSession { source: "claude".into(), session_id: Some("abc-1".into()) }),
            )
        });
        assert!(matches!(
            layout,
            LayoutSession::Pane { ref agent, .. }
                if agent.as_ref().is_some_and(|a| a.source == "claude")
        ));
    }
}
