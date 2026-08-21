//! 48px 自定义标题栏里的横向标签布局。
//!
//! 这里只负责同一组 workspace tab 的第二种呈现；激活、关闭、重命名、排序
//! 与 dock 都调用 `NebulaWorkspace` 既有动作，不维护平行状态。

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, App, Context, FontWeight, InteractiveElement as _,
    IntoElement as _, KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, ParentElement as _,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Window, div, ease_out_quint, img, px,
};
use gpui_component::menu::PopupMenuItem;

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::terminal::view::SidebarActivity;

use super::{
    NebulaWorkspace, OpenSettings, TAB_LABEL_ICON_SIZE, TAB_LABEL_ICON_W, TabDrag, TabDragAxis,
    TabPresentation, ToggleShellPicker,
};

pub(super) const TOP_TAB_H: f32 = 34.0;
/// 与顶部模式正文卡片的 `px_2` 左边距同源，让首个 tab 和终端左缘对齐。
pub(super) const TOP_TAB_LEFT_INSET: f32 = 8.0;
const TOP_TAB_MIN_W: f32 = 120.0;
const TOP_TAB_MAX_W: f32 = 220.0;
const TOP_TAB_GAP: f32 = 4.0;
const TOP_TAB_STATUS_W: f32 = 28.0;
/// 标题栏中不属于 tab 视口的固定预算：左内边距、四枚 32px 操作按钮、
/// 三枚 34px 窗口按钮，以及至少 72px 的可拖拽空白。
const TOP_TAB_RESERVED_W: f32 = TOP_TAB_LEFT_INSET + 32.0 * 4.0 + 34.0 * 3.0 + 72.0;

fn tab_width(viewport_w: f32, count: usize) -> f32 {
    if count == 0 || viewport_w <= 1.0 {
        return TOP_TAB_MAX_W;
    }
    (viewport_w / count as f32).clamp(TOP_TAB_MIN_W, TOP_TAB_MAX_W)
}

fn tab_strip_width(tab_w: f32, count: usize) -> f32 {
    tab_w * count as f32 + TOP_TAB_GAP * count.saturating_sub(1) as f32
}

/// 普通鼠标滚轮没有 X 分量时，把主滚动轴映射到横向；触控板原生横向
/// 分量更强时则保留它。
fn horizontal_wheel_delta(x: f32, y: f32) -> f32 {
    if x.abs() > y.abs() { x } else { y }
}

impl NebulaWorkspace {
    pub(super) fn render_top_title_bar(
        &self,
        files_active: bool,
        git_active: bool,
        settings_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = theme.list_hover;
        let dark = theme.is_dark();
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let mono_family: SharedString = settings
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Cascadia Mono"))
            .into();
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        let tab_capacity_w =
            (f32::from(window.viewport_size().width) - TOP_TAB_RESERVED_W).max(TOP_TAB_MIN_W);
        let tab_w = tab_width(tab_capacity_w, self.tabs.len());
        let tab_viewport_w = tab_strip_width(tab_w, self.tabs.len()).min(tab_capacity_w);
        let pitch = tab_w + TOP_TAB_GAP;
        let drag = self
            .tab_drag
            .as_ref()
            .filter(|drag| drag.active && drag.axis == TabDragAxis::Horizontal)
            .map(|drag| (drag.source, Self::drag_slot(drag, self.tabs.len()), drag.offset));
        let items_running = std::cell::Cell::new(false);

        let items = (0..self.tabs.len())
            .map(|ix| {
                let active = ix == self.active;
                let TabPresentation {
                    title,
                    is_settings,
                    activity,
                    logo_image,
                    program_glyph,
                    shell_tag,
                    color,
                    renaming,
                } = self.tab_presentation(ix, cx, dark);
                let hover_group: SharedString = format!("top-tab-hover-{ix}").into();
                let status_color = if active { active_fg } else { muted };
                let resting_status: Option<gpui::AnyElement> = match activity {
                    SidebarActivity::Running => {
                        items_running.set(true);
                        let (track, head) =
                            crate::gpui_shell::theme::sidebar_spinner_colors(cx, active);
                        Some(Self::spinner(self.spinner_phase, track, head).into_any_element())
                    },
                    SidebarActivity::Done => Some(
                        div().size(px(6.0)).rounded_full().bg(theme.primary).into_any_element(),
                    ),
                    SidebarActivity::Attention => Some(
                        Icon::new(IconName::TriangleAlert)
                            .xsmall()
                            .text_color(theme.warning)
                            .into_any_element(),
                    ),
                    SidebarActivity::Failed => Some(
                        Icon::new(IconName::CircleX)
                            .xsmall()
                            .text_color(theme.danger)
                            .into_any_element(),
                    ),
                    SidebarActivity::Idle => shell_tag.map(|tag| {
                        div()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px * 0.8))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(status_color)
                            .child(tag)
                            .into_any_element()
                    }),
                };
                let strip = color.map(|color| gpui::Rgba {
                    r: color.r as f32 / 255.0,
                    g: color.g as f32 / 255.0,
                    b: color.b as f32 / 255.0,
                    a: 1.0,
                });
                let (dragged, shift) = match drag {
                    Some((source, _, _)) if ix == source => (true, 0.0),
                    Some((source, target, _)) if source < target && ix > source && ix <= target => {
                        (false, -pitch)
                    },
                    Some((source, target, _)) if source > target && ix >= target && ix < source => {
                        (false, pitch)
                    },
                    _ => (false, 0.0),
                };

                let row = h_flex()
                    .id(("top-tab", ix))
                    .group(hover_group.clone())
                    .relative()
                    .w(px(tab_w))
                    .h(px(TOP_TAB_H))
                    .flex_shrink_0()
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap_2()
                    .px_2()
                    // 顶部 tab 的底边直接接正文，只保留上侧圆角；四角全圆
                    // 会把它重新画成悬浮在标题栏里的药丸。
                    .rounded_tl(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
                    .rounded_tr(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
                    .font_weight(FontWeight::NORMAL)
                    .cursor_pointer()
                    // TitleBar 的父层是 WindowControlArea::Drag；tab 必须自己
                    // 占住命中区，否则拖动标签会变成拖动窗口。
                    .occlude()
                    .when(active, |item| item.bg(active_bg).text_color(active_fg))
                    .when(!active && !dragged, |item| {
                        item.text_color(muted).hover(|style| style.bg(hover_bg))
                    })
                    .when(!active && dragged, |item| item.text_color(muted).bg(hover_bg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate_tab(ix, window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.tab_drag = Some(TabDrag {
                                source: ix,
                                press_x: f32::from(event.position.x),
                                press_y: f32::from(event.position.y),
                                axis: TabDragAxis::Horizontal,
                                pitch,
                                offset: 0.0,
                                active: false,
                                dock: None,
                            });
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.request_close_tab(ix, window, cx);
                        }),
                    )
                    .on_double_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.begin_rename(ix, window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_tab_context_menu(ix, event.position, window, cx);
                        }),
                    )
                    .when_some(strip, |row, color| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(6.0))
                                .right(px(6.0))
                                .bottom_0()
                                .h(px(2.5))
                                .rounded_full()
                                .bg(color),
                        )
                    })
                    .when(is_settings, |row| {
                        row.child(
                            div()
                                .w(px(TAB_LABEL_ICON_W))
                                .flex_shrink_0()
                                .flex()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::Settings)
                                        .small()
                                        .text_color(if active { active_fg } else { muted }),
                                ),
                        )
                    })
                    .when_some(logo_image, |row, image| {
                        row.child(
                            img(image)
                                .size(px(TAB_LABEL_ICON_SIZE))
                                .flex_shrink_0()
                                .object_fit(ObjectFit::Contain),
                        )
                    })
                    .when_some(program_glyph, |row, glyph| {
                        row.child(
                            div()
                                .w(px(TAB_LABEL_ICON_W))
                                .flex_shrink_0()
                                .font_family(mono_family.clone())
                                .text_size(px(label_px))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(if active { active_fg } else { muted })
                                .child(glyph),
                        )
                    })
                    .child(match renaming {
                        Some(input) => div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_key_down(cx.listener(
                                |this, event: &KeyDownEvent, window, cx| {
                                    if event.keystroke.key == "escape" {
                                        cx.stop_propagation();
                                        this.cancel_rename(window, cx);
                                    }
                                },
                            ))
                            .child(
                                Input::new(&input)
                                    .w_full()
                                    .text_size(px(label_px))
                                    .font_family(mono_family.clone()),
                            )
                            .into_any_element(),
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px))
                            .font_weight(FontWeight::LIGHT)
                            .child(title)
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .relative()
                            .w(px(TOP_TAB_STATUS_W))
                            .h_full()
                            .flex_shrink_0()
                            .when_some(resting_status, |slot, status| {
                                slot.child(
                                    h_flex()
                                        .absolute()
                                        .inset_0()
                                        .justify_end()
                                        .items_center()
                                        .group_hover(hover_group.clone(), |item| item.invisible())
                                        .child(status),
                                )
                            })
                            .child(
                                h_flex()
                                    .absolute()
                                    .inset_0()
                                    .justify_end()
                                    .items_center()
                                    .invisible()
                                    .group_hover(hover_group, |slot| slot.visible())
                                    .child(
                                        Button::new(("top-close-tab", ix))
                                            .icon(IconName::Close)
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(
                                                move |this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.request_close_tab(ix, window, cx);
                                                },
                                            )),
                                    ),
                            ),
                    );

                if dragged {
                    gpui::deferred(
                        row.left(px(drag.map(|(_, _, offset)| offset).unwrap_or(0.0))).shadow_md(),
                    )
                    .into_any_element()
                } else if shift != 0.0 {
                    if nebula_settings::RuntimeSettings::load().tab_reveal
                        == nebula_settings::TabRevealName::Instant
                    {
                        row.left(px(shift)).into_any_element()
                    } else {
                        row.with_animation(
                            ("top-tab-make-way", ix),
                            Animation::new(Duration::from_millis(120))
                                .with_easing(ease_out_quint()),
                            move |row, t| row.left(px(shift * t)),
                        )
                        .into_any_element()
                    }
                } else {
                    row.into_any_element()
                }
            })
            .collect::<Vec<_>>();

        if items_running.get() {
            cx.on_next_frame(window, |this, _, cx| {
                let now = std::time::Instant::now();
                let dt = now - this.spinner_last;
                this.spinner_last = now;
                this.spinner_phase = (this.spinner_phase + dt.as_secs_f32() / 0.8).rem_euclid(1.0);
                cx.notify();
            });
        }

        let menu_workspace = cx.entity().downgrade();

        h_flex()
            .size_full()
            .min_w_0()
            .items_center()
            .child(
                // Windows Terminal 的 TabView 贴住标题栏底边；只让 tab 与其
                // 相邻操作按钮下沉，标题栏两侧的独立工具仍保持垂直居中。
                h_flex()
                    .h_full()
                    .flex_shrink()
                    .min_w_0()
                    .items_end()
                    .child(
                        div()
                            .id("top-tabs-viewport")
                            .relative()
                            .w(px(tab_viewport_w))
                            .flex_shrink()
                            .min_w_0()
                            .h(px(TOP_TAB_H))
                            .overflow_hidden()
                            // 待命拖拽在越过 4px 前由此接收移动；激活后根罩层接管。
                            .on_mouse_move(cx.listener(|this, event, window, cx| {
                                this.update_tab_drag(event, window, cx);
                            }))
                            .on_scroll_wheel(cx.listener(Self::on_top_tabs_wheel))
                            .child(
                                h_flex()
                                    .id("top-tabs-scroll")
                                    .size_full()
                                    .items_center()
                                    .gap(px(TOP_TAB_GAP))
                                    .overflow_x_scroll()
                                    .track_scroll(&self.top_tabs_scroll)
                                    .children(items),
                            ),
                    )
                    .child(
                        Button::new("top-new-tab")
                            .icon(IconName::Plus)
                            .ghost()
                            .tooltip("新建终端 (Ctrl+Shift+T)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_terminal(window, cx);
                            })),
                    )
                    .child(
                        Button::new("top-tabs-menu")
                            .icon(IconName::EllipsisVertical)
                            .ghost()
                            .selected(settings_active)
                            .tooltip("更多")
                            .dropdown_menu_with_anchor(
                                gpui::Corner::TopRight,
                                move |menu, _, _| {
                                    let shell_picker = menu_workspace.clone();
                                    let settings = menu_workspace.clone();
                                    menu.external_link_icon(false)
                                        .item(
                                            PopupMenuItem::new("选择终端")
                                                .icon(IconName::SquareTerminal)
                                                .action(Box::new(ToggleShellPicker))
                                                .on_click(move |_, window, cx| {
                                                    if let Some(workspace) = shell_picker.upgrade() {
                                                        workspace.update(cx, |this, cx| {
                                                            this.open_shell_palette(window, cx);
                                                        });
                                                    }
                                                }),
                                        )
                                        .separator()
                                        .item(
                                            PopupMenuItem::new("设置")
                                                .icon(IconName::Settings)
                                                .action(Box::new(OpenSettings))
                                                .on_click(move |_, window, cx| {
                                                    if let Some(workspace) = settings.upgrade() {
                                                        workspace.update(cx, |this, cx| {
                                                            this.open_settings(window, cx);
                                                        });
                                                    }
                                                }),
                                        )
                                },
                            ),
                    ),
            )
            // Windows Terminal 的新建 split button 紧跟 TabView；剩余空间
            // 才是可拖拽标题栏，文件树与 Git 固定在其右侧。
            .child(div().h_full().flex_1().min_w_0())
            .child(
                Button::new("top-toggle-file-tree")
                    .icon(if files_active { IconName::FolderOpen } else { IconName::FolderClosed })
                    .ghost()
                    .selected(files_active)
                    .tooltip("目录树 (Ctrl+Shift+F)")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_file_tree(cx))),
            )
            .child(
                Button::new("top-toggle-git-tree")
                    .icon(IconName::GitHub)
                    .ghost()
                    .selected(git_active)
                    .tooltip("Git 状态 (Ctrl+Shift+G)")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_git_tree(cx))),
            )
            .into_any_element()
    }

    fn on_top_tabs_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(TOP_TAB_MIN_W * 0.6));
        let delta = horizontal_wheel_delta(f32::from(delta.x), f32::from(delta.y));
        if delta == 0.0 {
            return;
        }
        let mut offset = self.top_tabs_scroll.offset();
        offset.x += px(delta);
        self.top_tabs_scroll.set_offset(offset);
        cx.stop_propagation();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_width_fills_then_clamps_to_single_row_bounds() {
        assert_eq!(tab_width(600.0, 3), 200.0);
        assert_eq!(tab_width(1200.0, 3), TOP_TAB_MAX_W);
        assert_eq!(tab_width(500.0, 10), TOP_TAB_MIN_W);
        assert_eq!(tab_width(0.0, 3), TOP_TAB_MAX_W);
        assert_eq!(tab_strip_width(200.0, 3), 608.0);
    }

    #[test]
    fn wheel_uses_the_stronger_axis_for_horizontal_tabs() {
        assert_eq!(horizontal_wheel_delta(2.0, -40.0), -40.0);
        assert_eq!(horizontal_wheel_delta(24.0, -3.0), 24.0);
    }
}
