//! Split-pane behaviour for [`WindowContext`]: the recursive layout tree, its
//! geometry, splitting/closing panes, resizing and divider drag handling.
//!
//! A tab owns a [`Layout`](super::Layout) binary tree (a plain binary tree)
//! with panes at the leaves. `collect_layout` turns that tree into per-pane
//! `SizeInfo` rectangles plus the divider rectangles between them; the rest are
//! `impl WindowContext` methods the parent module calls back into (`pub(super)`).

use std::mem;

use winit::event::{ElementState, Event as WinitEvent, MouseButton, WindowEvent};
use winit::window::CursorIcon;

use nebula_terminal::event::{Notify, OnResize, WindowSize};
use nebula_terminal::event_loop::Msg;
use nebula_terminal::grid::Dimensions;
use nebula_terminal::term::test::TermSize;

use crate::display::{
    SizeInfo, SplitDirection, SplitNav, NEBULA_SPLIT_DIVIDER_GAP, NEBULA_SPLIT_HIT_SLOP,
};

use super::{Layout, Pane, PaneId, WindowContext};

/// A screen-space rectangle `(x, y, w, h)` with a top-left origin.
type ScreenRect = (f32, f32, f32, f32);

/// One divider's identity: its on-screen rect plus the split node that owns it
/// (addressed by tree path), so any divider — not just the root's — can be
/// hit-tested and dragged (hit-tested and draggable dividers).
#[derive(Clone)]
pub(super) struct DividerInfo {
    pub(super) rect: ScreenRect,
    pub(super) direction: SplitDirection,
    /// Path from the layout root: `false` = first child, `true` = second.
    pub(super) path: Vec<bool>,
    /// Content rect the owning split node lays its children into.
    pub(super) viewport: ScreenRect,
}

/// Live drag state for one divider, stored on the `WindowContext`.
#[derive(Clone)]
pub(super) struct SplitDragState {
    path: Vec<bool>,
    direction: SplitDirection,
    viewport: ScreenRect,
    /// Last *unclamped* pointer ratio along the drag axis. Dragging past the
    /// clamp range and releasing closes the squeezed pane (drag-to-close).
    raw_ratio: f32,
}

/// Releasing a divider drag with the raw pointer ratio beyond this margin
/// closes the pane being squeezed shut (`< margin` closes the first child,
/// `> 1 - margin` the second).
const NEBULA_SPLIT_CLOSE_MARGIN: f32 = 0.06;

/// Outcome of removing a leaf from a [`Layout`] tree.
enum RemoveResult {
    /// The pane wasn't in this tree.
    NotFound,
    /// The pane was the tree's only leaf; the caller should close the tab.
    WasRoot,
    /// The pane was removed and its parent collapsed; focus should move to the
    /// given surviving leaf.
    Collapsed(PaneId),
}

impl Layout {
    /// Collect every pane id at the leaves into `out` (depth-first).
    pub(super) fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Leaf(id) => out.push(*id),
            Layout::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            },
        }
    }

    /// The first (left/top-most) leaf in the tree.
    fn first_leaf(&self) -> PaneId {
        match self {
            Layout::Leaf(id) => *id,
            Layout::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Replace the `target` leaf with a `Split` of `[target, new]`. Returns
    /// `true` if `target` was found.
    fn split_leaf(
        &mut self,
        target: PaneId,
        new: PaneId,
        direction: SplitDirection,
        ratio: f32,
    ) -> bool {
        match self {
            Layout::Leaf(id) if *id == target => {
                let old = *id;
                *self = Layout::Split {
                    direction,
                    ratio,
                    preview_ratio: None,
                    dragging: false,
                    first: Box::new(Layout::Leaf(old)),
                    second: Box::new(Layout::Leaf(new)),
                };
                true
            },
            Layout::Leaf(_) => false,
            Layout::Split { first, second, .. } => {
                first.split_leaf(target, new, direction, ratio)
                    || second.split_leaf(target, new, direction, ratio)
            },
        }
    }

    /// Remove the `target` leaf, collapsing its parent so the sibling subtree
    /// takes over the freed space.
    fn remove_leaf(&mut self, target: PaneId) -> RemoveResult {
        if let Layout::Leaf(id) = self {
            return if *id == target { RemoveResult::WasRoot } else { RemoveResult::NotFound };
        }

        if let Layout::Split { first, second, .. } = self {
            // Direct child is the target → collapse to the sibling.
            if matches!(first.as_ref(), Layout::Leaf(id) if *id == target) {
                let survivor = mem::replace(second.as_mut(), Layout::Leaf(0));
                let focus = survivor.first_leaf();
                *self = survivor;
                return RemoveResult::Collapsed(focus);
            }
            if matches!(second.as_ref(), Layout::Leaf(id) if *id == target) {
                let survivor = mem::replace(first.as_mut(), Layout::Leaf(0));
                let focus = survivor.first_leaf();
                *self = survivor;
                return RemoveResult::Collapsed(focus);
            }
            // Recurse into subtrees.
            return match first.remove_leaf(target) {
                RemoveResult::NotFound => second.remove_leaf(target),
                other => other,
            };
        }

        RemoveResult::NotFound
    }
}

/// Recursively assign each leaf a viewport `SizeInfo` and collect the divider
/// rectangles between split children. `vx/vy/vw/vh` is the content rectangle
/// (screen pixels, top-left origin) this subtree fills. `path` addresses the
/// current node from the root (`false` = first child, `true` = second).
#[allow(clippy::too_many_arguments)]
fn collect_layout(
    layout: &Layout,
    cell_w: f32,
    cell_h: f32,
    divider: f32,
    use_preview: bool,
    vx: f32,
    vy: f32,
    vw: f32,
    vh: f32,
    path: &mut Vec<bool>,
    panes: &mut Vec<(PaneId, SizeInfo)>,
    dividers: &mut Vec<DividerInfo>,
) {
    match layout {
        Layout::Leaf(id) => {
            // The renderer draws a cell at `padding + i * cell` and uses
            // `extent - 2 * padding` as the content size, so encoding the
            // top-left offset as padding places this pane's viewport at
            // `[vx, vx + vw] x [vy, vy + vh]`.
            let view = SizeInfo::new(vw + 2.0 * vx, vh + 2.0 * vy, cell_w, cell_h, vx, vy, false);
            panes.push((*id, view));
        },
        Layout::Split { direction, ratio, preview_ratio, first, second, .. } => {
            let r = if use_preview { preview_ratio.unwrap_or(*ratio) } else { *ratio };
            match direction {
                SplitDirection::LeftRight => {
                    let usable = (vw - divider).max(cell_w);
                    let first_w = (usable * r).floor().max(cell_w).min((usable - cell_w).max(cell_w));
                    let second_w = (usable - first_w).max(cell_w);
                    dividers.push(DividerInfo {
                        rect: (vx + first_w, vy, divider, vh),
                        direction: *direction,
                        path: path.clone(),
                        viewport: (vx, vy, vw, vh),
                    });
                    path.push(false);
                    collect_layout(
                        first, cell_w, cell_h, divider, use_preview, vx, vy, first_w, vh, path,
                        panes, dividers,
                    );
                    path.pop();
                    path.push(true);
                    collect_layout(
                        second, cell_w, cell_h, divider, use_preview, vx + first_w + divider, vy,
                        second_w, vh, path, panes, dividers,
                    );
                    path.pop();
                },
                SplitDirection::TopBottom => {
                    let usable = (vh - divider).max(cell_h);
                    let first_h = (usable * r).floor().max(cell_h).min((usable - cell_h).max(cell_h));
                    let second_h = (usable - first_h).max(cell_h);
                    dividers.push(DividerInfo {
                        rect: (vx, vy + first_h, vw, divider),
                        direction: *direction,
                        path: path.clone(),
                        viewport: (vx, vy, vw, vh),
                    });
                    path.push(false);
                    collect_layout(
                        first, cell_w, cell_h, divider, use_preview, vx, vy, vw, first_h, path,
                        panes, dividers,
                    );
                    path.pop();
                    path.push(true);
                    collect_layout(
                        second, cell_w, cell_h, divider, use_preview, vx, vy + first_h + divider, vw,
                        second_h, path, panes, dividers,
                    );
                    path.pop();
                },
            }
        },
    }
}

impl WindowContext {
    /// The pane that currently owns keyboard focus (active tab's active pane).
    pub(super) fn focused_pane_id(&self) -> PaneId {
        self.tabs[self.active_tab].active_pane
    }

    /// The active tab's layout tree.
    pub(super) fn active_layout(&self) -> &Layout {
        &self.tabs[self.active_tab].layout
    }

    fn active_layout_mut(&mut self) -> &mut Layout {
        &mut self.tabs[self.active_tab].layout
    }

    /// Per-pane `SizeInfo`s and divider rectangles for the active tab. With
    /// `use_preview`, splits use their dragging preview ratio (for divider
    /// placement); otherwise their committed ratio (for pane content).
    pub(super) fn layout_geometry(
        &self,
        use_preview: bool,
    ) -> (Vec<(PaneId, SizeInfo)>, Vec<ScreenRect>) {
        let (panes, dividers) = self.layout_geometry_ex(use_preview);
        (panes, dividers.into_iter().map(|d| d.rect).collect())
    }

    /// Like [`layout_geometry`], but keeps each divider's owning-node identity
    /// so the drag handler can resize any split, not just the root.
    fn layout_geometry_ex(
        &self,
        use_preview: bool,
    ) -> (Vec<(PaneId, SizeInfo)>, Vec<DividerInfo>) {
        // A zoomed pane fills the whole window; splits and dividers are hidden.
        if let Some(zoomed) = self.zoom {
            return (vec![(zoomed, self.display.size_info)], Vec::new());
        }
        let size = self.display.size_info;
        let scale = self.display.window.scale_factor as f32;
        let divider = (NEBULA_SPLIT_DIVIDER_GAP * scale).round().max(1.0);
        let cell_w = size.cell_width();
        let cell_h = size.cell_height();
        let pad_x = size.padding_x();
        let pad_y = size.padding_y();
        // Asymmetric layout: the left padding carries the tab sidebar, the right
        // is the plain content margin. The pane content spans from pad_x to
        // (width - padding_right) — using 2*pad_x here would short the rightmost
        // pane by the sidebar width and leave a grey band down the right edge.
        let vw = (size.width() - pad_x - size.padding_right()).max(0.0);
        let vh = (size.height() - 2.0 * pad_y).max(0.0);

        let mut panes = Vec::new();
        let mut dividers = Vec::new();
        let mut path = Vec::new();
        collect_layout(
            self.active_layout(),
            cell_w,
            cell_h,
            divider,
            use_preview,
            pad_x,
            pad_y,
            vw,
            vh,
            &mut path,
            &mut panes,
            &mut dividers,
        );
        (panes, dividers)
    }

    /// The pane whose rectangle contains the screen point `(x, y)`, if any.
    pub(super) fn pane_at_position(&self, x: f32, y: f32) -> Option<PaneId> {
        let (panes, _) = self.layout_geometry(false);
        panes.into_iter().find_map(|(id, view)| {
            let x0 = view.padding_x();
            let y0 = view.padding_y();
            let x1 = view.width() - view.padding_x();
            let y1 = view.height() - view.padding_y();
            (x >= x0 && x < x1 && y >= y0 && y < y1).then_some(id)
        })
    }

    /// Move focus to the nearest pane in direction `nav` from the focused one.
    /// "Nearest" prefers panes aligned on the perpendicular axis, so e.g. moving
    /// right from a tall left pane lands on whichever right pane overlaps it most.
    pub(super) fn focus_split(&mut self, nav: SplitNav) {
        self.zoom = None;
        let focused = self.focused_pane_id();
        let (rects, _) = self.layout_geometry(false);

        // Centre of the currently focused pane.
        let Some((_, fview)) = rects.iter().find(|(id, _)| *id == focused) else { return };
        let center = |v: &SizeInfo| {
            let cx = v.padding_x() + (v.width() - 2.0 * v.padding_x()) * 0.5;
            let cy = v.padding_y() + (v.height() - 2.0 * v.padding_y()) * 0.5;
            (cx, cy)
        };
        let (fcx, fcy) = center(fview);

        let mut best: Option<(PaneId, f32)> = None;
        for (id, v) in &rects {
            if *id == focused {
                continue;
            }
            let (cx, cy) = center(v);
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
            // Primary distance along the travel axis; penalise perpendicular drift
            // so we stay in the same row/column when possible.
            let dist = match nav {
                SplitNav::Left | SplitNav::Right => dx.abs() + dy.abs() * 4.0,
                SplitNav::Up | SplitNav::Down => dy.abs() + dx.abs() * 4.0,
            };
            if best.map_or(true, |(_, b)| dist < b) {
                best = Some((*id, dist));
            }
        }

        if let Some((id, _)) = best {
            self.tabs[self.active_tab].active_pane = id;
            self.dirty = true;
            self.display.window.request_redraw();
        }
    }

    /// Split the focused pane along `direction`; the freshly spawned pane takes
    /// focus. Works at any depth, so repeated splits nest into a layout tree
    /// (panes close via the close-pane action).
    pub(super) fn split_focused(&mut self, direction: SplitDirection) {
        self.zoom = None;
        let focused = self.focused_pane_id();
        let cwd = self.focused_cwd();
        if let Some(new_id) = self.spawn_pane_detached(cwd, self.display.size_info) {
            self.active_layout_mut().split_leaf(focused, new_id, direction, 0.5);
            self.tabs[self.active_tab].active_pane = new_id;
            self.resize_active_layout();
            // Kick off the slide-in reveal over the new pane's final rect, so
            // it wipes in from the divider instead of popping into place.
            if let Some((_, view)) =
                self.layout_geometry(false).0.into_iter().find(|(id, _)| *id == new_id)
            {
                self.display.nebula_split_reveal = Some((
                    view.padding_x(),
                    view.padding_y(),
                    view.width() - 2.0 * view.padding_x(),
                    view.height() - 2.0 * view.padding_y(),
                    direction,
                    std::time::Instant::now(),
                ));
                self.display.window.request_redraw();
            }
            self.dirty = true;
        }
    }

    /// Toggle zoom of the focused pane (temporary full-window). No-op without a
    /// split, since a single pane already fills the window.
    pub(super) fn toggle_zoom(&mut self) {
        if self.zoom.is_some() {
            self.zoom = None;
        } else if matches!(self.active_layout(), Layout::Split { .. }) {
            self.zoom = Some(self.focused_pane_id());
        } else {
            return;
        }
        self.resize_active_layout();
        self.dirty = true;
        self.display.window.request_redraw();
    }

    /// Close the focused pane (collapsing its split or closing the tab).
    pub(super) fn close_focused_pane(&mut self) -> bool {
        let id = self.focused_pane_id();
        log::debug!("nebula: close_focused_pane pane={id}");
        self.close_pane(id)
    }

    /// Close one pane: remove it from its tab's layout (collapsing the parent),
    /// shut down its PTY, and move focus to the surviving sibling. If it was the
    /// tab's only pane, close the whole tab. Returns `true` if the window closes.
    pub(super) fn close_pane(&mut self, id: PaneId) -> bool {
        self.zoom = None;
        let tab_idx = self.tabs.iter().position(|t| {
            let mut ids = Vec::new();
            t.layout.leaves(&mut ids);
            ids.contains(&id)
        });
        let Some(tab_idx) = tab_idx else { return false };

        match self.tabs[tab_idx].layout.remove_leaf(id) {
            RemoveResult::WasRoot => self.close_tab(tab_idx),
            RemoveResult::Collapsed(focus) => {
                if let Some(i) = self.pane_index(id) {
                    let pane = self.panes.remove(i);
                    let _ = pane.notifier.0.send(Msg::Shutdown);
                }
                self.tabs[tab_idx].active_pane = focus;
                if tab_idx == self.active_tab {
                    self.resize_active_layout();
                }
                self.dirty = true;
                false
            },
            RemoveResult::NotFound => false,
        }
    }

    /// Resize every pane in the active tab to its current layout rectangle.
    ///
    /// Full variant: grids AND PTYs (+ welcome-intro reprint). Use for one-shot
    /// structural changes (split created/closed, tab spawned) where the child
    /// must learn its size immediately.
    pub(super) fn resize_active_layout(&mut self) {
        self.resize_active_layout_grids();
        self.resize_active_layout_ptys();
    }

    /// Resize only the grids to the current layout — the cheap, per-tick half
    /// used during interactive drags. PTY notification (which makes the in-box
    /// ConPTY repaint its whole viewport) waits for the settle timer.
    pub(super) fn resize_active_layout_grids(&mut self) {
        let (panes, _) = self.layout_geometry(false);
        for (id, view) in panes {
            if let Some(i) = self.pane_index(id) {
                Self::apply_view_grid(&mut self.panes[i], &view);
            }
        }
    }

    /// Push the current layout sizes to every pane's PTY and re-print the
    /// welcome intro where needed — the deferred half, run once per settled
    /// resize instead of once per drag tick.
    pub(super) fn resize_active_layout_ptys(&mut self) {
        let (panes, _) = self.layout_geometry(false);
        for (id, view) in panes {
            if let Some(i) = self.pane_index(id) {
                Self::apply_view_pty(
                    &mut self.panes[i],
                    &view,
                    self.display.nebula_fetch_enabled,
                    self.display.nebula_shell,
                );
            }
        }
    }

    /// Resize a single pane's terminal grid to match a `view`.
    fn apply_view_grid(pane: &mut Pane, view: &SizeInfo) {
        let term_size = TermSize::new(view.columns(), view.screen_lines());
        pane.terminal.lock().resize(term_size);
    }

    /// Notify a pane's PTY of its `view` size and re-print the welcome intro
    /// for pristine panes (ConPTY's repaint shreds the two-column art).
    fn apply_view_pty(
        pane: &mut Pane,
        view: &SizeInfo,
        fetch_enabled: bool,
        shell: crate::display::NebulaShell,
    ) {
        let window_size: WindowSize = (*view).into();
        crate::display::nebula_link_log(format!(
            "pty_resize cols={} lines={} view_px={}x{}",
            view.columns(),
            view.screen_lines(),
            view.width(),
            view.height(),
        ));
        pane.notifier.on_resize(window_size);
        if !fetch_enabled {
            pane.intro_cols = None;
            return;
        }
        // A pristine pane still shows only the welcome intro: re-print it at
        // the new width ("等比例" re-layout), otherwise ConPTY's reflow shreds
        // the two-column art. Once the user types, never touch their screen.
        if let Some(cols) = pane.intro_cols {
            if pane.nebula_state.touched {
                pane.intro_cols = None;
            } else if cols != view.columns() {
                pane.intro_cols = Some(view.columns());
                pane.notifier.notify(super::welcome::nebula_fastfetch_intro_command_for(
                    view.columns(),
                    shell,
                ));
            }
        }
    }

    /// The layout node addressed by `path` from the active tab's root.
    fn split_node_mut(&mut self, path: &[bool]) -> Option<&mut Layout> {
        let mut node = self.active_layout_mut();
        for &second in path {
            match node {
                Layout::Split { first, second: s, .. } => {
                    node = if second { s.as_mut() } else { first.as_mut() };
                },
                Layout::Leaf(_) => return None,
            }
        }
        Some(node)
    }

    /// The divider under `(x, y)`, if any. The visual line is thin, but the
    /// hit target is widened so it is easy to grab.
    fn divider_at(&self, x: f32, y: f32) -> Option<DividerInfo> {
        let (_, dividers) = self.layout_geometry_ex(false);
        let slop = (NEBULA_SPLIT_HIT_SLOP * self.display.window.scale_factor as f32).max(4.0);
        dividers.into_iter().find(|d| {
            let (dx, dy, dw, dh) = d.rect;
            x >= dx - slop && x <= dx + dw + slop && y >= dy - slop && y <= dy + dh + slop
        })
    }

    /// Mouse position → this drag's split ratio, *unclamped*. Deliberately not
    /// quantized to whole cells: the preview must track the pointer
    /// pixel-for-pixel to feel smooth; the ratio snaps to the grid
    /// only on commit. Values outside `0..1` mean the pointer is past the pane
    /// edge (drag-to-close territory).
    fn split_drag_ratio(&self, drag: &SplitDragState, x: f32, y: f32) -> f32 {
        let size = self.display.size_info;
        let scale = self.display.window.scale_factor as f32;
        let divider = (NEBULA_SPLIT_DIVIDER_GAP * scale).round().max(1.0);
        let (pos, origin, extent, cell) = match drag.direction {
            SplitDirection::LeftRight => {
                (x, drag.viewport.0, drag.viewport.2, size.cell_width().max(1.0))
            },
            SplitDirection::TopBottom => {
                (y, drag.viewport.1, drag.viewport.3, size.cell_height().max(1.0))
            },
        };
        let usable = (extent - divider).max(cell);
        (pos - origin - divider * 0.5) / usable
    }

    /// Map a raw drag ratio onto the preview: inside the normal band it tracks
    /// the pointer; in the close zone it pins hard against the edge so the pane
    /// visibly collapses, signalling "release to close".
    fn split_preview_ratio(raw: f32) -> f32 {
        if raw < NEBULA_SPLIT_CLOSE_MARGIN {
            0.02
        } else if raw > 1.0 - NEBULA_SPLIT_CLOSE_MARGIN {
            0.98
        } else {
            raw.clamp(0.10, 0.90)
        }
    }

    /// Start dragging the divider described by `info`.
    fn begin_split_drag(&mut self, info: &DividerInfo) {
        if let Some(Layout::Split { dragging, ratio, preview_ratio, .. }) =
            self.split_node_mut(&info.path)
        {
            *dragging = true;
            *preview_ratio = Some(*ratio);
            self.split_drag = Some(SplitDragState {
                path: info.path.clone(),
                direction: info.direction,
                viewport: info.viewport,
                raw_ratio: 0.5,
            });
        }
    }

    /// Update the dragged node's preview ratio; returns true if it changed.
    fn set_split_drag_preview(&mut self, value: f32) -> bool {
        let Some(path) = self.split_drag.as_ref().map(|d| d.path.clone()) else { return false };
        if let Some(Layout::Split { ratio, preview_ratio, .. }) = self.split_node_mut(&path) {
            let current = preview_ratio.unwrap_or(*ratio);
            if (current - value).abs() > f32::EPSILON {
                *preview_ratio = Some(value);
                return true;
            }
        }
        false
    }

    /// Commit the dragged preview into the real ratio (snapped to whole cells
    /// so pane sizes line up with the character grid) and stop dragging.
    fn commit_split_drag(&mut self) {
        let Some(drag) = self.split_drag.take() else { return };

        // Drag-to-close: released with the pointer past the edge margin. Only a
        // Leaf child is closed (a nested split would nuke several panes at
        // once); otherwise fall through to a normal clamped commit.
        let close_second = if drag.raw_ratio < NEBULA_SPLIT_CLOSE_MARGIN {
            Some(false)
        } else if drag.raw_ratio > 1.0 - NEBULA_SPLIT_CLOSE_MARGIN {
            Some(true)
        } else {
            None
        };
        if let Some(second) = close_second {
            let target = match self.split_node_mut(&drag.path) {
                Some(Layout::Split { first, second: s, .. }) => {
                    let child = if second { s.as_ref() } else { first.as_ref() };
                    match child {
                        Layout::Leaf(id) => Some(*id),
                        Layout::Split { .. } => None,
                    }
                },
                _ => None,
            };
            if let Some(id) = target {
                // The split node collapses into the survivor when the leaf is
                // removed, taking its dragging/preview state with it.
                self.close_pane(id);
                return;
            }
        }

        let size = self.display.size_info;
        let scale = self.display.window.scale_factor as f32;
        let divider = (NEBULA_SPLIT_DIVIDER_GAP * scale).round().max(1.0);
        let (extent, cell) = match drag.direction {
            SplitDirection::LeftRight => (drag.viewport.2, size.cell_width().max(1.0)),
            SplitDirection::TopBottom => (drag.viewport.3, size.cell_height().max(1.0)),
        };
        let usable = (extent - divider).max(cell);
        if let Some(Layout::Split { ratio, preview_ratio, dragging, .. }) =
            self.split_node_mut(&drag.path)
        {
            if let Some(p) = preview_ratio.take() {
                let quantized = (((p * usable) / cell).round() * cell / usable).clamp(0.10, 0.90);
                *ratio = quantized;
            }
            *dragging = false;
        }
    }

    fn set_mouse_position_from_window(&mut self, x: f64, y: f64) {
        let size = self.display.size_info;
        self.mouse.x = x.clamp(0.0, (size.width() - 1.0).max(0.0) as f64) as usize;
        self.mouse.y = y.clamp(0.0, (size.height() - 1.0).max(0.0) as f64) as usize;
    }

    /// Consume mouse events used for dragging a split divider before the normal
    /// terminal input processor sees them.
    pub(super) fn preprocess_split_mouse(&mut self) {
        if matches!(self.active_layout(), Layout::Leaf(_)) && self.split_drag.is_none() {
            return;
        }

        let mut redraw = false;
        let mut apply_resize = false;
        let mut filtered = Vec::with_capacity(self.event_queue.len());

        let cursor_for = |direction: SplitDirection| match direction {
            SplitDirection::TopBottom => CursorIcon::NsResize,
            SplitDirection::LeftRight => CursorIcon::EwResize,
        };

        for event in mem::take(&mut self.event_queue) {
            let mut consume = false;

            match &event {
                WinitEvent::WindowEvent {
                    event: WindowEvent::CursorMoved { position, .. },
                    ..
                } => {
                    let x = position.x as f32;
                    let y = position.y as f32;
                    if let Some(drag) = self.split_drag.clone() {
                        self.set_mouse_position_from_window(position.x, position.y);
                        let raw = self.split_drag_ratio(&drag, x, y);
                        if let Some(d) = self.split_drag.as_mut() {
                            d.raw_ratio = raw;
                        }
                        if self.set_split_drag_preview(Self::split_preview_ratio(raw)) {
                            redraw = true;
                        }
                        self.display.window.set_mouse_cursor(cursor_for(drag.direction));
                        consume = true;
                    } else if let Some(hit) = self.divider_at(x, y) {
                        self.set_mouse_position_from_window(position.x, position.y);
                        self.display.window.set_mouse_cursor(cursor_for(hit.direction));
                        consume = true;
                    }
                },
                WinitEvent::WindowEvent {
                    event:
                        WindowEvent::MouseInput {
                            state: ElementState::Pressed,
                            button: MouseButton::Left,
                            ..
                        },
                    ..
                } => {
                    if let Some(hit) = self.divider_at(self.mouse.x as f32, self.mouse.y as f32) {
                        self.mouse.left_button_state = ElementState::Pressed;
                        self.begin_split_drag(&hit);
                        self.display.window.set_mouse_cursor(cursor_for(hit.direction));
                        consume = true;
                    }
                },
                WinitEvent::WindowEvent {
                    event:
                        WindowEvent::MouseInput {
                            state: ElementState::Released,
                            button: MouseButton::Left,
                            ..
                        },
                    ..
                } if self.split_drag.is_some() => {
                    let direction = self.split_drag.as_ref().map(|d| d.direction);
                    self.mouse.left_button_state = ElementState::Released;
                    self.commit_split_drag();
                    // Only notify Term/PTY once at the end of the drag. Sending a
                    // resize on every mouse move causes full-screen TUIs (vim,
                    // claude, etc.) to repaint several incompatible layouts.
                    apply_resize = true;
                    if let Some(direction) = direction {
                        self.display.window.set_mouse_cursor(cursor_for(direction));
                    }
                    consume = true;
                },
                _ => {},
            }

            if !consume {
                filtered.push(event);
            }
        }

        self.event_queue = filtered;
        if apply_resize {
            self.resize_active_layout();
            redraw = true;
        }
        if redraw {
            self.dirty = true;
            self.display.window.request_redraw();
        }
    }
}
