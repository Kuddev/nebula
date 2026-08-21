//! 标签拖拽的单轴排序与终端 dock。
//!
//! 左侧栏和顶部栏共享同一套存储顺序与 dock 语义；差别只有列表轴和每格
//! 步距，因此拖拽状态捕获这两个量，而不是复制两套状态机。

use gpui::{Context, MouseButton, Pixels, Point, Window};
use nebula_split::{SplitNav, SplitTree};

use super::{NebulaWorkspace, TAB_DRAG_THRESHOLD, WorkspaceTab, dock_tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabDragAxis {
    Vertical,
    Horizontal,
}

/// 受约束的单轴 tab 拖拽：存储顺序只在释放时提交；拖进活动终端区域则
/// 按最近边把整棵分屏树 dock 进去。
pub(super) struct TabDrag {
    pub(super) source: usize,
    pub(super) press_x: f32,
    pub(super) press_y: f32,
    pub(super) axis: TabDragAxis,
    pub(super) pitch: f32,
    pub(super) offset: f32,
    pub(super) active: bool,
    pub(super) dock: Option<SplitNav>,
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
                self.tab_drag = None;
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
        let Some(WorkspaceTab::Terminal { panes, .. }) = self.tabs.get(self.active) else {
            return None;
        };
        let bounds = self.pane_bounds.borrow();
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

    fn dock_nav_at(&self, x: f32, y: f32) -> Option<SplitNav> {
        let area = self.active_terminal_area()?;
        if !area.contains(x, y) {
            return None;
        }
        let nx = (x - area.x) / area.w;
        let ny = (y - area.y) / area.h;
        let (dl, dr, dt, db) = (nx, 1.0 - nx, ny, 1.0 - ny);
        let min = dl.min(dr).min(dt).min(db);
        Some(if min == dl {
            SplitNav::Left
        } else if min == dr {
            SplitNav::Right
        } else if min == dt {
            SplitNav::Up
        } else {
            SplitNav::Down
        })
    }

    fn dock_tab_into_active(
        &mut self,
        source: usize,
        nav: SplitNav,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dock_allowed(source) {
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
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed }) =
            self.tabs.get_mut(self.active)
        else {
            unreachable!("dock_allowed 已保证 active 是 Terminal");
        };
        let old = std::mem::replace(tree, SplitTree::leaf(src_focused));
        *tree = dock_tree(old, src_tree, nav);
        panes.extend(src_panes);
        *focused = src_focused;
        *zoomed = false;
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
