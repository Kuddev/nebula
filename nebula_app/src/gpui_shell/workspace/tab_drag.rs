//! 标签拖拽的单轴排序与终端 dock。
//!
//! 左侧栏和顶部栏共享同一套存储顺序与 dock 语义；差别只有列表轴和每格
//! 步距，因此拖拽状态捕获这两个量，而不是复制两套状态机。

use gpui::{Context, MouseButton, Pixels, Point, Window, px};
use nebula_split::{SplitNav, SplitTree};

use super::{
    NebulaWorkspace, TAB_DRAG_THRESHOLD, WorkspaceTab,
    windowing::{self, CrossWindowDropDestination, CrossWindowTabDrag},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabDragAxis {
    Vertical,
    Horizontal,
}

/// 受约束的单轴 tab 拖拽：存储顺序只在释放时提交；拖进活动终端区域则
/// 按最近边把整棵分屏树 dock 进去。
pub(super) struct TabDrag {
    pub(super) source: usize,
    /// 手势开始时冻结的 terminal pane 身份。跨窗提交不能在 mouse-up 时再按
    /// source 下标猜，因为拖拽期间其它 tab 可能被关闭或重排。
    pub(super) cross_window: Option<CrossWindowTabDrag>,
    pub(super) cross_window_target: Option<u64>,
    pub(super) press_x: f32,
    pub(super) press_y: f32,
    pub(super) axis: TabDragAxis,
    pub(super) pitch: f32,
    pub(super) offset: f32,
    pub(super) active: bool,
    pub(super) dock: Option<DockTarget>,
}

impl NebulaWorkspace {
    /// 根节点只负责把移出 tab hitbox 的待命拖拽继续交给状态机。
    /// GPUI 的元素监听离开 hover 后不再派发 move，因此这里不能只挂在 tab 行。
    pub(super) fn continue_pending_tab_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_drag.as_ref().is_some_and(|drag| !drag.active) {
            self.update_tab_drag(event, window, cx);
        }
    }

    /// overlay 可能来不及在激活和松手之间渲染；根节点必须兜底提交已激活
    /// 的 dock/reorder，而未过阈值时只清状态，保留 tab 的普通点击语义。
    pub(super) fn release_tab_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab_drag.as_ref().map(|drag| drag.active) {
            Some(true) => self.finish_tab_drag(window, cx),
            Some(false) => {
                if let Some(drag) = self.tab_drag.take() {
                    windowing::clear_cross_window_drag_target(drag.cross_window_target, cx);
                }
                cx.notify();
            },
            None => {},
        }
    }

    /// 释放事件走 workspace 根节点的 capture phase，确保终端子元素即使吞掉
    /// bubble 事件，dock 仍能按松手位置完成。最后位置必须再算一次：激活
    /// overlay 需要下一帧，快速拖放可能没有任何一次 move 落到 overlay 上。
    pub(super) fn release_tab_drag_at(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.tab_drag.is_none() {
            return false;
        }
        self.update_tab_drag_position(f32::from(position.x), f32::from(position.y), cx);
        let active = self.tab_drag.as_ref().is_some_and(|drag| drag.active);
        let destination = active.then(|| self.update_cross_window_drag_target(cx)).flatten();
        if let Some(destination) = destination {
            let drag = self.tab_drag.take().expect("active drag checked above");
            windowing::clear_cross_window_drag_target(drag.cross_window_target, cx);
            let payload = drag.cross_window.expect("only terminal tabs resolve a target window");
            // source workspace 仍在当前 mouse-up 回调里借用。延迟到事件结束后再
            // 进入目标窗口，避免 accept 回头 update 源 entity 时发生重入借用。
            cx.defer(move |cx| {
                windowing::drop_tab_to_existing_window(payload, destination, cx);
            });
            cx.notify();
            return true;
        }
        let viewport = window.viewport_size();
        let outside = position.x < px(0.0)
            || position.y < px(0.0)
            || position.x > viewport.width
            || position.y > viewport.height;
        if active && outside {
            if let Some(drag) = self.tab_drag.take() {
                windowing::clear_cross_window_drag_target(drag.cross_window_target, cx);
                if let Some(payload) = drag.cross_window {
                    cx.defer(move |cx| windowing::move_tab_to_new_window(payload, cx));
                } else {
                    self.schedule_move_tab_to_new_window(drag.source, cx);
                }
                cx.notify();
                return true;
            }
        }
        self.release_tab_drag(window, cx);
        active
    }

    /// 源下标加单轴位移换算出的整槽数；越过半格即换位。
    pub(super) fn drag_slot(drag: &TabDrag, len: usize) -> usize {
        let slots = (drag.offset / drag.pitch.max(1.0)).round() as isize;
        (drag.source as isize + slots).clamp(0, len.saturating_sub(1) as isize) as usize
    }

    pub(super) fn update_tab_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish_tab_drag(window, cx);
            return;
        }
        self.update_tab_drag_position(f32::from(event.position.x), f32::from(event.position.y), cx);
        let cross_window_target = self.update_cross_window_drag_target(cx).is_some();
        // 拖到 tab 视口边缘就自动滚（仅顶栏模式且真的溢出时生效）。放在位移
        // 换算之后：让位槽位仍按存储顺序算，滚动只改可视窗口。
        if !cross_window_target
            && self
                .tab_drag
                .as_ref()
                .is_some_and(|drag| drag.active && drag.axis == TabDragAxis::Horizontal)
        {
            self.autoscroll_top_tabs_for_drag(f32::from(event.position.x), window, cx);
        }
    }

    fn update_cross_window_drag_target(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<CrossWindowDropDestination> {
        let (payload, previous_target) =
            self.tab_drag.as_ref().filter(|drag| drag.active).and_then(|drag| {
                drag.cross_window.clone().map(|payload| (payload, drag.cross_window_target))
            })?;
        let destination = windowing::update_cross_window_drag_target(&payload, previous_target, cx);
        if let Some(drag) = self.tab_drag.as_mut() {
            drag.cross_window_target = destination.map(|target| target.window_id);
        }
        destination
    }

    fn update_tab_drag_position(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        // dock 必须在可变借用 drag 前计算；只有两个 Terminal tab 之间允许。
        let source = self.tab_drag.as_ref().map(|drag| drag.source);
        let dock =
            source.filter(|&source| self.dock_allowed(source)).and_then(|_| self.dock_nav_at(x, y));
        let drag = self.tab_drag.as_mut().expect("checked above");
        let dx = x - drag.press_x;
        let dy = y - drag.press_y;
        if !drag.active && (dy.abs() >= TAB_DRAG_THRESHOLD || dx.abs() >= TAB_DRAG_THRESHOLD) {
            drag.active = true;
        }
        if drag.active {
            let delta = match drag.axis {
                TabDragAxis::Vertical => dy,
                TabDragAxis::Horizontal => dx,
            };
            let before = -(drag.source as f32) * drag.pitch;
            let after = (len.saturating_sub(1) as f32 - drag.source as f32) * drag.pitch;
            drag.offset = delta.clamp(before, after.max(before));
            drag.dock = dock;
            cx.notify();
        }
    }

    pub(super) fn finish_tab_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.take() else { return };
        windowing::clear_cross_window_drag_target(drag.cross_window_target, cx);
        if drag.active {
            if let Some(nav) = drag.dock {
                self.dock_tab_into_active(drag.source, nav, window, cx);
            } else {
                let target = Self::drag_slot(&drag, self.tabs.len());
                self.move_tab(drag.source, target, window, cx);
            }
        }
        cx.notify();
    }

    fn dock_allowed(&self, source: usize) -> bool {
        source != self.active
            && self.tabs.get(source).is_some_and(WorkspaceTab::is_terminal)
            && self.tabs.get(self.active).is_some_and(WorkspaceTab::is_terminal)
    }

    pub(super) fn active_terminal_area(&self) -> Option<nebula_split::Rect> {
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed, .. }) =
            self.tabs.get(self.active)
        else {
            return None;
        };
        if !*zoomed && !tree.is_leaf() {
            // 旧壳始终用当前 Tab 的完整终端视口做 dock 命中。分屏以后直接
            // 取递归布局的根节点，不能再靠多个 Pane 的边界间接拼接；否则
            // 第一次合并后几何源发生变化，后续 Tab 就无法稳定继续并入。
            if let Some(b) = self.split_bounds.borrow().get(&(self.active, Vec::new())).copied() {
                let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
                if w > 0.0 && h > 0.0 {
                    return Some(nebula_split::Rect::new(
                        f32::from(b.origin.x),
                        f32::from(b.origin.y),
                        w,
                        h,
                    ));
                }
            }
        }
        let bounds = self.pane_bounds.borrow();
        if *zoomed || tree.is_leaf() {
            // 缩放态只认屏幕上实际可见的 Pane；其余 Pane 的上一帧边界可能
            // 仍在缓存里，不能把 dock 命中区扩到当前终端之外。
            let pane = panes.iter().find(|pane| pane.id == *focused).or_else(|| panes.first())?;
            let b = bounds.get(&pane.id)?;
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            return (w > 0.0 && h > 0.0).then(|| {
                nebula_split::Rect::new(f32::from(b.origin.x), f32::from(b.origin.y), w, h)
            });
        }
        let mut acc: Option<(f32, f32, f32, f32)> = None;
        for pane in panes {
            let Some(b) = bounds.get(&pane.id) else { continue };
            let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (x1, y1) = (x0 + f32::from(b.size.width), y0 + f32::from(b.size.height));
            acc = Some(match acc {
                Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
                None => (x0, y0, x1, y1),
            });
        }
        let (x0, y0, x1, y1) = acc?;
        (x1 > x0 && y1 > y0).then(|| nebula_split::Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    pub(crate) fn dock_nav_at(&self, x: f32, y: f32) -> Option<DockTarget> {
        let areas = self.dock_pane_areas();
        dock_target_at(self.active_terminal_area()?, &areas, x, y)
    }

    fn dock_pane_areas(&self) -> Vec<(u64, nebula_split::Rect)> {
        let Some(area) = self.active_terminal_area() else { return vec![] };
        let Some(WorkspaceTab::Terminal { tree, zoomed, .. }) = self.tabs.get(self.active) else {
            return vec![];
        };
        // A zoomed tree must be restored before adding a split, otherwise its post-drop
        // geometry cannot match the visible full-pane preview.
        if *zoomed && !tree.is_leaf() {
            return vec![];
        }
        tree.layout(area, 1.0, 1.0, nebula_split::DIVIDER_GAP, false).panes
    }

    pub(super) fn dock_preview_area(&self, target: DockTarget) -> Option<nebula_split::Rect> {
        let area = if let Some(pane) = target.pane {
            self.dock_pane_areas().into_iter().find(|(id, _)| *id == pane)?.1
        } else {
            self.active_terminal_area()?
        };
        SplitTree::leaf(0)
            .joined(SplitTree::leaf(1), target.nav)
            .layout(area, 1.0, 1.0, nebula_split::DIVIDER_GAP, false)
            .panes
            .into_iter()
            .find(|(pane, _)| *pane == 1)
            .map(|(_, area)| area)
    }

    fn dock_tab_into_active(
        &mut self,
        source: usize,
        target: DockTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dock_allowed(source) || !self.tabs.get(self.active).is_some_and(|tab| matches!(tab, WorkspaceTab::Terminal { tree, .. } if target.pane.is_none_or(|pane| tree.contains(pane)))) {
            return;
        }
        let Some((
            WorkspaceTab::Terminal {
                panes: src_panes, tree: src_tree, focused: src_focused, ..
            },
            _meta,
        )) = self.remove_tab_at(source)
        else {
            unreachable!("dock_allowed 已保证 source 是 Terminal");
        };
        if source < self.active {
            self.active -= 1;
        }
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed, broadcast, .. }) =
            self.tabs.get_mut(self.active)
        else {
            unreachable!("dock_allowed 已保证 active 是 Terminal");
        };
        if let Some(pane) = target.pane {
            tree.dock_at_leaf(pane, src_tree, target.nav)
                .expect("destination checked before detaching source");
        } else {
            let previous = std::mem::replace(tree, SplitTree::leaf(src_focused));
            *tree = previous.joined(src_tree, target.nav);
        }
        panes.extend(src_panes);
        *focused = src_focused;
        *zoomed = false;
        // 两个原本独立的 tab 合并后，旧广播范围已经失真；显式关闭，避免
        // 用户下一次击键意外扩散到刚并入的 pane。
        *broadcast = false;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn move_tab(
        &mut self,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let Some((tab, meta)) = self.remove_tab_at(from) else { return };
        self.insert_tab_at(to, tab, meta);
        self.active = if self.active == from {
            to
        } else {
            let mut ix = self.active;
            if ix > from {
                ix -= 1;
            }
            if ix >= to {
                ix += 1;
            }
            ix
        };
        self.focus_active(window, cx);
        cx.notify();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DockTarget {
    pub(crate) pane: Option<u64>,
    pub(crate) nav: SplitNav,
}

fn dock_target_at(
    area: nebula_split::Rect,
    panes: &[(u64, nebula_split::Rect)],
    x: f32,
    y: f32,
) -> Option<DockTarget> {
    if !area.contains(x, y) || panes.is_empty() {
        return None;
    }
    // The outer rim retains full-height/full-width insertion; inside it, preview
    // and insertion belong exclusively to the hovered leaf.
    let rim = 14.0_f32.min(area.w * 0.05).min(area.h * 0.05);
    if panes.len() > 1
        && (x - area.x < rim
            || area.x + area.w - x < rim
            || y - area.y < rim
            || area.y + area.h - y < rim)
    {
        return Some(DockTarget { pane: None, nav: nearest_edge(area, x, y) });
    }
    let (pane, area) = panes.iter().find(|(_, area)| area.contains(x, y))?;
    Some(DockTarget { pane: Some(*pane), nav: nearest_edge(*area, x, y) })
}

fn nearest_edge(area: nebula_split::Rect, x: f32, y: f32) -> SplitNav {
    let nx = (x - area.x) / area.w.max(1.0);
    let ny = (y - area.y) / area.h.max(1.0);
    [
        (nx, SplitNav::Left),
        (1.0 - nx, SplitNav::Right),
        (ny, SplitNav::Up),
        (1.0 - ny, SplitNav::Down),
    ]
    .into_iter()
    .min_by(|a, b| a.0.total_cmp(&b.0))
    .unwrap()
    .1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outer_rim_and_quadrant_drops_have_distinct_preview_targets() {
        let area = nebula_split::Rect::new(0.0, 0.0, 1200.0, 800.0);
        let panes = [
            (1, nebula_split::Rect::new(0.0, 0.0, 599.0, 399.0)),
            (2, nebula_split::Rect::new(601.0, 0.0, 599.0, 399.0)),
            (3, nebula_split::Rect::new(0.0, 401.0, 599.0, 399.0)),
            (4, nebula_split::Rect::new(601.0, 401.0, 599.0, 399.0)),
        ];
        assert_eq!(
            dock_target_at(area, &panes, 610.0, 600.0),
            Some(DockTarget { pane: Some(4), nav: SplitNav::Left })
        );
        assert_eq!(
            dock_target_at(area, &panes, 1195.0, 600.0),
            Some(DockTarget { pane: None, nav: SplitNav::Right })
        );
        assert_eq!(dock_target_at(area, &panes, 1250.0, 600.0), None);
    }

    #[test]
    fn direction_is_relative_to_the_hovered_pane() {
        let pane = nebula_split::Rect::new(600.0, 400.0, 600.0, 400.0);
        assert_eq!(nearest_edge(pane, 610.0, 600.0), SplitNav::Left);
        assert_eq!(nearest_edge(pane, 900.0, 410.0), SplitNav::Up);
        assert_eq!(nearest_edge(pane, 1190.0, 600.0), SplitNav::Right);
        assert_eq!(nearest_edge(pane, 900.0, 790.0), SplitNav::Down);
    }
}
