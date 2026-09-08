//! Graft an existing pane tree beside one leaf, preserving unrelated splits.
use super::*;

impl<T: Copy + Eq> SplitTree<T> {
    pub fn joined(self, source: Self, side: SplitNav) -> Self {
        let (direction, before) = match side {
            SplitNav::Left => (SplitDirection::LeftRight, true),
            SplitNav::Right => (SplitDirection::LeftRight, false),
            SplitNav::Up => (SplitDirection::TopBottom, true),
            SplitNav::Down => (SplitDirection::TopBottom, false),
        };
        let (first, second) = if before { (source, self) } else { (self, source) };
        Self::Split {
            direction,
            ratio: 0.5,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Return the source unchanged if the destination disappeared during the gesture.
    pub fn dock_at_leaf(&mut self, target: T, source: Self, side: SplitNav) -> Result<(), Self> {
        match self {
            Self::Leaf(id) if *id == target => {
                let previous = std::mem::replace(self, Self::Leaf(target));
                *self = previous.joined(source, side);
                Ok(())
            },
            Self::Split { first, second, .. } => match first.dock_at_leaf(target, source, side) {
                Ok(()) => Ok(()),
                Err(source) => second.dock_at_leaf(target, source, side),
            },
            _ => Err(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifth_pane_only_splits_the_hovered_quadrant() {
        let mut tree = SplitTree::leaf(1).joined(SplitTree::leaf(2), SplitNav::Right);
        tree.split_leaf(1, 3, SplitDirection::TopBottom, 0.4);
        tree.split_leaf(2, 4, SplitDirection::TopBottom, 0.6);
        let viewport = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let before = tree.layout(viewport, 1.0, 1.0, DIVIDER_GAP, false).panes;
        tree.dock_at_leaf(4, SplitTree::leaf(5), SplitNav::Right).unwrap();
        let after = tree.layout(viewport, 1.0, 1.0, DIVIDER_GAP, false).panes;
        for id in [1, 2, 3] {
            assert_eq!(
                before.iter().find(|(pane, _)| *pane == id),
                after.iter().find(|(pane, _)| *pane == id)
            );
        }
        let target = before.iter().find(|(pane, _)| *pane == 4).unwrap().1;
        let dropped = after.iter().find(|(pane, _)| *pane == 5).unwrap().1;
        assert_eq!(dropped.h, target.h);
        assert_eq!(dropped.y, target.y);
        assert!(dropped.w < target.w && dropped.x > target.x);
        let original = tree.clone();
        assert!(tree.dock_at_leaf(99, SplitTree::leaf(6), SplitNav::Down).is_err());
        assert_eq!(tree.leaves(), original.leaves());
    }

    #[test]
    fn docking_preserves_a_multi_pane_source() {
        let source = SplitTree::leaf(7).joined(SplitTree::leaf(8), SplitNav::Down);
        let mut target = SplitTree::leaf(1).joined(SplitTree::leaf(2), SplitNav::Right);
        target.dock_at_leaf(2, source, SplitNav::Up).unwrap();
        assert_eq!(target.leaves(), [1, 7, 8, 2]);
    }
}
