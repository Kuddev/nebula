//! 侧栏 TABS 列表的整行滚动（旧壳 `chrome_tab_layout` + `nebula_tabs_scroll`）。
//!
//! 标签多到超出侧栏剩余高度时，只画一个连续窗口，右侧贴覆盖式 thumb
//! （`overlay_scrollbar`）：不占行宽、不压状态徽章。滚轮按整行步进；thumb
//! 仅在列表悬停或拖拽时绘制。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Point, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};

use crate::display::ui::widgets::{self, OverlayScrollbar};
use crate::gpui_shell::prelude::*;

use super::{NebulaWorkspace, TAB_ROW_H, TAB_ROW_PITCH};

/// 行间距（pitch − 行高）。旧壳 `gap = s(8)`。
const TAB_ROW_GAP: f32 = TAB_ROW_PITCH - TAB_ROW_H;
/// 旧壳 `tab_right_pad`：给覆盖式滚动条留的恒定沟槽，出现与否都不改行宽。
pub(super) const TAB_SCROLL_GUTTER: f32 = 9.0;

/// n 行占的高度：间距只在行之间，末行后面不留。
pub(super) fn rows_h(n: usize, pitch: f32, gap: f32) -> f32 {
    if n == 0 { 0.0 } else { n as f32 * pitch - gap }
}

/// 视口里能放下几行。尚未量到高度时先展示全部，避免首帧空列表。
///
/// 与旧壳 `chrome_tab_layout` 同一条规则：不够一行就是 0，**绝不**退成
/// 一行。折叠动画若把裁剪高度写回来，强制 1 行会把整列锁成一行滚动区。
pub(super) fn visible_count(want: usize, avail_h: f32, pitch: f32, gap: f32) -> usize {
    if want == 0 {
        return 0;
    }
    if avail_h <= 1.0 {
        return want;
    }
    let mut show = want;
    while show > 0 && rows_h(show, pitch, gap) > avail_h + 0.5 {
        show -= 1;
    }
    show
}

pub(super) fn max_scroll(want: usize, show: usize) -> usize {
    want.saturating_sub(show)
}

pub(super) fn clamp_scroll(scroll: usize, max: usize) -> usize {
    scroll.min(max)
}

pub(super) fn index_visible(ix: usize, scroll: usize, show: usize) -> bool {
    show > 0 && ix >= scroll && ix < scroll + show
}

/// 激活项滚进窗口：在窗口上方则对齐窗口顶，在下方则对齐窗口底。
pub(super) fn reveal_index(ix: usize, scroll: usize, show: usize) -> usize {
    if show == 0 {
        return 0;
    }
    if ix < scroll {
        ix
    } else if ix >= scroll + show {
        ix + 1 - show
    } else {
        scroll
    }
}

/// 旧壳 `sidebar_wheel`：一次手势一行，符号与 winit 的 `-signum` 一致。
pub(super) fn wheel_rows(delta_y: f32) -> i32 {
    if delta_y > 0.0 {
        -1
    } else if delta_y < 0.0 {
        1
    } else {
        0
    }
}

pub(super) fn apply_wheel(scroll: usize, max: usize, rows: i32) -> usize {
    (scroll as i32 + rows).clamp(0, max as i32) as usize
}

fn point_in_rect((rx, ry, rw, rh): (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rx && y >= ry && x < rx + rw && y < ry + rh
}

struct TabsWindow {
    scroll: usize,
    show: usize,
    max: usize,
    want: usize,
}

impl NebulaWorkspace {
    fn tabs_window(&self) -> TabsWindow {
        let want = self.tabs.len();
        let show = visible_count(want, self.tabs_viewport_h, TAB_ROW_PITCH, TAB_ROW_GAP);
        let max = max_scroll(want, show);
        let scroll = clamp_scroll(self.tabs_scroll, max);
        TabsWindow { scroll, show, max, want }
    }

    pub(super) fn tabs_visible_window(&self) -> (usize, usize) {
        let window = self.tabs_window();
        (window.scroll, window.show)
    }

    pub(super) fn clamp_tabs_scroll(&mut self) {
        let max = self.tabs_window().max;
        let next = clamp_scroll(self.tabs_scroll, max);
        if next != self.tabs_scroll {
            self.tabs_scroll = next;
        }
    }

    pub(super) fn reveal_active_tab(&mut self) {
        let window = self.tabs_window();
        let next = reveal_index(self.active, window.scroll, window.show);
        self.tabs_scroll = clamp_scroll(next, window.max);
        self.top_tabs_scroll.scroll_to_item(if self.settings_open {
            self.tabs.len()
        } else {
            self.active
        });
    }

    fn tabs_overlay_bar(&self) -> Option<OverlayScrollbar> {
        if self.tabs_section_collapsed {
            return None;
        }
        let window = self.tabs_window();
        if window.show == 0 || window.want <= window.show {
            return None;
        }
        let viewport = rows_h(window.show, TAB_ROW_PITCH, TAB_ROW_GAP).max(1.0);
        let content = rows_h(window.want, TAB_ROW_PITCH, TAB_ROW_GAP);
        let width = self.tabs_list_width.max(1.0);
        let height = self.tabs_viewport_h.max(viewport);
        widgets::overlay_scrollbar(
            (0.0, 0.0, width, height),
            viewport,
            content,
            window.scroll as f32 * TAB_ROW_PITCH,
            1.0,
        )
    }

    fn local_tabs_point(&self, position: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(position.x - self.tabs_list_origin.x),
            f32::from(position.y - self.tabs_list_origin.y),
        )
    }

    fn press_tabs_scrollbar(&mut self, event: &MouseDownEvent) -> bool {
        let Some(bar) = self.tabs_overlay_bar() else { return false };
        let (x, y) = self.local_tabs_point(event.position);
        if !bar.hit_test(x, y) {
            return false;
        }
        let grab = if point_in_rect(bar.thumb, x, y) { y - bar.thumb.1 } else { bar.thumb.3 * 0.5 };
        self.tabs_scroll_grab = Some(grab);
        self.tabs_scroll = bar.target_offset(y, grab, self.tabs_window().max);
        true
    }

    fn drag_tabs_scrollbar_to(&mut self, position: Point<Pixels>) -> bool {
        let Some(grab) = self.tabs_scroll_grab else { return false };
        let Some(bar) = self.tabs_overlay_bar() else { return false };
        let (_, y) = self.local_tabs_point(position);
        let next = bar.target_offset(y, grab, self.tabs_window().max);
        if next == self.tabs_scroll {
            return false;
        }
        self.tabs_scroll = next;
        true
    }

    fn end_tabs_scrollbar_drag(&mut self) -> bool {
        self.tabs_scroll_grab.take().is_some()
    }

    fn on_tabs_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_y = f32::from(event.delta.pixel_delta(px(TAB_ROW_PITCH)).y);
        let rows = wheel_rows(delta_y);
        if rows == 0 {
            return;
        }
        let max = self.tabs_window().max;
        let next = apply_wheel(self.tabs_scroll, max, rows);
        if next != self.tabs_scroll {
            self.tabs_scroll = next;
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// 列表槽：恒定右沟槽 + 覆盖 thumb + 滚轮。`items` 已是可见窗口。
    pub(super) fn wrap_tabs_scroll_list(
        &self,
        items: impl IntoIterator<Item = gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let bar = self.tabs_overlay_bar();
        let dragging = self.tabs_scroll_grab.is_some();
        let show_thumb = bar.is_some() && (self.tabs_list_hot || dragging);
        let workspace = cx.entity().downgrade();
        let thumb_color = {
            let theme = cx.theme();
            let alpha = if dragging {
                0.78
            } else if self.tabs_list_hot {
                0.64
            } else {
                0.46
            };
            theme.scrollbar_thumb.opacity(alpha)
        };

        div()
            .id("sidebar-tabs-list")
            .flex_1()
            .w_full()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .pr(px(TAB_SCROLL_GUTTER))
            .on_scroll_wheel(cx.listener(Self::on_tabs_wheel))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.tabs_list_hot != *hovered {
                    this.tabs_list_hot = *hovered;
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = workspace.update(cx, |this, cx| {
                            this.remember_tabs_list_bounds(bounds, cx);
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(v_flex().w_full().gap_2().children(items))
            .when_some(bar.filter(|_| show_thumb), |list, bar| {
                list.child(
                    div()
                        .absolute()
                        .left(px(bar.thumb.0))
                        .top(px(bar.thumb.1))
                        .w(px(bar.thumb.2))
                        .h(px(bar.thumb.3))
                        .rounded_full()
                        .bg(thumb_color)
                        .occlude(),
                )
            })
            .when_some(bar, |list, bar| {
                list.child(
                    div()
                        .id("sidebar-tabs-scrollbar")
                        .absolute()
                        .left(px(bar.hit.0))
                        .top(px(bar.hit.1))
                        .w(px(bar.hit.2))
                        .h(px(bar.hit.3))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                if this.press_tabs_scrollbar(event) {
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    fn remember_tabs_list_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let h = f32::from(bounds.size.height);
        let w = f32::from(bounds.size.width);
        let origin = bounds.origin;
        let height_changed = (self.tabs_viewport_h - h).abs() > 0.5;
        let width_changed = (self.tabs_list_width - w).abs() > 0.5;
        self.tabs_list_origin = origin;
        if !height_changed && !width_changed {
            return;
        }
        // 折叠中/已折叠：裁剪高度不是 `tabs_avail`。写回去会让
        // `visible_count` 按 20–40px 算出 0/1 行，展开后整列锁死。
        if !self.tabs_section_collapsed && !self.tabs_fold_frozen && h >= TAB_ROW_H {
            self.tabs_viewport_h = h;
        }
        self.tabs_list_width = w;
        self.clamp_tabs_scroll();
        cx.notify();
    }

    pub(super) fn tabs_scrollbar_drag_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.tabs_scroll_grab.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                    if this.drag_tabs_scrollbar_to(event.position) {
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.end_tabs_scrollbar_drag() {
                            cx.notify();
                        }
                    }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_wheel, clamp_scroll, index_visible, max_scroll, reveal_index, rows_h, visible_count,
        wheel_rows,
    };

    const PITCH: f32 = 42.0;
    const GAP: f32 = 8.0;

    #[test]
    fn row_height_drops_the_trailing_gap() {
        assert_eq!(rows_h(0, PITCH, GAP), 0.0);
        assert_eq!(rows_h(1, PITCH, GAP), 34.0);
        assert_eq!(rows_h(3, PITCH, GAP), 3.0 * PITCH - GAP);
    }

    #[test]
    fn visible_count_fits_whole_rows_into_the_band() {
        // 100px 放下 2 行（76px），放不下 3 行（118px）。
        assert_eq!(visible_count(10, 100.0, PITCH, GAP), 2);
        assert_eq!(visible_count(2, 100.0, PITCH, GAP), 2);
        assert_eq!(visible_count(10, 0.0, PITCH, GAP), 10);
        assert_eq!(visible_count(0, 100.0, PITCH, GAP), 0);
        // 旧壳：不够一行就是 0，不能退成「一行滚动区」。
        assert_eq!(visible_count(10, 20.0, PITCH, GAP), 0);
    }

    #[test]
    fn scrollbar_only_exists_when_the_window_overflows() {
        assert_eq!(max_scroll(10, 10), 0);
        assert_eq!(max_scroll(10, 3), 7);
        assert_eq!(clamp_scroll(9, 7), 7);
        assert!(!index_visible(0, 2, 3));
        assert!(index_visible(2, 2, 3));
        assert!(index_visible(4, 2, 3));
        assert!(!index_visible(5, 2, 3));
    }

    #[test]
    fn activating_a_scrolled_out_tab_moves_the_window() {
        assert_eq!(reveal_index(0, 2, 3), 0);
        assert_eq!(reveal_index(3, 2, 3), 2);
        assert_eq!(reveal_index(9, 0, 3), 7);
    }

    #[test]
    fn wheel_steps_one_row_and_clamps() {
        assert_eq!(wheel_rows(12.0), -1);
        assert_eq!(wheel_rows(-3.0), 1);
        assert_eq!(wheel_rows(0.0), 0);
        assert_eq!(apply_wheel(0, 5, -1), 0);
        assert_eq!(apply_wheel(5, 5, 1), 5);
        assert_eq!(apply_wheel(2, 5, 1), 3);
    }
}
